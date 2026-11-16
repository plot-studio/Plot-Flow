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

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};

use super::capture::{self, CaptureStream};
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
/// Silence that ends a speech run.
const END_SILENCE_MS: u64 = 600;
/// Hard cap of one segment.
const MAX_SEGMENT_MS: u64 = 20_000;
/// Runs shorter than this (clicks, coughs) are not transcribed.
const MIN_SEGMENT_MS: u64 = 400;
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
/// Status event period.
const STATUS_PERIOD: Duration = Duration::from_millis(250);

fn samples_to_ms(samples: u64) -> u64 {
    samples * 1000 / SAMPLE_RATE
}

fn ms_to_samples(ms: u64) -> u64 {
    ms * SAMPLE_RATE / 1000
}

/* ───────────────────────────── Segmenter ───────────────────────────── */

/// A finished speech run.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechRun {
    /// Start position in 16 kHz samples since the meeting start.
    pub start: u64,
    pub samples: Vec<f32>,
}

impl SpeechRun {
    pub fn t0_ms(&self) -> u64 {
        samples_to_ms(self.start)
    }

    pub fn t1_ms(&self) -> u64 {
        samples_to_ms(self.start + self.samples.len() as u64)
    }
}

/// Turns the smoothed VAD's speech/noise stream into bounded speech runs.
///
/// The smoothed VAD already adds pre-roll and a hangover tail of
/// [`END_SILENCE_MS`], so the first `Noise` after speech means "≥600 ms of
/// silence"; a run is also cut when it reaches `max_samples`.
pub struct Segmenter {
    max_samples: usize,
    current: Option<SpeechRun>,
}

impl Segmenter {
    pub fn new(max_samples: usize) -> Self {
        Self {
            max_samples: max_samples.max(1),
            current: None,
        }
    }

    /// Feed a speech slice that ends at absolute position `end_pos`.
    pub fn push_speech(&mut self, end_pos: u64, slice: &[f32]) -> Option<SpeechRun> {
        let run = self.current.get_or_insert_with(|| SpeechRun {
            start: end_pos.saturating_sub(slice.len() as u64),
            samples: Vec::with_capacity(16_000 * 4),
        });
        run.samples.extend_from_slice(slice);
        if run.samples.len() >= self.max_samples {
            return self.current.take();
        }
        None
    }

    /// Feed a non-speech decision: closes the open run, if any.
    pub fn push_noise(&mut self) -> Option<SpeechRun> {
        self.current.take()
    }

    /// Close the open run (pause, stop, gap in the device stream).
    pub fn flush(&mut self) -> Option<SpeechRun> {
        self.current.take()
    }

