//! Embedded Windows ConPTY terminal manager, gate detection, and Git worktree helper (#13).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::{json, Value};
use syki_core::rpc::RpcEvent;
use windows::core::{w, HSTRING, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND,
};
use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, CreatePipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES,
    STARTUPINFOEXW,
};

use crate::rpc_dispatch::EventSender;

const MAX_HISTORY_BYTES: usize = 256 * 1024;
const STILL_ACTIVE_CODE: u32 = 259;

const CTRL_TAG_STDIN: u8 = 0;
const CTRL_TAG_RESIZE: u8 = 1;
const CTRL_TAG_CLOSE: u8 = 2;

fn write_ctrl_frame(writer: &mut File, tag: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut hdr = [0u8; 5];
    hdr[0] = tag;
    hdr[1..5].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    writer.write_all(&hdr)?;
    if !payload.is_empty() {
        writer.write_all(payload)?;
    }
    writer.flush()
}

fn read_ctrl_frame(reader: &mut File) -> std::io::Result<Option<(u8, Vec<u8>)>> {
    let mut hdr = [0u8; 5];
    match reader.read_exact(&mut hdr) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let tag = hdr[0];
    let len = u32::from_le_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
    if len > 16 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "control frame too large",
        ));
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut payload)?;
    }
    Ok(Some((tag, payload)))
}

enum PtyBackend {
    Local {
        hpcon: isize,
        process_handle: isize,
        writer: Mutex<File>,
    },
    ElevatedBridge {
        ctrl_writer: Mutex<File>,
    },
}

