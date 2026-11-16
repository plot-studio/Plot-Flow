//! Splitting long recordings into model-sized windows.
//!
//! GigaAM (and other attention/CTC models) are trained on ~25 s utterances
//! and degrade sharply past that window; the app used to hand them whole
//! 60–160 s dictations, which came back as garbled text. [`plan_chunks`]
//! finds pauses so every chunk stays under the model's advertised limit and
//! each cut lands in silence rather than in the middle of a word.
//!
//! Everything here is pure and sample-rate agnostic so it can be unit tested
//! without a model.

use std::ops::Range;

/// Energy analysis frame (20 ms).
const FRAME_MS: usize = 20;
/// Preferred chunk length as a fraction of the hard limit: leaves headroom so
/// the model stays comfortably inside its trained window.
const PREFERRED_FRACTION: f64 = 0.8;
/// Never cut earlier than this fraction of the hard limit.
const MIN_FRACTION: f64 = 0.45;
/// A frame quieter than this fraction of the search window's mean RMS counts
/// as pause.
const PAUSE_RMS_FRACTION: f32 = 0.3;
/// Absolute floor so digital silence still counts as pause.
const PAUSE_RMS_FLOOR: f32 = 1e-4;
/// Shortest pause run (in frames) worth cutting at: 2 → 40 ms.
const MIN_PAUSE_FRAMES: usize = 2;
/// A trailing remainder shorter than this is glued to the previous chunk;
/// exceeding the limit by well under a second is harmless, a lone fragment
/// of a few hundred milliseconds is not worth a model call.
const MIN_TAIL_MS: usize = 500;

/// The recording is only ever read by the model, never listened to, so it is
/// normalized as loud as possible: any audio whose peak is below this gets
/// lifted.
const QUIET_PEAK: f32 = 0.9;
/// Peak the boost aims for (just under clipping).
const TARGET_PEAK: f32 = 0.95;
/// Maximum software gain (≈ +24 dB): enough for a voice from the next room,
/// while a near-silent chunk of pure hiss is not blown up into fake words.
const MAX_GAIN: f32 = 16.0;

/// Split `audio` (mono, `sample_rate` Hz) into contiguous ranges no longer
/// than `max_chunk_ms`, cutting at the quietest stretch available in each
/// window. Returns a single full range when the audio already fits.
pub fn plan_chunks(audio: &[f32], sample_rate: usize, max_chunk_ms: u64) -> Vec<Range<usize>> {
    let len = audio.len();
    if len == 0 {
        return Vec::new();
    }
    let max_samples = (max_chunk_ms as usize).saturating_mul(sample_rate) / 1000;
    if max_samples == 0 || len <= max_samples {
        return vec![0..len];
    }

    let frame = (sample_rate * FRAME_MS / 1000).max(1);
    let preferred = ((max_samples as f64) * PREFERRED_FRACTION) as usize;
    let min_split = (((max_samples as f64) * MIN_FRACTION) as usize).max(frame);
    let min_tail = sample_rate * MIN_TAIL_MS / 1000;

    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut start = 0usize;
    while len - start > max_samples {
        let lo = start + min_split;
        let hi = start + max_samples;
        let cut = find_cut(audio, lo, hi, start + preferred, frame);
        // Guarantee progress even on degenerate input.
        let cut = cut.clamp(lo, hi).max(start + frame);
        ranges.push(start..cut);
        start = cut;
    }

    if start < len {
        let tail = len - start;
        match ranges.last_mut() {
            Some(last) if tail < min_tail => last.end = len,
            _ => ranges.push(start..len),
        }
    }
    ranges
}

