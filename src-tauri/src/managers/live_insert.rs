//! Live insert: paste the dictation phrase by phrase while the user speaks.
//!
//! GigaAM cannot stream, so the app used to wait for the end of a recording
//! and paste everything at once — after a two-minute dictation that is a
//! long wait, and any failure loses the lot. Here the recorder's 16 kHz
//! speech frames (VAD already removed the silence) are collected; when the
//! speaker pauses, the collected phrase is recognized with the normal
//! pipeline (chunking, filler removal, no trailing period) and pasted at the
//! cursor right away.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};

use super::transcription::TranscriptionManager;
use crate::smart_insert::Delivery;

const SAMPLE_RATE: usize = 16_000;
/// A pause this long ends a phrase. With VAD on, silence means frames stop
/// arriving; with VAD off it means quiet frames.
const PAUSE: Duration = Duration::from_millis(700);
/// Shortest phrase worth a model call (shorter bits wait for more speech).
const MIN_PHRASE_MS: usize = 600;
/// Longest phrase: flush even without a pause, so a monologue keeps flowing.
const MAX_PHRASE_MS: usize = 20_000;
/// RMS below this counts as silence when VAD is off.
const SILENCE_RMS: f32 = 0.004;
/// Wait before the single retry of a failed phrase recognition.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// How the next phrase joins the text pasted so far.
pub(crate) fn joiner(previous: Option<&str>, next: &str) -> &'static str {
    let Some(previous) = previous else {
        return "";
    };
    let ends_sentence = previous
        .trim_end()
        .ends_with(['.', '!', '?', '…', ',', ':', ';']);
    let starts_upper = next.chars().next().is_some_and(char::is_uppercase);
    if starts_upper && !ends_sentence {
        // The previous phrase lost its full stop (we strip trailing periods);
        // a capitalized continuation is a new sentence.
        ". "
    } else {
        " "
    }
}

/// Recognize a phrase; the engine may be briefly busy, so retry once.
fn transcribe_with_retry(tm: &TranscriptionManager, audio: Vec<f32>) -> anyhow::Result<String> {
    match tm.transcribe(audio.clone()) {
        Ok(text) => Ok(text),
        Err(e) => {
            if tm.live_insert_cancelled() {
                return Err(e);
            }
            warn!("Live insert: phrase recognition failed ({e}); retrying in {RETRY_DELAY:?}");
            std::thread::sleep(RETRY_DELAY);
            if tm.live_insert_cancelled() {
                return Err(e);
            }
            tm.transcribe(audio)
        }
    }
}

/// Worker loop. Returns the full text delivered during the recording —
/// pasted, or held by smart insert because the cursor was not in a text
/// field (history keeps it either way).
pub(crate) fn run(tm: &TranscriptionManager, frames: mpsc::Receiver<Vec<f32>>) -> String {
    let mut phrase: Vec<f32> = Vec::new();
    let mut last_voice = Instant::now();
    let mut pasted = String::new();
    let mut last_piece: Option<String> = None;
    crate::smart_insert::begin_session();

    loop {
        let disconnected = match frames.recv_timeout(Duration::from_millis(100)) {
            Ok(frame) => {
                let rms =
                    (frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32).sqrt();
                if rms >= SILENCE_RMS {
                    last_voice = Instant::now();
                }
                phrase.extend_from_slice(&frame);
                false
            }
            Err(mpsc::RecvTimeoutError::Timeout) => false,
            Err(mpsc::RecvTimeoutError::Disconnected) => true,
        };
        if tm.live_insert_cancelled() {
            debug!("Live insert worker stopping (cancelled)");
            return pasted;
        }

        let phrase_ms = phrase.len() * 1000 / SAMPLE_RATE;
        let paused = last_voice.elapsed() >= PAUSE;
        let flush =
            disconnected || phrase_ms >= MAX_PHRASE_MS || (paused && phrase_ms >= MIN_PHRASE_MS);
        if flush && !phrase.is_empty() {
            let audio = std::mem::take(&mut phrase);
            match transcribe_with_retry(tm, audio) {
                Ok(text) => {
                    let text = text.trim().to_string();
                    if !text.is_empty() && !tm.live_insert_cancelled() {
                        let lead = joiner(last_piece.as_deref(), &text);
                        if deliver_now(tm, &text, lead) {
                            pasted.push_str(lead);
                            pasted.push_str(&text);
                            last_piece = Some(text);
                        }
                    }
                }
                Err(e) => error!("Live insert: phrase recognition failed: {e}"),
            }
        }
        if disconnected {
            info!(
                "Live insert finished: {} chars pasted",
                pasted.chars().count()
            );
            return pasted;
        }
    }
}

/// Paste (or, with smart insert, hold) on the main thread as the normal path
/// does, and wait for it, so phrases land in order. `true` when the phrase
/// was delivered — pasted or held for Ctrl+V.
fn deliver_now(tm: &TranscriptionManager, text: &str, lead: &'static str) -> bool {
    let app = tm.app_handle().clone();
    let (tx, rx) = mpsc::channel();
    let app_for_paste = app.clone();
    let text = text.to_string();
    let scheduled = app.run_on_main_thread(move || {
        let _ = tx.send(crate::smart_insert::deliver(&app_for_paste, &text, lead));
    });
    if let Err(e) = scheduled {
        error!("Live insert: cannot schedule paste: {e:?}");
        return false;
    }
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Delivery::Pasted) | Ok(Delivery::Held) => true,
        Ok(Delivery::Failed(e)) => {
            error!("Live insert: paste failed: {e}");
            false
        }
        Err(e) => {
            error!("Live insert: paste did not complete: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::joiner;

    #[test]
    fn first_phrase_has_no_joiner() {
        assert_eq!(joiner(None, "Привет"), "");
    }

    #[test]
    fn capitalized_continuation_starts_a_sentence() {
        assert_eq!(joiner(Some("Сделай сайт"), "Потом посмотрим"), ". ");
    }

    #[test]
    fn lowercase_continuation_is_the_same_sentence() {
        assert_eq!(joiner(Some("Сделай сайт"), "и потом посмотрим"), " ");
    }

    #[test]
    fn existing_punctuation_is_kept() {
        assert_eq!(joiner(Some("Как дела?"), "Хорошо"), " ");
        assert_eq!(joiner(Some("Значит,"), "Делаем"), " ");
    }
}
