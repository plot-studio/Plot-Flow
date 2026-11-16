//! One running meeting: per-source capture/segmentation threads, a single
//! transcription worker and a status ticker.
//!
//! Data flow per source:
//! cpal callback → ring (capture.rs) → processing thread: resample to 16 kHz,
//! meter level, 30 s WAV chunks, Silero VAD → [`Segmenter`] → tentative
//! segment row + [`SegmentJob`] on a bounded queue → transcription worker →
//! final segment row + `meeting-segment` event.
//!
//! Audio threads never block on the worker: jobs are sent with `try_send`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};

use super::capture::{self, CaptureStream};
use super::remote_speakers::RemoteSpeakers;
use super::store::MeetingStore;
use super::types::{
    MeetingErrorPayload, MeetingSegment, MeetingSource, MeetingState, MeetingStatus,
};
use crate::audio_toolkit::audio::FrameResampler;
use crate::audio_toolkit::vad::{frames_for_duration_ms, SmoothedVad, VadFrame};
use crate::audio_toolkit::{SileroVad, VoiceActivityDetector};
use crate::managers::transcription::TranscriptionManager;

/// Sample rate of everything after the resampler.
pub const SAMPLE_RATE: u64 = 16_000;
/// Silero frame length.
const FRAME_MS: u64 = 30;
/// 16 kHz samples per processing frame.
const FRAME_SAMPLES: u64 = SAMPLE_RATE * FRAME_MS / 1000;
/// Silence that ends a speech run.
const END_SILENCE_MS: u64 = 600;
/// Past this length a run is cut at the first quiet frame (a short pause).
const SOFT_MAX_SEGMENT_MS: u64 = 15_000;
/// Hard cap of one segment (GigaAM's window is 25 s).
const HARD_MAX_SEGMENT_MS: u64 = 22_000;
/// A frame quieter than this share of the run's mean RMS counts as a pause.
const QUIET_FRAME_RATIO: f32 = 0.3;
/// Runs with less *voiced* audio than this (clicks, coughs) are not
/// transcribed. Pre-roll and the hangover tail do not count.
const MIN_VOICED_MS: u64 = 240;
/// Pre-roll kept before detected speech onset.
const PREFILL_MS: u64 = 300;
/// Voiced time needed to confirm speech onset.
const ONSET_MS: u64 = 60;
const SILERO_THRESHOLD: f32 = 0.3;
/// Length of one crash-safety WAV chunk.
const WAV_CHUNK_SECONDS: u64 = 30;
/// Pending transcription jobs before new segments are rejected.
const JOB_QUEUE_CAPACITY: usize = 48;
/// Processing thread poll interval.
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// If a source delivers no audio for this long while a run is open, the run is
/// closed (WASAPI loopback simply stops calling back when nothing plays).
const IDLE_FLUSH: Duration = Duration::from_millis(800);
/// A gap between the session clock and the delivered audio larger than this is
/// treated as silence the device did not deliver.
const GAP_TOLERANCE_SAMPLES: u64 = SAMPLE_RATE / 2;
/// The gap jump is only applied when the device really paused delivery for at
/// least this long; otherwise the difference is clock drift and is ignored.
const GAP_MIN_IDLE: Duration = Duration::from_millis(400);
/// How often the capture thread checks for a lost or switched device.
const DEVICE_CHECK_PERIOD: Duration = Duration::from_secs(2);
/// Per-frame level history kept per source (for the echo rule), ≈ 30 s.
const LEVEL_HISTORY_FRAMES: usize = 1_000;
/// Echo rule: the system track counts as "active" above this meter level
/// (≈ -36 dBFS).
const ECHO_SYSTEM_ACTIVE_LEVEL: f32 = 0.4;
/// Echo rule: the system level must exceed the mic level by this much (≈ 3 dB).
const ECHO_LEVEL_MARGIN: f32 = 0.05;
/// Echo rule: share of the mic run's frames that must satisfy both conditions.
const ECHO_MAJORITY: f32 = 0.75;
/// Echo rule: shorter mic runs are never treated as echo.
const ECHO_MIN_FRAMES: usize = 8;
/// Retries of a failed recognition (the engine may be busy or reloading).
const TRANSCRIBE_RETRIES: u32 = 3;
const TRANSCRIBE_RETRY_BACKOFF: Duration = Duration::from_secs(2);
/// Text stored for a segment whose recognition failed for good.
const UNRECOGNIZED_TEXT: &str = "[не распознано]";
/// Status event period.
const STATUS_PERIOD: Duration = Duration::from_millis(250);
/// How long `spawn_source` waits for the device to open.
const SOURCE_OPEN_TIMEOUT: Duration = Duration::from_secs(10);

fn samples_to_ms(samples: u64) -> u64 {
    samples * 1000 / SAMPLE_RATE
}

fn ms_to_samples(ms: u64) -> u64 {
    ms * SAMPLE_RATE / 1000
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

/* ───────────────────────────── Segmenter ───────────────────────────── */

/// A finished speech run.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechRun {
    /// Start position in 16 kHz samples since the meeting start.
    pub start: u64,
    pub samples: Vec<f32>,
    /// Samples the raw VAD classified as voice (excludes pre-roll/hangover).
    pub voiced_samples: u64,
}

impl SpeechRun {
    pub fn t0_ms(&self) -> u64 {
        samples_to_ms(self.start)
    }

    pub fn t1_ms(&self) -> u64 {
        samples_to_ms(self.start + self.samples.len() as u64)
    }

    pub fn voiced_ms(&self) -> u64 {
        samples_to_ms(self.voiced_samples)
    }
}

/// Turns the smoothed VAD's speech/noise stream into bounded speech runs.
///
/// The smoothed VAD already adds pre-roll and a hangover tail of
/// [`END_SILENCE_MS`], so the first `Noise` after speech means "≥600 ms of
/// silence". Long runs are cut: past `soft_max_samples` at the first quiet
/// frame (unvoiced, or much quieter than the run so far), and unconditionally
/// at `hard_max_samples`.
pub struct Segmenter {
    soft_max_samples: usize,
    hard_max_samples: usize,
    current: Option<SpeechRun>,
    /// Sum of squares of the open run (for its mean RMS).
    energy: f64,
}

