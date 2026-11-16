//! Tauri commands of meeting mode. Blocking work (opening devices, waiting
//! for the transcription queue) runs on Tauri's blocking pool so the UI
//! thread never stalls.

use std::sync::Arc;

use tauri::{AppHandle, LogicalSize, Manager};

use super::export;
use super::types::{
    MeetingDetail, MeetingExportFormat, MeetingInfo, MeetingSources, MeetingStatus,
};
use super::MeetingManager;

/// Main window width with the transcript panel closed / open (logical px).
const WINDOW_WIDTH_CLOSED: f64 = 960.0;
const WINDOW_WIDTH_OPEN: f64 = 1380.0;
const WINDOW_HEIGHT: f64 = 600.0;

fn manager(app: &AppHandle) -> Result<Arc<MeetingManager>, String> {
    app.try_state::<Arc<MeetingManager>>()
        .map(|state| Arc::clone(state.inner()))
        .ok_or_else(|| "Режим совещания недоступен".to_string())
}

async fn blocking<R, F>(f: F) -> Result<R, String>
where
    F: FnOnce() -> Result<R, String> + Send + 'static,
    R: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

fn detail(manager: &MeetingManager, meeting_id: i64) -> Result<MeetingDetail, String> {
    manager
        .store()
        .get_detail(meeting_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Совещание не найдено".to_string())
}

/// Start a meeting with the chosen sources.
#[tauri::command]
#[specta::specta]
pub async fn meeting_start(app: AppHandle, sources: MeetingSources) -> Result<MeetingInfo, String> {
    let manager = manager(&app)?;
    blocking(move || manager.start(sources)).await
}

/// Stop the meeting; resolves after pending segments are transcribed.
#[tauri::command]
#[specta::specta]
pub async fn meeting_stop(app: AppHandle) -> Result<MeetingInfo, String> {
    let manager = manager(&app)?;
    blocking(move || manager.stop()).await
}

/// Pause recording.
#[tauri::command]
#[specta::specta]
pub fn meeting_pause(app: AppHandle) -> Result<(), String> {
    manager(&app)?.pause()
}

/// Resume recording.
#[tauri::command]
#[specta::specta]
pub fn meeting_resume(app: AppHandle) -> Result<(), String> {
    manager(&app)?.resume()
}

/// Stop and delete the running meeting.
#[tauri::command]
#[specta::specta]
pub async fn meeting_discard(app: AppHandle) -> Result<(), String> {
    let manager = manager(&app)?;
    blocking(move || manager.discard()).await
}

/// Current recorder status.
#[tauri::command]
#[specta::specta]
pub fn meeting_status(app: AppHandle) -> Result<MeetingStatus, String> {
    Ok(manager(&app)?.status())
}

/// All meetings, newest first.
#[tauri::command]
#[specta::specta]
pub fn meeting_list(app: AppHandle) -> Result<Vec<MeetingInfo>, String> {
    manager(&app)?.store().list_infos().map_err(|e| e.to_string())
}

/// One meeting with speakers and segments.
#[tauri::command]
#[specta::specta]
pub fn meeting_get(app: AppHandle, meeting_id: i64) -> Result<MeetingDetail, String> {
    let manager = manager(&app)?;
    detail(&manager, meeting_id)
}

/// Rename a meeting.
#[tauri::command]
#[specta::specta]
pub fn meeting_rename(app: AppHandle, meeting_id: i64, title: String) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("Название не может быть пустым".to_string());
    }
    manager(&app)?
        .store()
        .rename_meeting(meeting_id, title)
        .map_err(|e| e.to_string())
}

/// Give a speaker a name (empty clears it back to the default label).
#[tauri::command]
#[specta::specta]
pub fn meeting_rename_speaker(
    app: AppHandle,
    meeting_id: i64,
    speaker_id: i64,
    name: String,
) -> Result<(), String> {
    manager(&app)?
        .store()
        .rename_speaker(meeting_id, speaker_id, &name)
        .map_err(|e| e.to_string())
}

/// Delete a meeting (not the running one).
#[tauri::command]
#[specta::specta]
pub fn meeting_delete(app: AppHandle, meeting_id: i64) -> Result<(), String> {
    manager(&app)?.delete(meeting_id)
}

/// Write the transcript to `<data dir>/meetings/exports/` and return the path.
#[tauri::command]
#[specta::specta]
pub fn meeting_export(
    app: AppHandle,
    meeting_id: i64,
    format: MeetingExportFormat,
) -> Result<String, String> {
    let manager = manager(&app)?;
    let detail = detail(&manager, meeting_id)?;
    let started = chrono::DateTime::from_timestamp_millis(detail.info.started_at)
        .map(|utc| utc.with_timezone(&chrono::Local));
    let date = started
        .map(|local| local.format("%d.%m.%Y %H:%M").to_string())
        .unwrap_or_default();
    let date_slug = started
        .map(|local| local.format("%Y-%m-%d-%H%M").to_string())
        .unwrap_or_else(|| "export".to_string());

    let content = export::render(&detail, format, &date)?;
    let dir = manager.meetings_dir().join("exports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Не удалось создать папку: {e}"))?;
    let path = dir.join(export::export_file_name(meeting_id, &date_slug, format));
    std::fs::write(&path, content).map_err(|e| format!("Не удалось сохранить файл: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Widen the main window to show the transcript panel on the right, or
/// shrink it back. The window is otherwise fixed-size (see `lib.rs`).
#[tauri::command]
#[specta::specta]
pub fn meeting_set_panel(app: AppHandle, open: bool) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "Главное окно не найдено".to_string())?;
    let width = if open {
        WINDOW_WIDTH_OPEN
    } else {
        WINDOW_WIDTH_CLOSED
    };
    let size = LogicalSize::new(width, WINDOW_HEIGHT);
    let map = |e: tauri::Error| e.to_string();
    if open {
        // Growing: raise the maximum first so the new size is not clamped.
        window.set_max_size(Some(size)).map_err(map)?;
        window.set_min_size(Some(size)).map_err(map)?;
    } else {
        // Shrinking: lower the minimum first.
        window.set_min_size(Some(size)).map_err(map)?;
        window.set_max_size(Some(size)).map_err(map)?;
    }
    window.set_size(size).map_err(map)?;
    Ok(())
}

/// Ask the local LLM for a short summary, store it and return it.
#[tauri::command]
#[specta::specta]
pub async fn meeting_summarize(app: AppHandle, meeting_id: i64) -> Result<String, String> {
    let manager = manager(&app)?;
    let detail = detail(&manager, meeting_id)?;
    let transcript = export::transcript_for_summary(&detail);
    if transcript.trim().is_empty() {
        return Err("В совещании нет текста для пересказа".to_string());
    }
    let summary = crate::llm_local::summarize(&app, &transcript).await?;
    let summary = summary.trim().to_string();
    manager
        .store()
        .set_summary(meeting_id, Some(&summary))
        .map_err(|e| e.to_string())?;
    Ok(summary)
}
