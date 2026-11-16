//! Resumable, SHA-256-verified download of one pinned file.
//!
//! Shared by the on-demand installs (the local LLM in `llm_local`, the
//! speaker-diarization model of meeting mode). The caller passes a progress
//! callback and decides what event to emit.

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

/// No bytes for this long → the download is considered stalled.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const MAX_ATTEMPTS: usize = 4;

/// Progress callback: `(downloaded, total)` bytes.
pub type Progress<'a> = &'a (dyn Fn(u64, u64) + Send + Sync);

/// Why a verified download failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// Every attempt failed (network, disk, server answer); last reason.
    Transfer(String),
    /// The file arrived but its SHA-256 did not match (it was deleted).
    Corrupt,
    /// Could not read the file back for hashing.
    Io(String),
}

/// HTTP client used for pinned downloads.
pub fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .user_agent("plot-flow")
        .build()
        .map_err(|e| format!("Ошибка сети: {}", e))
}

/// Downloads `url` to `dest` (a temp path), resuming a partial file, then
/// checks size and SHA-256. A corrupt file is deleted so the next attempt
/// starts clean.
pub async fn download_verified(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    size: u64,
    sha256: &str,
    progress: Progress<'_>,
) -> Result<(), DownloadError> {
    let mut last_err = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        match download_resumable(client, url, dest, size, progress).await {
            Ok(()) => {
                last_err.clear();
                break;
            }
            Err(e) => {
                log::warn!("Download {} attempt {} failed: {}", url, attempt, e);
                last_err = e;
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
                }
            }
        }
    }
    if !last_err.is_empty() {
        return Err(DownloadError::Transfer(last_err));
    }

    let path = dest.to_path_buf();
    let expected = sha256.to_ascii_lowercase();
    let actual = tauri::async_runtime::spawn_blocking(move || sha256_file(&path))
        .await
        .map_err(|e| DownloadError::Io(e.to_string()))?
        .map_err(DownloadError::Io)?;
    if actual != expected {
        let _ = std::fs::remove_file(dest);
        return Err(DownloadError::Corrupt);
    }
    Ok(())
}

async fn download_resumable(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    size: u64,
    progress: Progress<'_>,
) -> Result<(), String> {
    let mut have = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    if have > size {
        let _ = std::fs::remove_file(dest);
        have = 0;
    }
    if have == size {
        progress(size, size);
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
    progress(downloaded, size);
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
            progress(downloaded, size);
            last_emit = Instant::now();
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    progress(downloaded, size);
    if downloaded != size {
        return Err(format!("загружено {} из {} байт", downloaded, size));
    }
    Ok(())
}

/// Replace `to` with `from` (same volume: a rename).
pub fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(to);
    std::fs::rename(from, to).map_err(|e| format!("Не удалось сохранить {}: {}", to.display(), e))
}

/// Lower-case hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> Result<String, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
