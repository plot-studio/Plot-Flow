//! Remote speakers of the computer-sound track: «Собеседник 1», «Собеседник 2»…
//!
//! The diarizer is NVIDIA Streaming Sortformer 4spk v2.1 (GGUF, run through
//! transcribe.cpp in its own `Model`/`Session`, never the speech engine).
//! transcribe.cpp 0.2 exposes Sortformer only as an offline `run` over one
//! buffer, and its speaker ids are arrival-ordered *within that run*. To keep
//! ids stable for the whole meeting, every system segment is diarized together
//! with short enrollment clips of the speakers already known, placed in front
//! of it ([`SpeakerTracker::prepare`]). The id the model gives a clip tells
//! which known speaker that id is; an id without a clip is a new speaker
//! (up to [`MAX_REMOTE_SPEAKERS`], the model limit).
//!
//! The pure logic (turn arithmetic, split planning, id mapping) is separate
//! from the engine so it can be unit-tested without the model.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use transcribe_cpp::{
    Backend, Model, ModelOptions, RunExtension, RunOptions, Session, SessionOptions,
    SortformerPreset, SortformerStreamOptions,
};

/// Sample rate of the meeting pipeline and of the model.
pub const SAMPLE_RATE: u64 = 16_000;
/// Sortformer 4spk tells at most four speakers apart in one run.
pub const MAX_REMOTE_SPEAKERS: u8 = 4;
/// A speaker change splits a segment only when each side is at least this long.
pub const MIN_PART_MS: u64 = 1_500;
/// Less talk than this inside a segment is a blip (cough, backchannel), not a speaker.
const MIN_TALK_MS: u64 = 400;
/// An id the model has not matched to a known clip becomes a new remote
/// speaker only with at least this much talk in the segment.
const MIN_NEW_SPEAKER_MS: u64 = 1_200;
/// Resolution of the per-segment speaker labelling.
const GRID_MS: u64 = 80;
/// Enrollment clip length bounds. The first clip of a speaker may be short.
const ENROLL_FIRST_MIN_MS: u64 = 1_000;
const ENROLL_MIN_MS: u64 = 2_000;
const ENROLL_MAX_MS: u64 = 6_000;
/// Silence between the clips and before the segment.
const ENROLL_GAP_MS: u64 = 500;
/// A clip is attributed to a model id that covers at least this share of it.
const ENROLL_MATCH_RATIO: f32 = 0.4;
/// Share of the talk in a clip candidate that must belong to its speaker.
const ENROLL_PURITY: f32 = 0.85;
const ENROLL_FIRST_PURITY: f32 = 0.7;
/// Where a cut between two speakers may move to find a quiet spot.
const CUT_SEARCH_MS: u64 = 300;
const CUT_WINDOW_MS: u64 = 20;
/// CPU threads for the diarizer (the speech engine also runs on the CPU).
const DIARIZER_THREADS: i32 = 4;

pub fn ms_to_samples(ms: u64) -> usize {
    (ms * SAMPLE_RATE / 1000) as usize
}

pub fn samples_to_ms(samples: usize) -> u64 {
    samples as u64 * 1000 / SAMPLE_RATE
}

fn overlap(a0: u64, a1: u64, b0: u64, b1: u64) -> u64 {
    a1.min(b1).saturating_sub(a0.max(b0))
}

/* ───────────────────────────── Turns ───────────────────────────── */

/// One "speaker X is active" run reported by the model. `speaker` is the
/// model's id for this run only (1-based, arrival order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Turn {
    pub t0_ms: u64,
    pub t1_ms: u64,
    pub speaker: i32,
}

impl Turn {
    pub fn new(t0_ms: u64, t1_ms: u64, speaker: i32) -> Self {
        Self {
            t0_ms,
            t1_ms,
            speaker,
        }
    }
}

/// Total talk per model id inside `[t0, t1)`, longest first (ties: lower id).
pub fn talk_time(turns: &[Turn], t0: u64, t1: u64) -> Vec<(i32, u64)> {
    let mut acc: Vec<(i32, u64)> = Vec::new();
    for turn in turns {
        let ms = overlap(turn.t0_ms, turn.t1_ms, t0, t1);
        if ms == 0 {
            continue;
        }
        match acc.iter_mut().find(|(speaker, _)| *speaker == turn.speaker) {
            Some(entry) => entry.1 += ms,
            None => acc.push((turn.speaker, ms)),
        }
    }
    acc.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    acc
}