impl Segmenter {
    pub fn new(soft_max_samples: usize, hard_max_samples: usize) -> Self {
        let hard_max_samples = hard_max_samples.max(1);
        Self {
            soft_max_samples: soft_max_samples.clamp(1, hard_max_samples),
            hard_max_samples,
            current: None,
            energy: 0.0,
        }
    }

    /// Feed a speech slice that ends at absolute position `end_pos`;
    /// `voiced_samples` is how much of it the raw VAD heard as voice.
    pub fn push_speech(
        &mut self,
        end_pos: u64,
        slice: &[f32],
        voiced_samples: usize,
    ) -> Option<SpeechRun> {
        let run = self.current.get_or_insert_with(|| SpeechRun {
            start: end_pos.saturating_sub(slice.len() as u64),
            samples: Vec::with_capacity(16_000 * 4),
            voiced_samples: 0,
        });
        let mean_rms = if run.samples.is_empty() {
            0.0
        } else {
            (self.energy / run.samples.len() as f64).sqrt() as f32
        };
        let slice_rms = rms(slice);
        let quiet = voiced_samples == 0 || slice_rms < mean_rms * QUIET_FRAME_RATIO;

        run.samples.extend_from_slice(slice);
        run.voiced_samples += voiced_samples as u64;
        self.energy += slice.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>();

        let len = run.samples.len();
        if len >= self.hard_max_samples || (len >= self.soft_max_samples && quiet) {
            return self.take();
        }
        None
    }

    /// Feed a non-speech decision: closes the open run, if any.
    pub fn push_noise(&mut self) -> Option<SpeechRun> {
        self.take()
    }

    /// Close the open run (pause, stop, gap in the device stream).
    pub fn flush(&mut self) -> Option<SpeechRun> {
        self.take()
    }

    fn take(&mut self) -> Option<SpeechRun> {
        self.energy = 0.0;
        self.current.take()
    }

    pub fn has_open_run(&self) -> bool {
        self.current.is_some()
    }
}

/* ───────────────────────────── Gap & echo rules ───────────────────────────── */

/// Where the source position should jump to when the device paused delivery
/// (loopback during silence), or `None` to keep counting samples.
///
/// `idle` is how long no audio arrived before this batch. Without a real
/// pause the difference between the wall clock and the sample count is just
/// clock drift and is ignored (jumping would cut a run mid-word).
fn gap_realign(pos: u64, batch_16k: u64, clock_pos: u64, idle: Duration) -> Option<u64> {
    if idle < GAP_MIN_IDLE {
        return None;
    }
    if clock_pos > pos + batch_16k + GAP_TOLERANCE_SAMPLES {
        Some(clock_pos - batch_16k)
    } else {
        None
    }
}

/// Conservative echo test for a microphone run without headphones: the
/// remote voice played by the speakers reaches the mic, but the loopback
/// track carries it louder. `mic` and `sys` are `(frame index, meter level)`
/// over the run's span. The run is echo only when, for most of its frames,
/// the system track was active *and* louder than the mic.
pub fn is_probable_echo(mic: &[(u64, f32)], sys: &[(u64, f32)]) -> bool {
    if mic.len() < ECHO_MIN_FRAMES || sys.is_empty() {
        return false;
    }
    let sys_levels: HashMap<u64, f32> = sys.iter().copied().collect();
    let mut active = 0usize;
    let mut dominated = 0usize;
    for &(index, mic_level) in mic {
        let level = sys_levels
            .get(&index)
            .or_else(|| sys_levels.get(&index.wrapping_sub(1)))
            .or_else(|| sys_levels.get(&(index + 1)));
        let Some(&sys_level) = level else {
            continue;
        };
        if sys_level >= ECHO_SYSTEM_ACTIVE_LEVEL {
            active += 1;
            if sys_level > mic_level + ECHO_LEVEL_MARGIN {
                dominated += 1;
            }
        }
    }
    let needed = (mic.len() as f32 * ECHO_MAJORITY).ceil() as usize;
    active >= needed && dominated >= needed
}

/// Recent per-frame levels of one source.
#[derive(Default)]
struct LevelHistory {
    frames: VecDeque<(u64, f32)>,
}

impl LevelHistory {
    fn push(&mut self, index: u64, level: f32) {
        if self.frames.len() >= LEVEL_HISTORY_FRAMES {
            self.frames.pop_front();
        }
        self.frames.push_back((index, level));
    }

    fn between(&self, from: u64, to: u64) -> Vec<(u64, f32)> {
        self.frames
            .iter()
            .filter(|(index, _)| (from..=to).contains(index))
            .copied()
            .collect()
    }

    fn clear(&mut self) {
        self.frames.clear();
    }
}

/* ───────────────────────────── Session clock ───────────────────────────── */

/// Wall clock of the meeting that does not advance while paused.
#[derive(Debug)]
pub struct SessionClock {
    started: Instant,
    paused_total: Duration,
    paused_at: Option<Instant>,
}

impl SessionClock {
    pub fn new(now: Instant) -> Self {
        Self {
            started: now,
            paused_total: Duration::ZERO,
            paused_at: None,
        }
    }

    pub fn pause(&mut self, now: Instant) {
        if self.paused_at.is_none() {
            self.paused_at = Some(now);
        }
    }

    pub fn resume(&mut self, now: Instant) {
        if let Some(at) = self.paused_at.take() {
            self.paused_total += now.saturating_duration_since(at);
        }
    }

    pub fn elapsed_ms(&self, now: Instant) -> u64 {
        let end = self.paused_at.unwrap_or(now);
        end.saturating_duration_since(self.started)
            .saturating_sub(self.paused_total)
            .as_millis() as u64
    }
}

/* ───────────────────────────── Shared state ───────────────────────────── */

fn state_to_u8(state: MeetingState) -> u8 {
    match state {
        MeetingState::Idle => 0,
        MeetingState::Recording => 1,
        MeetingState::Paused => 2,
        MeetingState::Finalizing => 3,
    }
}

fn state_from_u8(value: u8) -> MeetingState {
    match value {
        1 => MeetingState::Recording,
        2 => MeetingState::Paused,
        3 => MeetingState::Finalizing,
        _ => MeetingState::Idle,
    }
}

fn source_index(source: MeetingSource) -> usize {
    match source {
        MeetingSource::Microphone => 0,
        MeetingSource::System => 1,
    }
}

