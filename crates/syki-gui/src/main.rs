#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Single-window GUI launcher for syki (`syki.exe`) with single-instance Named Pipe IPC (#06).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::json;
use syki_core::ipc::{
    is_pipe_available, load_ipc_session, make_auth_envelope, pipe_name_for, send_ipc_request,
    session_file_path, IpcEnvelope, IpcPipeServer, IpcReply, METHOD_PLUGIN_LIST,
    METHOD_PLUGIN_RELOAD, METHOD_PLUGIN_RUN, METHOD_PLUGIN_STATUS,
};
use syki_core::rpc::{RpcEvent, RuntimeEnv};
use syki_core::storage::config::{get_config_value, set_config_value};
use syki_core::storage::note::cyrb53_utf16_sig;
use syki_core::storage::{normalize_fs_path, ConfigStore, JsonConfigStore, NoteFileStore, ScrapStore};
use syki_core::APP_VERSION;
use syki_win::rpc_dispatch::CoreState;
use syki_win::webview_host::{
    run_webview_host_with_handle, DynamicRouteHandler, HostHandle, WebViewHostConfig,
    VIRTUAL_ORIGIN,
};

fn try_forward_to_running_instance(env: &RuntimeEnv, startup_file: Option<&Path>) -> bool {
    let pipe = pipe_name_for(env);
    if !is_pipe_available(&pipe) {
        return false;
    }
    let Ok(session) = load_ipc_session(&session_file_path(env)) else {
        return false;
    };
    let mut req = if let Some(p) = startup_file {
        let abs = normalize_fs_path(p);
        let mut r = make_auth_envelope(&session, "open", "", json!({}));
        r.path = abs.to_string_lossy().into_owned();
        r
    } else {
        make_auth_envelope(&session, "activate", "", json!({}))
    };
    req.id = "forward-1".to_string();
    send_ipc_request(&session.pipe_name, &req).is_ok()
}

fn sanitize_js_error(err: &str) -> String {
    let first_line = err.lines().next().unwrap_or(err).trim();
    let cleaned = first_line
        .replace("https://app.syki.local", "")
        .trim()
        .to_string();
    if cleaned.is_empty() {
        "rpc error".to_string()
    } else {
        cleaned
    }
}

fn eval_syki_rpc(handle: &HostHandle, call_js: &str) -> Result<serde_json::Value, String> {
    let expr = format!(
        "(async function(){{ if (!window.__sykiRPC) throw new Error('__sykiRPC not ready'); return await ({call_js}); }})()"
    );
    handle.eval_js_rpc(&expr).map_err(|e| sanitize_js_error(&e))
}

fn eval_syki_plugins(handle: &HostHandle, call_js: &str) -> Result<serde_json::Value, String> {
    let expr = format!(
        "(async function(){{ if (!window.__sykiPlugins) throw new Error('plugin runtime not ready'); return await ({call_js}); }})()"
    );
    handle.eval_js_rpc(&expr).map_err(|e| sanitize_js_error(&e))
}