/// The id that talks most inside `[t0, t1)`, if anyone does.
pub fn dominant_speaker(turns: &[Turn], t0: u64, t1: u64) -> Option<i32> {
    talk_time(turns, t0, t1)
        .first()
        .map(|(speaker, _)| *speaker)
}

/// Turns of a whole run shifted into segment coordinates: the segment starts
/// at `offset_ms` of the run and lasts `len_ms`.
pub fn segment_turns(turns: &[Turn], offset_ms: u64, len_ms: u64) -> Vec<Turn> {
    turns
        .iter()
        .filter_map(|turn| {
            let t0 = turn.t0_ms.saturating_sub(offset_ms).min(len_ms);
            let t1 = turn.t1_ms.saturating_sub(offset_ms).min(len_ms);
            (t1 > t0).then_some(Turn::new(t0, t1, turn.speaker))
        })
        .collect()
}

/* ───────────────────────────── Split planning ───────────────────────────── */

/// A piece of a segment attributed to one model id (`None`: nobody detected).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    pub t0_ms: u64,
    pub t1_ms: u64,
    pub speaker: Option<i32>,
}

impl Part {
    fn len(&self) -> u64 {
        self.t1_ms - self.t0_ms
    }
}

/// Cut a segment of `len_ms` into single-speaker parts. Every part is at
/// least `min_part_ms` long (unless the segment itself is shorter): a
/// speaker change splits the segment only when both sides talk that long.
/// Parts cover `[0, len_ms)` without gaps.
pub fn plan_parts(turns: &[Turn], len_ms: u64, min_part_ms: u64) -> Vec<Part> {
    let whole = |speaker| {
        vec![Part {
            t0_ms: 0,
            t1_ms: len_ms,
            speaker,
        }]
    };
    if len_ms == 0 {
        return whole(None);
    }
    let talk = talk_time(turns, 0, len_ms);
    let kept: Vec<i32> = talk
        .iter()
        .filter(|(_, ms)| *ms >= MIN_TALK_MS)
        .map(|(speaker, _)| *speaker)
        .collect();
    if kept.is_empty() {
        return whole(talk.first().map(|(speaker, _)| *speaker));
    }
    if kept.len() == 1 {
        return whole(Some(kept[0]));
    }

    // Label a fixed grid with the most active speaker of each cell.
    let cells = len_ms.div_ceil(GRID_MS) as usize;
    let mut labels: Vec<Option<i32>> = Vec::with_capacity(cells);
    for i in 0..cells {
        let c0 = i as u64 * GRID_MS;
        let c1 = (c0 + GRID_MS).min(len_ms);
        let prev = labels.last().copied().flatten();
        let mut best: Option<(i32, u64)> = None;
        for turn in turns.iter().filter(|t| kept.contains(&t.speaker)) {
            let ms = overlap(turn.t0_ms, turn.t1_ms, c0, c1);
            if ms == 0 {
                continue;
            }
            best = match best {
                None => Some((turn.speaker, ms)),
                Some((_, best_ms)) if ms > best_ms => Some((turn.speaker, ms)),
                // Overlapping speech: stay with the current speaker.
                Some((_, best_ms)) if ms == best_ms && Some(turn.speaker) == prev => {
                    Some((turn.speaker, ms))
                }
                keep => keep,
            };
        }
        labels.push(best.map(|(speaker, _)| speaker));
    }
    // Pauses belong to the speaker before them (leading ones to the first).
    let first = labels.iter().flatten().next().copied();
    let mut current = first;
    for label in labels.iter_mut() {
        if label.is_some() {
            current = *label;
        } else {
            *label = current;
        }
    }

    let mut parts: Vec<Part> = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        let c0 = i as u64 * GRID_MS;
        let c1 = (c0 + GRID_MS).min(len_ms);
        match parts.last_mut() {
            Some(last) if last.speaker == *label => last.t1_ms = c1,
            _ => parts.push(Part {
                t0_ms: c0,
                t1_ms: c1,
                speaker: *label,
            }),
        }
    }

    // Absorb parts that are too short into their longer neighbour.
    loop {
        coalesce(&mut parts);
        if parts.len() <= 1 {
            break;
        }
        let (idx, shortest) = parts
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.len()))
            .min_by_key(|(_, len)| *len)
            .unwrap_or((0, u64::MAX));
        if shortest >= min_part_ms {
            break;
        }
        let left = idx.checked_sub(1);
        let right = (idx + 1 < parts.len()).then_some(idx + 1);
        let target = match (left, right) {
            (Some(l), Some(r)) => {
                if parts[l].len() >= parts[r].len() {
                    l
                } else {
                    r
                }
            }
            (Some(l), None) => l,
            (None, Some(r)) => r,
            (None, None) => break,
        };
        if target < idx {
            parts[target].t1_ms = parts[idx].t1_ms;
        } else {
            parts[target].t0_ms = parts[idx].t0_ms;
        }
        parts.remove(idx);
    }
    parts
}