/// State shared by all threads of one meeting.
pub struct SessionShared {
    pub meeting_id: i64,
    state: AtomicU8,
    /// Capture threads should flush and exit.
    stop_capture: AtomicBool,
    /// Ticker should exit.
    stop_ticker: AtomicBool,
    /// Meeting is being discarded: the worker skips remaining jobs.
    pub discard: AtomicBool,
    /// The app is exiting: the worker stops retrying failed jobs.
    pub exiting: AtomicBool,
    clock: Mutex<SessionClock>,
    mic_level: AtomicU32,
    system_level: AtomicU32,
    /// Per-frame levels of each source, indexed by [`source_index`].
    level_history: [Mutex<LevelHistory>; 2],
    segment_count: AtomicU32,
    last_error: Mutex<Option<String>>,
}

impl SessionShared {
    pub fn new(meeting_id: i64) -> Self {
        Self {
            meeting_id,
            state: AtomicU8::new(state_to_u8(MeetingState::Recording)),
            stop_capture: AtomicBool::new(false),
            stop_ticker: AtomicBool::new(false),
            discard: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            clock: Mutex::new(SessionClock::new(Instant::now())),
            mic_level: AtomicU32::new(0),
            system_level: AtomicU32::new(0),
            level_history: [
                Mutex::new(LevelHistory::default()),
                Mutex::new(LevelHistory::default()),
            ],
            segment_count: AtomicU32::new(0),
            last_error: Mutex::new(None),
        }
    }

    pub fn state(&self) -> MeetingState {
        state_from_u8(self.state.load(Ordering::Acquire))
    }

    pub fn set_state(&self, state: MeetingState) {
        self.state.store(state_to_u8(state), Ordering::Release);
    }

    pub fn is_paused(&self) -> bool {
        self.state() == MeetingState::Paused
    }

    /// Restart the clock at "now" (called once all streams are open).
    pub fn reset_clock(&self) {
        if let Ok(mut clock) = self.clock.lock() {
            *clock = SessionClock::new(Instant::now());
        }
    }

    pub fn pause(&self) {
        if let Ok(mut clock) = self.clock.lock() {
            clock.pause(Instant::now());
        }
        self.set_state(MeetingState::Paused);
    }

    pub fn resume(&self) {
        if let Ok(mut clock) = self.clock.lock() {
            clock.resume(Instant::now());
        }
        self.set_state(MeetingState::Recording);
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.clock
            .lock()
            .map(|clock| clock.elapsed_ms(Instant::now()))
            .unwrap_or(0)
    }

    fn set_level(&self, source: MeetingSource, level: f32) {
        let slot = match source {
            MeetingSource::Microphone => &self.mic_level,
            MeetingSource::System => &self.system_level,
        };
        slot.store(level.to_bits(), Ordering::Relaxed);
    }

    fn level(&self, source: MeetingSource) -> f32 {
        let slot = match source {
            MeetingSource::Microphone => &self.mic_level,
            MeetingSource::System => &self.system_level,
        };
        f32::from_bits(slot.load(Ordering::Relaxed))
    }

    fn record_frame_level(&self, source: MeetingSource, frame_index: u64, level: f32) {
        if let Ok(mut history) = self.level_history[source_index(source)].lock() {
            history.push(frame_index, level);
        }
    }

    fn frame_levels(&self, source: MeetingSource, from: u64, to: u64) -> Vec<(u64, f32)> {
        self.level_history[source_index(source)]
            .lock()
            .map(|history| history.between(from, to))
            .unwrap_or_default()
    }

    fn clear_frame_levels(&self, source: MeetingSource) {
        if let Ok(mut history) = self.level_history[source_index(source)].lock() {
            history.clear();
        }
    }

    pub fn set_error(&self, message: impl Into<String>) {
        if let Ok(mut slot) = self.last_error.lock() {
            *slot = Some(message.into());
        }
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|slot| slot.clone())
    }

    pub fn segment_count(&self) -> u32 {
        self.segment_count.load(Ordering::Relaxed)
    }

    pub fn request_capture_stop(&self) {
        self.stop_capture.store(true, Ordering::Release);
    }

    pub fn request_ticker_stop(&self) {
        self.stop_ticker.store(true, Ordering::Release);
    }

    /// Snapshot for `meeting_status` / `meeting-status`.
    pub fn status(&self) -> MeetingStatus {
        let state = self.state();
        let live = matches!(state, MeetingState::Recording);
        MeetingStatus {
            state,
            meeting_id: Some(self.meeting_id),
            elapsed_ms: self.elapsed_ms(),
            mic_level: if live {
                self.level(MeetingSource::Microphone)
            } else {
                0.0
            },
            system_level: if live {
                self.level(MeetingSource::System)
            } else {
                0.0
            },
            segment_count: self.segment_count(),
            last_error: self.last_error(),
        }
    }
}

/// Report a non-fatal problem: remembered in the status and emitted.
pub fn report_error(app: &AppHandle, shared: &SessionShared, message: String) {
    log::warn!("Meeting {}: {message}", shared.meeting_id);
    let _ = app.emit(
        "meeting-error",
        MeetingErrorPayload {
            message: message.clone(),
        },
    );
    shared.set_error(message);
}

/* ───────────────────────────── Jobs & context ───────────────────────────── */

/// One finished speech run waiting for recognition.
pub struct SegmentJob {
    pub segment_id: i64,
    pub source: MeetingSource,
    pub speaker_id: Option<i64>,
    pub t0_ms: u64,
    pub t1_ms: u64,
    pub samples: Vec<f32>,
}

/// Everything a processing thread needs besides its stream and job queue.
#[derive(Clone)]
pub struct SourceContext {
    pub app: AppHandle,
    pub store: Arc<MeetingStore>,
    pub shared: Arc<SessionShared>,
    pub source: MeetingSource,
    pub speaker_id: Option<i64>,
    pub audio_dir: PathBuf,
    pub vad_model_path: PathBuf,
    pub selected_microphone: Option<String>,
}

/// Resolve the bundled Silero model the same way `managers::audio` does.
pub fn resolve_vad_model(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .resolve(
            "resources/models/silero_vad_v4.onnx",
            tauri::path::BaseDirectory::Resource,
        )
        .map_err(|e| format!("Не найдена модель VAD: {e}"))
}

