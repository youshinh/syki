//! syki-cli: the lean command-line front-end for syki (#01, #06).
//!
//! Parsing is hand-rolled and the binary pulls in no argument-parsing crate,
//! so a cold start stays well under the 5 ms budget. Only the subcommands
//! listed in [`KEPT_SUBCOMMANDS`] are recognised; everything that was pruned
//! during the 2.0 slim-down is reported as unknown.

use std::io::{IsTerminal, Read};
use std::process::exit;

use serde_json::{json, Value};
use syki_core::ipc::{
    build_plugin_rpc_envelope, is_pipe_available, load_ipc_session, make_auth_envelope,
    pipe_name_for, send_ipc_request, session_file_path, IpcEnvelope, METHOD_PLUGIN_LIST,
    METHOD_PLUGIN_RELOAD, METHOD_PLUGIN_RUN, METHOD_PLUGIN_STATUS,
};
use syki_core::rpc::RuntimeEnv;
use syki_core::storage::{ConfigStore, JsonConfigStore, ScrapStore};
use syki_core::APP_VERSION;

mod plugin_check;

const MAX_STDIN_PIPE_BYTES: usize = 10 * 1024 * 1024;

/// The only subcommands retained in the slim 2.0 command surface.
const KEPT_SUBCOMMANDS: &[&str] = &[
    "buffer", "tab", "ui", "pty", "ocr", "speech", "info", "scrap", "config", "plugin",
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
    println!("syki-cli {} - lean syki front-end", APP_VERSION);
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
        "plugin" => "Manage and run plugins",
        _ => "General-purpose command",
    }
}

fn subcommand_usage(name: &str) -> &'static str {
    match name {
        "buffer" => "    syki-cli buffer get [tabId]\n    syki-cli buffer set <text|-> [tabId]\n    syki-cli buffer append <text|-> [tabId]",
        "tab" => "    syki-cli tab list\n    syki-cli tab new <title> [content]\n    syki-cli tab open <path>\n    syki-cli tab switch <tabId>\n    syki-cli tab close [tabId] [--force]\n    syki-cli tab pdf <outputPath>",
        "ui" => "    syki-cli ui activate\n    syki-cli ui new-tab\n    syki-cli ui open <path>\n    syki-cli ui state\n    syki-cli ui set <json>\n    syki-cli ui panel <name>",
        "scrap" => "    syki-cli scrap dir\n    syki-cli scrap append <text|->\n    syki-cli scrap search <query>",
        "config" => "    syki-cli config get [key]\n    syki-cli config set <key> <value>",
        "plugin" => "    syki-cli plugin check <path>\n    syki-cli plugin list\n    syki-cli plugin status [plugin_id]\n    syki-cli plugin reload <plugin_id>\n    syki-cli plugin run <command_id> [--args <json>] [--show]",
        "pty" => "    syki-cli pty list\n    syki-cli pty presets",
        "ocr" => "    syki-cli ocr run <image>\n    syki-cli ocr <image>",
        "speech" => "    syki-cli speech status",
        "info" => "    syki-cli info",
        _ => "    syki-cli <SUBCOMMAND> [ARGUMENTS...]",
    }
}

