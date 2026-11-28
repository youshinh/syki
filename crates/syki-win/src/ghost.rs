//! Managed local Ghost Text runtime (`syki-ghost-v3.1.gguf` + `llama-server.exe`).
//!
//! - Zero impact on `syki.exe` startup or binary size: nothing runs until the user
//!   installs `syki-ghost` and triggers autocomplete.
//! - On-demand download with strict SHA-256 verification into `<data_dir>/ghost/`
//!   (or portable `<exe_dir>/ghost/` if present).
//! - Dynamic ephemeral port binding (`127.0.0.1:0`), `BELOW_NORMAL_PRIORITY_CLASS`,
//!   and automatic idle sleep (terminates sidecar -> 0 MB RAM after idle timeout
//!   or when `syki` hides to tray).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::Digest;
use syki_core::rpc::RpcEvent;

use crate::rpc_dispatch::EventSender;

pub const GHOST_MODEL_ID: &str = "syki-ghost";
pub const GHOST_MODEL_VERSION: &str = "v3.1";
pub const GHOST_MODEL_FILE: &str = "syki-ghost-v3.1.gguf";
pub const GHOST_MODEL_URL: &str =
    "https://huggingface.co/youyouyouyou/syki-ghost/resolve/main/syki-ghost-v3.1.gguf";
pub const GHOST_MODEL_SHA256: &str =
    "3b9ffc23b43720b922e1519f5e9ff4a2d8c0f9f697d2cbbda5e11d8e55cb56f7";
pub const GHOST_MODEL_SIZE: u64 = 186_871_104;

pub const GHOST_RUNTIME_FILE: &str = "llama-server.exe";
pub const GHOST_RUNTIME_URL: &str =
    "https://github.com/ggml-org/llama.cpp/releases/download/b12818/llama-b12818-bin-win-cpu-x64.zip";
pub const GHOST_RUNTIME_SHA256: &str =
    "cc1e850be7b46be0e1e425c7c6df7eb2608970748862fd120611a1cd263e9d94";
pub const GHOST_RUNTIME_SIZE: u64 = 25_368_893;

pub const GHOST_PRESET_BASE_URL: &str = "syki-ghost://local";
const IDLE_SLEEP_SECS: u64 = 180;
const HEALTH_POLL_TIMEOUT_MS: u64 = 10_000;

static INSTALL_CANCELLED: AtomicBool = AtomicBool::new(false);
static INSTALL_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
static IDLE_WATCHER_STARTED: AtomicBool = AtomicBool::new(false);

struct GhostServerState {
    child: Option<Child>,
    port: u16,
    last_used: Instant,
}

fn server_state() -> &'static Mutex<GhostServerState> {
    static STATE: OnceLock<Mutex<GhostServerState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(GhostServerState {
            child: None,
            port: 0,
            last_used: Instant::now(),
        })
    })
}

pub fn is_managed_ghost_target(base_url: &str, model: &str) -> bool {
    let b = base_url.trim().to_ascii_lowercase();
    if b == GHOST_PRESET_BASE_URL || b.starts_with("syki-ghost://") {
        return true;
    }
    b.is_empty() && model.trim().eq_ignore_ascii_case(GHOST_MODEL_ID)
}

fn portable_ghost_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join("ghost");
    if dir.join(GHOST_RUNTIME_FILE).is_file() && dir.join(GHOST_MODEL_FILE).is_file() {
        Some(dir)
    } else {
        None
    }
}

pub fn ghost_dir(data_dir: &Path) -> PathBuf {
    if let Some(p) = portable_ghost_dir() {
        return p;
    }
    data_dir.join("ghost")
}