/// Silero wrapper that remembers its last raw decision, so the processing
/// thread can tell real voice from the smoothed VAD's pre-roll/hangover.
struct TappedVad {
    inner: SileroVad,
    last_voice: Arc<AtomicBool>,
}

impl VoiceActivityDetector for TappedVad {
    fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> anyhow::Result<VadFrame<'a>> {
        let voice = self.inner.push_frame(frame)?.is_speech();
        self.last_voice.store(voice, Ordering::Relaxed);
        Ok(if voice {
            VadFrame::Speech(frame)
        } else {
            VadFrame::Noise
        })
    }

    fn frame_samples(&self) -> usize {
        self.inner.frame_samples()
    }

    fn reset(&mut self) {
        self.last_voice.store(false, Ordering::Relaxed);
        self.inner.reset();
    }
}

fn build_vad(model_path: &PathBuf, last_voice: Arc<AtomicBool>) -> Result<SmoothedVad, String> {
    let silero = SileroVad::new(model_path, SILERO_THRESHOLD)
        .map_err(|e| format!("Не удалось создать VAD: {e}"))?;
    let frame_samples = silero.frame_samples();
    Ok(SmoothedVad::new(
        Box::new(TappedVad {
            inner: silero,
            last_voice,
        }),
        frames_for_duration_ms(PREFILL_MS, frame_samples),
        frames_for_duration_ms(END_SILENCE_MS, frame_samples),
        frames_for_duration_ms(ONSET_MS, frame_samples),
    ))
}

/* ───────────────────────────── WAV chunks ───────────────────────────── */

/// Writes 16 kHz mono audio as consecutive 30-second WAV files
/// (`mic-0001.wav`, `sys-0001.wav`, ...). A crash loses at most one chunk.
struct WavChunker {
    dir: PathBuf,
    prefix: &'static str,
    buffer: Vec<f32>,
    index: u32,
    failed: bool,
}

impl WavChunker {
    fn new(dir: PathBuf, prefix: &'static str) -> Self {
        Self {
            dir,
            prefix,
            buffer: Vec::with_capacity((WAV_CHUNK_SECONDS * SAMPLE_RATE) as usize),
            index: 0,
            failed: false,
        }
    }

    fn push(&mut self, frame: &[f32]) {
        if self.failed {
            return;
        }
        self.buffer.extend_from_slice(frame);
        if self.buffer.len() as u64 >= WAV_CHUNK_SECONDS * SAMPLE_RATE {
            self.write_chunk();
        }
    }

    fn write_chunk(&mut self) {
        if self.buffer.is_empty() || self.failed {
            self.buffer.clear();
            return;
        }
        self.index += 1;
        let path = self
            .dir
            .join(format!("{}-{:04}.wav", self.prefix, self.index));
        if let Err(e) = crate::audio_toolkit::save_wav_file(&path, &self.buffer) {
            // Audio is only a safety net; the transcript keeps going.
            log::error!("Meeting: failed to write {path:?}: {e}; audio backup disabled");
            self.failed = true;
        }
        self.buffer.clear();
    }

    fn finish(&mut self) {
        self.write_chunk();
    }
}

/* ───────────────────────────── Processing thread ───────────────────────────── */

struct SourceProcessor {
    ctx: SourceContext,
    jobs: SyncSender<SegmentJob>,
    vad: SmoothedVad,
    /// Last raw (unsmoothed) Silero decision, written by [`TappedVad`].
    raw_voice: Arc<AtomicBool>,
    vad_failed: bool,
    segmenter: Segmenter,
    wav: WavChunker,
    /// 16 kHz samples consumed since the meeting start (incl. inserted gaps).
    pos: u64,
    level: f32,
}

impl SourceProcessor {
    fn process_frame(&mut self, frame: &[f32]) {
        self.pos += frame.len() as u64;

        let instant = capture::meter_level(frame);
        self.level = instant.max(self.level * 0.85);
        self.ctx.shared.set_level(self.ctx.source, self.level);
        self.ctx.shared.record_frame_level(
            self.ctx.source,
            self.pos.saturating_sub(1) / FRAME_SAMPLES,
            instant,
        );

        self.wav.push(frame);

        let finished = match self.vad.push_frame(frame) {
            Ok(VadFrame::Speech(slice)) => {
                let voiced = if self.raw_voice.load(Ordering::Relaxed) {
                    frame.len()
                } else {
                    0
                };
                self.segmenter.push_speech(self.pos, slice, voiced)
            }
            Ok(VadFrame::Noise) => self.segmenter.push_noise(),
            Err(e) => {
                if !self.vad_failed {
                    self.vad_failed = true;
                    log::error!("Meeting VAD error ({:?}): {e}", self.ctx.source);
                }
                self.segmenter.push_noise()
            }
        };
        if let Some(run) = finished {
            self.dispatch(run);
        }
    }

    /// Close the open run and reset VAD state (pause, stop, stream gap).
    fn flush(&mut self) {
        if let Some(run) = self.segmenter.flush() {
            self.dispatch(run);
        }
        self.vad.reset();
    }

    /// True when a microphone run is most likely the remote voice coming out
    /// of the speakers (no headphones).
    fn is_echo(&self, run: &SpeechRun) -> bool {
        if self.ctx.source != MeetingSource::Microphone || run.samples.is_empty() {
            return false;
        }
        let from = run.start / FRAME_SAMPLES;
        let to = (run.start + run.samples.len() as u64 - 1) / FRAME_SAMPLES;
        let shared = &self.ctx.shared;
        let mic = shared.frame_levels(MeetingSource::Microphone, from, to);
        let sys = shared.frame_levels(MeetingSource::System, from.saturating_sub(1), to + 1);
        is_probable_echo(&mic, &sys)
    }

