//! syki-cli: the lean command-line front-end for Syki Lite (#01, #06).
//!
//! Parsing is hand-rolled and the binary pulls in no argument-parsing crate,
//! so a cold start stays well under the 5 ms budget. Only the subcommands
//! listed in [`KEPT_SUBCOMMANDS`] are recognised; everything that was pruned
//! during the 2.0 slim-down is reported as unknown.

use std::io::{IsTerminal, Read};
use std::process::exit;

use serde_json::{json, Value};
use syki_core::ipc::{
    is_pipe_available, load_ipc_session, make_auth_envelope, pipe_name_for, send_ipc_request,
    session_file_path,
};
use syki_core::rpc::RuntimeEnv;
use syki_core::storage::{ConfigStore, JsonConfigStore, ScrapStore};
use syki_core::APP_VERSION;

const MAX_STDIN_PIPE_BYTES: usize = 10 * 1024 * 1024;

/// The only subcommands retained in the slim 2.0 command surface.
const KEPT_SUBCOMMANDS: &[&str] = &[
    "buffer", "tab", "ui", "pty", "ocr", "speech", "info", "scrap", "config",
];

/// Every pruned subcommand, kept for the help text and for regression tests.
const PRUNED_SUBCOMMANDS: &[&str] = &["jev", "slot", "lesson", "pack", "drop", "git"];

/// Outcome of parsing the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    /// Print the package version and exit successfully.
    Version,
    /// Print the top-level usage and exit successfully.
    Help,
    /// Print the usage of one kept subcommand and exit successfully.
    SubcommandHelp(&'static str),
    /// Print the instance report as JSON and exit successfully.
    Info,
    /// Dispatch a kept subcommand with trailing arguments.
    Dispatch(&'static str, Vec<String>),
    /// Kept subcommand invoked with no sub-arguments yet.
    Unimplemented(&'static str),
    /// Token that is not part of the kept command surface.
    Unknown(String),
}

/// Maps raw argv tokens (excluding the program name) to a [`CliAction`].
pub fn parse_cli_action<I, S>(args: I) -> CliAction
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let argv: Vec<String> = args.into_iter().map(Into::into).collect();
    let Some(first) = argv.first().map(String::as_str) else {
        return CliAction::Help;
    };

    if matches!(first, "--version" | "-V") {
        return CliAction::Version;
    }
    if matches!(first, "--help" | "-h") {
        return CliAction::Help;
    }

    if let Some(kept) = KEPT_SUBCOMMANDS.iter().copied().find(|name| *name == first) {
        if second_requests_help(&argv) {
            return CliAction::SubcommandHelp(kept);
        }
        if kept == "info" {
            return CliAction::Info;
        }
        if argv.len() > 1 {
            return CliAction::Dispatch(kept, argv[1..].to_vec());
        }
        return CliAction::Unimplemented(kept);
    }

    // Pruned subcommands (even with `--help`) and any other token.
    CliAction::Unknown(first.to_string())
}

fn second_requests_help(argv: &[String]) -> bool {
    argv.get(1).map(String::as_str) == Some("--help")
        || argv.get(1).map(String::as_str) == Some("-h")
}

fn print_top_level_help() {
    println!("syki-cli {} - lean Syki Lite front-end", APP_VERSION);
    println!();
    println!("USAGE:");
    println!("    syki-cli [COMMAND] [ARGUMENTS...]");
    println!("    <stdin> | syki-cli");
    println!();
    println!("FLAGS:");
    println!("    -V, --version   Print the package version");
    println!("    -h, --help      Print this usage");
    println!();
    println!("COMMANDS:");
    for name in KEPT_SUBCOMMANDS {
        println!("    {:<8} {}", name, subcommand_summary(name));
    }
    println!();
    println!("PRUNED (no longer available): {}", PRUNED_SUBCOMMANDS.join(", "));
}

fn subcommand_summary(name: &str) -> &'static str {
    match name {
        "buffer" => "Inspect and drive text buffers",
        "tab" => "Manage workspace tabs",
        "ui" => "Control the shell window",
        "pty" => "Run and observe pseudo-terminals",
        "ocr" => "Extract text from images",
        "speech" => "Speak and transcribe audio",
        "info" => "Print the instance report as JSON",
        "scrap" => "Append to or inspect daily scraps",
        "config" => "Read or write configuration",
        _ => "General-purpose command",
    }
}