struct PtySession {
    _id: String,
    _label: String,
    backend: PtyBackend,
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
        self.start_session_with_size(explicit_id, preset, dir, label, 100, 30, sender)
    }

    pub fn start_session_with_size(
        &self,
        explicit_id: &str,
        preset: &str,
        dir: &str,
        label: &str,
        cols: u16,
        rows: u16,
        sender: Option<EventSender>,
    ) -> Result<Value, String> {
        let id = if explicit_id.trim().is_empty() {
            let n = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
            format!("pty-{n}")
        } else {
            explicit_id.trim().to_string()
        };

        let work_dir = resolve_work_dir(dir);
        if let Some(inner_preset) = preset
            .trim()
            .strip_prefix("admin:")
            .or_else(|| preset.trim().strip_prefix("ADMIN:"))
        {
            return self.start_elevated_session_with_size(
                &id,
                inner_preset,
                &work_dir,
                label,
                cols,
                rows,
                sender,
            );
        }

        let (hpcon, proc_handle, writer, mut reader) =
            spawn_conpty_process(preset, &work_dir, cols, rows)?;
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
            backend: PtyBackend::Local {
                hpcon: hpcon.0,
                process_handle: proc_handle.0 as isize,
                writer: Mutex::new(writer),
            },
            history: Arc::clone(&history),
            closed: Arc::clone(&closed),
        });
        self.sessions
            .lock()
            .expect("pty sessions mutex")
            .insert(id.clone(), session);

        let id_bg = id.clone();
        let sessions_bg = Arc::clone(&self.sessions);
        let proc_raw = proc_handle.0 as isize;
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
        self.write_bytes(id, data.as_bytes())
    }

    pub fn write_bytes(&self, id: &str, data: &[u8]) -> Result<(), String> {
        let session = self
            .sessions
            .lock()
            .expect("pty sessions mutex")
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown pty session: {id}"))?;
        match &session.backend {
            PtyBackend::Local { writer, .. } => {
                let mut w = writer.lock().expect("pty writer mutex");
                w.write_all(data)
                    .and_then(|_| w.flush())
                    .map_err(|e| format!("ptyWrite failed: {e}"))
            }
            PtyBackend::ElevatedBridge { ctrl_writer } => {
                let mut w = ctrl_writer.lock().expect("pty ctrl writer mutex");
                write_ctrl_frame(&mut w, CTRL_TAG_STDIN, data)
                    .map_err(|e| format!("ptyWrite(elevated) failed: {e}"))
            }
        }
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let session = self
            .sessions
            .lock()
            .expect("pty sessions mutex")
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown pty session: {id}"))?;
        let c = cols.max(10);
        let r = rows.max(4);
        match &session.backend {
            PtyBackend::Local { hpcon, .. } => {
                let coord = COORD {
                    X: c as i16,
                    Y: r as i16,
                };
                unsafe {
                    ResizePseudoConsole(HPCON(*hpcon), coord)
                        .map_err(|e| format!("ResizePseudoConsole failed: {e}"))
                }
            }
            PtyBackend::ElevatedBridge { ctrl_writer } => {
                let mut payload = [0u8; 4];
                payload[0..2].copy_from_slice(&c.to_le_bytes());
                payload[2..4].copy_from_slice(&r.to_le_bytes());
                let mut w = ctrl_writer.lock().expect("pty ctrl writer mutex");
                write_ctrl_frame(&mut w, CTRL_TAG_RESIZE, &payload)
                    .map_err(|e| format!("ptyResize(elevated) failed: {e}"))
            }
        }
    }

    pub fn rename(&self, id: &str, new_label: &str) -> Result<String, String> {
        let guard = self.sessions.lock().expect("pty sessions mutex");
        let _ = guard
            .get(id)
            .ok_or_else(|| format!("unknown pty session: {id}"))?;
        Ok(new_label.trim().to_string())
    }

    pub fn list_sessions(&self) -> Value {
        let guard = self.sessions.lock().expect("pty sessions mutex");
        let mut items: Vec<Value> = guard
            .values()
            .filter(|s| !s.closed.load(Ordering::Acquire))
            .map(|s| {
                json!({
                    "id": s._id,
                    "label": s._label,
                    "active": true
                })
            })
            .collect();
        items.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        Value::Array(items)
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
            match &s.backend {
                PtyBackend::Local {
                    hpcon,
                    process_handle,
                    ..
                } => unsafe {
                    let _ = TerminateProcess(HANDLE(*process_handle as *mut _), 1);
                    ClosePseudoConsole(HPCON(*hpcon));
                },
                PtyBackend::ElevatedBridge { ctrl_writer } => {
                    if let Ok(mut w) = ctrl_writer.lock() {
                        let _ = write_ctrl_frame(&mut w, CTRL_TAG_CLOSE, &[]);
                    }
                }
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

    fn start_elevated_session_with_size(
        &self,
        id: &str,
        preset: &str,
        work_dir: &Path,
        label: &str,
        cols: u16,
        rows: u16,
        sender: Option<EventSender>,
    ) -> Result<Value, String> {
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let pipe_base = format!(r"\\.\pipe\syki-elev-pty-{pid}-{nonce}");
        let in_pipe_name = format!("{pipe_base}-in");
        let out_pipe_name = format!("{pipe_base}-out");

        let in_h = unsafe {
            CreateNamedPipeW(
                &HSTRING::from(in_pipe_name.as_str()),
                PIPE_ACCESS_OUTBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                65536,
                65536,
                0,
                None,
            )
        };
        if in_h == INVALID_HANDLE_VALUE {
            return Err("CreateNamedPipeW(in) failed".to_string());
        }

        let out_h = unsafe {
            CreateNamedPipeW(
                &HSTRING::from(out_pipe_name.as_str()),
                PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                65536,
                65536,
                0,
                None,
            )
        };
        if out_h == INVALID_HANDLE_VALUE {
            unsafe {
                let _ = CloseHandle(in_h);
            }
            return Err("CreateNamedPipeW(out) failed".to_string());
        }

        let exe_path = std::env::current_exe()
            .map_err(|e| format!("current_exe failed: {e}"))?;
        let args = format!(
            "--elevated-pty-helper --pipe \"{}\" --preset \"{}\" --dir \"{}\" --cols {} --rows {}",
            pipe_base,
            preset.replace('"', ""),
            work_dir.to_string_lossy().replace('"', ""),
            cols.max(10),
            rows.max(4)
        );

        let exe_h = HSTRING::from(exe_path.as_os_str());
        let args_h = HSTRING::from(args);
        let dir_h = HSTRING::from(work_dir.as_os_str());
        let res = unsafe {
            ShellExecuteW(
                None,
                w!("runas"),
                &exe_h,
                &args_h,
                &dir_h,
                SW_HIDE,
            )
        };
        let code = res.0 as isize;
        if code <= 32 {
            unsafe {
                let _ = CloseHandle(in_h);
                let _ = CloseHandle(out_h);
            }
            if code == 5 || code == 1223 {
                return Err("管理者権限での起動がキャンセルされました".to_string());
            }
            return Err(format!("管理者起動に失敗しました (code {code})"));
        }

        // Wait for the elevated helper to connect to both pipes.
        let mut ok_in = unsafe { ConnectNamedPipe(in_h, None) }.is_ok();
        if !ok_in && unsafe { windows::Win32::Foundation::GetLastError() } == ERROR_PIPE_CONNECTED {
            ok_in = true;
        }
        let mut ok_out = if ok_in {
            unsafe { ConnectNamedPipe(out_h, None) }.is_ok()
        } else {
            false
        };
        if !ok_out && ok_in && unsafe { windows::Win32::Foundation::GetLastError() } == ERROR_PIPE_CONNECTED {
            ok_out = true;
        }
        if !ok_in || !ok_out {
            unsafe {
                let _ = CloseHandle(in_h);
                let _ = CloseHandle(out_h);
            }
            return Err("管理者ターミナルヘルパーとの接続に失敗しました".to_string());
        }

        let ctrl_writer = unsafe { File::from_raw_handle(in_h.0 as *mut _) };
        let mut reader = unsafe { File::from_raw_handle(out_h.0 as *mut _) };
        let history = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let lbl = if label.trim().is_empty() {
            format!("Admin {preset}")
        } else {
            label.trim().to_string()
        };

        let session = Arc::new(PtySession {
            _id: id.to_string(),
            _label: lbl.clone(),
            backend: PtyBackend::ElevatedBridge {
                ctrl_writer: Mutex::new(ctrl_writer),
            },
            history: Arc::clone(&history),
            closed: Arc::clone(&closed),
        });
        self.sessions
            .lock()
            .expect("pty sessions mutex")
            .insert(id.to_string(), session);

        let id_bg = id.to_string();
        let sessions_bg = Arc::clone(&self.sessions);
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
            closed_bg.store(true, Ordering::Release);
            sessions_bg.lock().expect("pty sessions mutex").remove(&id_bg);
            if let Some(tx) = sender {
                tx(RpcEvent {
                    topic: "pty:exit".to_string(),
                    payload: json!({ "id": id_bg, "code": 0, "error": "" }),
                });
            }
        });

        Ok(json!({
            "id": id,
            "preset": format!("admin:{preset}"),
            "dir": work_dir.to_string_lossy(),
            "label": lbl
        }))
    }
}

