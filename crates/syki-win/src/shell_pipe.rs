//! Shell Pipe (`Ctrl+E`) execution via `cmd.exe /d /c` with `CREATE_NO_WINDOW`,
//! ANSI stripping, 10 MiB output cap, timeout, and cancellation (#06).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;
use syki_core::rpc::RpcEvent;

use crate::rpc_dispatch::EventSender;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_CLI_OUTPUT_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    pub output: String,
    pub error: String,
    pub exit_code: i32,
}

/// Strips ANSI CSI and OSC escape sequences from command output.
pub fn strip_ansi(input: &str) -> String {
    if !input.contains('\x1b') {
        return input.to_string();
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // CSI sequence: ESC [ ... final byte 0x40..=0x7E
                i += 2;
                while i < bytes.len() && !(0x40..=0x7E).contains(&bytes[i]) {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
                continue;
            } else if i + 1 < bytes.len() && bytes[i + 1] == b']' {
                // OSC sequence: ESC ] ... (BEL or ESC \)
                i += 2;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Rejects empty or obviously catastrophic shell commands before spawning `cmd.exe`.
pub fn validate_cli_command(cmd: &str) -> Result<(), String> {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return Err("command is empty".to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    let blocked = [
        "format ",
        "diskpart",
        "bcdedit",
        "shutdown ",
        "rm -rf /",
        "del /s /q c:\\",
        "rd /s /q c:\\",
    ];
    for pat in blocked {
        if lower.contains(pat) {
            return Err(format!("blocked dangerous command pattern: {pat}"));
        }
    }
    Ok(())
}

fn read_capped<R: Read>(mut reader: R, cap: usize) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if buf.len() < cap {
                    let take = n.min(cap - buf.len());
                    buf.extend_from_slice(&chunk[..take]);
                    if take < n {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
        }
    }
    let mut text = strip_ansi(&String::from_utf8_lossy(&buf));
    if truncated {
        text.push_str("\n...[output truncated at 10 MiB]");
    }
    text
}

/// Runs `cmd_str` via `cmd.exe /d /c` with `input` piped to stdin and optional cancellation flag.
pub fn run_command_filter(
    cmd_str: &str,
    input: &str,
    cancel_flag: Option<Arc<AtomicBool>>,
) -> CommandResult {
    let trimmed = cmd_str.trim();
    if let Err(reason) = validate_cli_command(trimmed) {
        return CommandResult {
            output: String::new(),
            error: reason,
            exit_code: 126,
        };
    }

    let mut child = match Command::new("cmd.exe")
        .args(["/d", "/c", trimmed])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return CommandResult {
                output: String::new(),
                error: format!("failed to spawn cmd.exe: {e}"),
                exit_code: 1,
            };
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let input_bytes = input.as_bytes().to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input_bytes);
        });
    }

    let stdout_handle = child.stdout.take().map(|out| {
        std::thread::spawn(move || read_capped(out, MAX_CLI_OUTPUT_BYTES))
    });
    let stderr_handle = child.stderr.take().map(|err| {
        std::thread::spawn(move || read_capped(err, MAX_CLI_OUTPUT_BYTES))
    });

    let start = Instant::now();
    let mut timed_out = false;
    let mut cancelled = false;
    let exit_code = loop {
        if let Some(flag) = cancel_flag.as_ref() {
            if flag.load(Ordering::Acquire) {
                cancelled = true;
                let _ = child.kill();
                let _ = child.wait();
                break 130;
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(1),
            Ok(None) => {
                if start.elapsed() >= DEFAULT_TIMEOUT {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break 124;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(_) => break 1,
        }
    };

    let output = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let mut error = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    if timed_out {
        if !error.is_empty() {
            error.push('\n');
        }
        error.push_str("command timed out after 30s");
    } else if cancelled {
        if !error.is_empty() {
            error.push('\n');
        }
        error.push_str("command cancelled");
    }

    CommandResult {
        output,
        error,
        exit_code,
    }
}

/// Tracks active async `Ctrl+E` command filter runs so `cancelCommandFilter` can abort them.
#[derive(Default)]
pub struct ShellPipeManager {
    cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl ShellPipeManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn run_async(
        &self,
        req_id: String,
        cmd_str: String,
        input: String,
        sender: Option<EventSender>,
    ) {
        let flag = Arc::new(AtomicBool::new(false));
        self.cancels
            .lock()
            .expect("cancels mutex")
            .insert(req_id.clone(), Arc::clone(&flag));
        let cancels = Arc::clone(&self.cancels);

        std::thread::spawn(move || {
            let res = run_command_filter(&cmd_str, &input, Some(flag));
            cancels.lock().expect("cancels mutex").remove(&req_id);
            if let Some(tx) = sender {
                let err_str = if res.exit_code != 0 && !res.error.is_empty() {
                    res.error.clone()
                } else {
                    String::new()
                };
                tx(RpcEvent {
                    topic: "cli:result".to_string(),
                    payload: json!({
                        "reqId": req_id,
                        "result": res,
                        "error": err_str
                    }),
                });
            }
        });
    }

    pub fn cancel(&self, req_id: &str) -> bool {
        if let Some(flag) = self.cancels.lock().expect("cancels mutex").get(req_id) {
            flag.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_csi_and_osc_sequences() {
        assert_eq!(strip_ansi("\x1b[32mhello\x1b[0m"), "hello");
        assert_eq!(strip_ansi("a\x1b]0;title\x07b"), "ab");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn validate_cli_command_blocks_empty_and_dangerous_patterns() {
        assert!(validate_cli_command("   ").is_err());
        assert!(validate_cli_command("format C:").is_err());
        assert!(validate_cli_command("findstr /n foo").is_ok());
    }

    #[test]
    fn run_command_filter_pipes_stdin_through_cmd() {
        let res = run_command_filter("findstr /i world", "hello\nworld\nbye\n", None);
        assert_eq!(res.exit_code, 0);
        assert!(res.output.trim().contains("world"));
    }

    #[test]
    fn shell_pipe_manager_async_emits_cli_result() {
        let mgr = ShellPipeManager::new();
        let (tx, rx) = std::sync::mpsc::channel::<RpcEvent>();
        mgr.run_async(
            "cli-1".to_string(),
            "echo ok".to_string(),
            String::new(),
            Some(Arc::new(move |ev| {
                let _ = tx.send(ev);
            })),
        );
        let ev = rx.recv_timeout(Duration::from_secs(3)).expect("cli:result");
        assert_eq!(ev.topic, "cli:result");
        assert_eq!(ev.payload["reqId"], "cli-1");
        assert_eq!(ev.payload["result"]["exitCode"], 0);
        assert_eq!(ev.payload["result"]["output"].as_str().unwrap().trim(), "ok");
    }
}
