//! Meeting mode («Совещание»): records the room microphone and the computer's
//! sound (WASAPI loopback), transcribes speech live into who-said-what
//! segments, stores them in `meetings.db`, exports text and asks the local LLM
//! for a short summary.
//!
//! Speakers: microphone → «Я» (slot 0). The computer sound is one
//! «Собеседник» (slot 1) unless the optional diarization model is installed;
//! then the worker tells remote speakers apart («Собеседник 1…4», slots 1–4,
//! see `diarize.rs` / `remote_speakers.rs`).

pub mod capture;
pub mod commands;
pub mod diarize;
pub mod diarizer_model;
pub mod export;
pub mod remote_speakers;
pub mod session;
pub mod store;
pub mod types;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};

use session::{SessionShared, SourceContext};
use store::MeetingStore;
use types::{
    MeetingInfo, MeetingRecordStatus, MeetingSource, MeetingSources, MeetingState, MeetingStatus,
};

/// How long `stop` waits for queued segments to be transcribed.
const WORKER_DRAIN_TIMEOUT: Duration = Duration::from_secs(300);
const CAPTURE_JOIN_TIMEOUT: Duration = Duration::from_secs(10);
const TICKER_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Thread handles of the running meeting.
struct SessionHandles {
    sources: Vec<JoinHandle<()>>,
    worker: JoinHandle<()>,
    ticker: JoinHandle<()>,
}

/// Owner of the meeting database and of the (at most one) running meeting.
pub struct MeetingManager {
    app: AppHandle,
    store: Arc<MeetingStore>,
    /// `<app data dir>/meetings` — per-meeting audio folders and `exports/`.
    meetings_dir: PathBuf,
    /// Shared state of the running (or finalizing) meeting.
    active: Mutex<Option<Arc<SessionShared>>>,
    /// Threads of the running meeting; taken by stop/discard.
    handles: Mutex<Option<SessionHandles>>,
    /// Last error outside a session (e.g. a failed start).
    idle_error: Mutex<Option<String>>,
}

impl MeetingManager {
    /// Open `meetings.db` in the app data directory (portable-aware, same as
    /// `HistoryManager`) and recover meetings interrupted by a crash.
    pub fn new(app: &AppHandle) -> anyhow::Result<Self> {
        let data_dir = crate::portable::app_data_dir(app)?;
        let meetings_dir = data_dir.join("meetings");
        std::fs::create_dir_all(&meetings_dir)?;
        let store = MeetingStore::open(data_dir.join("meetings.db"))?;
        Ok(Self {
            app: app.clone(),
            store: Arc::new(store),
            meetings_dir,
            active: Mutex::new(None),
            handles: Mutex::new(None),
            idle_error: Mutex::new(None),
        })
    }

    /// Database handle.
    pub fn store(&self) -> &Arc<MeetingStore> {
        &self.store
    }

    /// `<app data dir>/meetings`.
    pub fn meetings_dir(&self) -> &PathBuf {
        &self.meetings_dir
    }

    fn active_shared(&self) -> Option<Arc<SessionShared>> {
        self.active.lock().ok().and_then(|slot| slot.clone())
    }

    /// Id of the meeting being recorded or finalized, if any.
    pub fn active_meeting_id(&self) -> Option<i64> {
        self.active_shared().map(|shared| shared.meeting_id)
    }

    /// Current recorder status.
    pub fn status(&self) -> MeetingStatus {
        match self.active_shared() {
            Some(shared) => shared.status(),
            None => MeetingStatus {
                state: MeetingState::Idle,
                meeting_id: None,
                elapsed_ms: 0,
                mic_level: 0.0,
                system_level: 0.0,
                segment_count: 0,
                last_error: self.idle_error.lock().ok().and_then(|e| e.clone()),
            },
        }
    }

    fn emit_status(&self) {
        let _ = self.app.emit("meeting-status", self.status());
    }

    fn set_idle_error(&self, message: Option<String>) {
        if let Ok(mut slot) = self.idle_error.lock() {
            *slot = message;
        }
    }

