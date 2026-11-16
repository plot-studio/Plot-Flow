//! Lifecycle of the hidden `llama-server` child process.
//!
//! The server is started lazily by `summarize`, bound to 127.0.0.1 on a free
//! port, and stopped after `IDLE_TIMEOUT` without requests or on app exit so
//! the VRAM goes back to speech recognition.

use super::{prompt, LlmPaths};
use once_cell::sync::Lazy;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Stop the server after this long without requests.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Model load on a cold disk can take a while; CPU load is slower still.
const START_TIMEOUT: Duration = Duration::from_secs(180);
const REAPER_TICK: Duration = Duration::from_secs(30);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

struct Server {
    child: Child,
    port: u16,
    gpu: bool,
    pid_path: std::path::PathBuf,
}

static SERVER: Lazy<Mutex<Option<Server>>> = Lazy::new(|| Mutex::new(None));
static LAST_USED: Lazy<Mutex<Instant>> = Lazy::new(|| Mutex::new(Instant::now()));
static REAPER_STARTED: AtomicBool = AtomicBool::new(false);
/// Held for the whole duration of a summary: one request at a time, and the
/// idle reaper never kills a server that is in use.
pub(super) static RUN_LOCK: Lazy<tauri::async_runtime::Mutex<()>> =
    Lazy::new(|| tauri::async_runtime::Mutex::new(()));

/// Where the running server listens and whether it uses the GPU.
#[derive(Clone, Copy, Debug)]
pub(super) struct Endpoint {
    pub port: u16,
    pub gpu: bool,
}

pub(super) fn touch() {
    if let Ok(mut t) = LAST_USED.lock() {
        *t = Instant::now();
    }
}

fn idle_for() -> Duration {
    LAST_USED
        .lock()
        .map(|t| t.elapsed())
        .unwrap_or(Duration::ZERO)
}

/// `true` while a live server process exists.
pub fn is_running() -> bool {
    let Ok(mut guard) = SERVER.lock() else {
        return false;
    };
    let alive = match guard.as_mut() {
        Some(s) => matches!(s.child.try_wait(), Ok(None)),
        None => false,
    };
    if !alive {
        if let Some(s) = guard.take() {
            let _ = std::fs::remove_file(&s.pid_path);
        }
    }
    alive
}

fn current_endpoint() -> Option<Endpoint> {
    if !is_running() {
        return None;
    }
    SERVER.lock().ok().and_then(|g| {
        g.as_ref().map(|s| Endpoint {
            port: s.port,
            gpu: s.gpu,
        })
    })
}

/// Kills the server (if any). Safe to call from any thread, including the
/// app's exit handler.
pub fn stop() {
    let server = SERVER.lock().ok().and_then(|mut g| g.take());
    if let Some(mut s) = server {
        log::info!("Stopping local LLM server (pid {})", s.child.id());
        let _ = s.child.kill();
        let _ = s.child.wait();
        let _ = std::fs::remove_file(&s.pid_path);
    }
}

/// Kills a server left over from a previous run that crashed (its pid is in
/// `server.pid`). The image-name filter keeps us from killing an unrelated
/// process that reused the pid. Called at app startup and before a start.
pub(super) fn kill_stale(p: &LlmPaths) {
    if is_running() {
        // Our own live server: its pid file is current, not stale.
        return;
    }
    let Ok(text) = std::fs::read_to_string(&p.pid_path) else {
        return;
    };
    let _ = std::fs::remove_file(&p.pid_path);
    let Ok(pid) = text.trim().parse::<u32>() else {
        return;
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let pid_filter = format!("PID eq {}", pid);
        let name_filter = format!("IMAGENAME eq {}", super::SERVER_EXE);
        let _ = Command::new("taskkill")
            .args([
                "/F",
                "/FI",
                pid_filter.as_str(),
                "/FI",
                name_filter.as_str(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
    }
}

fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("нет свободного порта: {}", e))?;
    listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| e.to_string())
}