fn spawn_conpty_process(
    preset: &str,
    work_dir: &Path,
    cols: u16,
    rows: u16,
) -> Result<(HPCON, HANDLE, File, File), String> {
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

    let size = COORD {
        X: cols.max(10) as i16,
        Y: rows.max(4) as i16,
    };
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
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
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
    let reader = unsafe { File::from_raw_handle(out_read.0 as *mut _) };
    Ok((hpcon, pi.hProcess, writer, reader))
}

/// Entrypoint for the hidden elevated ConPTY helper (`syki.exe --elevated-pty-helper ...`).
pub fn run_elevated_pty_helper(args: &[String]) -> i32 {
    let mut pipe_base = String::new();
    let mut preset = "shell".to_string();
    let mut dir = String::new();
    let mut cols: u16 = 100;
    let mut rows: u16 = 30;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pipe" if i + 1 < args.len() => {
                pipe_base = args[i + 1].clone();
                i += 2;
            }
            "--preset" if i + 1 < args.len() => {
                preset = args[i + 1].clone();
                i += 2;
            }
            "--dir" if i + 1 < args.len() => {
                dir = args[i + 1].clone();
                i += 2;
            }
            "--cols" if i + 1 < args.len() => {
                cols = args[i + 1].parse().unwrap_or(100);
                i += 2;
            }
            "--rows" if i + 1 < args.len() => {
                rows = args[i + 1].parse().unwrap_or(30);
                i += 2;
            }
            _ => i += 1,
        }
    }
    if pipe_base.is_empty() {
        return 1;
    }
    run_elevated_pty_helper_inner(&pipe_base, &preset, &dir, cols, rows)
}