    /// Start recording a new meeting. Blocking (opens audio devices); call
    /// from a blocking-capable thread.
    pub fn start(&self, sources: MeetingSources) -> Result<MeetingInfo, String> {
        if !sources.microphone && !sources.system_audio {
            return Err("Выберите хотя бы один источник звука".to_string());
        }
        // Reserve the slot first so two concurrent starts cannot both proceed.
        let shared = {
            let mut active = self
                .active
                .lock()
                .map_err(|_| "Внутренняя ошибка состояния".to_string())?;
            if active.is_some() {
                return Err("Совещание уже идёт".to_string());
            }
            let now_ms = chrono::Utc::now().timestamp_millis();
            let title = default_title();
            let meeting_id = self
                .store
                .create_meeting(&title, now_ms, sources)
                .map_err(|e| format!("Не удалось создать совещание: {e}"))?;
            let shared = Arc::new(SessionShared::new(meeting_id));
            *active = Some(Arc::clone(&shared));
            shared
        };

        match self.start_threads(&shared, sources) {
            Ok(info) => {
                self.set_idle_error(None);
                self.emit_status();
                Ok(info)
            }
            Err(message) => {
                let _ = self.store.delete_meeting(shared.meeting_id);
                let _ =
                    std::fs::remove_dir_all(self.meetings_dir.join(shared.meeting_id.to_string()));
                if let Ok(mut active) = self.active.lock() {
                    *active = None;
                }
                self.set_idle_error(Some(message.clone()));
                self.emit_status();
                Err(message)
            }
        }
    }

    fn start_threads(
        &self,
        shared: &Arc<SessionShared>,
        sources: MeetingSources,
    ) -> Result<MeetingInfo, String> {
        let meeting_id = shared.meeting_id;
        let audio_dir = self.meetings_dir.join(meeting_id.to_string());
        std::fs::create_dir_all(&audio_dir)
            .map_err(|e| format!("Не удалось создать папку совещания: {e}"))?;
        let _ = self
            .store
            .set_audio_dir(meeting_id, &meeting_id.to_string());

        let vad_model_path = session::resolve_vad_model(&self.app)?;
        let selected_microphone = crate::settings::get_settings(&self.app).selected_microphone;

        // Make sure the speech model is (being) loaded before the first segment.
        if let Some(tm) = self
            .app
            .try_state::<Arc<crate::managers::transcription::TranscriptionManager>>()
        {
            tm.initiate_model_load();
        }

        let (job_tx, job_rx) = session::job_channel();
        let mut threads = Vec::new();
        let mut errors = Vec::new();
        let mut opened = MeetingSources {
            microphone: false,
            system_audio: false,
        };

        let wanted = [
            (MeetingSource::Microphone, sources.microphone, 0, 0),
            (MeetingSource::System, sources.system_audio, 1, 1),
        ];
        for (source, enabled, slot, color) in wanted {
            if !enabled {
                continue;
            }
            let speaker = match self.store.add_speaker(
                meeting_id,
                slot,
                source,
                source.default_label(),
                color,
            ) {
                Ok(speaker) => speaker,
                Err(e) => {
                    errors.push(format!("Не удалось сохранить участника: {e}"));
                    continue;
                }
            };
            let ctx = SourceContext {
                app: self.app.clone(),
                store: Arc::clone(&self.store),
                shared: Arc::clone(shared),
                source,
                speaker_id: Some(speaker.id),
                audio_dir: audio_dir.clone(),
                vad_model_path: vad_model_path.clone(),
                selected_microphone: selected_microphone.clone(),
            };
            match session::spawn_source(ctx, job_tx.clone()) {
                Ok(handle) => {
                    threads.push(handle);
                    match source {
                        MeetingSource::Microphone => opened.microphone = true,
                        MeetingSource::System => opened.system_audio = true,
                    }
                }
                Err(message) => {
                    log::warn!("Meeting {meeting_id}: {source:?} unavailable: {message}");
                    let _ = self.store.delete_speaker(speaker.id);
                    errors.push(match source {
                        MeetingSource::Microphone => format!("Микрофон: {message}"),
                        MeetingSource::System => format!("Звук компьютера: {message}"),
                    });
                }
            }
        }
        // Only the source threads hold senders now: the worker ends when they do.
        drop(job_tx);

        if threads.is_empty() {
            return Err(errors.join("; "));
        }
        if !errors.is_empty() {
            shared.set_error(errors.join("; "));
            let _ = self.app.emit(
                "meeting-error",
                types::MeetingErrorPayload {
                    message: errors.join("; "),
                },
            );
        }
        let _ = self.store.set_sources(meeting_id, opened);

        // Timestamps count from the moment every stream is running.
        shared.reset_clock();

        let worker = match session::spawn_worker(
            self.app.clone(),
            Arc::clone(&self.store),
            Arc::clone(shared),
            job_rx,
        ) {
            Ok(worker) => worker,
            Err(message) => {
                shared.request_capture_stop();
                for handle in threads {
                    session::join_with_timeout(handle, CAPTURE_JOIN_TIMEOUT, "capture thread");
                }
                return Err(message);
            }
        };
        let ticker = match session::spawn_ticker(
            self.app.clone(),
            Arc::clone(&self.store),
            Arc::clone(shared),
        ) {
            Ok(ticker) => ticker,
            Err(message) => {
                shared.request_capture_stop();
                for handle in threads {
                    session::join_with_timeout(handle, CAPTURE_JOIN_TIMEOUT, "capture thread");
                }
                session::join_with_timeout(worker, CAPTURE_JOIN_TIMEOUT, "worker");
                return Err(message);
            }
        };

        if let Ok(mut slot) = self.handles.lock() {
            *slot = Some(SessionHandles {
                sources: threads,
                worker,
                ticker,
            });
        }

        // Threads are running now: never fail from here on.
        Ok(self
            .store
            .get_info(meeting_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| MeetingInfo {
                id: meeting_id,
                title: default_title(),
                started_at: chrono::Utc::now().timestamp_millis(),
                ended_at: None,
                duration_ms: 0,
                sources: opened,
                segment_count: 0,
                speaker_count: 0,
                status: MeetingRecordStatus::Recording,
                summary: None,
            }))
    }

