//! Embedded Windows ConPTY terminal manager, gate detection, and Git worktree helper (#13).

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::{json, Value};
use syki_core::rpc::RpcEvent;
use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    STARTUPINFOEXW,
};

use crate::rpc_dispatch::EventSender;

const MAX_HISTORY_BYTES: usize = 256 * 1024;
const STILL_ACTIVE_CODE: u32 = 259;

struct PtySession {
    _id: String,
    _label: String,
    hpcon: isize,
    process_handle: isize,
    writer: Mutex<File>,
    history: Arc<Mutex<Vec<u8>>>,
    closed: Arc<AtomicBool>,
}

/// Manages active Windows ConPTY sessions.
#[derive(Default)]
pub struct PtyManager {
    sessions: Arc<Mutex<HashMap<String, Arc<PtySession>>>>,
    seq: AtomicU64,
}

impl PtyManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start_session(
        &self,
        explicit_id: &str,
        preset: &str,
        dir: &str,
        label: &str,
        sender: Option<EventSender>,
    ) -> Result<Value, String> {
        let id = if explicit_id.trim().is_empty() {
            let n = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
            format!("pty-{n}")
        } else {
            explicit_id.trim().to_string()
        };

        let work_dir = resolve_work_dir(dir);
        let cmd_line = command_for_preset(preset);

        let mut in_read = HANDLE::default();
        let mut in_write = HANDLE::default();
        let mut out_read = HANDLE::default();
        let mut out_write = HANDLE::default();

        unsafe {
            CreatePipe(&mut in_read, &mut in_write, None, 0)
                .map_err(|e| format!("CreatePipe(in) failed: {e}"))?;
            if let Err(e) = CreatePipe(&mut out_read, &mut out_write, None, 0) {
                let _ = CloseHandle(in_read);
                let _ = CloseHandle(in_write);
                return Err(format!("CreatePipe(out) failed: {e}"));
            }
        }

        let size = COORD { X: 100, Y: 30 };
        let hpcon: HPCON = match unsafe { CreatePseudoConsole(size, in_read, out_write, 0) } {
            Ok(h) => h,
            Err(e) => {
                unsafe {
                    let _ = CloseHandle(in_read);
                    let _ = CloseHandle(in_write);
                    let _ = CloseHandle(out_read);
                    let _ = CloseHandle(out_write);
                }
                return Err(format!("CreatePseudoConsole failed: {e}"));
            }
        };

        // Child owns the PTY ends after CreatePseudoConsole.
        unsafe {
            let _ = CloseHandle(in_read);
            let _ = CloseHandle(out_write);
        }

        let mut attr_size: usize = 0;
        unsafe {
            let _ = InitializeProcThreadAttributeList(None, 1, Some(0), &mut attr_size);
        }
        let mut attr_buf = vec![0u64; (attr_size.max(64) + 7) / 8];
        let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut _);
        unsafe {
            InitializeProcThreadAttributeList(Some(attr_list), 1, Some(0), &mut attr_size)
                .map_err(|e| format!("InitializeProcThreadAttributeList failed: {e}"))?;
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                Some(hpcon.0 as *const _),
                std::mem::size_of::<HPCON>(),
                None,
                None,
            )
            .map_err(|e| format!("UpdateProcThreadAttribute failed: {e}"))?;
        }

        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = attr_list;

        let mut cmd_utf16: Vec<u16> = cmd_line.encode_utf16().chain(std::iter::once(0)).collect();
        let cwd_h = HSTRING::from(work_dir.as_os_str());
        let mut pi = PROCESS_INFORMATION::default();

        let created = unsafe {
            CreateProcessW(
                None,
                Some(PWSTR(cmd_utf16.as_mut_ptr())),
                None,
                None,
                false,
                EXTENDED_STARTUPINFO_PRESENT,
                None,
                &cwd_h,
                &si.StartupInfo,
                &mut pi,
            )
        };

        unsafe {
            DeleteProcThreadAttributeList(attr_list);
        }

        if let Err(e) = created {
            unsafe {
                ClosePseudoConsole(hpcon);
                let _ = CloseHandle(in_write);
                let _ = CloseHandle(out_read);
            }
            return Err(format!("CreateProcessW failed: {e}"));
        }

        unsafe {
            let _ = CloseHandle(pi.hThread);
        }

        let writer = unsafe { File::from_raw_handle(in_write.0 as *mut _) };
        let mut reader = unsafe { File::from_raw_handle(out_read.0 as *mut _) };
        let history = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let lbl = if label.trim().is_empty() {
            preset.to_string()
        } else {
            label.trim().to_string()
        };

        let session = Arc::new(PtySession {
            _id: id.clone(),
            _label: lbl.clone(),
            hpcon: hpcon.0,
            process_handle: pi.hProcess.0 as isize,
            writer: Mutex::new(writer),
            history: Arc::clone(&history),
            closed: Arc::clone(&closed),
        });
        self.sessions
            .lock()
            .expect("pty sessions mutex")
            .insert(id.clone(), session);

        let id_bg = id.clone();
        let sessions_bg = Arc::clone(&self.sessions);
        let proc_raw = pi.hProcess.0 as isize;
        let hist_bg = Arc::clone(&history);
        let closed_bg = Arc::clone(&closed);

        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        {
                            let mut h = hist_bg.lock().expect("pty history mutex");
                            h.extend_from_slice(chunk);
                            if h.len() > MAX_HISTORY_BYTES {
                                let drain = h.len() - MAX_HISTORY_BYTES;
                                h.drain(..drain);
                            }
                        }
                        if let Some(tx) = sender.as_ref() {
                            tx(RpcEvent {
                                topic: "pty:data".to_string(),
                                payload: json!({
                                    "id": id_bg,
                                    "b64": STANDARD.encode(chunk)
                                }),
                            });
                        }
                    }
                }
            }

            let hproc = HANDLE(proc_raw as *mut _);
            let mut exit_code = 0u32;
            unsafe {
                let _ = WaitForSingleObject(hproc, 1000);
                let _ = GetExitCodeProcess(hproc, &mut exit_code);
                if exit_code == STILL_ACTIVE_CODE {
                    exit_code = 0;
                }
                let _ = CloseHandle(hproc);
            }
            closed_bg.store(true, Ordering::Release);
            sessions_bg.lock().expect("pty sessions mutex").remove(&id_bg);

            if let Some(tx) = sender {
                tx(RpcEvent {
                    topic: "pty:exit".to_string(),
                    payload: json!({ "id": id_bg, "code": exit_code, "error": "" }),
                });
            }
        });

        Ok(json!({
            "id": id,
            "preset": preset,
            "dir": work_dir.to_string_lossy(),
            "label": lbl
        }))
    }

    pub fn write(&self, id: &str, data: &str) -> Result<(), String> {
        let session = self
            .sessions
            .lock()
            .expect("pty sessions mutex")
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown pty session: {id}"))?;
        let mut w = session.writer.lock().expect("pty writer mutex");
        w.write_all(data.as_bytes())
            .and_then(|_| w.flush())
            .map_err(|e| format!("ptyWrite failed: {e}"))
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let session = self
            .sessions
            .lock()
            .expect("pty sessions mutex")
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown pty session: {id}"))?;
        let coord = COORD {
            X: cols.max(10) as i16,
            Y: rows.max(4) as i16,
        };
        unsafe {
            ResizePseudoConsole(HPCON(session.hpcon), coord)
                .map_err(|e| format!("ResizePseudoConsole failed: {e}"))
        }
    }

    pub fn rename(&self, id: &str, new_label: &str) -> Result<String, String> {
        let guard = self.sessions.lock().expect("pty sessions mutex");
        let _ = guard
            .get(id)
            .ok_or_else(|| format!("unknown pty session: {id}"))?;
        Ok(new_label.trim().to_string())
    }

    pub fn history(&self, id: &str) -> String {
        let session = self
            .sessions
            .lock()
            .expect("pty sessions mutex")
            .get(id)
            .cloned();
        match session {
            Some(s) => {
                let h = s.history.lock().expect("pty history mutex");
                String::from_utf8_lossy(&h).into_owned()
            }
            None => String::new(),
        }
    }

    pub fn close(&self, id: &str) -> bool {
        let session = self
            .sessions
            .lock()
            .expect("pty sessions mutex")
            .remove(id);
        let Some(s) = session else {
            return false;
        };
        if !s.closed.swap(true, Ordering::AcqRel) {
            unsafe {
                let _ = TerminateProcess(HANDLE(s.process_handle as *mut _), 1);
                ClosePseudoConsole(HPCON(s.hpcon));
            }
        }
        true
    }

    pub fn close_all(&self) {
        let ids: Vec<String> = self
            .sessions
            .lock()
            .expect("pty sessions mutex")
            .keys()
            .cloned()
            .collect();
        for id in ids {
            self.close(&id);
        }
    }
}