    /// Persist a tentative segment and queue it for recognition.
    fn dispatch(&mut self, mut run: SpeechRun) {
        if run.voiced_ms() < MIN_VOICED_MS {
            return;
        }
        if self.is_echo(&run) {
            log::debug!(
                "Meeting {}: mic run {}–{} ms dropped as speaker echo",
                self.ctx.shared.meeting_id,
                run.t0_ms(),
                run.t1_ms()
            );
            return;
        }
        let ctx = &self.ctx;
        let (t0, t1) = (run.t0_ms(), run.t1_ms());
        let segment = match ctx.store.insert_segment(
            ctx.shared.meeting_id,
            t0,
            t1,
            ctx.speaker_id,
            ctx.source,
            "",
            false,
        ) {
            Ok(segment) => segment,
            Err(e) => {
                report_error(
                    &ctx.app,
                    &ctx.shared,
                    format!("Не удалось сохранить фрагмент: {e}"),
                );
                return;
            }
        };
        let _ = ctx.app.emit("meeting-segment", segment.clone());

        // Normalize loudness before recognition: only the text matters.
        crate::audio_toolkit::boost_quiet_audio(&mut run.samples);
        let job = SegmentJob {
            segment_id: segment.id,
            source: ctx.source,
            speaker_id: ctx.speaker_id,
            t0_ms: t0,
            t1_ms: t1,
            samples: run.samples,
        };
        match self.jobs.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(job)) | Err(TrySendError::Disconnected(job)) => {
                let _ = ctx.store.delete_segment(job.segment_id);
                emit_dropped(&ctx.app, &segment);
                report_error(
                    &ctx.app,
                    &ctx.shared,
                    "Распознавание не успевает: фрагмент пропущен".to_string(),
                );
            }
        }
    }
}

/// Tell the UI a tentative segment will not get text (empty final row).
fn emit_dropped(app: &AppHandle, segment: &MeetingSegment) {
    let mut dropped = segment.clone();
    dropped.text.clear();
    dropped.is_final = true;
    let _ = app.emit("meeting-segment", dropped);
}

/// Spawn the capture + processing thread of one source. Returns once the
/// stream is open (Ok) or failed to open (Err; the thread has exited).
///
/// The job sender is handed to the thread only after the device opened in
/// time. If opening times out, the hand-over channel is dropped: the thread
/// exits by itself as soon as `open_source` returns (closing the stream on
/// its own thread), and — not holding a job sender — never keeps the
/// transcription worker alive.
pub fn spawn_source(
    ctx: SourceContext,
    jobs: SyncSender<SegmentJob>,
) -> Result<JoinHandle<()>, String> {
    let (init_tx, init_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    let (go_tx, go_rx) = mpsc::sync_channel::<SyncSender<SegmentJob>>(1);
    let source = ctx.source;
    let handle = std::thread::Builder::new()
        .name(format!("meeting-{}", source.file_prefix()))
        .spawn(move || run_source(ctx, init_tx, go_rx))
        .map_err(|e| format!("Не удалось запустить поток записи: {e}"))?;

    match init_rx.recv_timeout(SOURCE_OPEN_TIMEOUT) {
        Ok(Ok(())) => {
            if go_tx.send(jobs).is_err() {
                let _ = handle.join();
                return Err(format!(
                    "Источник «{}» неожиданно остановился",
                    source.default_label()
                ));
            }
            Ok(handle)
        }
        Ok(Err(message)) => {
            drop(go_tx);
            let _ = handle.join();
            Err(message)
        }
        Err(_) => {
            // Abort: the thread sees the closed channel after open_source.
            drop(go_tx);
            drop(init_rx);
            log::warn!("Meeting: {source:?} did not open within {SOURCE_OPEN_TIMEOUT:?}; abandoning its thread");
            Err(format!(
                "Источник «{}» не ответил вовремя",
                source.default_label()
            ))
        }
    }
}

/// Name of the current default output device (loopback target), if any.
fn current_default_output() -> Option<String> {
    capture::default_output_device_name()
}

fn run_source(
    ctx: SourceContext,
    init_tx: SyncSender<Result<(), String>>,
    go_rx: Receiver<SyncSender<SegmentJob>>,
) {
    let raw_voice = Arc::new(AtomicBool::new(false));
    let vad = match build_vad(&ctx.vad_model_path, Arc::clone(&raw_voice)) {
        Ok(vad) => vad,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };
    let mut stream: CaptureStream =
        match capture::open_source(ctx.source, ctx.selected_microphone.as_deref()) {
            Ok(stream) => stream,
            Err(e) => {
                let _ = init_tx.send(Err(e));
                return;
            }
        };
    if init_tx.send(Ok(())).is_err() {
        log::info!("Meeting {:?}: opened too late, closing", ctx.source);
        return;
    }
    // Wait for the job sender; a closed channel means spawn_source gave up.
    let jobs = match go_rx.recv() {
        Ok(jobs) => jobs,
        Err(_) => {
            log::info!("Meeting {:?}: start aborted, closing", ctx.source);
            return;
        }
    };
    log::info!(
        "Meeting {}: {:?} capture running on {:?} @ {} Hz",
        ctx.shared.meeting_id,
        ctx.source,
        stream.device_name,
        stream.sample_rate
    );

    let mut in_rate = stream.sample_rate.max(1) as usize;
    let mut resampler = FrameResampler::new(
        in_rate,
        SAMPLE_RATE as usize,
        Duration::from_millis(FRAME_MS),
    );
    let mut processor = SourceProcessor {
        wav: WavChunker::new(ctx.audio_dir.clone(), ctx.source.file_prefix()),
        segmenter: Segmenter::new(
            ms_to_samples(SOFT_MAX_SEGMENT_MS) as usize,
            ms_to_samples(HARD_MAX_SEGMENT_MS) as usize,
        ),
        vad,
        raw_voice,
        vad_failed: false,
        pos: 0,
        level: 0.0,
        jobs,
        ctx,
    };

    let shared = Arc::clone(&processor.ctx.shared);
    let source = processor.ctx.source;
    let mut was_paused = false;
    let mut last_data = Instant::now();
    let mut error_reported = false;
    let mut last_device_check = Instant::now();

    loop {
        let stopping = shared.stop_capture.load(Ordering::Acquire);

        if shared.is_paused() && !stopping {
            // Drop audio captured while paused.
            stream.drain(|_| {});
            if !was_paused {
                processor.flush();
                resampler.reset();
                was_paused = true;
                shared.set_level(source, 0.0);
            }
            std::thread::sleep(POLL_INTERVAL);
            continue;
        }
        if was_paused {
            // The clock excluded the pause; continue from where it stands.
            was_paused = false;
            processor.pos = ms_to_samples(shared.elapsed_ms());
            shared.clear_frame_levels(source);
            last_data = Instant::now();
        }

        // Collect what the callback delivered since the last poll.
        let mut batch: Vec<f32> = Vec::new();
        stream.drain(|slice| batch.extend_from_slice(slice));

        if !batch.is_empty() {
            let idle = last_data.elapsed();
            last_data = Instant::now();
            let batch_16k = batch.len() as u64 * SAMPLE_RATE / in_rate as u64;
            let clock_pos = ms_to_samples(shared.elapsed_ms());
            if let Some(new_pos) = gap_realign(processor.pos, batch_16k, clock_pos, idle) {
                // The device delivered nothing for a while (loopback during
                // silence): close the run and jump to the wall clock.
                processor.flush();
                processor.pos = new_pos;
            }
            resampler.push(&batch, |frame| processor.process_frame(frame));
        } else if processor.segmenter.has_open_run() && last_data.elapsed() >= IDLE_FLUSH {
            processor.flush();
            shared.set_level(source, 0.0);
        }

        // Device lost, or (loopback) the default output switched — e.g. a
        // Bluetooth headset connected mid-call: reopen on the current device.
        if !stopping && last_device_check.elapsed() >= DEVICE_CHECK_PERIOD {
            last_device_check = Instant::now();
            let failed = stream.has_failed();
            let switched = source == MeetingSource::System
                && current_default_output().is_some_and(|name| name != stream.device_name);
            if failed || switched {
                match capture::open_source(source, processor.ctx.selected_microphone.as_deref()) {
                    Ok(new_stream) => {
                        log::info!(
                            "Meeting {:?}: switching capture {:?} -> {:?} ({})",
                            source,
                            stream.device_name,
                            new_stream.device_name,
                            if failed {
                                "device lost"
                            } else {
                                "default changed"
                            }
                        );
                        // Finish what the old device delivered, then close it
                        // (on this thread, as cpal requires).
                        let mut rest: Vec<f32> = Vec::new();
                        stream.drain(|slice| rest.extend_from_slice(slice));
                        resampler.push(&rest, |frame| processor.process_frame(frame));
                        resampler.finish(|frame| processor.process_frame(frame));
                        processor.flush();
                        log_overruns(source, &stream);
                        stream = new_stream;
                        in_rate = stream.sample_rate.max(1) as usize;
                        resampler = FrameResampler::new(
                            in_rate,
                            SAMPLE_RATE as usize,
                            Duration::from_millis(FRAME_MS),
                        );
                        shared.set_level(source, 0.0);
                        if error_reported {
                            log::info!("Meeting {source:?}: capture restored");
                        }
                        error_reported = false;
                        // `last_data` is left alone: the next batch measures the
                        // switch as idle time and the gap logic realigns.
                    }
                    Err(e) => {
                        if failed && !error_reported {
                            error_reported = true;
                            report_error(
                                &processor.ctx.app,
                                &shared,
                                format!("Источник «{}» отключился: {e}", source.default_label()),
                            );
                        } else if !failed {
                            log::warn!("Meeting {source:?}: new default device not usable: {e}");
                        }
                    }
                }
            }
        }

        if stopping {
            resampler.finish(|frame| processor.process_frame(frame));
            processor.flush();
            processor.wav.finish();
            shared.set_level(source, 0.0);
            break;
        }

        std::thread::sleep(POLL_INTERVAL);
    }

    log_overruns(source, &stream);
    drop(stream);
    log::info!("Meeting {source:?} capture thread finished");
}

fn log_overruns(source: MeetingSource, stream: &CaptureStream) {
    let overruns = stream.flags.overrun_samples.load(Ordering::Relaxed);
    if overruns > 0 {
        log::warn!("Meeting {source:?} capture dropped {overruns} samples (ring full)");
    }
}

/* ───────────────────────────── Transcription worker ───────────────────────────── */

/// Spawn the single recognition worker. It exits when every job sender
/// (one per source thread) has been dropped and the queue is empty.
pub fn spawn_worker(
    app: AppHandle,
    store: Arc<MeetingStore>,
    shared: Arc<SessionShared>,
    jobs: Receiver<SegmentJob>,
) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("meeting-transcribe".to_string())
        .spawn(move || run_worker(app, store, shared, jobs))
        .map_err(|e| format!("Не удалось запустить распознавание: {e}"))
}

