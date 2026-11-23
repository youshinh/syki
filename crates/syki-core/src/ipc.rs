//! Single-instance Windows Named Pipe IPC server & client (`#06`).
//!
//! Uses `\\.\pipe\syki-<instance_id>` with a per-session random auth token
//! stored atomically in `<data_dir>/ipc-session.json`.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use windows::core::HSTRING;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{FlushFileBuffers, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, PIPE_READMODE_BYTE,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};

use crate::rpc::RuntimeEnv;
use crate::storage::atomic::write_atomic_bytes;

pub const MAX_IPC_LINE_BYTES: usize = 12 * 1024 * 1024;

pub const METHOD_PLUGIN_LIST: &str = "plugin.list";
pub const METHOD_PLUGIN_STATUS: &str = "plugin.status";
pub const METHOD_PLUGIN_RELOAD: &str = "plugin.reload";
pub const METHOD_PLUGIN_RUN: &str = "plugin.run";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IpcSession {
    pub pid: u32,
    pub pipe_name: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcEnvelope {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub auth: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub headless: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcReply {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl IpcReply {
    pub fn ok(id: impl Into<String>, result: Value) -> Self {
        Self {
            id: id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

/// Computes the instance-scoped Windows Named Pipe path.
pub fn pipe_name_for(env: &RuntimeEnv) -> String {
    let clean: String = env
        .instance_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let suffix = if clean.is_empty() {
        "primary"
    } else {
        clean.as_str()
    };
    format!(r"\\.\pipe\syki-{suffix}")
}

pub fn session_file_path(env: &RuntimeEnv) -> PathBuf {
    env.data_dir.join("ipc-session.json")
}

/// Generates a per-process random hex token without pulling in a `rand` crate.
pub fn generate_token() -> String {
    use sha2::{Digest, Sha256};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut hasher = Sha256::new();
    hasher.update(nanos.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    let stack_addr = &nanos as *const u128 as usize;
    hasher.update(stack_addr.to_le_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest.iter() {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

pub fn save_ipc_session(path: &Path, session: &IpcSession) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(session)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_atomic_bytes(path, &bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
}

pub fn load_ipc_session(path: &Path) -> std::io::Result<IpcSession> {
    let raw = std::fs::read(path)?;
    serde_json::from_slice(&raw)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn is_pipe_available(pipe_name: &str) -> bool {
    let pipe_h = HSTRING::from(pipe_name);
    unsafe { WaitNamedPipeW(&pipe_h, 50).as_bool() }
}

/// Sends a single [`IpcEnvelope`] over the Windows Named Pipe and waits for [`IpcReply`].
pub fn send_ipc_request(pipe_name: &str, env: &IpcEnvelope) -> Result<IpcReply, String> {
    let pipe_h = HSTRING::from(pipe_name);
    let mut file = None;
    for attempt in 0..5 {
        match OpenOptions::new().read(true).write(true).open(pipe_name) {
            Ok(f) => {
                file = Some(f);
                break;
            }
            Err(e) => {
                if attempt == 4 {
                    return Err(format!("failed to connect to pipe {pipe_name}: {e}"));
                }
                unsafe {
                    let _ = WaitNamedPipeW(&pipe_h, 150);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
    let mut stream = file.ok_or_else(|| format!("pipe unavailable: {pipe_name}"))?;
    let mut payload = serde_json::to_string(env).map_err(|e| e.to_string())?;
    payload.push('\n');
    stream
        .write_all(payload.as_bytes())
        .and_then(|_| stream.flush())
        .map_err(|e| format!("ipc write failed: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("ipc read failed: {e}"))?;
    if line.trim().is_empty() {
        return Err("ipc server closed connection without reply".to_string());
    }
    serde_json::from_str::<IpcReply>(line.trim()).map_err(|e| format!("invalid ipc reply: {e}"))
}

/// Background Windows Named Pipe listener for single-instance & CLI RPC.
pub struct IpcPipeServer {
    pipe_name: String,
    session_path: PathBuf,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl IpcPipeServer {
    pub fn start<F>(env: &RuntimeEnv, handler: F) -> Result<Self, String>
    where
        F: Fn(IpcEnvelope) -> IpcReply + Send + Sync + 'static,
    {
        let pipe_name = pipe_name_for(env);
        let session_path = session_file_path(env);
        let token = generate_token();
        let session = IpcSession {
            pid: std::process::id(),
            pipe_name: pipe_name.clone(),
            token: token.clone(),
        };
        save_ipc_session(&session_path, &session)
            .map_err(|e| format!("failed to write ipc-session.json: {e}"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let pipe_bg = pipe_name.clone();
        let handler = Arc::new(handler);

        let join = std::thread::spawn(move || {
            let pipe_h = HSTRING::from(pipe_bg.as_str());
            while !stop_bg.load(Ordering::Acquire) {
                let raw_handle: HANDLE = unsafe {
                    CreateNamedPipeW(
                        &pipe_h,
                        PIPE_ACCESS_DUPLEX,
                        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                        4,
                        64 * 1024,
                        64 * 1024,
                        0,
                        None,
                    )
                };
                if raw_handle == INVALID_HANDLE_VALUE || raw_handle.is_invalid() {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }

                let connected = unsafe {
                    let res = ConnectNamedPipe(raw_handle, None);
                    res.is_ok() || GetLastError() == ERROR_PIPE_CONNECTED
                };
                if !connected || stop_bg.load(Ordering::Acquire) {
                    unsafe {
                        let _ = DisconnectNamedPipe(raw_handle);
                        let _ = CloseHandle(raw_handle);
                    }
                    continue;
                }

                let mut file = unsafe { std::fs::File::from_raw_handle(raw_handle.0 as *mut _) };
                let mut limited = (&mut file).take(MAX_IPC_LINE_BYTES as u64);
                let mut reader = BufReader::new(&mut limited);
                let mut line = String::new();
                let reply = match reader.read_line(&mut line) {
                    Ok(0) => None,
                    Ok(_) => match serde_json::from_str::<IpcEnvelope>(line.trim()) {
                        Ok(req) => {
                            if req.auth != token {
                                Some(IpcReply::err(req.id, "unauthorized: invalid ipc token"))
                            } else {
                                Some(handler(req))
                            }
                        }
                        Err(e) => Some(IpcReply::err("", format!("invalid ipc json: {e}"))),
                    },
                    Err(e) => Some(IpcReply::err("", format!("ipc read error: {e}"))),
                };

                if let Some(resp) = reply {
                    if let Ok(mut json_line) = serde_json::to_string(&resp) {
                        json_line.push('\n');
                        let _ = file.write_all(json_line.as_bytes());
                        let _ = file.flush();
                    }
                }
                unsafe {
                    let _ = FlushFileBuffers(raw_handle);
                    let _ = DisconnectNamedPipe(raw_handle);
                }
                // `file` owns `raw_handle` and closes it on drop.
            }
        });

        Ok(Self {
            pipe_name,
            session_path,
            stop,
            join: Some(join),
        })
    }

    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    pub fn stop(&mut self) {
        if self.stop.swap(true, Ordering::AcqRel) {
            return;
        }
        // Unblock ConnectNamedPipe by briefly connecting if waiting.
        let _ = OpenOptions::new().read(true).write(true).open(&self.pipe_name);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
        let _ = std::fs::remove_file(&self.session_path);
    }
}

impl Drop for IpcPipeServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Helper to build an authenticated [`IpcEnvelope`] from a saved [`IpcSession`].
pub fn make_auth_envelope(session: &IpcSession, action: &str, method: &str, params: Value) -> IpcEnvelope {
    IpcEnvelope {
        id: "1".to_string(),
        auth: session.token.clone(),
        action: action.to_string(),
        method: method.to_string(),
        params,
        content: String::new(),
        path: String::new(),
        cwd: std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        headless: false,
    }
}

/// Builds an authenticated plugin JSON-RPC [`IpcEnvelope`].
pub fn build_plugin_rpc_envelope(
    auth: &str,
    method: &str,
    params: Value,
    headless: bool,
) -> IpcEnvelope {
    IpcEnvelope {
        id: "1".to_string(),
        auth: auth.to_string(),
        action: String::new(),
        method: method.to_string(),
        params,
        content: String::new(),
        path: String::new(),
        cwd: std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        headless,
    }
}

/// Returns true if `method` is one of the plugin JSON-RPC methods.
pub fn is_plugin_rpc_method(method: &str) -> bool {
    matches!(
        method,
        METHOD_PLUGIN_LIST | METHOD_PLUGIN_STATUS | METHOD_PLUGIN_RELOAD | METHOD_PLUGIN_RUN
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_env(label: &str) -> (PathBuf, RuntimeEnv) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("syki-ipc-{label}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let env = RuntimeEnv {
            data_dir: dir.clone(),
            instance_id: format!("test-{label}-{nanos}"),
        };
        (dir, env)
    }

    #[test]
    fn pipe_name_sanitizes_instance_id() {
        let env = RuntimeEnv {
            data_dir: PathBuf::from("."),
            instance_id: "my/inst:01".to_string(),
        };
        assert_eq!(pipe_name_for(&env), r"\\.\pipe\syki-my_inst_01");
    }

    #[test]
    fn named_pipe_server_handles_authenticated_requests_and_rejects_bad_tokens() {
        let (dir, env) = temp_env("roundtrip");
        let mut server = IpcPipeServer::start(&env, |req| {
            if req.action == "activate" {
                IpcReply::ok(req.id, json!({ "activated": true }))
            } else {
                IpcReply::ok(req.id, json!({ "method": req.method, "echo": req.params }))
            }
        })
        .expect("start pipe server");

        // Give the background thread a moment to create the pipe instance.
        std::thread::sleep(Duration::from_millis(40));

        let session = load_ipc_session(&session_file_path(&env)).expect("load session");
        assert_eq!(session.pipe_name, server.pipe_name());

        let ok_req = make_auth_envelope(&session, "activate", "", json!({}));
        let ok_reply = send_ipc_request(&session.pipe_name, &ok_req).expect("send ok");
        assert!(ok_reply.ok);
        assert_eq!(ok_reply.result.unwrap()["activated"], true);

        let mut bad_req = make_auth_envelope(&session, "", "config.get", json!({}));
        bad_req.auth = "wrong-token".to_string();
        let bad_reply = send_ipc_request(&session.pipe_name, &bad_req).expect("send bad");
        assert!(!bad_reply.ok);
        assert!(bad_reply.error.unwrap().contains("unauthorized"));

        server.stop();
        assert!(!session_file_path(&env).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ipc_envelope_headless_defaults_to_false_for_backwards_compatibility() {
        let legacy_json = r#"{"id":"1","auth":"tok","action":"activate","method":"ui.activate","params":{}}"#;
        let parsed: IpcEnvelope = serde_json::from_str(legacy_json).expect("deserialize legacy");
        assert!(!parsed.headless);
        assert_eq!(parsed.method, "ui.activate");
    }

    #[test]
    fn build_plugin_rpc_envelope_roundtrips_json() {
        let env = build_plugin_rpc_envelope(
            "secret-token",
            METHOD_PLUGIN_RUN,
            json!({ "command_id": "demo.echo", "args": { "msg": "hi" } }),
            true,
        );
        assert_eq!(env.auth, "secret-token");
        assert_eq!(env.method, METHOD_PLUGIN_RUN);
        assert!(env.headless);

        let serialized = serde_json::to_string(&env).expect("serialize");
        let deserialized: IpcEnvelope = serde_json::from_str(&serialized).expect("deserialize");
        assert_eq!(deserialized.auth, "secret-token");
        assert_eq!(deserialized.method, METHOD_PLUGIN_RUN);
        assert_eq!(deserialized.params["command_id"], "demo.echo");
        assert_eq!(deserialized.params["args"]["msg"], "hi");
        assert!(deserialized.headless);

        let visible_env = build_plugin_rpc_envelope("tok", METHOD_PLUGIN_RUN, json!({}), false);
        assert!(!visible_env.headless);
    }

    #[test]
    fn is_plugin_rpc_method_matches_only_plugin_methods() {
        assert!(is_plugin_rpc_method(METHOD_PLUGIN_LIST));
        assert!(is_plugin_rpc_method(METHOD_PLUGIN_STATUS));
        assert!(is_plugin_rpc_method(METHOD_PLUGIN_RELOAD));
        assert!(is_plugin_rpc_method(METHOD_PLUGIN_RUN));
        assert!(!is_plugin_rpc_method("ui.activate"));
        assert!(!is_plugin_rpc_method("plugin.unknown"));
        assert!(!is_plugin_rpc_method(""));
    }
}
