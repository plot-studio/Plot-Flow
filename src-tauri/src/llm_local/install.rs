//! Downloading and unpacking the llama.cpp runtime and the GGUF model.

use super::{paths, MODEL, RUNTIME};
use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;

pub(super) static INSTALLING: AtomicBool = AtomicBool::new(false);

/// No bytes for this long → the download is considered stalled.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const MAX_ATTEMPTS: usize = 4;

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

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .user_agent("plot-flow")
        .build()
        .map_err(|e| format!("Ошибка сети: {}", e))?;

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
    let _ = std::fs::remove_file(to);
    std::fs::rename(from, to).map_err(|e| format!("Не удалось сохранить {}: {}", to.display(), e))
}

/// Downloads `url` to `dest` (a temp path), resuming a partial file, then
/// checks size and SHA-256. A corrupt file is deleted so the next attempt
/// starts clean.
async fn download_verified(
    app: &AppHandle,
    client: &reqwest::Client,
    stage: &'static str,
    url: &str,
    dest: &Path,
    size: u64,
    sha256: &str,
) -> Result<(), String> {
    let mut last_err = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        match download_resumable(app, client, stage, url, dest, size).await {
            Ok(()) => {
                last_err.clear();
                break;
            }
            Err(e) => {
                log::warn!("LLM download {} attempt {} failed: {}", stage, attempt, e);
                last_err = e;
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
                }
            }
        }
    }
    if !last_err.is_empty() {
        return Err(format!("Не удалось скачать нейросеть: {}", last_err));
    }

    let path = dest.to_path_buf();
    let expected = sha256.to_ascii_lowercase();
    let actual = tauri::async_runtime::spawn_blocking(move || sha256_file(&path))
        .await
        .map_err(|e| e.to_string())??;
    if actual != expected {
        let _ = std::fs::remove_file(dest);
        return Err(
            "Скачанный файл повреждён (не совпала контрольная сумма). Попробуйте ещё раз."
                .to_string(),
        );
    }
    Ok(())
}

async fn download_resumable(
    app: &AppHandle,
    client: &reqwest::Client,
    stage: &'static str,
    url: &str,
    dest: &Path,
    size: u64,
) -> Result<(), String> {
    let mut have = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    if have > size {
        let _ = std::fs::remove_file(dest);
        have = 0;
    }
    if have == size {
        emit_progress(app, stage, size, size);
        return Ok(());
    }

    let mut req = client.get(url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={}-", have));
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let append = match status.as_u16() {
        206 if have > 0 => true,
        200 => false,
        416 => {
            // Range not satisfiable: the partial file is unusable.
            let _ = std::fs::remove_file(dest);
            return Err("сервер отклонил докачку, начинаю заново".to_string());
        }
        _ => return Err(format!("ответ сервера {}", status)),
    };
    if !append {
        have = 0;
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(dest)
        .await
        .map_err(|e| format!("не удалось открыть {}: {}", dest.display(), e))?;

    let mut stream = resp.bytes_stream();
    let mut downloaded = have;
    let mut last_emit = Instant::now()
        .checked_sub(PROGRESS_INTERVAL)
        .unwrap_or_else(Instant::now);
    emit_progress(app, stage, downloaded, size);
    loop {
        let next = tokio::time::timeout(STALL_TIMEOUT, stream.next())
            .await
            .map_err(|_| "загрузка зависла (нет данных 60 с)".to_string())?;
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|e| e.to_string())?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("ошибка записи на диск: {}", e))?;
        downloaded += chunk.len() as u64;
        if downloaded > size {
            drop(file);
            let _ = std::fs::remove_file(dest);
            return Err("файл больше ожидаемого".to_string());
        }
        if last_emit.elapsed() >= PROGRESS_INTERVAL {
            emit_progress(app, stage, downloaded, size);
            last_emit = Instant::now();
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    emit_progress(app, stage, downloaded, size);
    if downloaded != size {
        return Err(format!("загружено {} из {} байт", downloaded, size));
    }
    Ok(())
}

pub(super) fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
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

    #[test]
    fn sha256_of_known_content() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, b"abc").unwrap();
        assert_eq!(
            sha256_file(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