/// Choose a cut position in `[lo, hi)`: the middle of the longest quiet run
/// (ties go to the run closest to `preferred`), or the single quietest frame
/// in the later half of the window when nothing qualifies as a pause.
fn find_cut(audio: &[f32], lo: usize, hi: usize, preferred: usize, frame: usize) -> usize {
    let hi = hi.min(audio.len());
    if hi <= lo + frame {
        return hi;
    }
    let n = (hi - lo) / frame;
    let rms: Vec<f32> = (0..n)
        .map(|i| {
            let s = &audio[lo + i * frame..lo + (i + 1) * frame];
            (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
        })
        .collect();
    let mean = rms.iter().sum::<f32>() / n as f32;
    let threshold = (mean * PAUSE_RMS_FRACTION).max(PAUSE_RMS_FLOOR);
    let preferred_frame = preferred.saturating_sub(lo) / frame;

    let mut best: Option<(usize, usize)> = None; // (run length, middle frame)
    let mut i = 0;
    while i < n {
        if rms[i] >= threshold {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < n && rms[i] < threshold {
            i += 1;
        }
        let run = i - run_start;
        let mid = run_start + run / 2;
        if run < MIN_PAUSE_FRAMES {
            continue;
        }
        let better = match best {
            None => true,
            Some((best_run, best_mid)) => {
                run > best_run
                    || (run == best_run
                        && mid.abs_diff(preferred_frame) < best_mid.abs_diff(preferred_frame))
            }
        };
        if better {
            best = Some((run, mid));
        }
    }

    let cut_frame = match best {
        Some((_, mid)) => mid,
        None => {
            // No pause: least damaging hard cut in the later half.
            let from = n / 2;
            (from..n)
                .min_by(|a, b| rms[*a].total_cmp(&rms[*b]))
                .unwrap_or(n - 1)
        }
    };
    lo + cut_frame * frame
}

/// Amplify a very quiet recording in place so the model sees a normal level.
/// Returns the gain applied, or `None` when the level was already fine.
pub fn boost_quiet_audio(audio: &mut [f32]) -> Option<f32> {
    let peak = audio.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak <= 0.0 || peak >= QUIET_PEAK {
        return None;
    }
    let gain = (TARGET_PEAK / peak).min(MAX_GAIN);
    for s in audio.iter_mut() {
        *s *= gain;
    }
    Some(gain)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: usize = 16_000;

    /// Deterministic pseudo-noise "speech" at a given amplitude.
    fn speech(secs: f32, amp: f32, seed: &mut u32) -> Vec<f32> {
        (0..(secs * SR as f32) as usize)
            .map(|_| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let r = (*seed >> 8) as f32 / (1u32 << 24) as f32; // 0..1
                (r * 2.0 - 1.0) * amp
            })
            .collect()
    }

    fn silence(secs: f32) -> Vec<f32> {
        vec![0.0; (secs * SR as f32) as usize]
    }

    fn rms(a: &[f32]) -> f32 {
        (a.iter().map(|x| x * x).sum::<f32>() / a.len().max(1) as f32).sqrt()
    }

    #[test]
    fn short_audio_is_one_chunk() {
        let audio = vec![0.1; SR * 10];
        assert_eq!(plan_chunks(&audio, SR, 25_000), vec![0..SR * 10]);
    }

    #[test]
    fn empty_audio_has_no_chunks() {
        assert!(plan_chunks(&[], SR, 25_000).is_empty());
    }

    #[test]
    fn unlimited_model_never_splits() {
        let audio = vec![0.1; SR * 120];
        assert_eq!(plan_chunks(&audio, SR, 0), vec![0..SR * 120]);
    }

    #[test]
    fn cuts_fall_in_pauses_and_respect_the_limit() {
        // 90 s of dictation: 7 s phrases separated by 0.5 s pauses.
        let mut seed = 7;
        let mut audio = Vec::new();
        for _ in 0..12 {
            audio.extend(speech(7.0, 0.3, &mut seed));
            audio.extend(silence(0.5));
        }
        let max_ms = 25_000;
        let ranges = plan_chunks(&audio, SR, max_ms);

        assert!(ranges.len() >= 4, "expected several chunks, got {ranges:?}");
        let mut expected_start = 0;
        for r in &ranges {
            assert_eq!(r.start, expected_start, "chunks must be contiguous");
            assert!(
                r.end - r.start <= max_ms as usize * SR / 1000,
                "chunk too long: {r:?}"
            );
            expected_start = r.end;
        }
        assert_eq!(expected_start, audio.len(), "chunks must cover the audio");

        // Every interior cut sits inside a pause: ±20 ms around it is quiet.
        for r in &ranges[..ranges.len() - 1] {
            let w = SR * 20 / 1000;
            let around = &audio[r.end - w..r.end + w];
            assert!(rms(around) < 0.01, "cut at {} is not in a pause", r.end);
        }
    }

    #[test]
    fn continuous_speech_still_splits_within_limit() {
        let mut seed = 3;
        let audio = speech(61.0, 0.3, &mut seed);
        let ranges = plan_chunks(&audio, SR, 25_000);
        assert_eq!(ranges.len(), 3, "{ranges:?}");
        for r in &ranges {
            assert!(r.end - r.start <= 25 * SR);
        }
        assert_eq!(ranges.last().unwrap().end, audio.len());
    }

    #[test]
    fn tiny_tail_is_merged_into_previous_chunk() {
        // 25.3 s where the only pause is right before a 0.3 s tail.
        let mut seed = 11;
        let mut audio = speech(24.9, 0.3, &mut seed);
        audio.extend(silence(0.1));
        audio.extend(speech(0.3, 0.3, &mut seed));
        let ranges = plan_chunks(&audio, SR, 25_000);
        assert_eq!(ranges.len(), 1, "{ranges:?}");
        assert_eq!(ranges[0], 0..audio.len());
    }

    #[test]
    fn quiet_audio_is_boosted_and_capped() {
        let mut faint = vec![0.0, 0.02, -0.02, 0.01];
        let gain = boost_quiet_audio(&mut faint).expect("faint audio gets gain");
        assert!(
            (gain - 16.0).abs() < 1e-6,
            "gain is capped at 16x, got {gain}"
        );
        assert!((faint[1] - 0.32).abs() < 1e-6);

        let mut moderate = vec![0.0, 0.19, -0.1];
        let gain = boost_quiet_audio(&mut moderate).expect("0.19 peak is lifted");
        assert!((gain - 5.0).abs() < 1e-5);
        assert!((moderate[1] - 0.95).abs() < 1e-5);

        let mut loud = vec![0.0, 0.5, -0.92];
        assert!(boost_quiet_audio(&mut loud).is_none());
        assert_eq!(loud, vec![0.0, 0.5, -0.92]);

        let mut digital_silence = vec![0.0; 10];
        assert!(boost_quiet_audio(&mut digital_silence).is_none());
    }
}
