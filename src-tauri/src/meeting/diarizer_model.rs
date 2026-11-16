//! The speaker-diarization model of meeting mode: where it lives, whether it
//! is installed, and its on-demand download (`<app data>\models\diarization\`).
//!
//! NVIDIA Streaming Sortformer 4spk v2.1, Q8_0 GGUF converted for
//! transcribe.cpp by handy-computer (NVIDIA Open Model License).

use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter};

use crate::download::{self, DownloadError};

/// handy-computer/diar_streaming_sortformer_4spk-v2.1-gguf @ 60ebab3ef7cae9869be0abcde13360347e96ba03.
pub const MODEL_URL: &str = "https://huggingface.co/handy-computer/diar_streaming_sortformer_4spk-v2.1-gguf/resolve/60ebab3ef7cae9869be0abcde13360347e96ba03/diar_streaming_sortformer_4spk-v2.1-Q8_0.gguf";
pub const MODEL_FILE: &str = "diar_streaming_sortformer_4spk-v2.1-Q8_0.gguf";
pub const MODEL_SIZE: u64 = 139_310_336;
pub const MODEL_SHA256: &str = "a5dacdc650790266c7a362e54e6bf51952015487edaa606c4e11632bc32442a9";

static INSTALLING: AtomicBool = AtomicBool::new(false);

/// Answer of `meeting_diarizer_status`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Type)]
pub struct MeetingDiarizerStatus {
    pub installed: bool,
    pub installing: bool,
    /// Download size in MB (decimal, as the UI shows it).
    pub size_mb: u64,
}

#[derive(Clone, Serialize)]
struct ProgressPayload {
    downloaded: u64,
    total: u64,
}

#[derive(Clone, Serialize)]
struct FinishedPayload {
    ok: bool,
    error: Option<String>,
}

struct InstallGuard;
impl Drop for InstallGuard {
    fn drop(&mut self) {
        INSTALLING.store(false, Ordering::SeqCst);
    }
}

/// `<app data>\models\diarization` (portable-aware).
pub fn model_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let data = crate::portable::app_data_dir(app)
        .map_err(|e| format!("Не найдена папка данных приложения: {}", e))?;
    Ok(data.join("models").join("diarization"))
}

pub fn model_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(model_dir(app)?.join(MODEL_FILE))
}

/// Size check only; SHA-256 is verified once, at download time.
pub fn is_installed_at(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() == MODEL_SIZE)
        .unwrap_or(false)
}

/// Path of the installed model, or `None`.
pub fn installed_model(app: &AppHandle) -> Option<PathBuf> {
    model_path(app).ok().filter(|p| is_installed_at(p))
}

pub fn status(app: &AppHandle) -> MeetingDiarizerStatus {
    MeetingDiarizerStatus {
        installed: installed_model(app).is_some(),
        installing: INSTALLING.load(Ordering::SeqCst),
        size_mb: MODEL_SIZE.div_ceil(1_000_000),
    }
}

/// Download and verify the model. Emits `meeting-diarizer-progress`
/// `{downloaded, total}` and, at the end, `meeting-diarizer-finished`
/// `{ok, error}`.
pub async fn install(app: &AppHandle) -> Result<(), String> {
    if INSTALLING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("Модель уже скачивается".to_string());
    }
    let _guard = InstallGuard;
    let result = install_inner(app).await;
    let _ = app.emit(
        "meeting-diarizer-finished",
        FinishedPayload {
            ok: result.is_ok(),
            error: result.as_ref().err().cloned(),
        },
    );
    match &result {
        Ok(()) => log::info!("Diarization model installed"),
        Err(e) => log::error!("Diarization model install failed: {}", e),
    }
    result
}

async fn install_inner(app: &AppHandle) -> Result<(), String> {
    let path = model_path(app)?;
    if is_installed_at(&path) {
        return Ok(());
    }
    let dir = model_dir(app)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Не удалось создать папку {}: {}", dir.display(), e))?;
    let part = dir.join(format!("{MODEL_FILE}.part"));
    let client = download::client()?;
    let progress = |downloaded: u64, total: u64| {
        let _ = app.emit(
            "meeting-diarizer-progress",
            ProgressPayload { downloaded, total },
        );
    };
    download::download_verified(
        &client,
        MODEL_URL,
        &part,
        MODEL_SIZE,
        MODEL_SHA256,
        &progress,
    )
    .await
    .map_err(|e| match e {
        DownloadError::Transfer(e) => format!("Не удалось скачать модель: {}", e),
        DownloadError::Corrupt => {
            "Скачанный файл повреждён (не совпала контрольная сумма). Попробуйте ещё раз."
                .to_string()
        }
        DownloadError::Io(e) => e,
    })?;
    download::move_file(&part, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_model_looks_valid() {
        assert_eq!(MODEL_SHA256.len(), 64);
        assert!(MODEL_SHA256
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert!(MODEL_URL.starts_with("https://") && MODEL_URL.ends_with(MODEL_FILE));
        assert_eq!(MODEL_SIZE.div_ceil(1_000_000), 140);
    }

    #[test]
    fn installed_means_full_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MODEL_FILE);
        assert!(!is_installed_at(&path));
        std::fs::write(&path, b"partial").unwrap();
        assert!(!is_installed_at(&path));
    }
}
