#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Single-window GUI launcher for syki-lite (`syki.exe`).

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use syki_core::rpc::RuntimeEnv;
use syki_win::rpc_dispatch::CoreState;
use syki_win::webview_host::{run_webview_host_with_events, WebViewHostConfig, VIRTUAL_ORIGIN};

fn main() {
    let env = RuntimeEnv::from_env();
    let user_data_dir = env.webview_user_data_dir();
    if let Err(err) = fs::create_dir_all(&user_data_dir) {
        eprintln!(
            "failed to prepare WebView2 directory {}: {err}",
            user_data_dir.display()
        );
        std::process::exit(1);
    }

    let startup_file = std::env::args().nth(1).map(PathBuf::from);
    let core = Arc::new(CoreState::new(env, startup_file));
    let core_for_rpc = Arc::clone(&core);
    let core_for_events = Arc::clone(&core);

    let config = WebViewHostConfig {
        virtual_origin: VIRTUAL_ORIGIN,
        user_data_dir,
        title: "syki-lite".to_string(),
        debug: cfg!(debug_assertions),
    };

    if let Err(err) = run_webview_host_with_events(
        config,
        Arc::new(move |req| core_for_rpc.dispatch(req)),
        None,
        Some(Box::new(move |sender| {
            core_for_events.set_event_sender(sender);
        })),
    ) {
        eprintln!("syki-gui fatal error: {err}");
        std::process::exit(1);
    }
}