fn run_elevated_pty_helper_inner(
    pipe_base: &str,
    preset: &str,
    dir: &str,
    cols: u16,
    rows: u16,
) -> i32 {
    let Ok(mut ctrl_reader) = OpenOptions::new().read(true).open(format!("{pipe_base}-in")) else {
        return 2;
    };
    let Ok(mut data_writer) = OpenOptions::new().write(true).open(format!("{pipe_base}-out")) else {
        return 3;
    };

    let work_dir = resolve_work_dir(dir);
    let Ok((hpcon, proc_handle, mut pty_writer, mut pty_reader)) =
        spawn_conpty_process(preset, &work_dir, cols, rows)
    else {
        return 4;
    };

    let pump = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if data_writer.write_all(&buf[..n]).is_err() || data_writer.flush().is_err() {
                        break;
                    }
                }
            }
        }
    });

    while let Ok(Some((tag, payload))) = read_ctrl_frame(&mut ctrl_reader) {
        match tag {
            CTRL_TAG_STDIN => {
                if pty_writer.write_all(&payload).is_err() || pty_writer.flush().is_err() {
                    break;
                }
            }
            CTRL_TAG_RESIZE if payload.len() >= 4 => {
                let c = u16::from_le_bytes([payload[0], payload[1]]).max(10) as i16;
                let r = u16::from_le_bytes([payload[2], payload[3]]).max(4) as i16;
                let _ = unsafe { ResizePseudoConsole(hpcon, COORD { X: c, Y: r }) };
            }
            CTRL_TAG_CLOSE => break,
            _ => {}
        }
    }

    unsafe {
        let _ = TerminateProcess(proc_handle, 1);
        ClosePseudoConsole(hpcon);
        let _ = CloseHandle(proc_handle);
    }
    let _ = pump.join();
    0
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

fn resolve_ps_exe() -> &'static str {
    static PS_EXE: OnceLock<&'static str> = OnceLock::new();
    *PS_EXE.get_or_init(|| {
        if let Ok(pf) = std::env::var("ProgramFiles") {
            let pwsh = Path::new(&pf).join(concat!("Power", "Shell")).join("7").join("pwsh.exe");
            if pwsh.is_file() {
                return "pwsh.exe";
            }
        }
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in std::env::split_paths(&path_var) {
                if dir.join("pwsh.exe").is_file() {
                    return "pwsh.exe";
                }
            }
        }
        concat!("power", "shell.exe")
    })
}

fn command_for_preset(preset: &str) -> String {
    match preset.trim().to_ascii_lowercase().as_str() {
        "cc" | "claude" | "claude-code" => "cmd.exe /k claude".to_string(),
        "codex" => "cmd.exe /k codex".to_string(),
        "agy" | "antigravity" => "cmd.exe /k agy".to_string(),
        "gemini" => "cmd.exe /k gemini".to_string(),
        "cmd" => "cmd.exe".to_string(),
        _ => format!("{} -NoLogo", resolve_ps_exe()),
    }
}

pub fn available_presets() -> Value {
    json!([
        { "id": "cc", "label": "Claude Code", "short": "cc", "available": true },
        { "id": "codex", "label": "Codex", "short": "codex", "available": true },
        { "id": "agy", "label": "Agy", "short": "agy", "available": true },
        { "id": "shell", "label": "PS", "short": "ps", "available": true },
        { "id": "cmd", "label": "CMD", "short": "cmd", "available": true }
    ])
}

