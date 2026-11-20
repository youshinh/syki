#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Single-window GUI launcher for syki-lite (`syki.exe`).

use std::fs;
use std::sync::Arc;

use syki_core::rpc::RuntimeEnv;
use syki_win::webview_host::{
    default_tracer_handler, run_webview_host, WebViewHostConfig, VIRTUAL_ORIGIN,
};

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

    let config = WebViewHostConfig {
        virtual_origin: VIRTUAL_ORIGIN,
        user_data_dir,
        title: "syki-lite".to_string(),
        debug: cfg!(debug_assertions),
    };

    if let Err(err) = run_webview_host(config, Arc::new(default_tracer_handler), None) {
        eprintln!("syki-gui fatal error: {err}");
        std::process::exit(1);
    }
}
