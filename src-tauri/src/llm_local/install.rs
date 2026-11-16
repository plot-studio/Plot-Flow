//! Downloading and unpacking the llama.cpp runtime and the GGUF model.

use super::{paths, MODEL, RUNTIME};
use crate::download::{self, DownloadError};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter};

pub(super) static INSTALLING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Serialize)]
struct ProgressPayload {
    stage: &'static str,
    downloaded: u64,
    total: u64,
}

#[derive(Clone, Serialize)]
struct FinishedPayload {
    ok: bool,
    error: Option<String>,
}

/// Resets the `INSTALLING` flag however the install ends.
struct InstallGuard;
impl Drop for InstallGuard {
    fn drop(&mut self) {
        INSTALLING.store(false, Ordering::SeqCst);
    }
}

pub(super) async fn install(app: &AppHandle) -> Result<(), String> {
    if INSTALLING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("Установка нейросети уже идёт".to_string());
    }
    let _guard = InstallGuard;

    let result = install_inner(app).await;
    let _ = app.emit(
        "llm-install-finished",
        FinishedPayload {
            ok: result.is_ok(),
            error: result.as_ref().err().cloned(),
        },
    );
    match &result {
        Ok(()) => log::info!("Local LLM installed"),
        Err(e) => log::error!("Local LLM install failed: {}", e),
    }
    result
}

async fn install_inner(app: &AppHandle) -> Result<(), String> {
    let p = paths(app)?;
    std::fs::create_dir_all(&p.models_dir)
        .map_err(|e| format!("Не удалось создать папку {}: {}", p.models_dir.display(), e))?;
    std::fs::create_dir_all(&p.tmp_dir)
        .map_err(|e| format!("Не удалось создать папку {}: {}", p.tmp_dir.display(), e))?;

    let client = download::client()?;

    // 1. Runtime (llama-server + DLLs).
    if !super::runtime_installed(&p) {
        let zip_path = p.tmp_dir.join(RUNTIME.file_name);
        download_verified(
            app,
            &client,
            "server",
            RUNTIME.url,
            &zip_path,
            RUNTIME.size,
            RUNTIME.sha256,
        )
        .await?;
        let (zip, bin) = (zip_path.clone(), p.bin_dir.clone());
        tauri::async_runtime::spawn_blocking(move || extract_runtime(&zip, &bin))
            .await
            .map_err(|e| format!("Не удалось распаковать нейросеть: {}", e))??;
        let _ = std::fs::remove_file(&zip_path);
    } else {
        emit_progress(app, "server", RUNTIME.size, RUNTIME.size);
    }

    // 2. Model.
    if !super::model_installed(&p) {
        let part = p.tmp_dir.join(MODEL.file_name);
        download_verified(
            app,
            &client,
            "model",
            MODEL.url,
            &part,
            MODEL.size,
            MODEL.sha256,
        )
        .await?;
        move_file(&part, &p.model_path)?;
    } else {
        emit_progress(app, "model", MODEL.size, MODEL.size);
    }
    Ok(())
}

fn emit_progress(app: &AppHandle, stage: &'static str, downloaded: u64, total: u64) {
    let _ = app.emit(
        "llm-install-progress",
        ProgressPayload {
            stage,
            downloaded,
            total,
        },
    );
}

fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    download::move_file(from, to)
}

/// Shared verified download, reporting progress as `llm-install-progress`.
async fn download_verified(
    app: &AppHandle,
    client: &reqwest::Client,
    stage: &'static str,
    url: &str,
    dest: &Path,
    size: u64,
    sha256: &str,
) -> Result<(), String> {
    let progress = |downloaded: u64, total: u64| emit_progress(app, stage, downloaded, total);
    download::download_verified(client, url, dest, size, sha256, &progress)
        .await
        .map_err(|e| match e {
            DownloadError::Transfer(e) => format!("Не удалось скачать нейросеть: {}", e),
            DownloadError::Corrupt => {
                "Скачанный файл повреждён (не совпала контрольная сумма). Попробуйте ещё раз."
                    .to_string()
            }
            DownloadError::Io(e) => e,
        })
}

/// Files from the release zip that `llama-server.exe` needs. Other tools
/// (llama-cli, quantize, …) are skipped to save disk space.
pub(super) fn wanted_runtime_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == "llama-server.exe" || lower.starts_with("license") {
        return true;
    }
    // `llama-cli-impl.dll`, `llama-bench-impl.dll`, … belong to other tools.
    lower.ends_with(".dll") && (!lower.ends_with("-impl.dll") || lower == "llama-server-impl.dll")
}

/// Extracts the runtime into `bin.new` and swaps it in for `bin`, writing a
/// version marker last so a half-extracted runtime is never "installed".
fn extract_runtime(zip_path: &Path, bin_dir: &Path) -> Result<(), String> {
    let err = |e: String| format!("Не удалось распаковать нейросеть: {}", e);
    let staging: PathBuf = bin_dir.with_extension("new");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| err(e.to_string()))?;

    let file = std::fs::File::open(zip_path).map_err(|e| err(e.to_string()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| err(e.to_string()))?;
    let mut found_server = false;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| err(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        // The release zip is flat; take the bare file name either way.
        let Some(name) = rel.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        if !wanted_runtime_file(&name) {
            continue;
        }
        found_server |= name.eq_ignore_ascii_case("llama-server.exe");
        let mut out = std::fs::File::create(staging.join(&name)).map_err(|e| err(e.to_string()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| err(e.to_string()))?;
    }
    if !found_server {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err("в архиве нет llama-server.exe".to_string()));
    }

    // Make sure no old server holds the DLLs before swapping.
    super::server::stop();
    if bin_dir.exists() {
        std::fs::remove_dir_all(bin_dir).map_err(|e| err(e.to_string()))?;
    }
    std::fs::rename(&staging, bin_dir).map_err(|e| err(e.to_string()))?;
    std::fs::write(bin_dir.join(super::RUNTIME_MARKER), RUNTIME.tag)
        .map_err(|e| err(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_filter_keeps_server_and_dlls_only() {
        assert!(wanted_runtime_file("llama-server.exe"));
        assert!(wanted_runtime_file("ggml-vulkan.dll"));
        assert!(wanted_runtime_file("LICENSE-LLVM-OpenMP"));
        assert!(!wanted_runtime_file("llama-cli.exe"));
        assert!(!wanted_runtime_file("llama-quantize.exe"));
        assert!(wanted_runtime_file("llama-server-impl.dll"));
        assert!(wanted_runtime_file("llama-common.dll"));
        assert!(!wanted_runtime_file("llama-cli-impl.dll"));
    }
}