/// Merge neighbours with the same speaker.
fn coalesce(parts: &mut Vec<Part>) {
    let mut out: Vec<Part> = Vec::with_capacity(parts.len());
    for part in parts.drain(..) {
        match out.last_mut() {
            Some(last) if last.speaker == part.speaker => last.t1_ms = part.t1_ms,
            _ => out.push(part),
        }
    }
    *parts = out;
}

/// Move a cut at `cut_ms` to the quietest 20 ms window within ±300 ms, so a
/// split does not chop a word in half.
pub fn refine_cut(samples: &[f32], cut_ms: u64) -> u64 {
    let len_ms = samples_to_ms(samples.len());
    let from = cut_ms.saturating_sub(CUT_SEARCH_MS);
    let to = (cut_ms + CUT_SEARCH_MS).min(len_ms.saturating_sub(CUT_WINDOW_MS));
    if to <= from {
        return cut_ms;
    }
    let energy = |t0: u64| -> f32 {
        let a = ms_to_samples(t0).min(samples.len());
        let b = ms_to_samples(t0 + CUT_WINDOW_MS).min(samples.len());
        samples[a..b].iter().map(|s| s * s).sum::<f32>()
    };
    let mut best = (cut_ms, f32::MAX);
    let mut t = from;
    while t <= to {
        let e = energy(t);
        // Prefer the window closest to the model's cut among equals.
        let closer = t.abs_diff(cut_ms) < best.0.abs_diff(cut_ms);
        if e < best.1 || (e == best.1 && closer) {
            best = (t, e);
        }
        t += CUT_WINDOW_MS / 2;
    }
    best.0 + CUT_WINDOW_MS / 2
}

/* ───────────────────────────── Id mapping ───────────────────────────── */

/// Where an enrollment clip of remote speaker `slot` sits in a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipSpan {
    pub slot: u8,
    pub t0_ms: u64,
    pub t1_ms: u64,
}

/// Map the model ids of one run to known remote speakers through the
/// enrollment clips: an id covering enough of a clip is that clip's speaker
/// (the clip it covers most, when it covers several).
pub fn map_clips(turns: &[Turn], clips: &[ClipSpan]) -> HashMap<i32, u8> {
    let mut best: HashMap<i32, (u8, u64)> = HashMap::new();
    for clip in clips {
        let len = clip.t1_ms.saturating_sub(clip.t0_ms);
        for (speaker, ms) in talk_time(turns, clip.t0_ms, clip.t1_ms) {
            if (ms as f32) < len as f32 * ENROLL_MATCH_RATIO {
                continue;
            }
            let entry = best.entry(speaker).or_insert((clip.slot, 0));
            if ms > entry.1 {
                *entry = (clip.slot, ms);
            }
        }
    }
    best.into_iter()
        .map(|(speaker, (slot, _))| (speaker, slot))
        .collect()
}

/// A piece of a segment attributed to remote speaker `slot` (1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assigned {
    pub t0_ms: u64,
    pub t1_ms: u64,
    pub slot: u8,
}

/// The model input for one segment: enrollment clips, gaps, then the segment.
pub struct Prepared {
    pub input: Vec<f32>,
    pub clips: Vec<ClipSpan>,
    /// Where the segment starts in `input`.
    pub offset_ms: u64,
}

/// Remote speakers known so far in one meeting and their enrollment clips.
#[derive(Default)]
pub struct SpeakerTracker {
    /// `(slot, clip)`, at most one clip per slot.
    clips: Vec<(u8, Vec<f32>)>,
    /// Number of remote speakers created (slots `1..=count`).
    count: u8,
    last_slot: Option<u8>,
}

