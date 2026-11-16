//! SQLite persistence for meetings (`<app data dir>/meetings.db`).
//!
//! Follows the `history.rs` / `statistics.rs` pattern: one short-lived
//! connection per operation (so the store is trivially `Send + Sync`), WAL
//! journal, migrations tracked through `user_version` by `rusqlite_migration`.

use anyhow::{anyhow, Result};
use log::{debug, info, warn};
use rusqlite::{params, Connection, OptionalExtension, Row};
use rusqlite_migration::{Migrations, M};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::types::{
    MeetingDetail, MeetingInfo, MeetingRecordStatus, MeetingSegment, MeetingSource, MeetingSources,
    MeetingSpeaker,
};

const DATABASE_BUSY_TIMEOUT: Duration = Duration::from_secs(2);

static MIGRATIONS: &[M] = &[M::up(
    "CREATE TABLE IF NOT EXISTS meetings (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        title TEXT NOT NULL,
        started_at INTEGER NOT NULL,
        ended_at INTEGER,
        duration_ms INTEGER NOT NULL DEFAULT 0,
        mic INTEGER NOT NULL DEFAULT 1,
        system_audio INTEGER NOT NULL DEFAULT 0,
        status TEXT NOT NULL DEFAULT 'recording',
        summary TEXT,
        audio_dir TEXT NOT NULL DEFAULT ''
    );
    CREATE TABLE IF NOT EXISTS meeting_speakers (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
        slot INTEGER NOT NULL DEFAULT 0,
        source TEXT NOT NULL,
        label TEXT NOT NULL,
        name TEXT,
        color_index INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS meeting_segments (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        meeting_id INTEGER NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
        t0_ms INTEGER NOT NULL,
        t1_ms INTEGER NOT NULL,
        speaker_id INTEGER REFERENCES meeting_speakers(id) ON DELETE SET NULL,
        source TEXT NOT NULL,
        text TEXT NOT NULL DEFAULT '',
        is_final INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_meeting_segments_meeting
        ON meeting_segments(meeting_id, t0_ms, id);
    CREATE INDEX IF NOT EXISTS idx_meeting_speakers_meeting
        ON meeting_speakers(meeting_id, slot);",
)];

const INFO_COLUMNS: &str = "m.id, m.title, m.started_at, m.ended_at, m.duration_ms, m.mic, \
     m.system_audio, m.status, m.summary, \
     (SELECT COUNT(*) FROM meeting_segments s WHERE s.meeting_id = m.id AND s.text <> '') AS segment_count, \
     (SELECT COUNT(*) FROM meeting_speakers p WHERE p.meeting_id = m.id) AS speaker_count";

const SPEAKER_COLUMNS: &str = "id, meeting_id, slot, source, label, name, color_index";
const SEGMENT_COLUMNS: &str = "id, meeting_id, t0_ms, t1_ms, speaker_id, source, text, is_final";

/// Meeting database handle. Cheap to clone/share; opens a connection per call.
pub struct MeetingStore {
    db_path: PathBuf,
}

fn ms_to_u64(value: i64) -> u64 {
    value.max(0) as u64
}

fn u64_to_sql(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

impl MeetingStore {
    /// Open (creating if needed) the database at `db_path`, run migrations and
    /// mark meetings left in `recording` by a previous process as interrupted.
    pub fn open(db_path: PathBuf) -> Result<Self> {
        if let Some(parent) = db_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let store = Self { db_path };
        store.init_database()?;
        let recovered = store.recover_interrupted(chrono::Utc::now().timestamp_millis())?;
        if recovered > 0 {
            warn!("Marked {recovered} unfinished meeting(s) as interrupted after restart");
        }
        Ok(store)
    }

    /// Path of the SQLite file.
    pub fn path(&self) -> &Path {
        &self.db_path
    }

    fn init_database(&self) -> Result<()> {
        info!("Initializing meetings database at {:?}", self.db_path);
        let mut conn = Connection::open(&self.db_path)?;
        conn.busy_timeout(DATABASE_BUSY_TIMEOUT)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )?;
        let migrations = Migrations::new(MIGRATIONS.to_vec());
        #[cfg(debug_assertions)]
        migrations
            .validate()
            .map_err(|e| anyhow!("Invalid meeting migrations: {e}"))?;
        migrations
            .to_latest(&mut conn)
            .map_err(|e| anyhow!("Failed to migrate meetings.db: {e}"))?;
        Ok(())
    }

    fn conn(&self) -> Result<Connection> {
        let conn = Connection::open(&self.db_path)?;
        conn.busy_timeout(DATABASE_BUSY_TIMEOUT)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(conn)
    }

    /// Crash recovery: any meeting still `recording` becomes `interrupted`;
    /// tentative segments are closed so the UI stops showing "…" for them.
    /// Returns the number of meetings touched.
    pub fn recover_interrupted(&self, now_ms: i64) -> Result<usize> {
        let conn = self.conn()?;
        let ids: Vec<i64> = conn
            .prepare("SELECT id FROM meetings WHERE status = 'recording'")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in &ids {
            let last_t1: Option<i64> = conn
                .query_row(
                    "SELECT MAX(t1_ms) FROM meeting_segments WHERE meeting_id = ?1",
                    params![id],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .optional()?
                .flatten();
            conn.execute(
                "UPDATE meetings
                 SET status = 'interrupted',
                     ended_at = COALESCE(ended_at, ?2),
                     duration_ms = CASE WHEN duration_ms > 0 THEN duration_ms ELSE ?3 END
                 WHERE id = ?1",
                params![id, now_ms, last_t1.unwrap_or(0)],
            )?;
            conn.execute(
                "UPDATE meeting_segments SET is_final = 1 WHERE meeting_id = ?1 AND is_final = 0",
                params![id],
            )?;
        }
        // Tentative rows that never got text (the crash came before their
        // recognition) are noise: drop them. Older versions marked them final
        // first, so clean every interrupted meeting, not just the new ones.
        let removed = conn.execute(
            "DELETE FROM meeting_segments
             WHERE text = ''
               AND meeting_id IN (SELECT id FROM meetings WHERE status = 'interrupted')",
            [],
        )?;
        if removed > 0 {
            info!("Removed {removed} empty segment(s) of interrupted meetings");
        }
        Ok(ids.len())
    }

    /// Delete the segments of a meeting that never received text (tentative
    /// rows left behind when finalization was cut short). Returns the count.
    pub fn delete_empty_segments(&self, meeting_id: i64) -> Result<usize> {
        let removed = self.conn()?.execute(
            "DELETE FROM meeting_segments WHERE meeting_id = ?1 AND text = ''",
            params![meeting_id],
        )?;
        Ok(removed)
    }

    /// Insert a new meeting in `recording` state and return its id.
    pub fn create_meeting(
        &self,
        title: &str,
        started_at_ms: i64,
        sources: MeetingSources,
    ) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO meetings (title, started_at, ended_at, duration_ms, mic, system_audio, status, summary, audio_dir)
             VALUES (?1, ?2, NULL, 0, ?3, ?4, 'recording', NULL, '')",
            params![title, started_at_ms, sources.microphone, sources.system_audio],
        )?;
        let id = conn.last_insert_rowid();
        debug!("Created meeting {id} ({title})");
        Ok(id)
    }

    /// Record the directory (relative to the meetings folder) holding the WAV chunks.
    pub fn set_audio_dir(&self, meeting_id: i64, audio_dir: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE meetings SET audio_dir = ?2 WHERE id = ?1",
            params![meeting_id, audio_dir],
        )?;
        Ok(())
    }

    /// Replace the recorded sources (used when a requested source failed to open).
    pub fn set_sources(&self, meeting_id: i64, sources: MeetingSources) -> Result<()> {
        self.conn()?.execute(
            "UPDATE meetings SET mic = ?2, system_audio = ?3 WHERE id = ?1",
            params![meeting_id, sources.microphone, sources.system_audio],
        )?;
        Ok(())
    }

    /// Add a speaker slot to a meeting.
    pub fn add_speaker(
        &self,
        meeting_id: i64,
        slot: i32,
        source: MeetingSource,
        label: &str,
        color_index: i32,
    ) -> Result<MeetingSpeaker> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO meeting_speakers (meeting_id, slot, source, label, name, color_index)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
            params![meeting_id, slot, source.as_db_str(), label, color_index],
        )?;
        Ok(MeetingSpeaker {
            id: conn.last_insert_rowid(),
            meeting_id,
            slot,
            source,
            label: label.to_string(),
            name: None,
            color_index,
        })
    }

    /// Insert a segment (tentative when `is_final == false`).
    pub fn insert_segment(
        &self,
        meeting_id: i64,
        t0_ms: u64,
        t1_ms: u64,
        speaker_id: Option<i64>,
        source: MeetingSource,
        text: &str,
        is_final: bool,
    ) -> Result<MeetingSegment> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO meeting_segments (meeting_id, t0_ms, t1_ms, speaker_id, source, text, is_final)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                meeting_id,
                u64_to_sql(t0_ms),
                u64_to_sql(t1_ms),
                speaker_id,
                source.as_db_str(),
                text,
                is_final
            ],
        )?;
        Ok(MeetingSegment {
            id: conn.last_insert_rowid(),
            meeting_id,
            t0_ms,
            t1_ms,
            speaker_id,
            source,
            text: text.to_string(),
            is_final,
        })
    }

    /// Store the transcript of a segment and mark it final. Returns the updated
    /// row, or `None` when the segment no longer exists (meeting discarded).
    pub fn finalize_segment(
        &self,
        segment_id: i64,
        t1_ms: u64,
        text: &str,
    ) -> Result<Option<MeetingSegment>> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE meeting_segments SET t1_ms = ?2, text = ?3, is_final = 1 WHERE id = ?1",
            params![segment_id, u64_to_sql(t1_ms), text],
        )?;
        let segment = conn
            .query_row(
                &format!("SELECT {SEGMENT_COLUMNS} FROM meeting_segments WHERE id = ?1"),
                params![segment_id],
                Self::map_segment,
            )
            .optional()?;
        Ok(segment)
    }

    /// Move a (tentative) segment to another span and speaker — used when the
    /// diarizer attributes it to a remote speaker or splits it. Returns the
    /// updated row, or `None` when it no longer exists.
    pub fn update_segment_span(
        &self,
        segment_id: i64,
        t0_ms: u64,
        t1_ms: u64,
        speaker_id: Option<i64>,
    ) -> Result<Option<MeetingSegment>> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE meeting_segments SET t0_ms = ?2, t1_ms = ?3, speaker_id = ?4 WHERE id = ?1",
            params![segment_id, u64_to_sql(t0_ms), u64_to_sql(t1_ms), speaker_id],
        )?;
        let segment = conn
            .query_row(
                &format!("SELECT {SEGMENT_COLUMNS} FROM meeting_segments WHERE id = ?1"),
                params![segment_id],
                Self::map_segment,
            )
            .optional()?;
        Ok(segment)
    }

    /// Change the default label of a speaker (the user-given name stays).
    pub fn set_speaker_label(&self, speaker_id: i64, label: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE meeting_speakers SET label = ?2 WHERE id = ?1",
            params![speaker_id, label],
        )?;
        Ok(())
    }

    /// Persist the running duration (called periodically so an interrupted
    /// meeting keeps a plausible length).
    pub fn update_duration(&self, meeting_id: i64, duration_ms: u64) -> Result<()> {
        self.conn()?.execute(
            "UPDATE meetings SET duration_ms = ?2 WHERE id = ?1",
            params![meeting_id, u64_to_sql(duration_ms)],
        )?;
        Ok(())
    }

    /// Close a meeting with the given status.
    pub fn finish_meeting(
        &self,
        meeting_id: i64,
        ended_at_ms: i64,
        duration_ms: u64,
        status: MeetingRecordStatus,
    ) -> Result<()> {
        self.conn()?.execute(
            "UPDATE meetings SET ended_at = ?2, duration_ms = ?3, status = ?4 WHERE id = ?1",
            params![
                meeting_id,
                ended_at_ms,
                u64_to_sql(duration_ms),
                status.as_db_str()
            ],
        )?;
        Ok(())
    }

    /// Mark all tentative segments of a meeting final (used when finalization times out).
    pub fn close_tentative_segments(&self, meeting_id: i64) -> Result<usize> {
        let changed = self.conn()?.execute(
            "UPDATE meeting_segments SET is_final = 1 WHERE meeting_id = ?1 AND is_final = 0",
            params![meeting_id],
        )?;
        Ok(changed)
    }

    /// Summary of one meeting.
    pub fn get_info(&self, meeting_id: i64) -> Result<Option<MeetingInfo>> {
        let conn = self.conn()?;
        let info = conn
            .query_row(
                &format!("SELECT {INFO_COLUMNS} FROM meetings m WHERE m.id = ?1"),
                params![meeting_id],
                Self::map_info,
            )
            .optional()?;
        Ok(info)
    }

    /// All meetings, newest first.
    pub fn list_infos(&self) -> Result<Vec<MeetingInfo>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {INFO_COLUMNS} FROM meetings m ORDER BY m.started_at DESC, m.id DESC"
        ))?;
        let infos = stmt
            .query_map([], Self::map_info)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(infos)
    }

    /// Speakers of a meeting ordered by slot.
    pub fn list_speakers(&self, meeting_id: i64) -> Result<Vec<MeetingSpeaker>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SPEAKER_COLUMNS} FROM meeting_speakers WHERE meeting_id = ?1 ORDER BY slot, id"
        ))?;
        let speakers = stmt
            .query_map(params![meeting_id], Self::map_speaker)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(speakers)
    }

    /// Segments of a meeting in timeline order.
    pub fn list_segments(&self, meeting_id: i64) -> Result<Vec<MeetingSegment>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SEGMENT_COLUMNS} FROM meeting_segments WHERE meeting_id = ?1 ORDER BY t0_ms, id"
        ))?;
        let segments = stmt
            .query_map(params![meeting_id], Self::map_segment)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(segments)
    }

    /// Full detail of a meeting.
    pub fn get_detail(&self, meeting_id: i64) -> Result<Option<MeetingDetail>> {
        let Some(info) = self.get_info(meeting_id)? else {
            return Ok(None);
        };
        Ok(Some(MeetingDetail {
            info,
            speakers: self.list_speakers(meeting_id)?,
            segments: self.list_segments(meeting_id)?,
        }))
    }

    /// Rename a meeting.
    pub fn rename_meeting(&self, meeting_id: i64, title: &str) -> Result<()> {
        let changed = self.conn()?.execute(
            "UPDATE meetings SET title = ?2 WHERE id = ?1",
            params![meeting_id, title],
        )?;
        if changed == 0 {
            return Err(anyhow!("Meeting {meeting_id} not found"));
        }
        Ok(())
    }

    /// Give a speaker a user-defined name (empty string clears it).
    pub fn rename_speaker(&self, meeting_id: i64, speaker_id: i64, name: &str) -> Result<()> {
        let trimmed = name.trim();
        let value: Option<&str> = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        };
        let changed = self.conn()?.execute(
            "UPDATE meeting_speakers SET name = ?3 WHERE id = ?2 AND meeting_id = ?1",
            params![meeting_id, speaker_id, value],
        )?;
        if changed == 0 {
            return Err(anyhow!(
                "Speaker {speaker_id} of meeting {meeting_id} not found"
            ));
        }
        Ok(())
    }

    /// Store (or clear) the LLM summary of a meeting.
    pub fn set_summary(&self, meeting_id: i64, summary: Option<&str>) -> Result<()> {
        let changed = self.conn()?.execute(
            "UPDATE meetings SET summary = ?2 WHERE id = ?1",
            params![meeting_id, summary],
        )?;
        if changed == 0 {
            return Err(anyhow!("Meeting {meeting_id} not found"));
        }
        Ok(())
    }

    /// Remove one segment (used when recognition produced no text).
    pub fn delete_segment(&self, segment_id: i64) -> Result<()> {
        self.conn()?.execute(
            "DELETE FROM meeting_segments WHERE id = ?1",
            params![segment_id],
        )?;
        Ok(())
    }

    /// Remove one speaker slot (used when its source failed to open).
    pub fn delete_speaker(&self, speaker_id: i64) -> Result<()> {
        self.conn()?.execute(
            "DELETE FROM meeting_speakers WHERE id = ?1",
            params![speaker_id],
        )?;
        Ok(())
    }

    /// Relative audio directory recorded for a meeting.
    pub fn audio_dir(&self, meeting_id: i64) -> Result<Option<String>> {
        let conn = self.conn()?;
        let dir = conn
            .query_row(
                "SELECT audio_dir FROM meetings WHERE id = ?1",
                params![meeting_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(dir)
    }

    /// Delete a meeting with its speakers and segments. Returns `true` when a row was removed.
    pub fn delete_meeting(&self, meeting_id: i64) -> Result<bool> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM meeting_segments WHERE meeting_id = ?1",
            params![meeting_id],
        )?;
        conn.execute(
            "DELETE FROM meeting_speakers WHERE meeting_id = ?1",
            params![meeting_id],
        )?;
        let changed = conn.execute("DELETE FROM meetings WHERE id = ?1", params![meeting_id])?;
        Ok(changed > 0)
    }

    fn map_info(row: &Row<'_>) -> rusqlite::Result<MeetingInfo> {
        let status: String = row.get(7)?;
        Ok(MeetingInfo {
            id: row.get(0)?,
            title: row.get(1)?,
            started_at: row.get(2)?,
            ended_at: row.get(3)?,
            duration_ms: ms_to_u64(row.get::<_, i64>(4)?),
            sources: MeetingSources {
                microphone: row.get(5)?,
                system_audio: row.get(6)?,
            },
            status: MeetingRecordStatus::from_db_str(&status),
            summary: row.get(8)?,
            segment_count: row.get::<_, i64>(9)?.max(0) as u32,
            speaker_count: row.get::<_, i64>(10)?.max(0) as u32,
        })
    }

    fn map_speaker(row: &Row<'_>) -> rusqlite::Result<MeetingSpeaker> {
        let source: String = row.get(3)?;
        Ok(MeetingSpeaker {
            id: row.get(0)?,
            meeting_id: row.get(1)?,
            slot: row.get(2)?,
            source: MeetingSource::from_db_str(&source),
            label: row.get(4)?,
            name: row.get(5)?,
            color_index: row.get(6)?,
        })
    }

    fn map_segment(row: &Row<'_>) -> rusqlite::Result<MeetingSegment> {
        let source: String = row.get(5)?;
        Ok(MeetingSegment {
            id: row.get(0)?,
            meeting_id: row.get(1)?,
            t0_ms: ms_to_u64(row.get::<_, i64>(2)?),
            t1_ms: ms_to_u64(row.get::<_, i64>(3)?),
            speaker_id: row.get(4)?,
            source: MeetingSource::from_db_str(&source),
            text: row.get(6)?,
            is_final: row.get(7)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_temp() -> (tempfile::TempDir, MeetingStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = MeetingStore::open(dir.path().join("meetings.db")).expect("open store");
        (dir, store)
    }

    fn both() -> MeetingSources {
        MeetingSources {
            microphone: true,
            system_audio: true,
        }
    }

    #[test]
    fn create_list_and_detail_round_trip() {
        let (_dir, store) = open_temp();
        let id = store
            .create_meeting("Test", 1_700_000_000_000, both())
            .unwrap();
        store.set_audio_dir(id, &id.to_string()).unwrap();
        let speaker = store
            .add_speaker(id, 0, MeetingSource::Microphone, "Я", 0)
            .unwrap();
        let tentative = store
            .insert_segment(
                id,
                1_000,
                1_000,
                Some(speaker.id),
                MeetingSource::Microphone,
                "",
                false,
            )
            .unwrap();
        assert!(!tentative.is_final);
        let final_seg = store
            .finalize_segment(tentative.id, 3_500, "привет")
            .unwrap()
            .expect("segment exists");
        assert!(final_seg.is_final);
        assert_eq!(final_seg.t1_ms, 3_500);
        assert_eq!(final_seg.text, "привет");

        let list = store.list_infos().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].segment_count, 1);
        assert_eq!(list[0].speaker_count, 1);
        assert_eq!(list[0].status, MeetingRecordStatus::Recording);
        assert!(list[0].sources.system_audio);

        store
            .finish_meeting(
                id,
                1_700_000_100_000,
                100_000,
                MeetingRecordStatus::Completed,
            )
            .unwrap();
        let detail = store.get_detail(id).unwrap().expect("detail");
        assert_eq!(detail.info.status, MeetingRecordStatus::Completed);
        assert_eq!(detail.info.duration_ms, 100_000);
        assert_eq!(detail.speakers.len(), 1);
        assert_eq!(detail.segments.len(), 1);
        assert_eq!(detail.segments[0].speaker_id, Some(speaker.id));
    }

    #[test]
    fn newest_meeting_is_listed_first() {
        let (_dir, store) = open_temp();
        store.create_meeting("old", 1_000, both()).unwrap();
        let newest = store.create_meeting("new", 2_000, both()).unwrap();
        let list = store.list_infos().unwrap();
        assert_eq!(list[0].id, newest);
        assert_eq!(list[0].title, "new");
    }

    #[test]
    fn rename_and_delete() {
        let (_dir, store) = open_temp();
        let id = store.create_meeting("a", 1, both()).unwrap();
        let speaker = store
            .add_speaker(id, 0, MeetingSource::System, "Собеседник", 1)
            .unwrap();
        store.rename_meeting(id, "b").unwrap();
        store.rename_speaker(id, speaker.id, "Иван").unwrap();
        let detail = store.get_detail(id).unwrap().unwrap();
        assert_eq!(detail.info.title, "b");
        assert_eq!(detail.speakers[0].name.as_deref(), Some("Иван"));
        store.rename_speaker(id, speaker.id, "   ").unwrap();
        assert!(store.list_speakers(id).unwrap()[0].name.is_none());
        assert!(store.rename_meeting(id + 100, "x").is_err());

        store
            .insert_segment(
                id,
                0,
                10,
                Some(speaker.id),
                MeetingSource::System,
                "t",
                true,
            )
            .unwrap();
        assert!(store.delete_meeting(id).unwrap());
        assert!(store.get_detail(id).unwrap().is_none());
        assert!(store.list_segments(id).unwrap().is_empty());
        assert!(!store.delete_meeting(id).unwrap());
    }

    #[test]
    fn summary_and_segment_removal() {
        let (_dir, store) = open_temp();
        let id = store.create_meeting("s", 1, both()).unwrap();
        store.set_summary(id, Some("итог")).unwrap();
        assert_eq!(
            store.get_info(id).unwrap().unwrap().summary.as_deref(),
            Some("итог")
        );
        let seg = store
            .insert_segment(id, 0, 10, None, MeetingSource::Microphone, "", false)
            .unwrap();
        store.delete_segment(seg.id).unwrap();
        assert!(store.list_segments(id).unwrap().is_empty());
        assert!(store.set_summary(id + 1, None).is_err());
    }

    #[test]
    fn interrupted_meetings_are_recovered_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meetings.db");
        let id = {
            let store = MeetingStore::open(path.clone()).unwrap();
            let id = store.create_meeting("crash", 1, both()).unwrap();
            store
                .insert_segment(id, 0, 4_000, None, MeetingSource::Microphone, "x", true)
                .unwrap();
            store
                .insert_segment(id, 4_000, 4_000, None, MeetingSource::Microphone, "", false)
                .unwrap();
            id
        };
        let store = MeetingStore::open(path).unwrap();
        let info = store.get_info(id).unwrap().unwrap();
        assert_eq!(info.status, MeetingRecordStatus::Interrupted);
        assert!(info.ended_at.is_some());
        assert_eq!(info.duration_ms, 4_000);
        // The empty tentative row is gone; the recognized one stays.
        let segments = store.list_segments(id).unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "x");
        assert!(segments[0].is_final);
        assert_eq!(info.segment_count, 1);
    }

    #[test]
    fn segment_span_and_speaker_label_updates() {
        let (_dir, store) = open_temp();
        let id = store.create_meeting("d", 1, both()).unwrap();
        let first = store
            .add_speaker(id, 1, MeetingSource::System, "Собеседник", 1)
            .unwrap();
        let second = store
            .add_speaker(id, 2, MeetingSource::System, "Собеседник 2", 2)
            .unwrap();
        let seg = store
            .insert_segment(
                id,
                1_000,
                9_000,
                Some(first.id),
                MeetingSource::System,
                "",
                false,
            )
            .unwrap();
        let moved = store
            .update_segment_span(seg.id, 4_000, 9_000, Some(second.id))
            .unwrap()
            .expect("segment exists");
        assert_eq!((moved.t0_ms, moved.t1_ms), (4_000, 9_000));
        assert_eq!(moved.speaker_id, Some(second.id));
        assert!(store
            .update_segment_span(seg.id + 99, 0, 1, None)
            .unwrap()
            .is_none());

        store.rename_speaker(id, first.id, "Анна").unwrap();
        store.set_speaker_label(first.id, "Собеседник 1").unwrap();
        let speakers = store.list_speakers(id).unwrap();
        assert_eq!(speakers[0].label, "Собеседник 1");
        assert_eq!(speakers[0].name.as_deref(), Some("Анна"));
        assert_eq!(speakers[1].slot, 2);
    }

    #[test]
    fn segment_count_ignores_rows_without_text() {
        let (_dir, store) = open_temp();
        let id = store.create_meeting("c", 1, both()).unwrap();
        store
            .insert_segment(id, 0, 10, None, MeetingSource::Microphone, "a", true)
            .unwrap();
        store
            .insert_segment(id, 10, 20, None, MeetingSource::Microphone, "", false)
            .unwrap();
        assert_eq!(store.get_info(id).unwrap().unwrap().segment_count, 1);
        assert_eq!(store.delete_empty_segments(id).unwrap(), 1);
        assert_eq!(store.list_segments(id).unwrap().len(), 1);
    }
}