    /// Pause recording (audio is dropped, the clock stops).
    pub fn pause(&self) -> Result<(), String> {
        let shared = self
            .active_shared()
            .ok_or_else(|| "Совещание не идёт".to_string())?;
        match shared.state() {
            MeetingState::Recording => {
                shared.pause();
                self.emit_status();
                Ok(())
            }
            MeetingState::Paused => Ok(()),
            _ => Err("Совещание завершается".to_string()),
        }
    }

    /// Resume after [`MeetingManager::pause`].
    pub fn resume(&self) -> Result<(), String> {
        let shared = self
            .active_shared()
            .ok_or_else(|| "Совещание не идёт".to_string())?;
        match shared.state() {
            MeetingState::Paused => {
                shared.resume();
                self.emit_status();
                Ok(())
            }
            MeetingState::Recording => Ok(()),
            _ => Err("Совещание завершается".to_string()),
        }
    }

    /// Stop capture, wait for pending segments to be transcribed, then close
    /// the meeting. Blocking; returns the final info (also emitted as
    /// `meeting-finished`).
    pub fn stop(&self) -> Result<MeetingInfo, String> {
        let (shared, handles) = self.take_session()?;
        let meeting_id = shared.meeting_id;
        shared.set_state(MeetingState::Finalizing);
        self.emit_status();

        let duration_ms = shared.elapsed_ms();
        let drained = self.shutdown_threads(&shared, handles, None);

        // Rows still without text were never recognized (drain timed out).
        let _ = self.store.delete_empty_segments(meeting_id);
        let ended_at = chrono::Utc::now().timestamp_millis();
        let finish_result = self.store.finish_meeting(
            meeting_id,
            ended_at,
            duration_ms,
            MeetingRecordStatus::Completed,
        );
        if finish_result.is_ok() && drained {
            // Only the text matters to the owner: the WAV safety copies of a
            // cleanly finished meeting are not kept (interrupted ones are).
            let _ = std::fs::remove_dir_all(self.meetings_dir.join(meeting_id.to_string()));
            let _ = self.store.set_audio_dir(meeting_id, "");
        }
        self.clear_active();
        self.emit_status();
        // Unload settings were held back while the meeting ran.
        if let Some(tm) = self
            .app
            .try_state::<Arc<crate::managers::transcription::TranscriptionManager>>()
        {
            tm.maybe_unload_immediately("meeting");
        }
        finish_result.map_err(|e| format!("Не удалось завершить совещание: {e}"))?;

        let info = self
            .store
            .get_info(meeting_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Совещание не найдено".to_string())?;
        let _ = self.app.emit("meeting-finished", info.clone());
        Ok(info)
    }

    /// Stop and delete the running meeting with all its data.
    pub fn discard(&self) -> Result<(), String> {
        let (shared, handles) = self.take_session()?;
        let meeting_id = shared.meeting_id;
        shared
            .discard
            .store(true, std::sync::atomic::Ordering::Release);
        shared.set_state(MeetingState::Finalizing);
        self.emit_status();

        self.shutdown_threads(&shared, handles, None);

        let result = self.store.delete_meeting(meeting_id);
        let _ = std::fs::remove_dir_all(self.meetings_dir.join(meeting_id.to_string()));
        self.clear_active();
        self.emit_status();
        result
            .map(|_| ())
            .map_err(|e| format!("Не удалось удалить совещание: {e}"))
    }

    fn take_session(&self) -> Result<(Arc<SessionShared>, SessionHandles), String> {
        let shared = self
            .active_shared()
            .ok_or_else(|| "Совещание не идёт".to_string())?;
        let handles = self
            .handles
            .lock()
            .map_err(|_| "Внутренняя ошибка состояния".to_string())?
            .take()
            .ok_or_else(|| "Совещание уже завершается".to_string())?;
        Ok((shared, handles))
    }

    fn clear_active(&self) {
        if let Ok(mut active) = self.active.lock() {
            *active = None;
        }
    }

    /// Flush capture threads (their last speech runs are queued), let the
    /// worker drain the queue, then stop the ticker. With `deadline`, every
    /// wait is cut to fit before it (app exit). Returns `true` when the worker
    /// finished (every queued segment was handled).
    fn shutdown_threads(
        &self,
        shared: &Arc<SessionShared>,
        handles: SessionHandles,
        deadline: Option<Instant>,
    ) -> bool {
        let limit = |normal: Duration| match deadline {
            Some(deadline) => normal.min(deadline.saturating_duration_since(Instant::now())),
            None => normal,
        };
        shared.request_capture_stop();
        for handle in handles.sources {
            session::join_with_timeout(handle, limit(CAPTURE_JOIN_TIMEOUT), "capture thread");
        }
        // Senders are gone once the capture threads exit → worker drains and ends.
        let drained = session::join_with_timeout(
            handles.worker,
            limit(WORKER_DRAIN_TIMEOUT),
            "transcription worker",
        );
        shared.request_ticker_stop();
        session::join_with_timeout(handles.ticker, limit(TICKER_JOIN_TIMEOUT), "status ticker");
        drained
    }

    /// App exit while a meeting runs: flush the capture threads and let the
    /// worker transcribe what it can within `budget` (best effort), then mark
    /// the meeting interrupted. Its WAV backup stays on disk.
    pub fn shutdown_for_exit(&self, budget: Duration) {
        let Some(shared) = self.active_shared() else {
            return;
        };
        let meeting_id = shared.meeting_id;
        log::info!("Meeting {meeting_id}: app exiting, closing the meeting as interrupted");
        let deadline = Instant::now() + budget;
        let duration_ms = shared.elapsed_ms();
        shared
            .exiting
            .store(true, std::sync::atomic::Ordering::Release);
        shared.set_state(MeetingState::Finalizing);

        let handles = self.handles.lock().ok().and_then(|mut slot| slot.take());
        match handles {
            Some(handles) => {
                self.shutdown_threads(&shared, handles, Some(deadline));
            }
            None => {
                // A stop/discard is already tearing the threads down.
                shared.request_capture_stop();
            }
        }
        if shared.discard.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let _ = self.store.delete_empty_segments(meeting_id);
        let ended_at = chrono::Utc::now().timestamp_millis();
        if let Err(e) = self.store.finish_meeting(
            meeting_id,
            ended_at,
            duration_ms,
            MeetingRecordStatus::Interrupted,
        ) {
            log::error!("Meeting {meeting_id}: failed to mark interrupted at exit: {e}");
        }
        self.clear_active();
    }

    /// Delete a finished meeting (DB rows and audio folder).
    pub fn delete(&self, meeting_id: i64) -> Result<(), String> {
        if self.active_meeting_id() == Some(meeting_id) {
            return Err("Нельзя удалить идущее совещание".to_string());
        }
        let removed = self
            .store
            .delete_meeting(meeting_id)
            .map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir_all(self.meetings_dir.join(meeting_id.to_string()));
        if removed {
            Ok(())
        } else {
            Err("Совещание не найдено".to_string())
        }
    }
}

/// «Совещание 16.11.2026 14:05» in local time.
fn default_title() -> String {
    format!(
        "Совещание {}",
        chrono::Local::now().format("%d.%m.%Y %H:%M")
    )
}