fn print_subcommand_help(name: &str) {
    println!("syki-cli {} {}", APP_VERSION, name);
    println!("    {}", subcommand_summary(name));
    println!();
    println!("USAGE:");
    println!("{}", subcommand_usage(name));
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

struct PipeInput {
    text: String,
    truncated: bool,
}

fn decode_pipe_bytes(raw: &[u8], limit_bytes: usize) -> Result<PipeInput, String> {
    let truncated = raw.len() > limit_bytes;
    let slice = if truncated { &raw[..limit_bytes] } else { raw };

    let decoded = if !truncated {
        syki_core::storage::NoteFileStore::decode_bytes_strict(slice).map(|(s, _)| s)
    } else {
        // When truncated at an arbitrary byte cap, a multi-byte UTF-8 sequence may be split
        // and accidentally look like valid Shift_JIS. Prefer UTF-8 with 0..=3 byte trim first.
        let mut found = None;
        for trim in 0..=3 {
            if slice.len() > trim {
                let cand = &slice[..slice.len() - trim];
                if let Ok(s) = std::str::from_utf8(cand) {
                    found = Some(s.to_string());
                    break;
                }
            }
        }
        if found.is_none() {
            for trim in 0..=3 {
                if slice.len() > trim {
                    if let Ok((s, _)) =
                        syki_core::storage::NoteFileStore::decode_bytes_strict(&slice[..slice.len() - trim])
                    {
                        found = Some(s);
                        break;
                    }
                }
            }
        }
        found.ok_or_else(|| "failed to decode truncated input".to_string())
    }?;

    let mut text = decoded;
    if truncated {
        let mb = (limit_bytes / (1024 * 1024)).max(1);
        eprintln!("warning: stdin exceeded maxPipeSizeMB ({mb} MB) limit and was truncated");
        text.push_str(&format!(
            "\n\n[truncated: input exceeded maxPipeSizeMB ({mb} MB) limit]"
        ));
    }

    Ok(PipeInput { text, truncated })
}

fn read_capped_stdin(limit_bytes: usize) -> Result<PipeInput, String> {
    let mut buf = Vec::new();
    let mut limited = std::io::stdin().take((limit_bytes + 1) as u64);
    limited
        .read_to_end(&mut buf)
        .map_err(|e| format!("failed to read stdin: {e}"))?;
    decode_pipe_bytes(&buf, limit_bytes)
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

fn build_plugin_cli_envelope(auth: &str, args: &[String]) -> Result<IpcEnvelope, String> {
    let usage = "usage: syki-cli plugin <list | status [plugin_id] | reload <plugin_id> | run <command_id> [--args <json>] [--show]>";
    match args.first().map(String::as_str) {
        Some("list") => {
            if args.len() > 1 {
                return Err("usage: syki-cli plugin list".to_string());
            }
            Ok(build_plugin_rpc_envelope(auth, METHOD_PLUGIN_LIST, json!({}), true))
        }
        Some("status") => match args.len() {
            1 => Ok(build_plugin_rpc_envelope(auth, METHOD_PLUGIN_STATUS, json!({}), true)),
            2 => Ok(build_plugin_rpc_envelope(
                auth,
                METHOD_PLUGIN_STATUS,
                json!({ "plugin_id": args[1] }),
                true,
            )),
            _ => Err("usage: syki-cli plugin status [plugin_id]".to_string()),
        },
        Some("reload") => {
            if args.len() != 2 {
                return Err("usage: syki-cli plugin reload <plugin_id>".to_string());
            }
            Ok(build_plugin_rpc_envelope(
                auth,
                METHOD_PLUGIN_RELOAD,
                json!({ "plugin_id": args[1] }),
                true,
            ))
        }
        Some("run") => {
            let Some(command_id) = args.get(1) else {
                return Err("usage: syki-cli plugin run <command_id> [--args <json>] [--show]".to_string());
            };
            if command_id.starts_with('-') {
                return Err("usage: syki-cli plugin run <command_id> [--args <json>] [--show]".to_string());
            }
            let mut parsed_args = json!({});
            let mut headless = true;
            let mut i = 2;
            while i < args.len() {
                let token = args[i].as_str();
                if token == "--show" {
                    headless = false;
                    i += 1;
                } else if token == "--args" {
                    let raw = args
                        .get(i + 1)
                        .ok_or_else(|| "missing JSON value for --args".to_string())?;
                    parsed_args = serde_json::from_str::<Value>(raw)
                        .map_err(|e| format!("invalid JSON for --args: {e}"))?;
                    i += 2;
                } else if let Some(raw) = token.strip_prefix("--args=") {
                    parsed_args = serde_json::from_str::<Value>(raw)
                        .map_err(|e| format!("invalid JSON for --args: {e}"))?;
                    i += 1;
                } else {
                    return Err(format!("unknown argument for `plugin run`: {token}"));
                }
            }
            Ok(build_plugin_rpc_envelope(
                auth,
                METHOD_PLUGIN_RUN,
                json!({ "command_id": command_id, "args": parsed_args }),
                headless,
            ))
        }
        _ => Err(usage.to_string()),
    }
}

fn try_send_plugin_ipc(env: &RuntimeEnv, args: &[String]) -> Result<Value, String> {
    // Validate CLI arguments and JSON payload before touching the Named Pipe.
    let mut req = build_plugin_cli_envelope("", args)?;
    let pipe = pipe_name_for(env);
    if !is_pipe_available(&pipe) {
        return Err(format!("syki GUI instance is not running on {pipe}"));
    }
    let session = load_ipc_session(&session_file_path(env))
        .map_err(|e| format!("failed to read ipc-session.json: {e}"))?;
    req.auth = session.token;
    let reply = send_ipc_request(&session.pipe_name, &req)?;
    if reply.ok {
        Ok(reply.result.unwrap_or(Value::Null))
    } else {
        Err(reply.error.unwrap_or_else(|| "ipc error".to_string()))
    }
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
                        let out = syki_core::storage::config::get_config_value(&val, key)?;
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
                    syki_core::storage::config::set_config_value(&mut obj, key, parsed_val)?;
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
                if args.len() == 1 && std::io::stdin().is_terminal() {
                    return Err("usage: syki-cli scrap append <text|->".to_string());
                }
                let (text, truncated) = if args.get(1).map(String::as_str) == Some("-") || args.len() == 1 {
                    let limit = JsonConfigStore::new(env.data_dir.join("config.json")).max_pipe_size_bytes();
                    let input = read_capped_stdin(limit)?;
                    (input.text, input.truncated)
                } else {
                    (args[1..].join(" "), false)
                };
                if text.trim().is_empty() {
                    println!("{}", json!({ "ok": true, "skipped": true, "reason": "empty input" }));
                    return Ok(());
                }
                let saved = append_scrap_and_notify(&env, &text)?;
                println!("{}", json!({ "ok": true, "path": saved, "truncated": truncated }));
                Ok(())
            }
            Some("search") => {
                let query = args.get(1).ok_or("usage: syki-cli scrap search <query>")?;
                let res = try_send_ipc(&env, "", "scrap.search", json!({ "args": [query] }), String::new(), String::new())?;
                println!("{}", serde_json::to_string_pretty(&res).unwrap_or_else(|_| res.to_string()));
                Ok(())
            }
            _ => Err("usage: syki-cli scrap <dir | append <text|-> | search <query>>".to_string()),
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
                let abs = syki_core::storage::normalize_fs_path(std::path::Path::new(p));
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
        "plugin" => {
            if args.first().map(String::as_str) == Some("check") {
                if args.len() != 2 {
                    return Err("usage: syki-cli plugin check <path>".to_string());
                }
                let report = plugin_check::check_plugin_file(std::path::Path::new(&args[1]));
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report.to_json())
                        .expect("check report serialises")
                );
                if !report.ok {
                    exit(1);
                }
                return Ok(());
            }
            let res = try_send_plugin_ipc(&env, args)?;
            println!("{res}");
            Ok(())
        }
        "buffer" => match args.first().map(String::as_str) {
            Some("get") => {
                let rest: Vec<String> = args.iter().skip(1).cloned().collect();
                let res = try_send_ipc(&env, "", "buffer.get", json!({ "args": rest }), String::new(), String::new())?;
                println!("{res}");
                Ok(())
            }
            Some("set") => {
                let mut rest: Vec<String> = args.iter().skip(1).cloned().collect();
                if rest.is_empty() {
                    return Err("usage: syki-cli buffer set <text|-> [tabId]".to_string());
                }
                if rest.first().map(String::as_str) == Some("-") {
                    let limit = JsonConfigStore::new(env.data_dir.join("config.json")).max_pipe_size_bytes();
                    let input = read_capped_stdin(limit)?;
                    if input.text.trim().is_empty() {
                        return Err("empty stdin input; buffer was not modified".to_string());
                    }
                    rest[0] = input.text;
                }
                let res = try_send_ipc(&env, "", "buffer.set", json!({ "args": rest }), String::new(), String::new())?;
                println!("{res}");
                Ok(())
            }
            Some("append") => {
                let mut rest: Vec<String> = args.iter().skip(1).cloned().collect();
                if rest.is_empty() {
                    return Err("usage: syki-cli buffer append <text|-> [tabId]".to_string());
                }
                if rest.first().map(String::as_str) == Some("-") {
                    let limit = JsonConfigStore::new(env.data_dir.join("config.json")).max_pipe_size_bytes();
                    let input = read_capped_stdin(limit)?;
                    if input.text.trim().is_empty() {
                        return Err("empty stdin input; buffer was not modified".to_string());
                    }
                    rest[0] = input.text;
                }
                let res = try_send_ipc(&env, "", "buffer.append", json!({ "args": rest }), String::new(), String::new())?;
                println!("{res}");
                Ok(())
            }
            _ => Err("usage: syki-cli buffer <get [tabId] | set <text> [tabId] | append <text|-> [tabId]>".to_string()),
        },
        "ocr" => {
            let img_arg = if args.first().map(String::as_str) == Some("run") {
                args.get(1)
            } else {
                args.first()
            };
            let p = img_arg.ok_or("usage: syki-cli ocr run <image>")?;
            let norm = syki_core::storage::normalize_fs_path(std::path::Path::new(p));
            let res = try_send_ipc(
                &env,
                "",
                "ocr.run",
                json!({ "args": [norm.to_string_lossy()] }),
                String::new(),
                String::new(),
            )?;
            println!("{res}");
            Ok(())
        }
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
        let limit = JsonConfigStore::new(env.data_dir.join("config.json")).max_pipe_size_bytes();
        match read_capped_stdin(limit).and_then(|input| {
            if input.text.trim().is_empty() {
                return Ok((String::new(), false, true));
            }
            append_scrap_and_notify(&env, &input.text).map(|path| (path, input.truncated, false))
        }) {
            Ok((_path, _truncated, true)) => {
                println!("{}", json!({ "ok": true, "skipped": true, "reason": "empty input" }));
                exit(0);
            }
            Ok((path, truncated, false)) => {
                println!("{}", json!({ "ok": true, "path": path, "truncated": truncated }));
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
            report_failure(&format!("`{}` is not a syki command", token))
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
        for name in ["buffer", "tab", "ui", "pty", "ocr", "speech", "scrap", "config", "plugin"] {
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
        assert_eq!(
            parse_cli_action(argv(&["plugin", "list"])),
            CliAction::Dispatch("plugin", argv(&["list"]))
        );
    }

    #[test]
    fn plugin_cli_builds_expected_envelopes() {
        let list_env = build_plugin_cli_envelope("tok", &argv(&["list"])).expect("plugin list");
        assert_eq!(list_env.auth, "tok");
        assert_eq!(list_env.method, METHOD_PLUGIN_LIST);
        assert_eq!(list_env.params, json!({}));
        assert!(list_env.headless);

        let status_all = build_plugin_cli_envelope("tok", &argv(&["status"])).expect("plugin status");
        assert_eq!(status_all.method, METHOD_PLUGIN_STATUS);
        assert_eq!(status_all.params, json!({}));
        assert!(status_all.headless);

        let status_one =
            build_plugin_cli_envelope("tok", &argv(&["status", "my.plugin"])).expect("plugin status id");
        assert_eq!(status_one.method, METHOD_PLUGIN_STATUS);
        assert_eq!(status_one.params, json!({ "plugin_id": "my.plugin" }));
        assert!(status_one.headless);

        let reload_env =
            build_plugin_cli_envelope("tok", &argv(&["reload", "my.plugin"])).expect("plugin reload");
        assert_eq!(reload_env.method, METHOD_PLUGIN_RELOAD);
        assert_eq!(reload_env.params, json!({ "plugin_id": "my.plugin" }));
        assert!(reload_env.headless);

        let run_default =
            build_plugin_cli_envelope("tok", &argv(&["run", "demo.echo"])).expect("plugin run default");
        assert_eq!(run_default.method, METHOD_PLUGIN_RUN);
        assert_eq!(run_default.params, json!({ "command_id": "demo.echo", "args": {} }));
        assert!(run_default.headless);

        let run_with_args_and_show = build_plugin_cli_envelope(
            "tok",
            &argv(&["run", "demo.echo", "--args", r#"{"text":"hi"}"#, "--show"]),
        )
        .expect("plugin run --args --show");
        assert_eq!(run_with_args_and_show.method, METHOD_PLUGIN_RUN);
        assert_eq!(
            run_with_args_and_show.params,
            json!({ "command_id": "demo.echo", "args": { "text": "hi" } })
        );
        assert!(!run_with_args_and_show.headless);

        let run_eq_args =
            build_plugin_cli_envelope("tok", &argv(&["run", "demo.echo", "--args=[1,2]"]))
                .expect("plugin run --args=");
        assert_eq!(
            run_eq_args.params,
            json!({ "command_id": "demo.echo", "args": [1, 2] })
        );
        assert!(run_eq_args.headless);
    }

    #[test]
    fn plugin_cli_rejects_invalid_json_and_missing_args() {
        let bad_json =
            build_plugin_cli_envelope("tok", &argv(&["run", "demo.echo", "--args", "{not-json"]))
                .expect_err("invalid json should error");
        assert!(bad_json.contains("invalid JSON for --args"));

        let missing_json =
            build_plugin_cli_envelope("tok", &argv(&["run", "demo.echo", "--args"]))
                .expect_err("missing json value should error");
        assert!(missing_json.contains("missing JSON value"));

        assert!(build_plugin_cli_envelope("tok", &argv(&["reload"])).is_err());
        assert!(build_plugin_cli_envelope("tok", &argv(&["run"])).is_err());
        assert!(build_plugin_cli_envelope("tok", &argv(&["unknown"])).is_err());
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

    #[test]
    fn decode_pipe_bytes_handles_bom_cp932_utf16_and_truncation() {
        // UTF-8 BOM stripped
        let utf8_bom = [b"\xEF\xBB\xBF".as_slice(), "hello".as_bytes()].concat();
        let res = decode_pipe_bytes(&utf8_bom, 1024).unwrap();
        assert_eq!(res.text, "hello");
        assert!(!res.truncated);

        // Shift_JIS (CP932)
        let sjis = syki_core::storage::NoteFileStore::encode_str("日本語テスト", "Shift_JIS");
        let res = decode_pipe_bytes(&sjis, 1024).unwrap();
        assert_eq!(res.text, "日本語テスト");
        assert!(!res.truncated);

        // Invalid binary rejected
        assert!(decode_pipe_bytes(&[0x80, 0xFE, 0xFF, 0xFF], 1024).is_err());

        // Oversized multi-byte UTF-8 truncated cleanly without splitting char
        let s = "あ".repeat(10); // 30 bytes
        let res = decode_pipe_bytes(s.as_bytes(), 10).unwrap();
        assert!(res.truncated);
        assert!(res.text.starts_with("あああ"));
        assert!(res.text.contains("[truncated: input exceeded maxPipeSizeMB"));
    }
}