/// Launches an elevated (Administrator) external terminal window via Win32 `ShellExecuteW("runas")`.
pub fn launch_elevated(preset: &str, dir: &str) -> Result<Value, String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let work_dir = resolve_work_dir(dir);
    let work_dir_str = work_dir.to_string_lossy().into_owned();
    let norm = preset.trim().to_ascii_lowercase();
    let (exe, args) = match norm.as_str() {
        "cmd" => (
            "cmd.exe",
            format!("/k cd /d \"{}\"", work_dir_str),
        ),
        "cc" | "claude" | "claude-code" => (
            "cmd.exe",
            format!("/k cd /d \"{}\" && claude", work_dir_str),
        ),
        "codex" => (
            "cmd.exe",
            format!("/k cd /d \"{}\" && codex", work_dir_str),
        ),
        "agy" | "antigravity" => (
            "cmd.exe",
            format!("/k cd /d \"{}\" && agy", work_dir_str),
        ),
        "gemini" => (
            "cmd.exe",
            format!("/k cd /d \"{}\" && gemini", work_dir_str),
        ),
        _ => {
            let ps = resolve_ps_exe();
            let escaped_dir = work_dir_str.replace('\'', "''");
            (
                ps,
                format!("-NoLogo -NoExit -Command \"Set-Location -LiteralPath '{}'\"", escaped_dir),
            )
        }
    };

    let exe_h = HSTRING::from(exe);
    let args_h = HSTRING::from(args);
    let dir_h = HSTRING::from(work_dir.as_os_str());
    let res = unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            &exe_h,
            &args_h,
            &dir_h,
            SW_SHOWNORMAL,
        )
    };
    let code = res.0 as isize;
    if code <= 32 {
        if code == 5 || code == 1223 {
            return Err("管理者権限での起動がキャンセルされました".to_string());
        }
        return Err(format!("管理者起動に失敗しました (code {code})"));
    }
    Ok(json!({ "ok": true, "exe": exe, "dir": work_dir_str }))
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

    #[test]
    fn command_for_preset_maps_agent_presets() {
        assert_eq!(command_for_preset("agy"), "cmd.exe /k agy");
        assert_eq!(command_for_preset("cc"), "cmd.exe /k claude");
        assert_eq!(command_for_preset("claude-code"), "cmd.exe /k claude");
        assert_eq!(command_for_preset("codex"), "cmd.exe /k codex");
        assert_eq!(command_for_preset("gemini"), "cmd.exe /k gemini");
        assert_eq!(command_for_preset("cmd"), "cmd.exe");
        assert!(command_for_preset("shell").ends_with("-NoLogo"));
        assert!(command_for_preset("ps").ends_with("-NoLogo"));
    }

    #[test]
    fn elevated_bridge_streams_conpty_output_and_resize_over_named_pipes() {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let pipe_base = format!(r"\\.\pipe\syki-test-elev-{nanos}");
        let in_h = unsafe {
            CreateNamedPipeW(
                &HSTRING::from(format!("{pipe_base}-in")),
                PIPE_ACCESS_OUTBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                65536,
                65536,
                0,
                None,
            )
        };
        assert_ne!(in_h, INVALID_HANDLE_VALUE);
        let out_h = unsafe {
            CreateNamedPipeW(
                &HSTRING::from(format!("{pipe_base}-out")),
                PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                65536,
                65536,
                0,
                None,
            )
        };
        assert_ne!(out_h, INVALID_HANDLE_VALUE);

        let base_clone = pipe_base.clone();
        let helper = std::thread::spawn(move || {
            run_elevated_pty_helper_inner(&base_clone, "cmd", ".", 80, 24)
        });

        unsafe {
            let _ = ConnectNamedPipe(in_h, None);
            let _ = ConnectNamedPipe(out_h, None);
        }

        let mut ctrl_w = unsafe { File::from_raw_handle(in_h.0 as *mut _) };
        let mut data_r = unsafe { File::from_raw_handle(out_h.0 as *mut _) };

        std::thread::sleep(Duration::from_millis(400));
        write_ctrl_frame(&mut ctrl_w, CTRL_TAG_STDIN, b"echo SYKI_ELEV_OK\r\n").unwrap();
        write_ctrl_frame(&mut ctrl_w, CTRL_TAG_RESIZE, &[90, 0, 30, 0]).unwrap();

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut acc = String::new();
            while let Ok(n) = data_r.read(&mut buf) {
                if n == 0 {
                    break;
                }
                acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                if acc.contains("SYKI_ELEV_OK") {
                    let _ = tx.send(acc);
                    break;
                }
            }
        });

        let got = rx.recv_timeout(Duration::from_secs(6)).expect("SYKI_ELEV_OK from bridge");
        assert!(got.contains("SYKI_ELEV_OK"));
        let _ = write_ctrl_frame(&mut ctrl_w, CTRL_TAG_CLOSE, &[]);
        drop(ctrl_w);
        let _ = reader_thread.join();
        assert_eq!(helper.join().unwrap(), 0);
    }
}
