//! Free, fully local LLM that writes short meeting summaries.
//!
//! Runtime: the official llama.cpp Windows x64 Vulkan build (`llama-server`,
//! OpenAI-compatible `/v1/chat/completions`). Model: Qwen3.5-4B Q4_K_M GGUF
//! (Apache-2.0). Both are downloaded on demand into `<app data>\llm\`.

mod install;
pub mod prompt;
mod server;

use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::AppHandle;

/// A pinned download.
pub(crate) struct Asset {
    pub url: &'static str,
    pub file_name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    /// Release tag (runtime only); written as the "installed" marker.
    pub tag: &'static str,
}

/// llama.cpp release b12428 (2026-11-16), Windows x64 Vulkan build.
pub(crate) const RUNTIME: Asset = Asset {
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12428/llama-b12428-bin-win-vulkan-x64.zip",
    file_name: "llama-b12428-bin-win-vulkan-x64.zip",
    size: 38_289_478,
    sha256: "598b0672b20784752a6fbea3b73523f0905f094326b13b0d734ee47e94e6b986",
    tag: "b12428",
};

/// unsloth/Qwen3.5-4B-GGUF @ e87f176479d0855a907a41277aca2f8ee7a09523.
pub(crate) const MODEL: Asset = Asset {
    url: "https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/resolve/e87f176479d0855a907a41277aca2f8ee7a09523/Qwen3.5-4B-Q4_K_M.gguf",
    file_name: "Qwen3.5-4B-Q4_K_M.gguf",
    size: 2_740_937_888,
    sha256: "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4",
    tag: "",
};

pub(crate) const MODEL_DISPLAY_NAME: &str = "Qwen3.5-4B (Q4_K_M)";
pub(crate) const SERVER_EXE: &str = "llama-server.exe";
pub(crate) const RUNTIME_MARKER: &str = "plot-flow-runtime.txt";

/// Per request: generous, a long chunk on CPU can take many minutes.
const REQUEST_TIMEOUT_GPU: Duration = Duration::from_secs(10 * 60);
const REQUEST_TIMEOUT_CPU: Duration = Duration::from_secs(40 * 60);

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct LlmStatus {
    pub installed: bool,
    pub server_running: bool,
    pub model_name: String,
    pub model_size_mb: u64,
    pub installing: bool,
}

pub(crate) struct LlmPaths {
    pub bin_dir: PathBuf,
    pub models_dir: PathBuf,
    pub tmp_dir: PathBuf,
    pub model_path: PathBuf,
    pub server_exe: PathBuf,
    pub log_path: PathBuf,
    pub pid_path: PathBuf,
}

impl LlmPaths {
    fn new(root: PathBuf) -> Self {
        let bin_dir = root.join("bin");
        let models_dir = root.join("models");
        Self {
            server_exe: bin_dir.join(SERVER_EXE),
            model_path: models_dir.join(MODEL.file_name),
            tmp_dir: root.join("tmp"),
            log_path: root.join("server.log"),
            pid_path: root.join("server.pid"),
            bin_dir,
            models_dir,
        }
    }
}

/// `<app data>\llm\…`, portable-mode aware (same resolver as history.db).
pub(crate) fn paths(app: &AppHandle) -> Result<LlmPaths, String> {
    let data = crate::portable::app_data_dir(app)
        .map_err(|e| format!("Не найдена папка данных приложения: {}", e))?;
    Ok(LlmPaths::new(data.join("llm")))
}

pub(crate) fn runtime_installed(p: &LlmPaths) -> bool {
    p.server_exe.is_file()
        && std::fs::read_to_string(p.bin_dir.join(RUNTIME_MARKER))
            .map(|t| t.trim() == RUNTIME.tag)
            .unwrap_or(false)
}

/// Size check only; the full SHA-256 is verified once, at download time.
pub(crate) fn model_installed(p: &LlmPaths) -> bool {
    std::fs::metadata(&p.model_path)
        .map(|m| m.len() == MODEL.size)
        .unwrap_or(false)
}

fn status_for(p: Option<&LlmPaths>) -> LlmStatus {
    LlmStatus {
        installed: p
            .map(|p| runtime_installed(p) && model_installed(p))
            .unwrap_or(false),
        server_running: server::is_running(),
        model_name: MODEL_DISPLAY_NAME.to_string(),
        model_size_mb: MODEL.size / (1024 * 1024),
        installing: install::INSTALLING.load(Ordering::SeqCst),
    }
}

#[tauri::command]
#[specta::specta]
pub fn llm_status(app: AppHandle) -> Result<LlmStatus, String> {
    let p = paths(&app)?;
    Ok(status_for(Some(&p)))
}

#[tauri::command]
#[specta::specta]
pub async fn llm_install(app: AppHandle) -> Result<(), String> {
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return Err("Локальная нейросеть пока доступна только для Windows x64".to_string());
    }
    install::install(&app).await
}

/// Stops the server; call on app exit to free VRAM right away.
pub fn shutdown() {
    server::stop();
}

/// Kills a `llama-server` left running by a previous run that crashed (it
/// would keep holding VRAM). Call once at app startup; cheap when there is
/// no stale pid file.
pub fn kill_stale_at_startup(app: &AppHandle) {
    if let Ok(p) = paths(app) {
        if p.pid_path.exists() {
            std::thread::spawn(move || server::kill_stale(&p));
        }
    }
}