pub fn get_ghost_status(data_dir: &Path) -> Value {
    let dir = ghost_dir(data_dir);
    let runtime_path = dir.join(GHOST_RUNTIME_FILE);
    let model_path = dir.join(GHOST_MODEL_FILE);
    let runtime_installed = runtime_path.is_file();
    let model_installed = model_path.is_file();
    let installed = runtime_installed && model_installed;

    let (running, port) = {
        let mut st = server_state().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref mut child) = st.child {
            match child.try_wait() {
                Ok(None) => (true, st.port),
                _ => {
                    st.child = None;
                    st.port = 0;
                    (false, 0)
                }
            }
        } else {
            (false, 0)
        }
    };

    json!({
        "id": GHOST_MODEL_ID,
        "version": GHOST_MODEL_VERSION,
        "installed": installed,
        "runtimeInstalled": runtime_installed,
        "modelInstalled": model_installed,
        "installing": INSTALL_IN_PROGRESS.load(Ordering::Acquire),
        "running": running,
        "port": port,
        "dir": dir.to_string_lossy(),
        "modelFile": GHOST_MODEL_FILE,
        "modelUrl": GHOST_MODEL_URL,
        "modelSha256": GHOST_MODEL_SHA256,
        "modelSize": GHOST_MODEL_SIZE,
        "runtimeSize": GHOST_RUNTIME_SIZE,
        "totalSize": GHOST_MODEL_SIZE + GHOST_RUNTIME_SIZE,
        "presetBaseUrl": GHOST_PRESET_BASE_URL,
        "license": "MIT",
        "recommended": {
            "temperature": 0.0,
            "maxTokens": 24,
            "stop": ["\n"],
            "delayMs": 350
        }
    })
}

pub fn cancel_ghost_install() {
    INSTALL_CANCELLED.store(true, Ordering::Release);
}

fn download_with_sha256<F>(
    url: &str,
    expected_sha256: &str,
    expected_size: u64,
    dest_tmp: Option<&Path>,
    mut on_progress: F,
) -> Result<Option<Vec<u8>>, String>
where
    F: FnMut(u64, u64),
{
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .build();
    let resp = agent
        .get(url)
        .set("User-Agent", "syki-ghost-installer/0.1")
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    let total = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(expected_size);

    let mut reader = resp.into_reader();
    let mut hasher = sha2::Sha256::new();
    let mut done = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    let mut mem_buf = if dest_tmp.is_none() {
        Vec::with_capacity(total as usize)
    } else {
        Vec::new()
    };
    let mut file = match dest_tmp {
        Some(p) => Some(std::fs::File::create(p).map_err(|e| e.to_string())?),
        None => None,
    };

    let mut last_emit = Instant::now();
    loop {
        if INSTALL_CANCELLED.load(Ordering::Acquire) {
            drop(file);
            if let Some(p) = dest_tmp {
                let _ = std::fs::remove_file(p);
            }
            return Err("__CANCELED__".to_string());
        }
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
                if let Some(ref mut f) = file {
                    if let Err(e) = f.write_all(&buf[..n]) {
                        drop(file);
                        if let Some(p) = dest_tmp {
                            let _ = std::fs::remove_file(p);
                        }
                        return Err(format!("write failed: {e}"));
                    }
                } else {
                    mem_buf.extend_from_slice(&buf[..n]);
                }
                done += n as u64;
                if last_emit.elapsed() >= Duration::from_millis(120) {
                    on_progress(done, total);
                    last_emit = Instant::now();
                }
            }
            Err(e) => {
                drop(file);
                if let Some(p) = dest_tmp {
                    let _ = std::fs::remove_file(p);
                }
                return Err(format!("read failed: {e}"));
            }
        }
    }
    on_progress(done, total);
    drop(file);

    if !expected_sha256.is_empty() {
        let got = format!("{:x}", hasher.finalize());
        if !got.eq_ignore_ascii_case(expected_sha256) {
            if let Some(p) = dest_tmp {
                let _ = std::fs::remove_file(p);
            }
            return Err(format!(
                "SHA-256 verification failed (expected {expected_sha256}, got {got})"
            ));
        }
    }

    if dest_tmp.is_none() {
        Ok(Some(mem_buf))
    } else {
        Ok(None)
    }
}