impl SpeakerTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remote speakers created so far.
    pub fn speaker_count(&self) -> u8 {
        self.count
    }

    /// Length of the enrollment clip of `slot` (0 when none).
    pub fn clip_ms(&self, slot: u8) -> u64 {
        self.clips
            .iter()
            .find(|(s, _)| *s == slot)
            .map_or(0, |(_, clip)| samples_to_ms(clip.len()))
    }

    /// Build the model input for `segment`.
    pub fn prepare(&self, segment: &[f32]) -> Prepared {
        let gap = vec![0.0f32; ms_to_samples(ENROLL_GAP_MS)];
        let clip_total: usize = self.clips.iter().map(|(_, c)| c.len() + gap.len()).sum();
        let mut input = Vec::with_capacity(clip_total + segment.len());
        let mut clips = Vec::with_capacity(self.clips.len());
        for (slot, clip) in &self.clips {
            let t0_ms = samples_to_ms(input.len());
            input.extend_from_slice(clip);
            let t1_ms = samples_to_ms(input.len());
            clips.push(ClipSpan {
                slot: *slot,
                t0_ms,
                t1_ms,
            });
            input.extend_from_slice(&gap);
        }
        let offset_ms = samples_to_ms(input.len());
        input.extend_from_slice(segment);
        Prepared {
            input,
            clips,
            offset_ms,
        }
    }

    /// Attribute `segment` (whose run produced `turns`, in run coordinates)
    /// to remote speakers, creating new ones on first appearance and keeping
    /// the enrollment clips up to date. Returns pieces covering the segment.
    pub fn assign(
        &mut self,
        prepared: &Prepared,
        turns: &[Turn],
        segment: &[f32],
    ) -> Vec<Assigned> {
        let len_ms = samples_to_ms(segment.len());
        let mapped = map_clips(turns, &prepared.clips);
        let local = segment_turns(turns, prepared.offset_ms, len_ms);
        let parts = plan_parts(&local, len_ms, MIN_PART_MS);
        let talk = talk_time(&local, 0, len_ms);
        let talk_of = |speaker: i32| {
            talk.iter()
                .find(|(s, _)| *s == speaker)
                .map_or(0, |(_, ms)| *ms)
        };

        let mut created: HashMap<i32, u8> = HashMap::new();
        let mut out: Vec<(Part, u8, bool)> = Vec::with_capacity(parts.len());
        for part in parts {
            let resolved = part.speaker.and_then(|speaker| {
                if let Some(slot) = mapped.get(&speaker).or_else(|| created.get(&speaker)) {
                    return Some(*slot);
                }
                let first_ever = self.count == 0;
                if self.count < MAX_REMOTE_SPEAKERS
                    && (first_ever || talk_of(speaker) >= MIN_NEW_SPEAKER_MS)
                {
                    self.count += 1;
                    created.insert(speaker, self.count);
                    return Some(self.count);
                }
                None
            });
            let slot = match resolved.or(self.last_slot) {
                Some(slot) => slot,
                None => {
                    // Nobody known yet and nobody detected: the first speaker.
                    self.count = self.count.max(1);
                    1
                }
            };
            // Only ids the model itself tied to this speaker may update its clip.
            out.push((part, slot, resolved.is_some()));
        }

        for (part, slot, _) in out.iter().filter(|(_, _, own)| *own) {
            if let Some(speaker) = part.speaker {
                self.maybe_enroll(*slot, speaker, part, &local, segment);
            }
        }

        let mut assigned: Vec<Assigned> = Vec::with_capacity(out.len());
        for (part, slot, _) in out {
            match assigned.last_mut() {
                Some(last) if last.slot == slot => last.t1_ms = part.t1_ms,
                _ => assigned.push(Assigned {
                    t0_ms: part.t0_ms,
                    t1_ms: part.t1_ms,
                    slot,
                }),
            }
        }
        self.last_slot = assigned.last().map(|a| a.slot).or(self.last_slot);
        assigned
    }

    /// Keep (or improve) the enrollment clip of `slot` from this part when the
    /// part is clean enough.
    fn maybe_enroll(
        &mut self,
        slot: u8,
        speaker: i32,
        part: &Part,
        turns: &[Turn],
        segment: &[f32],
    ) {
        let existing = self.clip_ms(slot);
        if existing >= ENROLL_MAX_MS {
            return;
        }
        // The speaker's own stretch inside the part.
        let own: Vec<&Turn> = turns
            .iter()
            .filter(|t| {
                t.speaker == speaker && overlap(t.t0_ms, t.t1_ms, part.t0_ms, part.t1_ms) > 0
            })
            .collect();
        let (Some(first), Some(last)) = (own.first(), own.last()) else {
            return;
        };
        let span0 = first.t0_ms.max(part.t0_ms);
        let span1 = own
            .iter()
            .map(|t| t.t1_ms)
            .max()
            .unwrap_or(last.t1_ms)
            .min(part.t1_ms);
        if span1 <= span0 {
            return;
        }
        let talk = talk_time(turns, span0, span1);
        let total: u64 = talk.iter().map(|(_, ms)| ms).sum();
        let mine = talk
            .iter()
            .find(|(s, _)| *s == speaker)
            .map_or(0, |(_, ms)| *ms);
        if total == 0 {
            return;
        }
        let purity = mine as f32 / total as f32;
        let len = span1 - span0;
        let (min_len, min_purity) = if existing == 0 {
            (ENROLL_FIRST_MIN_MS, ENROLL_FIRST_PURITY)
        } else {
            (ENROLL_MIN_MS, ENROLL_PURITY)
        };
        if len < min_len || purity < min_purity || len <= existing {
            return;
        }
        // Middle of the stretch, at most ENROLL_MAX_MS.
        let take = len.min(ENROLL_MAX_MS);
        let start = span0 + (len - take) / 2;
        let a = ms_to_samples(start).min(segment.len());
        let b = ms_to_samples(start + take).min(segment.len());
        if b <= a {
            return;
        }
        let clip = segment[a..b].to_vec();
        match self.clips.iter_mut().find(|(s, _)| *s == slot) {
            Some(entry) => entry.1 = clip,
            None => {
                self.clips.push((slot, clip));
                self.clips.sort_by_key(|(s, _)| *s);
            }
        }
    }
}

