//! Worker-side glue between the diarizer and the meeting database: turns a
//! computer-sound segment into pieces attributed to «Собеседник N» rows.
//!
//! Without the model (not downloaded, failed to load, failing at run time)
//! everything silently stays as before: one «Собеседник» for the whole track.

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use tauri::AppHandle;

use super::diarize::{self, SortformerEngine, SpeakerTracker};
use super::diarizer_model;
use super::session::SessionShared;
use super::store::MeetingStore;
use super::types::MeetingSource;

/// Consecutive run failures before the diarizer is switched off for the meeting.
const MAX_RUN_FAILURES: u32 = 3;

/// One piece of a segment: absolute meeting time, its speaker row and the
/// sample range inside the segment's audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub t0_ms: u64,
    pub t1_ms: u64,
    pub speaker_id: Option<i64>,
    pub samples: std::ops::Range<usize>,
}

enum Engine {
    /// Model not installed (yet): checked again on the next segment.
    NotLoaded,
    Ready(Box<SortformerEngine>),
    /// Failed for this meeting: no more attempts.
    Off,
}

/// Remote speakers of one meeting.
pub struct RemoteSpeakers {
    meeting_id: i64,
    engine: Engine,
    tracker: SpeakerTracker,
    /// Remote speaker slot (1-based) → `meeting_speakers.id`.
    rows: HashMap<u8, i64>,
    failures: u32,
}

/// Default label of remote speaker `slot` when there are `count` of them.
pub fn remote_label(slot: u8, count: u8) -> String {
    if count <= 1 {
        MeetingSource::System.default_label().to_string()
    } else {
        format!("{} {slot}", MeetingSource::System.default_label())
    }
}

impl RemoteSpeakers {
    pub fn new(meeting_id: i64) -> Self {
        Self {
            meeting_id,
            engine: Engine::NotLoaded,
            tracker: SpeakerTracker::new(),
            rows: HashMap::new(),
            failures: 0,
        }
    }

    fn disable(&mut self, shared: &SessionShared, reason: &str) {
        log::warn!(
            "Meeting {}: speaker separation off: {reason}",
            self.meeting_id
        );
        shared.set_error(format!("Различение собеседников отключено: {reason}"));
        self.engine = Engine::Off;
    }

    fn ensure_engine(&mut self, app: &AppHandle, shared: &SessionShared) -> bool {
        match self.engine {
            Engine::Ready(_) => return true,
            Engine::Off => return false,
            Engine::NotLoaded => {}
        }
        let Some(path) = diarizer_model::installed_model(app) else {
            return false;
        };
        let cpu_only = crate::utils::env_flag_enabled("PLOT_FLOW_DIARIZER_CPU");
        let loaded = SortformerEngine::load(&path, cpu_only).or_else(|e| {
            if cpu_only {
                Err(e)
            } else {
                log::warn!("Diarizer: GPU load failed ({e}); retrying on CPU");
                SortformerEngine::load(&path, true)
            }
        });
        match loaded {
            Ok(engine) => {
                self.engine = Engine::Ready(Box::new(engine));
                true
            }
            Err(e) => {
                self.disable(shared, &e);
                false
            }
        }
    }

