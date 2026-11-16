//! Text exports of a meeting transcript: TXT, Markdown, JSON and SRT.
//!
//! All functions here are pure (no I/O, no clock) so they are unit-tested;
//! the command layer writes the result to `<data dir>/meetings/exports/`.

use super::types::{MeetingDetail, MeetingExportFormat, MeetingSegment, MeetingSpeaker};

/// Display name of the speaker of `segment`: the user-given name, else the
/// default label, else the source's default label.
pub fn speaker_name(speakers: &[MeetingSpeaker], segment: &MeetingSegment) -> String {
    segment
        .speaker_id
        .and_then(|id| speakers.iter().find(|speaker| speaker.id == id))
        .map(speaker_display_name)
        .unwrap_or_else(|| segment.source.default_label().to_string())
}

/// Name shown for a speaker row.
pub fn speaker_display_name(speaker: &MeetingSpeaker) -> String {
    match speaker.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => speaker.label.clone(),
    }
}

/// `HH:MM:SS` for a millisecond offset.
pub fn format_clock(ms: u64) -> String {
    let total = ms / 1000;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total / 60) % 60,
        total % 60
    )
}

/// `HH:MM:SS,mmm` (SRT timestamp).
pub fn format_srt_time(ms: u64) -> String {
    format!("{},{:03}", format_clock(ms), ms % 1000)
}

/// Segments that carry text, in timeline order.
fn spoken(detail: &MeetingDetail) -> Vec<&MeetingSegment> {
    let mut segments: Vec<&MeetingSegment> = detail
        .segments
        .iter()
        .filter(|segment| !segment.text.trim().is_empty())
        .collect();
    segments.sort_by_key(|segment| (segment.t0_ms, segment.id));
    segments
}