    pub fn has_open_run(&self) -> bool {
        self.current.is_some()
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
    clock: Mutex<SessionClock>,
    mic_level: AtomicU32,
    system_level: AtomicU32,
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
            clock: Mutex::new(SessionClock::new(Instant::now())),
            mic_level: AtomicU32::new(0),
            system_level: AtomicU32::new(0),
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

/// Everything a processing thread needs besides its stream.
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
    pub jobs: SyncSender<SegmentJob>,
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

fn build_vad(model_path: &PathBuf) -> Result<SmoothedVad, String> {
    let silero = SileroVad::new(model_path, SILERO_THRESHOLD)
        .map_err(|e| format!("Не удалось создать VAD: {e}"))?;
    let frame_samples = silero.frame_samples();
    Ok(SmoothedVad::new(
        Box::new(silero),
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
    vad: SmoothedVad,
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

        self.wav.push(frame);

        let finished = match self.vad.push_frame(frame) {
            Ok(VadFrame::Speech(slice)) => self.segmenter.push_speech(self.pos, slice),
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

    /// Persist a tentative segment and queue it for recognition.
    fn dispatch(&mut self, mut run: SpeechRun) {
        if (run.samples.len() as u64) < ms_to_samples(MIN_SEGMENT_MS) {
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
        match ctx.jobs.try_send(job) {
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
pub fn spawn_source(ctx: SourceContext) -> Result<JoinHandle<()>, String> {
    let (init_tx, init_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    let source = ctx.source;
    let handle = std::thread::Builder::new()
        .name(format!("meeting-{}", source.file_prefix()))
        .spawn(move || run_source(ctx, init_tx))
        .map_err(|e| format!("Не удалось запустить поток записи: {e}"))?;

    match init_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => Ok(handle),
        Ok(Err(message)) => {
            let _ = handle.join();
            Err(message)
        }
        Err(_) => Err(format!(
            "Источник «{}» не ответил вовремя",
            source.default_label()
        )),
    }
}

fn run_source(ctx: SourceContext, init_tx: SyncSender<Result<(), String>>) {
    let vad = match build_vad(&ctx.vad_model_path) {
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
    let _ = init_tx.send(Ok(()));
    log::info!(
        "Meeting {}: {:?} capture running on {:?} @ {} Hz",
        ctx.shared.meeting_id,
        ctx.source,
        stream.device_name,
        stream.sample_rate
    );

    let in_rate = stream.sample_rate.max(1) as usize;
    let mut resampler = FrameResampler::new(
        in_rate,
        SAMPLE_RATE as usize,
        Duration::from_millis(FRAME_MS),
    );
    let mut processor = SourceProcessor {
        wav: WavChunker::new(ctx.audio_dir.clone(), ctx.source.file_prefix()),
        segmenter: Segmenter::new(ms_to_samples(MAX_SEGMENT_MS) as usize),
        vad,
        vad_failed: false,
        pos: 0,
        level: 0.0,
        ctx,
    };

    let shared = Arc::clone(&processor.ctx.shared);
    let mut was_paused = false;
    let mut last_data = Instant::now();
    let mut error_reported = false;

    loop {
        let stopping = shared.stop_capture.load(Ordering::Acquire);

        if shared.is_paused() && !stopping {
            // Drop audio captured while paused.
            stream.drain(|_| {});
            if !was_paused {
                processor.flush();
                resampler.reset();
                was_paused = true;
                shared.set_level(processor.ctx.source, 0.0);
            }
            std::thread::sleep(POLL_INTERVAL);
            continue;
        }
        if was_paused {
            // The clock excluded the pause; continue from where it stands.
            was_paused = false;
            processor.pos = ms_to_samples(shared.elapsed_ms());
            last_data = Instant::now();
        }

        // Collect what the callback delivered since the last poll.
        let mut batch: Vec<f32> = Vec::new();
        stream.drain(|slice| batch.extend_from_slice(slice));

        if !batch.is_empty() {
            last_data = Instant::now();
            let batch_16k = batch.len() as u64 * SAMPLE_RATE / in_rate as u64;
            let clock_pos = ms_to_samples(shared.elapsed_ms());
            if clock_pos > processor.pos + batch_16k + GAP_TOLERANCE_SAMPLES {
                // The device delivered nothing for a while (loopback during
                // silence): close the run and jump to the wall clock.
                processor.flush();
                processor.pos = clock_pos.saturating_sub(batch_16k);
            }
            resampler.push(&batch, |frame| processor.process_frame(frame));
        } else if processor.segmenter.has_open_run() && last_data.elapsed() >= IDLE_FLUSH {
            processor.flush();
            shared.set_level(processor.ctx.source, 0.0);
        }

        if stream.has_failed() && !error_reported {
            error_reported = true;
            report_error(
                &processor.ctx.app,
                &shared,
                format!(
                    "Источник «{}» отключился",
                    processor.ctx.source.default_label()
                ),
            );
        }

        if stopping {
            resampler.finish(|frame| processor.process_frame(frame));
            processor.flush();
            processor.wav.finish();
            shared.set_level(processor.ctx.source, 0.0);
            break;
        }

        std::thread::sleep(POLL_INTERVAL);
    }

    let overruns = stream.flags.overrun_samples.load(Ordering::Relaxed);
    if overruns > 0 {
        log::warn!(
            "Meeting {:?} capture dropped {overruns} samples (ring full)",
            processor.ctx.source
        );
    }
    drop(stream);
    log::info!("Meeting {:?} capture thread finished", processor.ctx.source);
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
    while let Ok(job) = jobs.recv() {
        if shared.discard.load(Ordering::Acquire) {
            continue;
        }
        let text = match transcribe_job(&app, job.samples) {
            Ok(text) => text,
            Err(e) => {
                report_error(&app, &shared, format!("Ошибка распознавания: {e}"));
                String::new()
            }
        };
        if shared.discard.load(Ordering::Acquire) {
            continue;
        }
        let text = text.trim().to_string();
        if text.is_empty() {
            let _ = store.delete_segment(job.segment_id);
            let _ = app.emit(
                "meeting-segment",
                MeetingSegment {
                    id: job.segment_id,
                    meeting_id: shared.meeting_id,
                    t0_ms: job.t0_ms,
                    t1_ms: job.t1_ms,
                    speaker_id: job.speaker_id,
                    source: job.source,
                    text: String::new(),
                    is_final: true,
                },
            );
            continue;
        }
        match store.finalize_segment(job.segment_id, job.t1_ms, &text) {
            Ok(Some(segment)) => {
                shared.segment_count.fetch_add(1, Ordering::Relaxed);
                let _ = app.emit("meeting-segment", segment);
            }
            Ok(None) => {}
            Err(e) => report_error(&app, &shared, format!("Не удалось сохранить текст: {e}")),
        }
    }
    log::info!("Meeting {} transcription worker finished", shared.meeting_id);
}

fn transcribe_job(app: &AppHandle, samples: Vec<f32>) -> Result<String, String> {
    let manager = app
        .try_state::<Arc<TranscriptionManager>>()
        .map(|state| Arc::clone(state.inner()))
        .ok_or_else(|| "модуль распознавания не готов".to_string())?;
    if !manager.is_model_loaded() {
        // transcribe() waits for a load that is in progress.
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

    #[test]
    fn segmenter_starts_run_at_prefill_and_closes_on_noise() {
        let mut seg = Segmenter::new(100);
        // Onset emits 30 samples (prefill + current) ending at position 50.
        assert!(seg.push_speech(50, &[0.1; 30]).is_none());
        assert!(seg.push_speech(60, &[0.2; 10]).is_none());
        let run = seg.push_noise().expect("run closes on noise");
        assert_eq!(run.start, 20);
        assert_eq!(run.samples.len(), 40);
        assert!(seg.push_noise().is_none());
        assert!(!seg.has_open_run());
    }

    #[test]
    fn segmenter_cuts_long_runs() {
        let mut seg = Segmenter::new(25);
        assert!(seg.push_speech(10, &[0.0; 10]).is_none());
        assert!(seg.push_speech(20, &[0.0; 10]).is_none());
        let run = seg.push_speech(30, &[0.0; 10]).expect("cut at max length");
        assert_eq!(run.start, 0);
        assert_eq!(run.samples.len(), 30);
        // Speech continues: a new run starts at the next slice.
        assert!(seg.push_speech(40, &[0.0; 10]).is_none());
        let next = seg.flush().unwrap();
        assert_eq!(next.start, 30);
    }

    #[test]
    fn speech_run_timestamps() {
        let run = SpeechRun {
            start: 16_000,
            samples: vec![0.0; 8_000],
        };
        assert_eq!(run.t0_ms(), 1_000);
        assert_eq!(run.t1_ms(), 1_500);
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