/// Command-line arguments for `llama-server`.
pub(super) fn server_args(model: &str, port: u16, gpu: bool) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-m".into(),
        model.into(),
        "-c".into(),
        prompt::CONTEXT_TOKENS.to_string(),
        "-np".into(),
        "1".into(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "--no-webui".into(),
        "--no-mmproj".into(),
    ];
    let extra: [String; 4] = if gpu {
        [
            "-ngl".to_string(),
            "99".to_string(),
            "--reasoning-budget".to_string(),
            prompt::REASONING_BUDGET_TOKENS.to_string(),
        ]
    } else {
        // CPU only (~4 tok/s): no hidden thinking, it would take minutes.
        [
            "-ngl".to_string(),
            "0".to_string(),
            "-dev".to_string(),
            "none".to_string(),
        ]
    };
    args.extend(extra);
    if !gpu {
        args.push("--reasoning".to_string());
        args.push("off".to_string());
    }
    args
}

/// The `-m` argument. llama.cpp opens the model with narrow-char file APIs,
/// so a Cyrillic user name in the absolute path breaks the load. The server
/// runs with `current_dir(bin_dir)`, and `bin\` and `models\` are siblings,
/// so the model is passed as the pure-ASCII relative path `..\models\<file>`.
/// Falls back to the absolute path when the layout is unexpected.
pub(super) fn model_arg(p: &LlmPaths) -> String {
    let same_root = p.bin_dir.parent().is_some() && p.bin_dir.parent() == p.models_dir.parent();
    let in_models = p.model_path.parent() == Some(p.models_dir.as_path());
    if same_root && in_models {
        if let (Some(dir), Some(file)) = (p.models_dir.file_name(), p.model_path.file_name()) {
            let rel = std::path::Path::new("..").join(dir).join(file);
            let rel = rel.to_string_lossy().into_owned();
            if rel.is_ascii() {
                return rel;
            }
        }
    }
    p.model_path.to_string_lossy().into_owned()
}

fn spawn(p: &LlmPaths, port: u16, gpu: bool) -> Result<Child, String> {
    let log = std::fs::File::create(&p.log_path).map_err(|e| e.to_string())?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(&p.server_exe);
    cmd.args(server_args(&model_arg(p), port, gpu))
        .current_dir(&p.bin_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    attach_kill_on_close_job(&child);
    Ok(child)
}

/// Put the server into a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
/// The job handle is never closed by us, so Windows closes it when Plot Flow
/// exits for any reason (including a crash) and kills the server with it.
#[cfg(windows)]
fn attach_kill_on_close_job(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    // Raw handle value; HANDLE itself is not Sync.
    static JOB: OnceLock<Option<usize>> = OnceLock::new();
    let job = *JOB.get_or_init(|| unsafe {
        let job = match CreateJobObjectW(None, PCWSTR::null()) {
            Ok(job) => job,
            Err(e) => {
                log::warn!("Local LLM: CreateJobObjectW failed: {e}");
                return None;
            }
        };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Err(e) = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) {
            log::warn!("Local LLM: SetInformationJobObject failed: {e}");
            let _ = CloseHandle(job);
            return None;
        }
        Some(job.0 as usize)
    });
    let Some(job) = job else {
        return;
    };
    let process = HANDLE(child.as_raw_handle() as *mut std::ffi::c_void);
    if let Err(e) =
        unsafe { AssignProcessToJobObject(HANDLE(job as *mut std::ffi::c_void), process) }
    {
        log::warn!("Local LLM: could not attach the server to a job object: {e}");
    }
}