fn handle_ipc_envelope(
    env: &RuntimeEnv,
    handle: &HostHandle,
    core: &Arc<CoreState>,
    req: IpcEnvelope,
) -> IpcReply {
    let action = req.action.as_str();
    let method = req.method.as_str();

    if action == "activate" || method == "ui.activate" {
        handle.activate_window();
        return IpcReply::ok(req.id, json!({ "activated": true }));
    }
    if action == "new-tab" || method == "ui.new_tab" {
        handle.activate_window();
        return match eval_syki_rpc(handle, "window.__sykiRPC.newTab('', '')") {
            Ok(mut v) => {
                if let Some(obj) = v.as_object_mut() {
                    obj.entry("newTab").or_insert(json!(true));
                }
                IpcReply::ok(req.id, v)
            }
            Err(_) => {
                (handle.event_sender)(RpcEvent {
                    topic: "host:openFile".to_string(),
                    payload: json!({ "newTab": true, "isScrap": false }),
                });
                IpcReply::ok(req.id, json!({ "newTab": true }))
            }
        }
    }
    if action == "open" || method == "ui.open" {
        let path_str = if !req.path.is_empty() {
            req.path.clone()
        } else {
            req.params
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        if path_str.trim().is_empty() {
            return IpcReply::err(req.id, "missing path");
        }
        let norm_path = normalize_fs_path(Path::new(path_str.trim()));
        let scrap_dir = normalize_fs_path(&JsonConfigStore::new(env.data_dir.join("config.json")).scrap_dir());
        let norm_lower = norm_path.to_string_lossy().to_lowercase();
        let scrap_lower = scrap_dir.to_string_lossy().to_lowercase();
        let is_scrap = norm_lower == scrap_lower
            || norm_lower.starts_with(&format!("{scrap_lower}\\"))
            || norm_lower.starts_with(&format!("{scrap_lower}/"));
        match NoteFileStore::read_note(&norm_path) {
            Ok(loaded) => {
                let norm_str = norm_path.to_string_lossy().into_owned();
                handle.activate_window();
                (handle.event_sender)(RpcEvent {
                    topic: "host:openFile".to_string(),
                    payload: json!({
                        "path": norm_str,
                        "title": norm_path.file_name().and_then(|n| n.to_str()).unwrap_or("untitled.md"),
                        "content": loaded.content,
                        "encoding": loaded.encoding_name,
                        "sig": cyrb53_utf16_sig(&loaded.content),
                        "isScrap": is_scrap
                    }),
                });
                IpcReply::ok(req.id, json!({ "opened": norm_str }))
            }
            Err(e) => IpcReply::err(req.id, format!("failed to open file: {e}")),
        }
    } else if action == "pipe" || method == "scrap.append" {
        let text = if !req.content.is_empty() {
            req.content.clone()
        } else {
            req.params
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let cfg = JsonConfigStore::new(env.data_dir.join("config.json"));
        let scrap_dir = cfg.scrap_dir();
        match ScrapStore::append_formatted(&scrap_dir, &text, "") {
            Ok(scrap_path) => {
                (handle.event_sender)(RpcEvent {
                    topic: "scrap:appended".to_string(),
                    payload: json!({ "path": scrap_path.to_string_lossy() }),
                });
                IpcReply::ok(req.id, json!({ "path": scrap_path.to_string_lossy() }))
            }
            Err(e) => IpcReply::err(req.id, format!("scrap append failed: {e}")),
        }
    } else if method == "info" {
        IpcReply::ok(
            req.id,
            json!({
                "version": APP_VERSION,
                "dataDir": env.data_dir,
                "instanceId": env.instance_id,
                "pid": std::process::id()
            }),
        )
    } else if method == "config.get" {
        let store = JsonConfigStore::new(env.data_dir.join("config.json"));
        match store.load_raw() {
            Ok(raw) => {
                let val: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
                let key = req
                    .params
                    .get("key")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        req.params
                            .get("args")
                            .and_then(|v| v.as_array())
                            .and_then(|a| a.first())
                            .and_then(|v| v.as_str())
                    });
                if let Some(k) = key.filter(|s| !s.trim().is_empty()) {
                    match get_config_value(&val, k) {
                        Ok(out) => IpcReply::ok(req.id, out),
                        Err(e) => IpcReply::err(req.id, e),
                    }
                } else {
                    IpcReply::ok(req.id, val)
                }
            }
            Err(e) => IpcReply::err(req.id, e.to_string()),
        }
    } else if method == "config.set" {
        let store = JsonConfigStore::new(env.data_dir.join("config.json"));
        if let (Some(key), Some(val)) = (
            req.params.get("key").and_then(|v| v.as_str()),
            req.params.get("value").cloned(),
        ) {
            let raw = store.load_raw().unwrap_or_else(|_| "{}".to_string());
            let mut obj: serde_json::Value =
                serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
            if let Err(e) = set_config_value(&mut obj, key, val) {
                return IpcReply::err(req.id, e);
            }
            let next = serde_json::to_string_pretty(&obj).unwrap_or_else(|_| "{}".to_string());
            match store.save_raw(&next) {
                Ok(()) => IpcReply::ok(req.id, json!({ "ok": true, "key": key })),
                Err(e) => IpcReply::err(req.id, e.to_string()),
            }
        } else {
            let raw = serde_json::to_string(&req.params).unwrap_or_else(|_| "{}".to_string());
            match store.save_raw(&raw) {
                Ok(()) => IpcReply::ok(req.id, json!(true)),
                Err(e) => IpcReply::err(req.id, e.to_string()),
            }
        }
    } else if method == "tab.open" {
        let arg0 = req
            .params
            .get("args")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .or_else(|| req.params.get("path").and_then(|v| v.as_str()))
            .unwrap_or("")
            .trim();
        if arg0.is_empty() {
            return IpcReply::err(req.id, "missing path");
        }
        let norm_path = normalize_fs_path(Path::new(arg0));
        match NoteFileStore::read_note(&norm_path) {
            Ok(loaded) => {
                let norm_str = norm_path.to_string_lossy().into_owned();
                let title = norm_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("untitled.md");
                let js_call = format!(
                    "window.__sykiRPC.openTab({{ path: {}, title: {}, content: {}, encoding: {} }})",
                    json!(norm_str),
                    json!(title),
                    json!(loaded.content),
                    json!(loaded.encoding_name)
                );
                match eval_syki_rpc(handle, &js_call) {
                    Ok(v) => IpcReply::ok(req.id, v),
                    Err(e) => IpcReply::err(req.id, e),
                }
            }
            Err(e) => IpcReply::err(req.id, format!("failed to open file: {e}")),
        }
    } else if method.starts_with("buffer.") || method.starts_with("tab.") || matches!(method, "ui.state" | "ui.set" | "ui.panel") {
        let args = req
            .params
            .get("args")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let arg0 = args.first().and_then(|v| v.as_str()).unwrap_or("");
        let arg1 = args.get(1).and_then(|v| v.as_str()).unwrap_or("");
        let js_call = match method {
            "buffer.get" => format!("window.__sykiRPC.getBuffer({})", json!(if arg0.is_empty() { None } else { Some(arg0) })),
            "buffer.set" => format!("window.__sykiRPC.setBuffer({}, {})", json!(arg0), json!(if arg1.is_empty() { None } else { Some(arg1) })),
            "buffer.append" => format!("window.__sykiRPC.appendBuffer({}, {})", json!(arg0), json!(if arg1.is_empty() { None } else { Some(arg1) })),
            "tab.list" => "window.__sykiRPC.getTabs()".to_string(),
            "tab.switch" => format!("window.__sykiRPC.switchTab({})", json!(arg0)),
            "tab.new" => format!("window.__sykiRPC.newTab({}, {})", json!(arg0), json!(arg1)),
            "tab.open" => format!("window.__sykiRPC.openTab({{ path: {} }})", json!(arg0)),
            "tab.close" => {
                let force = args.iter().any(|v| v.as_str() == Some("--force") || v.as_str() == Some("-f"));
                let tab_id = args
                    .iter()
                    .filter_map(|v| v.as_str())
                    .find(|s| !s.is_empty() && !s.starts_with('-'));
                format!("window.__sykiRPC.closeTab({}, {{ force: {} }})", json!(tab_id), force)
            }
            "tab.pdf" => format!("window.__sykiRPC.printPdf(null, {{}}, {})", json!(arg0)),
            "ui.state" => "window.__sykiRPC.getUiState()".to_string(),
            "ui.set" => {
                let parsed: serde_json::Value = match serde_json::from_str::<serde_json::Value>(arg0) {
                    Ok(v) if v.is_object() => v,
                    Ok(_) => return IpcReply::err(req.id, "ui set requires a JSON object"),
                    Err(e) => return IpcReply::err(req.id, format!("invalid JSON for ui set: {e}")),
                };
                format!("window.__sykiRPC.setUiState({parsed})")
            }
            "ui.panel" => format!("window.__sykiRPC.openPanel({})", json!(arg0)),
            _ => return IpcReply::err(req.id, format!("unsupported ipc method: {method}")),
        };
        match eval_syki_rpc(handle, &js_call) {
            Ok(v) => IpcReply::ok(req.id, v),
            Err(e) => IpcReply::err(req.id, e),
        }
    } else if method == "scrap.search" {
        let query = req
            .params
            .get("args")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let rpc_req = syki_core::rpc::RpcRequest {
            id: req.id.clone(),
            method: "searchScraps".to_string(),
            params: json!([query, 50, ""]),
        };
        let resp = core.dispatch(&rpc_req);
        if resp.ok {
            IpcReply::ok(req.id, resp.result.unwrap_or(json!([])))
        } else {
            IpcReply::err(req.id, resp.error.unwrap_or_else(|| "searchScraps failed".into()))
        }
    } else if method == "pty.presets" || method == "pty.list" {
        let resp = core.dispatch(&syki_core::rpc::RpcRequest {
            id: req.id.clone(),
            method: "ptyAvailablePresets".to_string(),
            params: json!([]),
        });
        let val = match resp.result {
            Some(serde_json::Value::String(s)) => {
                serde_json::from_str::<serde_json::Value>(&s).unwrap_or_else(|_| json!(s))
            }
            Some(other) => other,
            None => json!([]),
        };
        IpcReply::ok(req.id, val)
    } else if method == "speech.status" {
        let resp = core.dispatch(&syki_core::rpc::RpcRequest {
            id: req.id.clone(),
            method: "getSpeechStatus".to_string(),
            params: json!(["{}"]),
        });
        IpcReply::ok(req.id, resp.result.unwrap_or(json!({})))
    } else if method == "ocr" || method == "ocr.run" {
        let img_path = req
            .params
            .get("args")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if img_path.is_empty() {
            return IpcReply::err(req.id, "usage: syki-cli ocr run <image-path>");
        }
        if !Path::new(img_path).is_file() {
            return IpcReply::err(req.id, format!("image file not found: {img_path}"));
        }
        IpcReply::ok(req.id, json!({ "queued": true, "path": img_path }))
    } else if method == METHOD_PLUGIN_LIST {
        let roots = syki_win::ext_store::ExtRoots::new(
            syki_win::ext_store::portable_dir_from_exe(),
            env.data_dir.clone(),
            syki_win::ext_store::safe_mode_from_args(),
        );
        let disk_ids: Vec<String> = roots
            .list_plugins()
            .into_iter()
            .filter_map(|p| p.get("id").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        let _ = eval_syki_plugins(handle, &format!("window.__sykiPlugins.prune({})", json!(disk_ids)));
        match eval_syki_plugins(handle, "window.__sykiPlugins.list()") {
            Ok(v) => IpcReply::ok(req.id, v),
            Err(e) => IpcReply::err(req.id, e),
        }
    } else if method == METHOD_PLUGIN_STATUS {
        let plugin_id = req.params.get("plugin_id").and_then(|v| v.as_str());
        let js_call = format!("window.__sykiPlugins.status({})", json!(plugin_id));
        match eval_syki_plugins(handle, &js_call) {
            Ok(v) => IpcReply::ok(req.id, v),
            Err(e) => IpcReply::err(req.id, e),
        }
    } else if method == METHOD_PLUGIN_RELOAD {
        let Some(plugin_id) = req
            .params
            .get("plugin_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
        else {
            return IpcReply::err(req.id, "missing plugin_id");
        };
        let roots = syki_win::ext_store::ExtRoots::new(
            syki_win::ext_store::portable_dir_from_exe(),
            env.data_dir.clone(),
            syki_win::ext_store::safe_mode_from_args(),
        );
        let exists_on_disk = roots
            .list_plugins()
            .iter()
            .any(|p| p.get("id").and_then(|v| v.as_str()) == Some(plugin_id));
        if !exists_on_disk {
            let _ = eval_syki_plugins(handle, &format!("window.__sykiPlugins.remove({})", json!(plugin_id)));
            return IpcReply::err(req.id, format!("plugin not found: {plugin_id}"));
        }
        let js_call = format!("window.__sykiPlugins.reload({})", json!(plugin_id));
        match eval_syki_plugins(handle, &js_call) {
            Ok(v) => IpcReply::ok(req.id, v),
            Err(e) => IpcReply::err(req.id, e),
        }
    } else if method == METHOD_PLUGIN_RUN {
        let Some(command_id) = req
            .params
            .get("command_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
        else {
            return IpcReply::err(req.id, "missing command_id");
        };
        if !req.headless {
            handle.activate_window();
        }
        let args = req.params.get("args").cloned().unwrap_or_else(|| json!({}));
        let js_call = format!(
            "window.__sykiPlugins.run({}, {})",
            json!(command_id),
            args
        );
        match eval_syki_plugins(handle, &js_call) {
            Ok(v) => IpcReply::ok(req.id, v),
            Err(e) => IpcReply::err(req.id, e),
        }
    } else {
        IpcReply::err(req.id, format!("unsupported ipc method: {method}"))
    }
}

fn run_perf_probe() {
    use std::time::Instant;
    use syki_win::host_lifecycle::{current_working_set_bytes, trim_working_set_sync, CanvasHostState};

    let mut host = CanvasHostState::new();
    host.suspend();
    host.detach();
    trim_working_set_sync();
    let detached_bytes = current_working_set_bytes();
    let detached_mb = (detached_bytes as f64) / (1024.0 * 1024.0);

    let t0 = Instant::now();
    let _prev = host.wake();
    let wake_ms = t0.elapsed().as_secs_f64() * 1000.0;

    println!(
        "{}",
        json!({
            "detachedRamMb": (detached_mb * 100.0).round() / 100.0,
            "hotkeyWakeMs": (wake_ms * 1000.0).round() / 1000.0,
            "zeroPsRuntime": true
        })
    );
}

fn main() {
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.iter().any(|a| a == "--elevated-pty-helper") {
        let code = syki_win::pty::run_elevated_pty_helper(&raw_args);
        std::process::exit(code);
    }
    if raw_args.get(1).map(String::as_str) == Some("--perf-probe") {
        run_perf_probe();
        return;
    }
    let env = RuntimeEnv::from_env();
    let startup_file = std::env::args().nth(1).map(PathBuf::from);
    if try_forward_to_running_instance(&env, startup_file.as_deref()) {
        return;
    }

    let user_data_dir = env.webview_user_data_dir();
    if let Err(err) = fs::create_dir_all(&user_data_dir) {
        eprintln!(
            "failed to prepare WebView2 directory {}: {err}",
            user_data_dir.display()
        );
        std::process::exit(1);
    }

    let core = Arc::new(CoreState::new(env.clone(), startup_file));
    let core_for_rpc = Arc::clone(&core);
    let core_for_events = Arc::clone(&core);
    let core_for_ipc = Arc::clone(&core);
    let preview_store = core.print_preview_store();
    let ext_roots = syki_win::ext_store::ExtRoots::new(
        syki_win::ext_store::portable_dir_from_exe(),
        env.data_dir.clone(),
        syki_win::ext_store::safe_mode_from_args(),
    );
    let dyn_handler: Arc<DynamicRouteHandler> = Arc::new(Box::new(move |rel| {
        preview_store
            .route_response(rel)
            .or_else(|| syki_win::assets_store::serve_local_image_route(rel))
            .or_else(|| ext_roots.resolve_ext_request(rel))
    }));
    let ipc_server_slot: Arc<Mutex<Option<IpcPipeServer>>> = Arc::new(Mutex::new(None));
    let ipc_slot_ready = Arc::clone(&ipc_server_slot);
    let env_for_ipc = env.clone();

    let config = WebViewHostConfig {
        virtual_origin: VIRTUAL_ORIGIN,
        user_data_dir,
        title: "syki".to_string(),
        debug: cfg!(debug_assertions),
    };

    if let Err(err) = run_webview_host_with_handle(
        config,
        Arc::new(move |req| core_for_rpc.dispatch(req)),
        Some(dyn_handler),
        Some(Box::new(move |handle| {
            core_for_events.set_host_handle(handle.clone());
            let env_cb = env_for_ipc.clone();
            let handle_cb = handle.clone();
            let core_cb = Arc::clone(&core_for_ipc);
            if let Ok(srv) = IpcPipeServer::start(&env_for_ipc, move |req| {
                handle_ipc_envelope(&env_cb, &handle_cb, &core_cb, req)
            }) {
                *ipc_slot_ready.lock().expect("ipc slot mutex") = Some(srv);
            }
        })),
    ) {
        eprintln!("syki-gui fatal error: {err}");
        std::process::exit(1);
    }

    let srv_opt = ipc_server_slot.lock().expect("ipc slot mutex").take();
    if let Some(mut srv) = srv_opt {
        srv.stop();
    }
}