    /// Attribute one system segment (starting at `t0_ms` of the meeting) to
    /// remote speakers. `None` keeps the segment as it is (single
    /// «Собеседник», `base_speaker`).
    pub fn split(
        &mut self,
        app: &AppHandle,
        store: &MeetingStore,
        shared: &SessionShared,
        base_speaker: Option<i64>,
        t0_ms: u64,
        samples: &[f32],
    ) -> Option<Vec<Piece>> {
        if shared.exiting.load(Ordering::Acquire) || samples.is_empty() {
            return None;
        }
        if !self.ensure_engine(app, shared) {
            return None;
        }
        let Engine::Ready(engine) = &mut self.engine else {
            return None;
        };
        let started = std::time::Instant::now();
        let assigned = match engine.diarize_segment(&mut self.tracker, samples) {
            Ok(assigned) => {
                self.failures = 0;
                assigned
            }
            Err(e) => {
                self.failures += 1;
                log::warn!("Meeting {}: diarization failed: {e}", self.meeting_id);
                if self.failures >= MAX_RUN_FAILURES {
                    self.disable(shared, &e);
                }
                return None;
            }
        };
        log::debug!(
            "Meeting {}: diarized {} ms of system audio in {} ms → {:?}",
            self.meeting_id,
            diarize::samples_to_ms(samples.len()),
            started.elapsed().as_millis(),
            assigned
        );
        let pieces = diarize::snap_pieces(&assigned, samples);

        let mut out = Vec::with_capacity(pieces.len());
        let last = pieces.len().saturating_sub(1);
        for (i, piece) in pieces.iter().enumerate() {
            let speaker_id = self.row_for_slot(store, base_speaker, piece.slot);
            let a = diarize::ms_to_samples(piece.t0_ms).min(samples.len());
            let b = if i == last {
                samples.len()
            } else {
                diarize::ms_to_samples(piece.t1_ms).min(samples.len())
            };
            if b <= a {
                continue;
            }
            out.push(Piece {
                t0_ms: t0_ms + piece.t0_ms,
                t1_ms: t0_ms + piece.t1_ms,
                speaker_id: speaker_id.or(base_speaker),
                samples: a..b,
            });
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// The `meeting_speakers` row of remote speaker `slot`, created on first
    /// appearance. Slot 1 is the row made at start («Собеседник»); when a
    /// second remote speaker appears it becomes «Собеседник 1».
    fn row_for_slot(&mut self, store: &MeetingStore, base: Option<i64>, slot: u8) -> Option<i64> {
        if let Some(id) = self.rows.get(&slot) {
            return Some(*id);
        }
        if slot == 1 {
            if let Some(base) = base {
                self.rows.insert(1, base);
                return Some(base);
            }
        }
        let count = self.tracker.speaker_count().max(slot);
        let row = match store.add_speaker(
            self.meeting_id,
            i32::from(slot),
            MeetingSource::System,
            &remote_label(slot, count),
            i32::from(slot),
        ) {
            Ok(row) => row,
            Err(e) => {
                log::warn!(
                    "Meeting {}: failed to add remote speaker {slot}: {e}",
                    self.meeting_id
                );
                return None;
            }
        };
        self.rows.insert(slot, row.id);
        // The first remote speaker gets its number once there are two.
        if count >= 2 {
            if let Some(first) = self.rows.get(&1).copied() {
                if let Err(e) = store.set_speaker_label(first, &remote_label(1, count)) {
                    log::warn!(
                        "Meeting {}: failed to relabel speaker: {e}",
                        self.meeting_id
                    );
                }
            }
        }
        Some(row.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_number_only_with_several_speakers() {
        assert_eq!(remote_label(1, 1), "Собеседник");
        assert_eq!(remote_label(1, 2), "Собеседник 1");
        assert_eq!(remote_label(3, 3), "Собеседник 3");
    }

    #[test]
    fn slots_map_to_rows_and_first_is_relabelled() {
        let dir = tempfile::tempdir().unwrap();
        let store = MeetingStore::open(dir.path().join("meetings.db")).unwrap();
        let id = store
            .create_meeting(
                "t",
                1,
                super::super::types::MeetingSources {
                    microphone: true,
                    system_audio: true,
                },
            )
            .unwrap();
        let base = store
            .add_speaker(id, 1, MeetingSource::System, "Собеседник", 1)
            .unwrap();
        let mut remote = RemoteSpeakers::new(id);
        assert_eq!(remote.row_for_slot(&store, Some(base.id), 1), Some(base.id));
        let second = remote.row_for_slot(&store, Some(base.id), 2).unwrap();
        assert_ne!(second, base.id);
        // Same slot → same row.
        assert_eq!(remote.row_for_slot(&store, Some(base.id), 2), Some(second));
        let speakers = store.list_speakers(id).unwrap();
        assert_eq!(speakers.len(), 2);
        assert_eq!(speakers[0].label, "Собеседник 1");
        assert_eq!(speakers[1].label, "Собеседник 2");
        assert_eq!(speakers[1].slot, 2);
        assert_eq!(speakers[1].color_index, 2);
    }
}