/* ───────────────────────────── Engine ───────────────────────────── */

/// A loaded Sortformer model with its own session.
pub struct SortformerEngine {
    session: Session,
    backend: String,
    preset: SortformerPreset,
}

impl SortformerEngine {
    /// Load the GGUF at `path`. `cpu_only` keeps it off the GPU. transcribe.cpp
    /// backends must already be initialized (the app does it at startup).
    pub fn load(path: &Path, cpu_only: bool) -> Result<Self, String> {
        let started = Instant::now();
        let options = ModelOptions {
            backend: if cpu_only {
                Backend::Cpu
            } else {
                Backend::Auto
            },
            device: None,
        };
        let model = Model::load_with(path, &options)
            .map_err(|e| format!("не удалось загрузить модель собеседников: {e}"))?;
        let arch = model.arch();
        if arch != "sortformer" {
            return Err(format!("это не модель Sortformer (arch «{arch}»)"));
        }
        let session = model
            .session_with(&SessionOptions {
                n_threads: DIARIZER_THREADS,
                ..SessionOptions::default()
            })
            .map_err(|e| format!("не удалось открыть сессию модели собеседников: {e}"))?;
        let backend = model.backend();
        log::info!(
            "Diarizer loaded on {backend} in {} ms ({})",
            started.elapsed().as_millis(),
            path.display()
        );
        Ok(Self {
            session,
            backend,
            preset: SortformerPreset::VeryHighLatency,
        })
    }

    /// Compute backend the model ended up on ("cpu", "vulkan", …).
    pub fn backend(&self) -> &str {
        &self.backend
    }

    /// Operating point (default: the offline, most accurate one).
    pub fn set_preset(&mut self, preset: SortformerPreset) {
        self.preset = preset;
    }

    /// Who speaks when in `pcm` (16 kHz mono).
    pub fn run(&mut self, pcm: &[f32]) -> Result<Vec<Turn>, String> {
        let options = RunOptions {
            family: Some(RunExtension::Sortformer(SortformerStreamOptions {
                preset: Some(self.preset),
            })),
            ..RunOptions::default()
        };
        let transcript = self
            .session
            .run(pcm, &options)
            .map_err(|e| format!("ошибка модели собеседников: {e}"))?;
        let mut turns: Vec<Turn> = transcript
            .speaker_segments
            .iter()
            .filter(|s| s.speaker_id > 0 && s.t1_ms > s.t0_ms && s.t0_ms >= 0)
            .map(|s| Turn::new(s.t0_ms as u64, s.t1_ms as u64, s.speaker_id))
            .collect();
        turns.sort_by_key(|t| (t.t0_ms, t.speaker));
        Ok(turns)
    }