fn resolve_work_dir(dir: &str) -> PathBuf {
    let trimmed = dir.trim();
    if !trimmed.is_empty() {
        let p = PathBuf::from(trimmed);
        if p.is_dir() {
            return p;
        }
    }
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn command_for_preset(preset: &str) -> &'static str {
    match preset.trim().to_ascii_lowercase().as_str() {
        "claude" => "cmd.exe /k claude",
        "codex" => "cmd.exe /k codex",
        "gemini" => "cmd.exe /k gemini",
        _ => "cmd.exe",
    }
}

pub fn available_presets() -> Value {
    json!([
        { "id": "cmd", "label": "Command Prompt", "available": true },
        { "id": "claude", "label": "Claude Code", "available": true },
        { "id": "codex", "label": "Codex CLI", "available": true },
        { "id": "gemini", "label": "Gemini CLI", "available": true }
    ])
}

/// Detects the primary verification/test command for `dir`.
pub fn detect_project_gate(dir: &Path) -> &'static str {
    if dir.join("Cargo.toml").is_file() {
        "cargo test"
    } else if dir.join("package.json").is_file() {
        "npm test"
    } else if dir.join("go.mod").is_file() {
        "go test ./..."
    } else if dir.join("pyproject.toml").is_file() || dir.join("pytest.ini").is_file() {
        "pytest"
    } else {
        ""
    }
}