fn run_worker(
    app: AppHandle,
    store: Arc<MeetingStore>,
    shared: Arc<SessionShared>,
    jobs: Receiver<SegmentJob>,
) {
    let mut remote = RemoteSpeakers::new(shared.meeting_id);
    while let Ok(job) = jobs.recv() {
        if shared.discard.load(Ordering::Acquire) {
            continue;
        }
        // Computer sound: tell the remote speakers apart (when the model is
        // installed); a segment with a clear speaker change is split.
        let pieces = if job.source == MeetingSource::System {
            remote.split(
                &app,
                &store,
                &shared,
                job.speaker_id,
                job.t0_ms,
                &job.samples,
            )
        } else {
            None
        };
        let Some(pieces) = pieces else {
            finish_piece(
                &app,
                &store,
                &shared,
                PieceRow {
                    segment_id: job.segment_id,
                    source: job.source,
                    speaker_id: job.speaker_id,
                    t0_ms: job.t0_ms,
                    t1_ms: job.t1_ms,
                },
                &job.samples,
            );
            continue;
        };
        let mut rows = Vec::with_capacity(pieces.len());
        for (i, piece) in pieces.iter().enumerate() {
            let segment = if i == 0 {
                // The tentative row of the job becomes the first piece.
                store
                    .update_segment_span(job.segment_id, piece.t0_ms, piece.t1_ms, piece.speaker_id)
                    .map_err(|e| e.to_string())
                    .and_then(|row| row.ok_or_else(String::new))
            } else {
                store
                    .insert_segment(
                        shared.meeting_id,
                        piece.t0_ms,
                        piece.t1_ms,
                        piece.speaker_id,
                        job.source,
                        "",
                        false,
                    )
                    .map_err(|e| e.to_string())
            };
            match segment {
                Ok(segment) => {
                    let _ = app.emit("meeting-segment", segment.clone());
                    rows.push((
                        PieceRow {
                            segment_id: segment.id,
                            source: job.source,
                            speaker_id: piece.speaker_id,
                            t0_ms: piece.t0_ms,
                            t1_ms: piece.t1_ms,
                        },
                        piece.samples.clone(),
                    ));
                }
                // The row is gone: the meeting is being discarded.
                Err(e) if e.is_empty() => break,
                Err(e) => {
                    report_error(&app, &shared, format!("Не удалось сохранить фрагмент: {e}"));
                }
            }
        }
        for (row, range) in rows {
            if shared.discard.load(Ordering::Acquire) {
                break;
            }
            finish_piece(&app, &store, &shared, row, &job.samples[range]);
        }
    }
    log::info!(
        "Meeting {} transcription worker finished",
        shared.meeting_id
    );
}