/// Summarizes a meeting transcript ("Я: … / Собеседник: …") into short
/// Russian markdown. Starts the local server on demand; long transcripts are
/// summarized in parts and then merged.
pub async fn summarize(app: &tauri::AppHandle, transcript: &str) -> Result<String, String> {
    let text = prompt::normalize_transcript(transcript);
    if text.trim().is_empty() {
        return Err("Расшифровка пустая — нечего пересказывать".to_string());
    }
    let p = paths(app)?;
    if !(runtime_installed(&p) && model_installed(&p)) {
        return Err("Модель не установлена".to_string());
    }
    if install::INSTALLING.load(Ordering::SeqCst) {
        return Err("Нейросеть ещё устанавливается, попробуйте позже".to_string());
    }

    let _run = server::RUN_LOCK.lock().await;
    server::touch();
    let ep = server::ensure_running(&p).await?;
    let result = summarize_text(ep, &text).await;
    server::touch();
    if result.is_err() && !server::is_running() {
        log::warn!("Local LLM server died during a request");
    }
    result
}

async fn summarize_text(ep: server::Endpoint, text: &str) -> Result<String, String> {
    let max = prompt::max_chunk_chars();
    let chunks = prompt::split_transcript(text, max);
    if chunks.len() <= 1 {
        let msg = prompt::transcript_message(text, None);
        return chat(ep, prompt::SYSTEM_PROMPT, &msg).await;
    }

    let n = chunks.len();
    log::info!("Local LLM: transcript split into {} parts", n);
    let mut partials = Vec::with_capacity(n);
    for (i, chunk) in chunks.iter().enumerate() {
        let msg = prompt::transcript_message(chunk, Some((i + 1, n)));
        partials.push(chat(ep, prompt::SYSTEM_PROMPT, &msg).await?);
        server::touch();
    }

    // Merge; if the partials themselves overflow the context, merge in groups.
    loop {
        let groups = prompt::group_for_merge(&partials, max);
        if groups.len() <= 1 {
            let msg = prompt::merge_message(&partials);
            return chat(ep, prompt::MERGE_PROMPT, &msg).await;
        }
        let mut next = Vec::with_capacity(groups.len());
        for g in groups {
            next.push(chat(ep, prompt::MERGE_PROMPT, &prompt::merge_message(&g)).await?);
            server::touch();
        }
        partials = next;
    }
}

async fn chat(ep: server::Endpoint, system: &str, user: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(10))
        .timeout(if ep.gpu {
            REQUEST_TIMEOUT_GPU
        } else {
            REQUEST_TIMEOUT_CPU
        })
        .build()
        .map_err(|e| format!("Ошибка нейросети: {}", e))?;

    let max_tokens = if ep.gpu {
        prompt::MAX_ANSWER_TOKENS
    } else {
        prompt::ANSWER_TOKENS
    };
    let body = serde_json::json!({
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "temperature": 0.2,
        "top_p": 0.8,
        "max_tokens": max_tokens,
        "chat_template_kwargs": { "enable_thinking": ep.gpu },
        "stream": false,
    });

    let resp = client
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", ep.port))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "Нейросеть не успела ответить (превышено время ожидания)".to_string()
            } else {
                format!("Нейросеть не отвечает: {}", e)
            }
        })?;
    let status = resp.status();
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Непонятный ответ нейросети: {}", e))?;
    if !status.is_success() {
        let msg = json["error"]["message"]
            .as_str()
            .unwrap_or("неизвестная ошибка");
        return Err(format!("Ошибка нейросети ({}): {}", status.as_u16(), msg));
    }
    let content = json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    let answer = prompt::clean_answer(content);
    if answer.is_empty() {
        return Err("Нейросеть вернула пустой ответ".to_string());
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_layout() {
        let p = LlmPaths::new(PathBuf::from("data").join("llm"));
        assert!(
            p.server_exe.ends_with("llm/bin/llama-server.exe")
                || p.server_exe.ends_with("llm\\bin\\llama-server.exe")
        );
        assert_eq!(p.model_path.file_name().unwrap(), MODEL.file_name);
        assert_eq!(p.model_path.parent().unwrap(), p.models_dir);
    }

    #[test]
    fn status_reflects_files_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = LlmPaths::new(dir.path().join("llm"));
        let s = status_for(Some(&p));
        assert!(!s.installed);
        assert_eq!(s.model_name, MODEL_DISPLAY_NAME);
        assert_eq!(s.model_size_mb, 2613);

        // Runtime without the version marker is not installed.
        std::fs::create_dir_all(&p.bin_dir).unwrap();
        std::fs::write(&p.server_exe, b"x").unwrap();
        assert!(!runtime_installed(&p));
        std::fs::write(p.bin_dir.join(RUNTIME_MARKER), RUNTIME.tag).unwrap();
        assert!(runtime_installed(&p));

        // A truncated model is not installed.
        std::fs::create_dir_all(&p.models_dir).unwrap();
        std::fs::write(&p.model_path, b"partial").unwrap();
        assert!(!model_installed(&p));
        assert!(!status_for(Some(&p)).installed);
    }

    #[test]
    fn pinned_hashes_look_valid() {
        for a in [&RUNTIME, &MODEL] {
            assert_eq!(a.sha256.len(), 64);
            assert!(a
                .sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            assert!(a.url.starts_with("https://") && a.url.ends_with(a.file_name));
        }
    }
}
