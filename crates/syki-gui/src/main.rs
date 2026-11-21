#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Single-window GUI launcher for syki-lite (`syki.exe`) with single-instance Named Pipe IPC (#06).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::json;
use syki_core::ipc::{
    is_pipe_available, load_ipc_session, make_auth_envelope, pipe_name_for, send_ipc_request,
    session_file_path, IpcEnvelope, IpcPipeServer, IpcReply,
};
use syki_core::rpc::{RpcEvent, RuntimeEnv};
use syki_core::storage::note::cyrb53_utf16_sig;
use syki_core::storage::{ConfigStore, JsonConfigStore, NoteFileStore, ScrapStore};
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
        let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let mut r = make_auth_envelope(&session, "open", "", json!({}));
        r.path = abs.to_string_lossy().into_owned();
        r
    } else {
        make_auth_envelope(&session, "activate", "", json!({}))
    };
    req.id = "forward-1".to_string();
    send_ipc_request(&session.pipe_name, &req).is_ok()
}

fn handle_ipc_envelope(env: &RuntimeEnv, handle: &HostHandle, req: IpcEnvelope) -> IpcReply {
    let action = req.action.as_str();
    let method = req.method.as_str();

    if action == "activate" || method == "ui.activate" {
        handle.activate_window();
        return IpcReply::ok(req.id, json!({ "activated": true }));
    }
    if action == "new-tab" || method == "ui.new_tab" {
        handle.activate_window();
        (handle.event_sender)(RpcEvent {
            topic: "host:openFile".to_string(),
            payload: json!({ "newTab": true }),
        });
        return IpcReply::ok(req.id, json!({ "newTab": true }));
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
        match NoteFileStore::read_note(Path::new(path_str.trim())) {
            Ok(loaded) => {
                handle.activate_window();
                (handle.event_sender)(RpcEvent {
                    topic: "host:openFile".to_string(),
                    payload: json!({
                        "path": loaded.path.to_string_lossy(),
                        "title": loaded.path.file_name().and_then(|n| n.to_str()).unwrap_or("untitled.md"),
                        "content": loaded.content,
                        "encoding": loaded.encoding_name,
                        "sig": cyrb53_utf16_sig(&loaded.content)
                    }),
                });
                IpcReply::ok(req.id, json!({ "opened": path_str }))
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
        match ScrapStore::append_formatted(&scrap_dir, &text, &req.cwd) {
            Ok(scrap_path) => {
                if let Ok(loaded) = NoteFileStore::read_note(&scrap_path) {
                    handle.activate_window();
                    (handle.event_sender)(RpcEvent {
                        topic: "host:openFile".to_string(),
                        payload: json!({
                            "path": loaded.path.to_string_lossy(),
                            "title": loaded.path.file_name().and_then(|n| n.to_str()).unwrap_or("scrap.md"),
                            "content": loaded.content,
                            "encoding": loaded.encoding_name,
                            "sig": cyrb53_utf16_sig(&loaded.content)
                        }),
                    });
                }
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
                IpcReply::ok(req.id, val)
            }
            Err(e) => IpcReply::err(req.id, e.to_string()),
        }
    } else if method == "config.set" {
        let store = JsonConfigStore::new(env.data_dir.join("config.json"));
        let raw = serde_json::to_string(&req.params).unwrap_or_else(|_| "{}".to_string());
        match store.save_raw(&raw) {
            Ok(()) => IpcReply::ok(req.id, json!(true)),
            Err(e) => IpcReply::err(req.id, e.to_string()),
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
    if std::env::args().nth(1).as_deref() == Some("--perf-probe") {
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
    let preview_store = core.print_preview_store();
    let dyn_handler: Arc<DynamicRouteHandler> = Arc::new(Box::new(move |rel| {
        preview_store
            .route_response(rel)
            .or_else(|| syki_win::assets_store::serve_local_image_route(rel))
    }));
    let ipc_server_slot: Arc<Mutex<Option<IpcPipeServer>>> = Arc::new(Mutex::new(None));
    let ipc_slot_ready = Arc::clone(&ipc_server_slot);
    let env_for_ipc = env.clone();

    let config = WebViewHostConfig {
        virtual_origin: VIRTUAL_ORIGIN,
        user_data_dir,
        title: "syki-lite".to_string(),
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
            if let Ok(srv) = IpcPipeServer::start(&env_for_ipc, move |req| {
                handle_ipc_envelope(&env_cb, &handle_cb, req)
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