/// Last few lines of the server log, for error messages.
fn log_tail(p: &LlmPaths) -> String {
    let text = std::fs::read(&p.log_path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(3);
    lines[start..].join(" | ")
}

async fn wait_healthy(child: &mut Child, port: u16) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("http://127.0.0.1:{}/health", port);
    let started = Instant::now();
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("процесс завершился ({})", status));
        }
        if let Ok(resp) = client.get(&url).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        if started.elapsed() > START_TIMEOUT {
            return Err("нейросеть не ответила за 3 минуты".to_string());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn start_one(p: &LlmPaths, gpu: bool) -> Result<(Child, u16), String> {
    let port = free_port()?;
    let mut child = spawn(p, port, gpu)?;
    match wait_healthy(&mut child, port).await {
        Ok(()) => Ok((child, port)),
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            let tail = log_tail(p);
            Err(if tail.is_empty() {
                e
            } else {
                format!("{}: {}", e, tail)
            })
        }
    }
}

/// Returns a running server, starting one if needed: Vulkan GPU first, CPU
/// if the GPU start fails (no Vulkan, not enough VRAM, driver crash).
/// Caller must hold `RUN_LOCK`.
pub(super) async fn ensure_running(p: &LlmPaths) -> Result<Endpoint, String> {
    if let Some(ep) = current_endpoint() {
        return Ok(ep);
    }
    kill_stale(p);
    if !p.server_exe.exists() {
        return Err("Модель не установлена".to_string());
    }

    let (child, port, gpu) = match start_one(p, true).await {
        Ok((child, port)) => (child, port, true),
        Err(gpu_err) => {
            log::warn!(
                "Local LLM GPU start failed, falling back to CPU: {}",
                gpu_err
            );
            match start_one(p, false).await {
                Ok((child, port)) => (child, port, false),
                Err(cpu_err) => {
                    return Err(format!("Не удалось запустить нейросеть: {}", cpu_err));
                }
            }
        }
    };
    log::info!(
        "Local LLM server started on port {} ({}), pid {}",
        port,
        if gpu { "GPU" } else { "CPU" },
        child.id()
    );
    let _ = std::fs::write(&p.pid_path, child.id().to_string());
    if let Ok(mut g) = SERVER.lock() {
        *g = Some(Server {
            child,
            port,
            gpu,
            pid_path: p.pid_path.clone(),
        });
    }
    touch();
    start_reaper();
    Ok(Endpoint { port, gpu })
}

fn start_reaper() {
    if REAPER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async {
        loop {
            tokio::time::sleep(REAPER_TICK).await;
            if idle_for() < IDLE_TIMEOUT {
                continue;
            }
            // Busy → skip; a summary in progress keeps the server.
            let Ok(_guard) = RUN_LOCK.try_lock() else {
                continue;
            };
            if idle_for() >= IDLE_TIMEOUT && is_running() {
                log::info!("Local LLM idle for {:?}, stopping", IDLE_TIMEOUT);
                stop();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_args_offload_and_think() {
        let a = server_args("m.gguf", 1234, true);
        let s = a.join(" ");
        assert!(s.contains("-ngl 99"));
        assert!(s.contains("--port 1234"));
        assert!(s.contains("--host 127.0.0.1"));
        assert!(s.contains("-c 8192"));
        assert!(s.contains("--reasoning-budget"));
        assert!(!s.contains("-dev none"));
    }

    #[test]
    fn cpu_args_disable_gpu_and_thinking() {
        let s = server_args("m.gguf", 1, false).join(" ");
        assert!(s.contains("-ngl 0"));
        assert!(s.contains("-dev none"));
        assert!(s.contains("--reasoning off"));
    }

    #[test]
    fn model_is_passed_as_ascii_relative_path() {
        let root = std::path::PathBuf::from("C:\\Users\\Игорь\\AppData\\Roaming\\plot\\llm");
        let p = super::super::LlmPaths::new(root);
        let arg = model_arg(&p);
        assert!(arg.is_ascii());
        assert!(arg.starts_with(".."));
        assert!(arg.ends_with(super::super::MODEL.file_name));
        // Resolved from bin\, it points at the real model file.
        assert_eq!(
            p.bin_dir.join(&arg).components().count(),
            p.bin_dir.components().count() + 3
        );
    }

    #[test]
    fn free_port_is_nonzero() {
        assert!(free_port().unwrap() > 0);
    }
}