pub fn install_ghost_async(data_dir: PathBuf, req_id: String, sender: Option<EventSender>) {
    if INSTALL_IN_PROGRESS
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        if let Some(ref tx) = sender {
            tx(RpcEvent {
                topic: "ghost:install".to_string(),
                payload: json!({
                    "reqId": req_id,
                    "stage": "init",
                    "state": "error",
                    "done": 0,
                    "total": 0,
                    "message": "Already installing syki-ghost"
                }),
            });
        }
        return;
    }

    INSTALL_CANCELLED.store(false, Ordering::Release);
    std::thread::spawn(move || {
        struct ResetGuard;
        impl Drop for ResetGuard {
            fn drop(&mut self) {
                INSTALL_IN_PROGRESS.store(false, Ordering::Release);
            }
        }
        let _guard = ResetGuard;

        let emit = |stage: &str, state: &str, done: u64, total: u64, message: &str| {
            if let Some(ref tx) = sender {
                tx(RpcEvent {
                    topic: "ghost:install".to_string(),
                    payload: json!({
                        "reqId": req_id,
                        "stage": stage,
                        "state": state,
                        "done": done,
                        "total": total,
                        "message": message
                    }),
                });
            }
        };

        let dir = data_dir.join("ghost");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            emit("init", "error", 0, 0, &e.to_string());
            return;
        }

        let need_runtime = !dir.join(GHOST_RUNTIME_FILE).is_file();
        let need_model = !dir.join(GHOST_MODEL_FILE).is_file();
        let grand_total = (if need_runtime { GHOST_RUNTIME_SIZE } else { 0 })
            + (if need_model { GHOST_MODEL_SIZE } else { 0 });
        let mut base_done = 0u64;

        // 1. Download and extract llama-server CPU runtime (MIT)
        if need_runtime {
            emit("runtime", "progress", 0, grand_total, "llama-server");
            match download_with_sha256(
                GHOST_RUNTIME_URL,
                GHOST_RUNTIME_SHA256,
                GHOST_RUNTIME_SIZE,
                None,
                |d, _| emit("runtime", "progress", base_done + d, grand_total, "llama-server"),
            ) {
                Ok(Some(zip_bytes)) => {
                    if let Err(e) = crate::speech::extract_zip_filtered(
                        &zip_bytes,
                        &dir,
                        &["llama-server.exe", "*.dll", "LICENSE*"],
                    ) {
                        emit("runtime", "error", base_done, grand_total, &e);
                        return;
                    }
                    if !dir.join(GHOST_RUNTIME_FILE).is_file() {
                        emit(
                            "runtime",
                            "error",
                            base_done,
                            grand_total,
                            "llama-server.exe not found in runtime archive",
                        );
                        return;
                    }
                    base_done += GHOST_RUNTIME_SIZE;
                }
                Ok(None) => {}
                Err(e) if e == "__CANCELED__" => {
                    emit("runtime", "canceled", base_done, grand_total, "");
                    return;
                }
                Err(e) => {
                    emit("runtime", "error", base_done, grand_total, &e);
                    return;
                }
            }
        }

        // 2. Download syki-ghost-v3.1.gguf (MIT)
        if need_model {
            let tmp_path = dir.join(".syki-ghost.tmp");
            emit("model", "progress", base_done, grand_total, GHOST_MODEL_FILE);
            match download_with_sha256(
                GHOST_MODEL_URL,
                GHOST_MODEL_SHA256,
                GHOST_MODEL_SIZE,
                Some(&tmp_path),
                |d, _| emit("model", "progress", base_done + d, grand_total, GHOST_MODEL_FILE),
            ) {
                Ok(_) => {
                    let dest = dir.join(GHOST_MODEL_FILE);
                    let _ = std::fs::remove_file(&dest);
                    if let Err(e) = std::fs::rename(&tmp_path, &dest) {
                        let _ = std::fs::remove_file(&tmp_path);
                        emit("model", "error", base_done, grand_total, &e.to_string());
                        return;
                    }
                    base_done += GHOST_MODEL_SIZE;
                }
                Err(e) if e == "__CANCELED__" => {
                    emit("model", "canceled", base_done, grand_total, "");
                    return;
                }
                Err(e) => {
                    emit("model", "error", base_done, grand_total, &e);
                    return;
                }
            }
        }

        emit("done", "done", grand_total.max(base_done), grand_total.max(base_done), "");
    });
}

pub fn stop_ghost_server() {
    let mut st = server_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut child) = st.child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    st.port = 0;
}

pub fn remove_ghost(data_dir: &Path) -> Result<(), String> {
    stop_ghost_server();
    let dir = data_dir.join("ghost");
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn ensure_idle_watcher() {
    if IDLE_WATCHER_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(30));
        let mut st = server_state().lock().unwrap_or_else(|e| e.into_inner());
        if st.child.is_some() && st.last_used.elapsed() >= Duration::from_secs(IDLE_SLEEP_SECS) {
            if let Some(mut child) = st.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            st.port = 0;
        }
    });
}

fn pick_ephemeral_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("failed to allocate local port: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("failed to inspect local port: {e}"))?
        .port();
    drop(listener);
    Ok(port)
}