/// Where the text of one recognized piece goes.
struct PieceRow {
    segment_id: i64,
    source: MeetingSource,
    speaker_id: Option<i64>,
    t0_ms: u64,
    t1_ms: u64,
}

/// Recognize `samples` and store the text in `row` (or drop the row when
/// nothing was heard), emitting the final `meeting-segment`.
fn finish_piece(
    app: &AppHandle,
    store: &MeetingStore,
    shared: &SessionShared,
    row: PieceRow,
    samples: &[f32],
) {
    let text = match transcribe_with_retry(app, shared, samples) {
        Ok(text) => text.trim().to_string(),
        Err(e) => {
            // Never lose a segment because the engine was busy or failed:
            // keep its place in the timeline with a marker.
            report_error(app, shared, format!("Ошибка распознавания: {e}"));
            UNRECOGNIZED_TEXT.to_string()
        }
    };
    if shared.discard.load(Ordering::Acquire) {
        return;
    }
    if text.is_empty() {
        // Recognition succeeded and heard nothing: drop the row.
        let _ = store.delete_segment(row.segment_id);
        let _ = app.emit(
            "meeting-segment",
            MeetingSegment {
                id: row.segment_id,
                meeting_id: shared.meeting_id,
                t0_ms: row.t0_ms,
                t1_ms: row.t1_ms,
                speaker_id: row.speaker_id,
                source: row.source,
                text: String::new(),
                is_final: true,
            },
        );
        return;
    }
    match store.finalize_segment(row.segment_id, row.t1_ms, &text) {
        Ok(Some(segment)) => {
            shared.segment_count.fetch_add(1, Ordering::Relaxed);
            let _ = app.emit("meeting-segment", segment);
        }
        Ok(None) => {}
        Err(e) => report_error(app, shared, format!("Не удалось сохранить текст: {e}")),
    }
}

/// Recognize `samples`, retrying up to [`TRANSCRIBE_RETRIES`] times with a
/// pause (the engine may be busy with dictation or reloading).
fn transcribe_with_retry(
    app: &AppHandle,
    shared: &SessionShared,
    samples: &[f32],
) -> Result<String, String> {
    let mut last_error = String::new();
    for attempt in 0..=TRANSCRIBE_RETRIES {
        if attempt > 0 {
            if shared.discard.load(Ordering::Acquire) || shared.exiting.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(TRANSCRIBE_RETRY_BACKOFF);
        }
        match transcribe_job(app, samples.to_vec()) {
            Ok(text) => return Ok(text),
            Err(e) => {
                log::warn!(
                    "Meeting {}: recognition attempt {} failed: {e}",
                    shared.meeting_id,
                    attempt + 1
                );
                last_error = e;
            }
        }
    }
    Err(last_error)
}

fn transcribe_job(app: &AppHandle, samples: Vec<f32>) -> Result<String, String> {
    let manager = app
        .try_state::<Arc<TranscriptionManager>>()
        .map(|state| Arc::clone(state.inner()))
        .ok_or_else(|| "модуль распознавания не готов".to_string())?;
    if !manager.is_model_loaded() {
        // transcribe() waits for a load that is in progress, and for an
        // engine another caller is using.
        manager.initiate_model_load();
    }
    manager.transcribe(samples).map_err(|e| e.to_string())
}

/* ───────────────────────────── Status ticker ───────────────────────────── */

/// Emit `meeting-status` ~4×/s and persist the running duration every 5 s.
pub fn spawn_ticker(
    app: AppHandle,
    store: Arc<MeetingStore>,
    shared: Arc<SessionShared>,
) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("meeting-status".to_string())
        .spawn(move || {
            let mut tick: u64 = 0;
            while !shared.stop_ticker.load(Ordering::Acquire) {
                let status = shared.status();
                let _ = app.emit("meeting-status", status.clone());
                tick += 1;
                if tick % 20 == 0 && status.state != MeetingState::Finalizing {
                    let _ = store.update_duration(shared.meeting_id, status.elapsed_ms);
                }
                std::thread::sleep(STATUS_PERIOD);
            }
        })
        .map_err(|e| format!("Не удалось запустить поток статуса: {e}"))
}

/// Join a thread, giving up after `timeout` (the thread is then left detached).
pub fn join_with_timeout(handle: JoinHandle<()>, timeout: Duration, what: &str) -> bool {
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            log::warn!("Meeting: {what} did not finish within {timeout:?}; detaching");
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = handle.join();
    true
}