    /// Diarize one segment against the known speakers: run the model on
    /// clips + segment and attribute the segment.
    pub fn diarize_segment(
        &mut self,
        tracker: &mut SpeakerTracker,
        segment: &[f32],
    ) -> Result<Vec<Assigned>, String> {
        let prepared = tracker.prepare(segment);
        let turns = self.run(&prepared.input)?;
        Ok(tracker.assign(&prepared, &turns, segment))
    }
}

/// Split `segment` at the boundaries of `assigned`, moving each cut to a
/// quiet spot. Returns `(t0_ms, t1_ms, slot)` relative to the segment start.
pub fn snap_pieces(assigned: &[Assigned], segment: &[f32]) -> Vec<Assigned> {
    let mut out: Vec<Assigned> = assigned.to_vec();
    for i in 1..out.len() {
        let cut = refine_cut(segment, out[i].t0_ms);
        let lo = out[i - 1].t0_ms + 1;
        let hi = out[i].t1_ms.saturating_sub(1);
        let cut = cut.clamp(lo.min(hi), hi.max(lo));
        out[i - 1].t1_ms = cut;
        out[i].t0_ms = cut;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(t0: u64, t1: u64, s: i32) -> Turn {
        Turn::new(t0, t1, s)
    }

    #[test]
    fn talk_time_sums_per_speaker_inside_span() {
        let turns = [t(0, 1_000, 1), t(1_500, 2_000, 1), t(800, 3_000, 2)];
        assert_eq!(talk_time(&turns, 0, 3_000), vec![(2, 2_200), (1, 1_500)]);
        assert_eq!(talk_time(&turns, 1_600, 1_800), vec![(1, 200), (2, 200)]);
        assert!(talk_time(&turns, 5_000, 6_000).is_empty());
    }

    #[test]
    fn dominant_speaker_over_span() {
        let turns = [t(0, 4_000, 1), t(4_000, 5_000, 2)];
        assert_eq!(dominant_speaker(&turns, 0, 5_000), Some(1));
        assert_eq!(dominant_speaker(&turns, 3_900, 5_000), Some(2));
        assert_eq!(dominant_speaker(&turns, 6_000, 7_000), None);
    }

    #[test]
    fn segment_turns_shift_and_clip() {
        let turns = [t(0, 2_000, 1), t(2_500, 4_000, 2), t(4_500, 9_000, 1)];
        let local = segment_turns(&turns, 3_000, 5_000);
        assert_eq!(local, vec![t(0, 1_000, 2), t(1_500, 5_000, 1)]);
    }

    #[test]
    fn single_speaker_is_not_split() {
        let turns = [t(100, 2_000, 1), t(2_400, 5_000, 1)];
        let parts = plan_parts(&turns, 5_000, MIN_PART_MS);
        assert_eq!(
            parts,
            vec![Part {
                t0_ms: 0,
                t1_ms: 5_000,
                speaker: Some(1)
            }]
        );
    }

    #[test]
    fn clear_change_splits_segment() {
        let turns = [t(0, 3_000, 1), t(3_200, 6_000, 2)];
        let parts = plan_parts(&turns, 6_000, MIN_PART_MS);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].speaker, Some(1));
        assert_eq!(parts[1].speaker, Some(2));
        assert_eq!(parts[0].t0_ms, 0);
        assert_eq!(parts[1].t1_ms, 6_000);
        assert_eq!(parts[0].t1_ms, parts[1].t0_ms);
        // The pause belongs to the speaker before it.
        assert!(parts[0].t1_ms >= 3_000 && parts[0].t1_ms <= 3_280);
    }

    #[test]
    fn short_interjection_does_not_split() {
        // Speaker 2 talks 1.0 s (< 1.5 s) in the middle: no split.
        let turns = [t(0, 3_000, 1), t(3_000, 4_000, 2), t(4_000, 8_000, 1)];
        let parts = plan_parts(&turns, 8_000, MIN_PART_MS);
        assert_eq!(
            parts,
            vec![Part {
                t0_ms: 0,
                t1_ms: 8_000,
                speaker: Some(1)
            }]
        );
    }

    #[test]
    fn short_tail_is_absorbed() {
        let turns = [t(0, 5_000, 1), t(5_000, 6_000, 2)];
        let parts = plan_parts(&turns, 6_000, MIN_PART_MS);
        assert_eq!(
            parts,
            vec![Part {
                t0_ms: 0,
                t1_ms: 6_000,
                speaker: Some(1)
            }]
        );
    }

    #[test]
    fn blips_and_silence() {
        // Only a 200 ms blip: the segment keeps that id but is not split.
        let parts = plan_parts(&[t(100, 300, 3)], 2_000, MIN_PART_MS);
        assert_eq!(
            parts,
            vec![Part {
                t0_ms: 0,
                t1_ms: 2_000,
                speaker: Some(3)
            }]
        );
        // Nothing at all.
        let parts = plan_parts(&[], 2_000, MIN_PART_MS);
        assert_eq!(
            parts,
            vec![Part {
                t0_ms: 0,
                t1_ms: 2_000,
                speaker: None
            }]
        );
    }

    #[test]
    fn three_turns_give_three_parts() {
        let turns = [t(0, 2_000, 1), t(2_000, 4_000, 2), t(4_000, 6_500, 1)];
        let parts = plan_parts(&turns, 6_500, MIN_PART_MS);
        let speakers: Vec<_> = parts.iter().map(|p| p.speaker).collect();
        assert_eq!(speakers, vec![Some(1), Some(2), Some(1)]);
        assert!(parts.iter().all(|p| p.len() >= MIN_PART_MS));
    }

    #[test]
    fn clips_map_model_ids_to_known_speakers() {
        let clips = [
            ClipSpan {
                slot: 1,
                t0_ms: 0,
                t1_ms: 4_000,
            },
            ClipSpan {
                slot: 2,
                t0_ms: 4_500,
                t1_ms: 8_500,
            },
        ];
        // The model calls slot 1 "2" and slot 2 "1" (any order works).
        let turns = [
            t(100, 3_900, 2),
            t(4_600, 8_400, 1),
            t(9_000, 12_000, 1),
            t(12_000, 15_000, 3),
        ];
        let map = map_clips(&turns, &clips);
        assert_eq!(map.get(&2), Some(&1));
        assert_eq!(map.get(&1), Some(&2));
        assert_eq!(map.get(&3), None);
        // A barely covered clip is not matched.
        let weak = [t(0, 500, 4)];
        assert!(map_clips(&weak, &clips).is_empty());
    }

    fn tone(ms: u64) -> Vec<f32> {
        (0..ms_to_samples(ms))
            .map(|i| ((i as f32) * 0.05).sin() * 0.3)
            .collect()
    }

    #[test]
    fn tracker_creates_and_reuses_slots() {
        let mut tracker = SpeakerTracker::new();
        // First segment: one speaker for 4 s → slot 1, clip enrolled.
        let seg = tone(4_000);
        let prepared = tracker.prepare(&seg);
        assert_eq!(prepared.offset_ms, 0);
        let got = tracker.assign(&prepared, &[t(0, 4_000, 1)], &seg);
        assert_eq!(
            got,
            vec![Assigned {
                t0_ms: 0,
                t1_ms: 4_000,
                slot: 1
            }]
        );
        assert_eq!(tracker.speaker_count(), 1);
        assert_eq!(tracker.clip_ms(1), 4_000);

        // Second segment: the clip comes first; the model calls the clip "1"
        // and a new voice "2" → slot 2.
        let seg = tone(3_000);
        let prepared = tracker.prepare(&seg);
        assert_eq!(
            prepared.clips,
            vec![ClipSpan {
                slot: 1,
                t0_ms: 0,
                t1_ms: 4_000
            }]
        );
        assert_eq!(prepared.offset_ms, 4_500);
        let turns = [t(0, 4_000, 1), t(4_500, 7_500, 2)];
        let got = tracker.assign(&prepared, &turns, &seg);
        assert_eq!(
            got,
            vec![Assigned {
                t0_ms: 0,
                t1_ms: 3_000,
                slot: 2
            }]
        );
        assert_eq!(tracker.speaker_count(), 2);

        // Third: the model numbers by arrival, so the known clips may come
        // back with other ids; the mapping still finds slot 1.
        let seg = tone(5_000);
        let prepared = tracker.prepare(&seg);
        assert_eq!(prepared.clips.len(), 2);
        let s1 = prepared.clips[0];
        let s2 = prepared.clips[1];
        let off = prepared.offset_ms;
        let turns = [
            t(s1.t0_ms, s1.t1_ms, 2),
            t(s2.t0_ms, s2.t1_ms, 1),
            t(off, off + 2_500, 2),
            t(off + 2_500, off + 5_000, 1),
        ];
        let got = tracker.assign(&prepared, &turns, &seg);
        let slots: Vec<u8> = got.iter().map(|a| a.slot).collect();
        assert_eq!(slots, vec![1, 2]);
        assert_eq!(got[0].t0_ms, 0);
        assert_eq!(got[1].t1_ms, 5_000);
        assert_eq!(tracker.speaker_count(), 2);
    }

    #[test]
    fn short_unknown_voice_is_not_a_new_speaker() {
        let mut tracker = SpeakerTracker::new();
        let seg = tone(4_000);
        let p = tracker.prepare(&seg);
        tracker.assign(&p, &[t(0, 4_000, 1)], &seg);
        // Next segment: unmatched id with only 800 ms of talk → last speaker.
        let seg = tone(1_000);
        let p = tracker.prepare(&seg);
        let off = p.offset_ms;
        let turns = [t(0, 4_000, 1), t(off, off + 800, 2)];
        let got = tracker.assign(&p, &turns, &seg);
        assert_eq!(
            got,
            vec![Assigned {
                t0_ms: 0,
                t1_ms: 1_000,
                slot: 1
            }]
        );
        assert_eq!(tracker.speaker_count(), 1);
    }

    #[test]
    fn at_most_four_remote_speakers() {
        let mut tracker = SpeakerTracker::new();
        for round in 0..6 {
            let seg = tone(3_000);
            let p = tracker.prepare(&seg);
            // The model never matches the clips: every voice looks new.
            let off = p.offset_ms;
            let got = tracker.assign(&p, &[t(off, off + 3_000, 9 + round)], &seg);
            assert_eq!(got.len(), 1);
            assert!(got[0].slot >= 1 && got[0].slot <= MAX_REMOTE_SPEAKERS);
        }
        assert_eq!(tracker.speaker_count(), MAX_REMOTE_SPEAKERS);
    }

    #[test]
    fn nobody_detected_falls_back_to_first_or_last_speaker() {
        let mut tracker = SpeakerTracker::new();
        let seg = tone(1_000);
        let p = tracker.prepare(&seg);
        let got = tracker.assign(&p, &[], &seg);
        assert_eq!(
            got,
            vec![Assigned {
                t0_ms: 0,
                t1_ms: 1_000,
                slot: 1
            }]
        );
        assert_eq!(tracker.speaker_count(), 1);
    }

    #[test]
    fn cut_moves_to_the_quiet_spot() {
        // Loud 0–1000 ms, silence 1100–1200 ms, loud after.
        let mut samples = tone(2_000);
        let (a, b) = (ms_to_samples(1_100), ms_to_samples(1_200));
        samples[a..b].iter_mut().for_each(|s| *s = 0.0);
        let cut = refine_cut(&samples, 1_000);
        assert!((1_100..=1_200).contains(&cut), "cut at {cut}");
        // Out-of-range cut on a short buffer is returned as is.
        assert_eq!(refine_cut(&samples[..100], 3), 3);
    }

    #[test]
    fn snap_keeps_pieces_contiguous() {
        let seg = tone(6_000);
        let assigned = [
            Assigned {
                t0_ms: 0,
                t1_ms: 3_000,
                slot: 1,
            },
            Assigned {
                t0_ms: 3_000,
                t1_ms: 6_000,
                slot: 2,
            },
        ];
        let snapped = snap_pieces(&assigned, &seg);
        assert_eq!(snapped[0].t0_ms, 0);
        assert_eq!(snapped[1].t1_ms, 6_000);
        assert_eq!(snapped[0].t1_ms, snapped[1].t0_ms);
        assert!(snapped[0].t1_ms.abs_diff(3_000) <= CUT_SEARCH_MS + CUT_WINDOW_MS);
    }
}