fn optimal_cpu_threads() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (cores / 2).clamp(2, 4)
}

/// Ensures the managed `syki-ghost` sidecar is running and returns its `/v1/completions` URL.
pub fn ensure_running_url(data_dir: &Path) -> Result<String, String> {
    ensure_idle_watcher();

    // Fast path: already running
    {
        let mut st = server_state().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref mut child) = st.child {
            if matches!(child.try_wait(), Ok(None)) && st.port > 0 {
                st.last_used = Instant::now();
                return Ok(format!("http://127.0.0.1:{}/v1/completions", st.port));
            }
            st.child = None;
            st.port = 0;
        }
    }

    let dir = ghost_dir(data_dir);
    let runtime = dir.join(GHOST_RUNTIME_FILE);
    let model = dir.join(GHOST_MODEL_FILE);
    if !runtime.is_file() || !model.is_file() {
        return Err(
            "syki-ghost is not installed yet. Install it from Settings > AI Models > Autocomplete."
                .to_string(),
        );
    }

    let port = pick_ephemeral_port()?;
    let threads = optimal_cpu_threads();

    let mut cmd = Command::new(&runtime);
    cmd.current_dir(&dir)
        .arg("-m")
        .arg(&model)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("-ngl")
        .arg("0")
        .arg("-t")
        .arg(threads.to_string())
        .arg("-c")
        .arg("2048")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to launch syki-ghost runtime: {e}"))?;

    // Wait for /health == ok
    let health_url = format!("http://127.0.0.1:{port}/health");
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(500))
        .build();
    let deadline = Instant::now() + Duration::from_millis(HEALTH_POLL_TIMEOUT_MS);
    let mut ready = false;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("syki-ghost exited early with status {status}"));
        }
        if let Ok(resp) = agent.get(&health_url).call() {
            if resp.status() == 200 {
                ready = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(80));
    }

    if !ready {
        let _ = child.kill();
        let _ = child.wait();
        return Err("syki-ghost timed out waiting for server health".to_string());
    }

    let mut st = server_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut old) = st.child.take() {
        let _ = old.kill();
        let _ = old.wait();
    }
    st.child = Some(child);
    st.port = port;
    st.last_used = Instant::now();

    Ok(format!("http://127.0.0.1:{port}/v1/completions"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_ghost_target_detection() {
        assert!(is_managed_ghost_target("syki-ghost://local", ""));
        assert!(is_managed_ghost_target("SYKI-GHOST://LOCAL", "anything"));
        assert!(is_managed_ghost_target("", "syki-ghost"));
        assert!(!is_managed_ghost_target("http://127.0.0.1:8082/v1/completions", "syki-ghost"));
        assert!(!is_managed_ghost_target("https://generativelanguage.googleapis.com", "gemini"));
    }

    #[test]
    fn manifest_constants_are_valid() {
        assert_eq!(GHOST_MODEL_SHA256.len(), 64);
        assert_eq!(GHOST_RUNTIME_SHA256.len(), 64);
        assert!(GHOST_MODEL_URL.starts_with("https://huggingface.co/"));
        assert!(GHOST_RUNTIME_URL.starts_with("https://github.com/ggml-org/llama.cpp/"));
    }

    #[test]
    fn status_and_remove_lifecycle_in_temp_dir() {
        let base = std::env::temp_dir().join(format!(
            "syki-ghost-unit-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let st0 = get_ghost_status(&base);
        assert_eq!(st0["installed"], false);
        assert_eq!(st0["presetBaseUrl"], GHOST_PRESET_BASE_URL);

        let gdir = base.join("ghost");
        std::fs::create_dir_all(&gdir).unwrap();
        std::fs::write(gdir.join(GHOST_RUNTIME_FILE), b"dummy").unwrap();
        std::fs::write(gdir.join(GHOST_MODEL_FILE), b"dummy").unwrap();

        let st1 = get_ghost_status(&base);
        assert_eq!(st1["installed"], true);
        assert_eq!(st1["runtimeInstalled"], true);
        assert_eq!(st1["modelInstalled"], true);

        remove_ghost(&base).expect("remove");
        let st2 = get_ghost_status(&base);
        assert_eq!(st2["installed"], false);
        let _ = std::fs::remove_dir_all(&base);
    }
}