/// Make the job channel used by a session.
pub fn job_channel() -> (SyncSender<SegmentJob>, Receiver<SegmentJob>) {
    mpsc::sync_channel(JOB_QUEUE_CAPACITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOUD: f32 = 0.5;

    #[test]
    fn segmenter_starts_run_at_prefill_and_closes_on_noise() {
        let mut seg = Segmenter::new(100, 100);
        // Onset emits 30 samples (prefill + current) ending at position 50.
        assert!(seg.push_speech(50, &[0.1; 30], 10).is_none());
        assert!(seg.push_speech(60, &[0.2; 10], 10).is_none());
        let run = seg.push_noise().expect("run closes on noise");
        assert_eq!(run.start, 20);
        assert_eq!(run.samples.len(), 40);
        assert_eq!(run.voiced_samples, 20);
        assert!(seg.push_noise().is_none());
        assert!(!seg.has_open_run());
    }

    #[test]
    fn segmenter_hard_cut_even_without_pause() {
        let mut seg = Segmenter::new(15, 25);
        assert!(seg.push_speech(10, &[LOUD; 10], 10).is_none());
        // Past the soft limit, but the frame is voiced and loud: no cut.
        assert!(seg.push_speech(20, &[LOUD; 10], 10).is_none());
        let run = seg
            .push_speech(30, &[LOUD; 10], 10)
            .expect("cut at hard max");
        assert_eq!(run.start, 0);
        assert_eq!(run.samples.len(), 30);
        // Speech continues: a new run starts at the next slice.
        assert!(seg.push_speech(40, &[LOUD; 10], 10).is_none());
        let next = seg.flush().unwrap();
        assert_eq!(next.start, 30);
    }

    #[test]
    fn segmenter_soft_cut_at_first_unvoiced_frame() {
        let mut seg = Segmenter::new(25, 100);
        assert!(seg.push_speech(10, &[LOUD; 10], 10).is_none());
        // Unvoiced frame before the soft limit: no cut.
        assert!(seg.push_speech(20, &[LOUD; 10], 0).is_none());
        // Past the soft limit and unvoiced (hangover frame): cut here.
        let run = seg.push_speech(30, &[LOUD; 10], 0).expect("soft cut");
        assert_eq!(run.samples.len(), 30);
        assert_eq!(run.voiced_samples, 10);
    }

    #[test]
    fn segmenter_soft_cut_at_quiet_voiced_frame() {
        let mut seg = Segmenter::new(15, 100);
        assert!(seg.push_speech(10, &[LOUD; 10], 10).is_none());
        assert!(seg.push_speech(20, &[LOUD; 10], 10).is_none());
        // Voiced but far below the run's mean RMS: counts as a pause.
        let run = seg.push_speech(30, &[0.01; 10], 10).expect("quiet cut");
        assert_eq!(run.samples.len(), 30);
        // Energy resets with the new run.
        assert!(seg.push_speech(40, &[0.01; 10], 10).is_none());
    }

    #[test]
    fn production_limits_fit_the_model_window() {
        assert!(SOFT_MAX_SEGMENT_MS < HARD_MAX_SEGMENT_MS);
        assert!(HARD_MAX_SEGMENT_MS < 25_000);
        assert_eq!(FRAME_SAMPLES, 480);
    }

    #[test]
    fn speech_run_timestamps() {
        let run = SpeechRun {
            start: 16_000,
            samples: vec![0.0; 8_000],
            voiced_samples: 4_000,
        };
        assert_eq!(run.t0_ms(), 1_000);
        assert_eq!(run.t1_ms(), 1_500);
        assert_eq!(run.voiced_ms(), 250);
    }

    #[test]
    fn gap_is_applied_only_after_idle() {
        let tol = GAP_TOLERANCE_SAMPLES;
        // Continuous delivery with drift beyond tolerance: ignored.
        assert_eq!(
            gap_realign(0, 480, 480 + tol + 1_000, Duration::from_millis(20)),
            None
        );
        // Real pause in delivery: jump to the clock.
        assert_eq!(
            gap_realign(0, 480, 480 + tol + 1_000, Duration::from_secs(2)),
            Some(tol + 1_000)
        );
        // Pause but within tolerance: keep counting.
        assert_eq!(gap_realign(0, 480, 480 + tol, Duration::from_secs(2)), None);
    }

    fn frames(levels: &[(u64, f32)]) -> Vec<(u64, f32)> {
        levels.to_vec()
    }

    #[test]
    fn echo_rule_drops_speaker_bleed() {
        let mic: Vec<_> = (0..20).map(|i| (i, 0.45)).collect();
        let sys: Vec<_> = (0..20).map(|i| (i, 0.7)).collect();
        assert!(is_probable_echo(&mic, &sys));
    }

    #[test]
    fn echo_rule_keeps_own_voice() {
        // Own voice louder at the mic than the remote in the loopback.
        let mic: Vec<_> = (0..20).map(|i| (i, 0.8)).collect();
        let sys: Vec<_> = (0..20).map(|i| (i, 0.6)).collect();
        assert!(!is_probable_echo(&mic, &sys));
        // Remote silent / quiet: not echo.
        let quiet_sys: Vec<_> = (0..20).map(|i| (i, 0.2)).collect();
        let soft_mic: Vec<_> = (0..20).map(|i| (i, 0.1)).collect();
        assert!(!is_probable_echo(&soft_mic, &quiet_sys));
        // No loopback data at all (silence, or no system source).
        assert!(!is_probable_echo(&soft_mic, &[]));
    }

    #[test]
    fn echo_rule_is_conservative() {
        // Only half the frames dominated by the system: keep.
        let mic: Vec<_> = (0..20).map(|i| (i, 0.45)).collect();
        let sys: Vec<_> = (0..20)
            .map(|i| (i, if i % 2 == 0 { 0.7 } else { 0.3 }))
            .collect();
        assert!(!is_probable_echo(&mic, &sys));
        // Too short to judge.
        assert!(!is_probable_echo(
            &frames(&[(0, 0.1), (1, 0.1)]),
            &frames(&[(0, 0.9), (1, 0.9)])
        ));
        // System frames offset by one index still pair up.
        let shifted: Vec<_> = (1..21).map(|i| (i, 0.7)).collect();
        assert!(is_probable_echo(&mic, &shifted));
    }

    #[test]
    fn level_history_is_bounded() {
        let mut history = LevelHistory::default();
        for i in 0..(LEVEL_HISTORY_FRAMES as u64 + 10) {
            history.push(i, 0.5);
        }
        assert_eq!(history.frames.len(), LEVEL_HISTORY_FRAMES);
        assert_eq!(history.between(0, 9).len(), 0);
        assert_eq!(history.between(10, 19).len(), 10);
    }

    #[test]
    fn clock_excludes_pauses() {
        let t0 = Instant::now();
        let mut clock = SessionClock::new(t0);
        assert_eq!(clock.elapsed_ms(t0 + Duration::from_secs(2)), 2_000);
        clock.pause(t0 + Duration::from_secs(2));
        assert_eq!(clock.elapsed_ms(t0 + Duration::from_secs(5)), 2_000);
        clock.resume(t0 + Duration::from_secs(5));
        assert_eq!(clock.elapsed_ms(t0 + Duration::from_secs(6)), 3_000);
    }

    #[test]
    fn state_round_trip() {
        for state in [
            MeetingState::Idle,
            MeetingState::Recording,
            MeetingState::Paused,
            MeetingState::Finalizing,
        ] {
            assert_eq!(state_from_u8(state_to_u8(state)), state);
        }
    }
}