fn print_subcommand_help(name: &str) {
    println!("syki-cli {} {}", APP_VERSION, name);
    println!("    {}", subcommand_summary(name));
    println!();
    println!("USAGE:");
    println!("    syki-cli {} [SUBCOMMAND] [ARGUMENTS...]", name);
    println!();
    println!("FLAGS:");
    println!("    -h, --help   Print this usage");
}

fn print_info_report() {
    let env = RuntimeEnv::from_env();
    let pipe = pipe_name_for(&env);
    let ipc_active = is_pipe_available(&pipe);
    let report = json!({
        "version": APP_VERSION,
        "dataDir": env.data_dir.to_string_lossy(),
        "instanceId": env.instance_id,
        "pipeName": pipe,
        "ipcActive": ipc_active,
    });
    println!("{}", serde_json::to_string_pretty(&report).expect("report serialises"));
}

fn read_capped_stdin() -> Result<String, String> {
    let mut buf = Vec::new();
    let mut limited = std::io::stdin().take((MAX_STDIN_PIPE_BYTES + 1) as u64);
    limited
        .read_to_end(&mut buf)
        .map_err(|e| format!("failed to read stdin: {e}"))?;
    if buf.len() > MAX_STDIN_PIPE_BYTES {
        buf.truncate(MAX_STDIN_PIPE_BYTES);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn try_send_ipc(env: &RuntimeEnv, action: &str, method: &str, params: Value, content: String, path: String) -> Result<Value, String> {
    let pipe = pipe_name_for(env);
    if !is_pipe_available(&pipe) {
        return Err(format!("syki GUI instance is not running on {pipe}"));
    }
    let session = load_ipc_session(&session_file_path(env))
        .map_err(|e| format!("failed to read ipc-session.json: {e}"))?;
    let mut req = make_auth_envelope(&session, action, method, params);
    req.content = content;
    req.path = path;
    let reply = send_ipc_request(&session.pipe_name, &req)?;
    if reply.ok {
        Ok(reply.result.unwrap_or(Value::Null))
    } else {
        Err(reply.error.unwrap_or_else(|| "ipc error".to_string()))
    }
}

fn append_scrap_and_notify(env: &RuntimeEnv, text: &str) -> Result<String, String> {
    if let Ok(res) = try_send_ipc(
        env,
        "pipe",
        "scrap.append",
        json!({ "content": text }),
        text.to_string(),
        String::new(),
    ) {
        if let Some(p) = res.get("path").and_then(|v| v.as_str()) {
            return Ok(p.to_string());
        }
    }
    let cfg = JsonConfigStore::new(env.data_dir.join("config.json"));
    let scrap_dir = cfg.scrap_dir();
    let path = ScrapStore::append_formatted(&scrap_dir, text, "").map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

fn dispatch_subcommand(cmd: &str, args: &[String]) -> Result<(), String> {
    let env = RuntimeEnv::from_env();
    match cmd {
        "config" => {
            let store = JsonConfigStore::new(env.data_dir.join("config.json"));
            match args.first().map(String::as_str) {
                Some("get") => {
                    let raw = store.load_raw().map_err(|e| e.to_string())?;
                    let val: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
                    if let Some(key) = args.get(1) {
                        let out = val.get(key).cloned().unwrap_or(Value::Null);
                        println!("{}", serde_json::to_string_pretty(&out).unwrap());
                    } else {
                        println!("{}", serde_json::to_string_pretty(&val).unwrap());
                    }
                    Ok(())
                }
                Some("set") => {
                    let key = args.get(1).ok_or("usage: syki-cli config set <key> <value>")?;
                    let raw_val = args.get(2).ok_or("usage: syki-cli config set <key> <value>")?;
                    let raw = store.load_raw().map_err(|e| e.to_string())?;
                    let mut obj: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
                    let parsed_val = serde_json::from_str::<Value>(raw_val)
                        .unwrap_or_else(|_| Value::String(raw_val.clone()));
                    if let Some(map) = obj.as_object_mut() {
                        map.insert(key.clone(), parsed_val);
                    }
                    let next = serde_json::to_string_pretty(&obj).map_err(|e| e.to_string())?;
                    store.save_raw(&next).map_err(|e| e.to_string())?;
                    println!("{}", json!({ "ok": true, "key": key }));
                    Ok(())
                }
                _ => Err("usage: syki-cli config <get [key] | set <key> <value>>".to_string()),
            }
        }
        "scrap" => match args.first().map(String::as_str) {
            Some("dir") => {
                let cfg = JsonConfigStore::new(env.data_dir.join("config.json"));
                let dir = cfg.scrap_dir();
                println!("{}", ScrapStore::resolve_scrap_dir(&dir).to_string_lossy());
                Ok(())
            }
            Some("append") => {
                let text = if args.get(1).map(String::as_str) == Some("-") || args.len() == 1 {
                    read_capped_stdin()?
                } else {
                    args[1..].join(" ")
                };
                let saved = append_scrap_and_notify(&env, &text)?;
                println!("{}", json!({ "ok": true, "path": saved }));
                Ok(())
            }
            _ => Err("usage: syki-cli scrap <dir | append <text|->>".to_string()),
        },
        "ui" => match args.first().map(String::as_str) {
            Some("activate") => {
                let res = try_send_ipc(&env, "activate", "ui.activate", json!({}), String::new(), String::new())?;
                println!("{res}");
                Ok(())
            }
            Some("new-tab") => {
                let res = try_send_ipc(&env, "new-tab", "ui.new_tab", json!({}), String::new(), String::new())?;
                println!("{res}");
                Ok(())
            }
            Some("open") => {
                let p = args.get(1).ok_or("usage: syki-cli ui open <path>")?;
                let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.into());
                let res = try_send_ipc(
                    &env,
                    "open",
                    "ui.open",
                    json!({ "path": abs.to_string_lossy() }),
                    String::new(),
                    abs.to_string_lossy().into_owned(),
                )?;
                println!("{res}");
                Ok(())
            }
            Some("state") => {
                let res = try_send_ipc(&env, "", "ui.state", json!({ "args": [] }), String::new(), String::new())?;
                println!("{res}");
                Ok(())
            }
            Some("set") => {
                let rest: Vec<String> = args.iter().skip(1).cloned().collect();
                let res = try_send_ipc(&env, "", "ui.set", json!({ "args": rest }), String::new(), String::new())?;
                println!("{res}");
                Ok(())
            }
            Some("panel") => {
                let rest: Vec<String> = args.iter().skip(1).cloned().collect();
                let res = try_send_ipc(&env, "", "ui.panel", json!({ "args": rest }), String::new(), String::new())?;
                println!("{res}");
                Ok(())
            }
            _ => Err("usage: syki-cli ui <activate | new-tab | open <path> | state | set <json> | panel <name>>".to_string()),
        },
        other => {
            let sub = args.first().map(String::as_str).unwrap_or("");
            let method = if sub.is_empty() {
                other.to_string()
            } else {
                format!("{other}.{sub}")
            };
            let rest: Vec<String> = args.iter().skip(1).cloned().collect();
            let res = try_send_ipc(&env, "", &method, json!({ "args": rest }), String::new(), String::new())?;
            println!("{res}");
            Ok(())
        }
    }
}

fn report_failure(message: &str) -> ! {
    eprint!("error: {}", message);
    eprintln!(" (run `syki-cli --help` for the command surface)");
    exit(2);
}

fn main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    if raw_args.is_empty() && !std::io::stdin().is_terminal() {
        let env = RuntimeEnv::from_env();
        match read_capped_stdin().and_then(|text| append_scrap_and_notify(&env, &text)) {
            Ok(path) => {
                println!("{}", json!({ "ok": true, "path": path }));
                exit(0);
            }
            Err(e) => report_failure(&e),
        }
    }

    let action = parse_cli_action(raw_args);
    match action {
        CliAction::Version => {
            println!("syki-cli {}", APP_VERSION);
            exit(0);
        }
        CliAction::Help => {
            print_top_level_help();
            exit(0);
        }
        CliAction::SubcommandHelp(name) => {
            print_subcommand_help(name);
            exit(0);
        }
        CliAction::Info => {
            print_info_report();
            exit(0);
        }
        CliAction::Dispatch(cmd, args) => {
            if let Err(err) = dispatch_subcommand(cmd, &args) {
                report_failure(&err);
            }
            exit(0);
        }
        CliAction::Unimplemented(name) => report_failure(&format!(
            "`{}` requires a subcommand or is not implemented yet",
            name
        )),
        CliAction::Unknown(token) => {
            report_failure(&format!("`{}` is not a Syki Lite command", token))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| (*t).to_string()).collect()
    }

    #[test]
    fn empty_arguments_show_top_level_help() {
        assert_eq!(parse_cli_action(argv(&[])), CliAction::Help);
    }

    #[test]
    fn version_and_help_flags_are_recognised() {
        assert_eq!(parse_cli_action(argv(&["--version"])), CliAction::Version);
        assert_eq!(parse_cli_action(argv(&["-V"])), CliAction::Version);
        assert_eq!(parse_cli_action(argv(&["--help"])), CliAction::Help);
        assert_eq!(parse_cli_action(argv(&["-h"])), CliAction::Help);
    }

    #[test]
    fn every_kept_subcommand_accepts_help() {
        for name in KEPT_SUBCOMMANDS {
            assert_eq!(
                parse_cli_action(argv(&[name, "--help"])),
                CliAction::SubcommandHelp(name),
                "`{}` should accept --help",
                name
            );
            assert_eq!(
                parse_cli_action(argv(&[name, "-h"])),
                CliAction::SubcommandHelp(name),
                "`{}` should accept -h",
                name
            );
        }
    }

    #[test]
    fn kept_subcommands_without_arguments_resolve_expected_actions() {
        assert_eq!(parse_cli_action(argv(&["info"])), CliAction::Info);
        assert_eq!(parse_cli_action(argv(&["info", "--json"])), CliAction::Info);
        for name in ["buffer", "tab", "ui", "pty", "ocr", "speech", "scrap", "config"] {
            assert_eq!(
                parse_cli_action(argv(&[name])),
                CliAction::Unimplemented(name),
                "`{}` should be reported as unimplemented",
                name
            );
        }
    }

    #[test]
    fn kept_subcommands_with_arguments_dispatch() {
        assert_eq!(
            parse_cli_action(argv(&["config", "get"])),
            CliAction::Dispatch("config", argv(&["get"]))
        );
        assert_eq!(
            parse_cli_action(argv(&["scrap", "append", "hello"])),
            CliAction::Dispatch("scrap", argv(&["append", "hello"]))
        );
        assert_eq!(
            parse_cli_action(argv(&["ui", "activate"])),
            CliAction::Dispatch("ui", argv(&["activate"]))
        );
    }

    #[test]
    fn pruned_subcommands_are_rejected_even_with_help() {
        for name in PRUNED_SUBCOMMANDS {
            let expected = CliAction::Unknown((*name).to_string());
            assert_eq!(parse_cli_action(argv(&[name])), expected.clone());
            assert_eq!(
                parse_cli_action(argv(&[name, "--help"])),
                expected,
                "`{}` must not resurrect its help text",
                name
            );
        }
    }

    #[test]
    fn unrelated_tokens_are_unknown() {
        assert_eq!(
            parse_cli_action(argv(&["launch"])),
            CliAction::Unknown("launch".to_string())
        );
        assert_eq!(
            parse_cli_action(argv(&["--bogus", "--help"])),
            CliAction::Unknown("--bogus".to_string())
        );
    }
}