/// Creates an isolated Git worktree if `dir` is a Git repository; otherwise returns `dir`.
pub fn prepare_worktree(dir: &str, id: &str) -> Value {
    let base = resolve_work_dir(dir);
    let safe_id: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let wt_name = if safe_id.is_empty() { "wt" } else { &safe_id };
    let wt_path = base.join(".worktrees").join(wt_name);

    let out = Command::new("git")
        .args(["-C", &base.to_string_lossy(), "rev-parse", "--is-inside-work-tree"])
        .output();
    let is_git = out
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false);
    if !is_git {
        return json!({ "ok": true, "worktreePath": base.to_string_lossy(), "isWorktree": false });
    }

    if !wt_path.exists() {
        let _ = std::fs::create_dir_all(wt_path.parent().unwrap_or(&base));
        let branch = format!("syki/{wt_name}");
        let add = Command::new("git")
            .args([
                "-C",
                &base.to_string_lossy(),
                "worktree",
                "add",
                "-B",
                &branch,
                &wt_path.to_string_lossy(),
            ])
            .output();
        if add.map(|o| o.status.success()).unwrap_or(false) {
            return json!({ "ok": true, "worktreePath": wt_path.to_string_lossy(), "isWorktree": true });
        }
    } else {
        return json!({ "ok": true, "worktreePath": wt_path.to_string_lossy(), "isWorktree": true });
    }

    json!({ "ok": true, "worktreePath": base.to_string_lossy(), "isWorktree": false })
}

pub fn suggest_dirs() -> Vec<String> {
    let mut dirs = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.to_string_lossy().into_owned());
    }
    if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        let s = home.to_string_lossy().into_owned();
        if !dirs.contains(&s) {
            dirs.push(s);
        }
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn detect_project_gate_identifies_cargo_npm_go_python() {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("syki-gate-{nanos}"));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(detect_project_gate(&root), "");

        std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(detect_project_gate(&root), "cargo test");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn conpty_starts_writes_echos_and_closes() {
        let mgr = PtyManager::new();
        let (tx, rx) = std::sync::mpsc::channel::<RpcEvent>();
        let started = mgr
            .start_session(
                "test-pty",
                "cmd",
                ".",
                "Test Shell",
                Some(Arc::new(move |ev| {
                    let _ = tx.send(ev);
                })),
            )
            .expect("start ConPTY");
        assert_eq!(started["id"], "test-pty");

        std::thread::sleep(Duration::from_millis(500));
        mgr.write("test-pty", "echo SYKI_PTY_OK\r\n").expect("ptyWrite");
        mgr.resize("test-pty", 80, 24).expect("ptyResize");

        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        let mut combined = String::new();
        while std::time::Instant::now() < deadline {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
                if ev.topic == "pty:data" {
                    if let Some(b64) = ev.payload["b64"].as_str() {
                        if let Ok(bytes) = STANDARD.decode(b64.as_bytes()) {
                            combined.push_str(&String::from_utf8_lossy(&bytes));
                            if combined.contains("SYKI_PTY_OK") {
                                break;
                            }
                        }
                    }
                }
            }
        }
        assert!(combined.contains("SYKI_PTY_OK"), "got output: {combined}");
        assert!(mgr.close("test-pty"));
    }
}