/// Plain transcript lines `Имя: текст` (input for the LLM summary).
pub fn transcript_for_summary(detail: &MeetingDetail) -> String {
    spoken(detail)
        .into_iter()
        .map(|segment| {
            format!(
                "{}: {}",
                speaker_name(&detail.speakers, segment),
                segment.text.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `[00:12:03] Имя: текст` per line.
pub fn to_txt(detail: &MeetingDetail) -> String {
    let mut out = String::new();
    for segment in spoken(detail) {
        out.push_str(&format!(
            "[{}] {}: {}\n",
            format_clock(segment.t0_ms),
            speaker_name(&detail.speakers, segment),
            segment.text.trim()
        ));
    }
    out
}

/// Markdown document. `date` is the pre-formatted local start date
/// (formatting with the local time zone happens in the command layer).
pub fn to_markdown(detail: &MeetingDetail, date: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", detail.info.title.trim()));
    out.push_str(&format!("- **Дата:** {date}\n"));
    out.push_str(&format!(
        "- **Длительность:** {}\n",
        format_clock(detail.info.duration_ms)
    ));
    let participants: Vec<String> = detail.speakers.iter().map(speaker_display_name).collect();
    if !participants.is_empty() {
        out.push_str(&format!("- **Участники:** {}\n", participants.join(", ")));
    }
    out.push('\n');

    if let Some(summary) = detail
        .info
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
    {
        out.push_str("## Краткое содержание\n\n");
        out.push_str(summary);
        out.push_str("\n\n");
    }

    out.push_str("## Расшифровка\n\n");
    for segment in spoken(detail) {
        out.push_str(&format!(
            "**[{}] {}:** {}\n\n",
            format_clock(segment.t0_ms),
            speaker_name(&detail.speakers, segment),
            segment.text.trim()
        ));
    }
    out
}

/// JSON with resolved speaker names next to the raw rows.
pub fn to_json(detail: &MeetingDetail) -> Result<String, String> {
    let segments: Vec<serde_json::Value> = spoken(detail)
        .into_iter()
        .map(|segment| {
            serde_json::json!({
                "start_ms": segment.t0_ms,
                "end_ms": segment.t1_ms,
                "start": format_clock(segment.t0_ms),
                "speaker": speaker_name(&detail.speakers, segment),
                "source": segment.source,
                "text": segment.text.trim(),
            })
        })
        .collect();
    let value = serde_json::json!({
        "id": detail.info.id,
        "title": detail.info.title,
        "started_at": detail.info.started_at,
        "ended_at": detail.info.ended_at,
        "duration_ms": detail.info.duration_ms,
        "summary": detail.info.summary,
        "speakers": detail.speakers.iter().map(|speaker| serde_json::json!({
            "id": speaker.id,
            "source": speaker.source,
            "name": speaker_display_name(speaker),
        })).collect::<Vec<_>>(),
        "segments": segments,
    });
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

/// SubRip subtitles, one cue per segment.
pub fn to_srt(detail: &MeetingDetail) -> String {
    let mut out = String::new();
    for (index, segment) in spoken(detail).into_iter().enumerate() {
        let end = segment.t1_ms.max(segment.t0_ms + 1);
        out.push_str(&format!(
            "{}\n{} --> {}\n{}: {}\n\n",
            index + 1,
            format_srt_time(segment.t0_ms),
            format_srt_time(end),
            speaker_name(&detail.speakers, segment),
            segment.text.trim()
        ));
    }
    out
}

/// Render `detail` in `format`.
pub fn render(
    detail: &MeetingDetail,
    format: MeetingExportFormat,
    date: &str,
) -> Result<String, String> {
    match format {
        MeetingExportFormat::Txt => Ok(to_txt(detail)),
        MeetingExportFormat::Md => Ok(to_markdown(detail, date)),
        MeetingExportFormat::Json => to_json(detail),
        MeetingExportFormat::Srt => Ok(to_srt(detail)),
    }
}

/// ASCII file name for an export: `meeting-<id>-<date>.<ext>`.
pub fn export_file_name(meeting_id: i64, date_slug: &str, format: MeetingExportFormat) -> String {
    let slug: String = date_slug
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("meeting-{meeting_id}-{slug}.{}", format.extension())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::types::{MeetingInfo, MeetingRecordStatus, MeetingSource, MeetingSources};

    fn detail() -> MeetingDetail {
        MeetingDetail {
            info: MeetingInfo {
                id: 7,
                title: "Планёрка".to_string(),
                started_at: 0,
                ended_at: Some(3_725_000),
                duration_ms: 3_725_000,
                sources: MeetingSources {
                    microphone: true,
                    system_audio: true,
                },
                segment_count: 3,
                speaker_count: 2,
                status: MeetingRecordStatus::Completed,
                summary: Some("Договорились о сроках.".to_string()),
            },
            speakers: vec![
                MeetingSpeaker {
                    id: 1,
                    meeting_id: 7,
                    slot: 0,
                    source: MeetingSource::Microphone,
                    label: "Я".to_string(),
                    name: None,
                    color_index: 0,
                },
                MeetingSpeaker {
                    id: 2,
                    meeting_id: 7,
                    slot: 1,
                    source: MeetingSource::System,
                    label: "Собеседник".to_string(),
                    name: Some("Иван".to_string()),
                    color_index: 1,
                },
            ],
            segments: vec![
                seg(
                    11,
                    723_000,
                    725_500,
                    Some(2),
                    MeetingSource::System,
                    "Добрый день",
                ),
                seg(
                    10,
                    1_200,
                    3_000,
                    Some(1),
                    MeetingSource::Microphone,
                    " Привет ",
                ),
                seg(
                    12,
                    800_000,
                    801_000,
                    Some(1),
                    MeetingSource::Microphone,
                    "   ",
                ),
            ],
        }
    }

    fn seg(
        id: i64,
        t0: u64,
        t1: u64,
        speaker: Option<i64>,
        source: MeetingSource,
        text: &str,
    ) -> MeetingSegment {
        MeetingSegment {
            id,
            meeting_id: 7,
            t0_ms: t0,
            t1_ms: t1,
            speaker_id: speaker,
            source,
            text: text.to_string(),
            is_final: true,
        }
    }

    #[test]
    fn clock_formats() {
        assert_eq!(format_clock(0), "00:00:00");
        assert_eq!(format_clock(723_999), "00:12:03");
        assert_eq!(format_clock(3_725_000), "01:02:05");
        assert_eq!(format_srt_time(1_234), "00:00:01,234");
    }

    #[test]
    fn txt_is_sorted_named_and_skips_empty() {
        assert_eq!(
            to_txt(&detail()),
            "[00:00:01] Я: Привет\n[00:12:03] Иван: Добрый день\n"
        );
    }

    #[test]
    fn speaker_falls_back_to_source_label() {
        let mut d = detail();
        d.segments[0].speaker_id = None;
        assert!(to_txt(&d).contains("Собеседник: Добрый день"));
    }

    #[test]
    fn markdown_has_header_summary_and_lines() {
        let md = to_markdown(&detail(), "16.11.2026 10:00");
        assert!(md.starts_with("# Планёрка\n\n"));
        assert!(md.contains("- **Дата:** 16.11.2026 10:00\n"));
        assert!(md.contains("- **Длительность:** 01:02:05\n"));
        assert!(md.contains("- **Участники:** Я, Иван\n"));
        assert!(md.contains("## Краткое содержание\n\nДоговорились о сроках.\n"));
        assert!(md.contains("**[00:12:03] Иван:** Добрый день\n"));
        let mut no_summary = detail();
        no_summary.info.summary = None;
        assert!(!to_markdown(&no_summary, "x").contains("Краткое содержание"));
    }

    #[test]
    fn srt_cues() {
        assert_eq!(
            to_srt(&detail()),
            "1\n00:00:01,200 --> 00:00:03,000\nЯ: Привет\n\n\
             2\n00:12:03,000 --> 00:12:05,500\nИван: Добрый день\n\n"
        );
    }

    #[test]
    fn json_contains_resolved_names() {
        let json = to_json(&detail()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["segments"].as_array().unwrap().len(), 2);
        assert_eq!(value["segments"][1]["speaker"], "Иван");
        assert_eq!(value["segments"][0]["source"], "microphone");
        assert_eq!(value["speakers"][1]["name"], "Иван");
    }

    #[test]
    fn summary_transcript_lines() {
        assert_eq!(
            transcript_for_summary(&detail()),
            "Я: Привет\nИван: Добрый день"
        );
    }

    #[test]
    fn file_name_is_ascii() {
        assert_eq!(
            export_file_name(7, "2026-11-16 10:00", MeetingExportFormat::Md),
            "meeting-7-2026-11-16-10-00.md"
        );
    }
}
