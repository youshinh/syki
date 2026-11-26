//! Virtual-origin WebView2 host and single `PostWebMessageAsJson` dispatcher (#01).
//!
//! Serves `frontend/` strictly from in-memory `include_bytes!` assets under
//! `https://app.syki.local/` and routes dynamic endpoints such as
//! `/api/print/preview.pdf` through an optional `DynamicRouteHandler`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};

use syki_core::rpc::{RpcEvent, RpcRequest, RpcResponse};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2, ICoreWebView2Controller,
    ICoreWebView2Controller2, ICoreWebView2Environment, ICoreWebView2EnvironmentOptions,
    ICoreWebView2Settings3, ICoreWebView2_19, ICoreWebView2_3, COREWEBVIEW2_COLOR,
    COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
    COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
};
use webview2_com::{
    CallDevToolsProtocolMethodCompletedHandler, CoTaskMemPWSTR,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    TrySuspendCompletedHandler, WebMessageReceivedEventHandler, WebResourceRequestedEventHandler,
};
use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    COLORREF, E_POINTER, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, UpdateWindow};
use windows::Win32::System::Com::{CoInitializeEx, IStream, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwareness, PROCESS_PER_MONITOR_DPI_AWARE};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, IsZoomed, KillTimer, PostMessageW, PostQuitMessage, RegisterClassW,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, ShowWindow, TranslateMessage, CW_USEDEFAULT,
    GWLP_USERDATA, MSG, SW_HIDE, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOW, WM_APP, WM_CLOSE,
    WM_DESTROY, WM_HOTKEY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP, WM_SETFOCUS, WM_SIZE,
    WM_TIMER, WNDCLASSW, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW,
};

use crate::host_lifecycle::{
    add_tray_icon, apply_hotkey, remove_tray_icon, show_tray_menu, trim_working_set_async,
    CanvasHostState, HOTKEY_ID_MAIN, HOTKEY_ID_QUICKCAPTURE, TRAY_CMD_EXIT, TRAY_CMD_QUICK_CAPTURE,
    TRAY_CMD_SHOW, WM_TRAY_ICON,
};
use crate::rpc_dispatch::EventSender;

const WM_APP_PUSH_EVENT: u32 = WM_APP + 1;
const WM_APP_ACTIVATE: u32 = WM_APP + 2;
const WM_APP_WINDOW_CMD: u32 = WM_APP + 3;
const WM_APP_DEVTOOLS_CALL: u32 = WM_APP + 4;
const WM_APP_ENSURE_ATTACHED: u32 = WM_APP + 5;

const TIMER_IDLE_DETACH: usize = 1;
const TIMER_DETACH_COMMIT: usize = 2;

struct DevToolsRequest {
    method: String,
    params_json: String,
    reply_tx: mpsc::Sender<Result<String, String>>,
}

const WIN_CMD_MINIMIZE: usize = 1;
const WIN_CMD_TOGGLE_MAX: usize = 2;
const WIN_CMD_HIDE: usize = 3;
const WIN_CMD_FORCE_QUIT: usize = 4;
const WIN_CMD_QUICK_CAPTURE: usize = 5;

/// Thread-safe handle to push `RpcEvent`s or control the Win32 host window.
#[derive(Clone)]
pub struct HostHandle {
    pub event_sender: EventSender,
    hwnd_raw: isize,
    hotkey_main: Arc<Mutex<String>>,
    hotkey_qc: Arc<Mutex<String>>,
}

impl HostHandle {
    pub fn activate_window(&self) {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.hwnd_raw as *mut _)),
                WM_APP_ACTIVATE,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }

    pub fn minimize(&self) {
        self.post_win_cmd(WIN_CMD_MINIMIZE);
    }

    pub fn toggle_maximize(&self) {
        self.post_win_cmd(WIN_CMD_TOGGLE_MAX);
    }

    pub fn hide_to_tray(&self) {
        self.post_win_cmd(WIN_CMD_HIDE);
    }

    pub fn force_quit(&self) {
        self.post_win_cmd(WIN_CMD_FORCE_QUIT);
    }

    pub fn open_quick_capture(&self) {
        self.post_win_cmd(WIN_CMD_QUICK_CAPTURE);
    }

    pub fn ensure_attached(&self) {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.hwnd_raw as *mut _)),
                WM_APP_ENSURE_ATTACHED,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }

    pub fn eval_js_rpc(&self, expr: &str) -> Result<serde_json::Value, String> {
        self.ensure_attached();
        let params = serde_json::json!({
            "expression": expr,
            "awaitPromise": true,
            "returnByValue": true
        });
        let raw = self.call_devtools_protocol("Runtime.evaluate", &params.to_string())?;
        let parsed: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| format!("invalid CDP Runtime.evaluate JSON: {e}"))?;
        if let Some(ex) = parsed.get("exceptionDetails") {
            let desc = ex
                .pointer("/exception/description")
                .or_else(|| ex.pointer("/exception/value"))
                .or_else(|| ex.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("JS evaluation exception");
            return Err(desc.to_string());
        }
        Ok(parsed
            .pointer("/result/value")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    pub fn call_devtools_protocol(
        &self,
        method: &str,
        params_json: &str,
    ) -> Result<String, String> {
        let (tx, rx) = mpsc::channel();
        let req = Box::new(DevToolsRequest {
            method: method.to_string(),
            params_json: params_json.to_string(),
            reply_tx: tx,
        });
        let raw = Box::into_raw(req);
        let posted = unsafe {
            PostMessageW(
                Some(HWND(self.hwnd_raw as *mut _)),
                WM_APP_DEVTOOLS_CALL,
                WPARAM(0),
                LPARAM(raw as isize),
            )
        };
        if posted.is_err() {
            unsafe {
                let _ = Box::from_raw(raw);
            }
            return Err("failed to post CDP request to UI thread".to_string());
        }
        rx.recv_timeout(std::time::Duration::from_secs(90))
            .map_err(|e| format!("CDP call timed out: {e}"))?
    }

    pub fn update_main_hotkey(&self, spec: &str) -> Result<bool, String> {
        crate::host_lifecycle::parse_shortcut(spec)
            .ok_or_else(|| format!("invalid shortcut: {spec}"))?;
        *self.hotkey_main.lock().expect("hotkey_main mutex") = spec.to_string();
        self.post_win_cmd(10);
        Ok(true)
    }

    pub fn update_qc_hotkey(&self, spec: &str) -> Result<bool, String> {
        if !spec.trim().is_empty() {
            crate::host_lifecycle::parse_shortcut(spec)
                .ok_or_else(|| format!("invalid shortcut: {spec}"))?;
        }
        *self.hotkey_qc.lock().expect("hotkey_qc mutex") = spec.to_string();
        self.post_win_cmd(11);
        Ok(true)
    }

    fn post_win_cmd(&self, cmd: usize) {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.hwnd_raw as *mut _)),
                WM_APP_WINDOW_CMD,
                WPARAM(cmd),
                LPARAM(0),
            );
        }
    }
}

pub const VIRTUAL_ORIGIN: &str = "https://app.syki.local/";

pub type DynamicRouteHandler =
    Box<dyn Fn(&str) -> Option<(u16, &'static str, Vec<u8>)> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct WebViewHostConfig {
    pub virtual_origin: &'static str,
    pub user_data_dir: PathBuf,
    pub title: String,
    pub debug: bool,
}

/// Parses an inbound JSON RPC request and serializes the [`RpcResponse`].
pub fn dispatch_rpc_message(
    raw_json: &str,
    handler: &dyn Fn(&RpcRequest) -> RpcResponse,
) -> String {
    match serde_json::from_str::<RpcRequest>(raw_json) {
        Ok(req) => serde_json::to_string(&handler(&req))
            .unwrap_or_else(|e| format!("{{\"id\":\"\",\"ok\":false,\"error\":\"{e}\"}}")),
        Err(err) => {
            let resp = RpcResponse::err("", format!("invalid rpc request: {err}"));
            serde_json::to_string(&resp)
                .unwrap_or_else(|_| "{\"id\":\"\",\"ok\":false,\"error\":\"invalid\"}".into())
        }
    }
}

/// Baseline RPC dispatcher for tracer-bullet boot verification (#01).
pub fn default_tracer_handler(req: &RpcRequest) -> RpcResponse {
    match req.method.as_str() {
        "getAppVersion" => RpcResponse::ok(&req.id, serde_json::json!(syki_core::APP_VERSION)),
        "uiReady" => RpcResponse::ok(&req.id, serde_json::json!(true)),
        "getPlatformCapabilities" => RpcResponse::ok(
            &req.id,
            serde_json::json!({
                "os": "win32",
                "nativeImeSwitch": true,
                "tray": true,
                "globalHotkey": true
            }),
        ),
        _ => RpcResponse::err(&req.id, format!("unknown method: {}", req.method)),
    }
}

/// Resolves a URI under [`VIRTUAL_ORIGIN`] against dynamic routes first, then bundled assets.
/// Decompresses gzip-bundled assets so WebView2's `WebResourceRequested` receives plain bytes.
pub fn resolve_virtual_request(
    uri: &str,
    dynamic_handler: Option<&DynamicRouteHandler>,
) -> Option<(u16, &'static str, Vec<u8>)> {
    if !uri.starts_with(VIRTUAL_ORIGIN) {
        return None;
    }
    let rel = &uri[VIRTUAL_ORIGIN.len() - 1..];
    if let Some(handler) = dynamic_handler {
        if let Some(resp) = handler(rel) {
            return Some(resp);
        }
    }
    crate::assets::resolve_static_asset(rel)
        .map(|a| (200, a.mime, crate::assets::decode_asset_bytes(a)))
}

pub type DetachGuard = Arc<dyn Fn() -> bool + Send + Sync>;

struct WindowState {
    controller: Rc<RefCell<Option<ICoreWebView2Controller>>>,
    webview: Rc<RefCell<Option<ICoreWebView2>>>,
    environment: Rc<RefCell<Option<ICoreWebView2Environment>>>,
    rpc_handler: Arc<dyn Fn(&RpcRequest) -> RpcResponse + Send + Sync>,
    dynamic_handler: Option<Arc<DynamicRouteHandler>>,
    virtual_origin: &'static str,
    debug: bool,
    event_queue: Arc<Mutex<VecDeque<String>>>,
    canvas: RefCell<CanvasHostState>,
    hotkey_main: Arc<Mutex<String>>,
    hotkey_qc: Arc<Mutex<String>>,
    data_dir: PathBuf,
    idle_detach_ms: u32,
    detach_commit_ms: u32,
    detach_guard: Option<DetachGuard>,
    event_sender: Rc<RefCell<Option<EventSender>>>,
    startup_t0: std::time::Instant,
}

const DEFAULT_BROWSER_ARGS: &str =
    "--disable-background-networking --disable-component-update --no-pings";

fn load_perf_params_value() -> Option<serde_json::Value> {
    let candidates: Vec<PathBuf> = [
        std::env::var_os("SYKI_PERF_PARAMS_FILE").map(PathBuf::from),
        std::env::current_exe().ok().and_then(|exe| {
            let dir = exe.parent()?;
            let local = dir.join("config").join("perf_params.json");
            if local.exists() {
                return Some(local);
            }
            // Dev layout: target/{debug,release}/syki.exe -> ../../config/perf_params.json
            let repo = dir.parent()?.parent()?.join("config").join("perf_params.json");
            if repo.exists() {
                return Some(repo);
            }
            None
        }),
    ]
    .into_iter()
    .flatten()
    .collect();

    for path in candidates {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if let Some(params) = val.get("params") {
            return Some(params.clone());
        }
        return Some(val);
    }
    None
}

fn write_perf_trace_event(phase: &str, elapsed_ms: f64) {
    let Ok(trace_path) = std::env::var("SYKI_PERF_TRACE_FILE") else {
        return;
    };
    if trace_path.trim().is_empty() {
        return;
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(trace_path)
    {
        let _ = writeln!(
            f,
            r#"{{"phase":"{}","ms":{:.2}}}"#,
            phase, elapsed_ms
        );
    }
}

fn client_rect_of(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
    }
    rect
}

fn reason_phrase_for(status: u16) -> &'static str {
    match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

fn build_preload_script(data_dir: &std::path::Path) -> String {
    let read_valid_json_str = |filename: &str| -> Option<String> {
        let raw = std::fs::read_to_string(data_dir.join(filename)).ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        serde_json::from_str::<serde_json::Value>(trimmed).ok()?;
        Some(raw)
    };
    let config_val = read_valid_json_str("config.json");
    let session_val = read_valid_json_str("session.json");
    let ext_flags = crate::ext_store::ExtRoots::new(
        crate::ext_store::portable_dir_from_exe(),
        data_dir.to_path_buf(),
        crate::ext_store::safe_mode_from_args(),
    )
    .preload_ext_flags();
    let perf_params = load_perf_params_value();
    let payload = serde_json::json!({
        "config": config_val,
        "session": session_val,
        "caps": {
            "os": "win32",
            "nativeImeSwitch": true,
            "tray": true,
            "globalHotkey": true,
            "aiOcr": true,
            "winrtOcr": false
        },
        "ext": ext_flags,
        "perf": perf_params
    });
    format!("window.__sykiPreload = {payload};")
}

fn build_response_headers(status: u16, mime: &str, rel_path: &str) -> String {
    let is_immutable_asset = crate::ext_store::is_immutable_virtual_asset(status, rel_path);
    let cache_hdr = if is_immutable_asset {
        "public, max-age=31536000, immutable"
    } else {
        "no-store"
    };
    if status == 200 && (rel_path.is_empty() || rel_path == "index.html") {
        format!(
            "Content-Type: {mime}\r\nCache-Control: {cache_hdr}\r\nContent-Security-Policy: {}",
            crate::ext_store::content_security_policy()
        )
    } else {
        format!("Content-Type: {mime}\r\nCache-Control: {cache_hdr}")
    }
}

/// Returns `true` when closing the window should hide to the tray rather than exit (`general.trayResident`, default `true`).
pub(crate) fn is_tray_resident_enabled(data_dir: &std::path::Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(data_dir.join("config.json")) else {
        return true;
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return true;
    };
    val.get("general")
        .and_then(|g| g.get("trayResident"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

fn read_startup_hotkeys_and_detach(data_dir: &std::path::Path) -> (String, String, u32) {
    let val = std::fs::read_to_string(data_dir.join("config.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
    let main_hk = val
        .as_ref()
        .and_then(|v| v.pointer("/shortcuts/globalSummon"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| "Ctrl+Alt+M".to_string());
    let qc_hk = val
        .as_ref()
        .and_then(|v| v.pointer("/shortcuts/quickCapture"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| "Ctrl+Shift+Q".to_string());
    let idle_detach_ms = std::env::var("SYKI_IDLE_DETACH_MS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .or_else(|| {
            val.as_ref()
                .and_then(|v| v.pointer("/webView/idleDetachMs"))
                .and_then(|v| v.as_u64())
                .map(|ms| ms.min(u32::MAX as u64) as u32)
        })
        .unwrap_or(300_000);
    (main_hk, qc_hk, idle_detach_ms)
}

fn set_webview_memory_low(wv: &ICoreWebView2, low: bool) {
    if let Ok(wv19) = windows::core::Interface::cast::<ICoreWebView2_19>(wv) {
        let level = if low {
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
        } else {
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
        };
        let _ = unsafe { wv19.SetMemoryUsageTargetLevel(level) };
    }
}

fn hide_and_suspend_window(hwnd: HWND, state: Option<&WindowState>) {
    if let Some(state) = state {
        state.canvas.borrow_mut().suspend();
        if let Some(ctrl) = state.controller.borrow().as_ref() {
            let _ = unsafe { ctrl.SetIsVisible(false) };
        }
        if let Some(wv) = state.webview.borrow().as_ref() {
            set_webview_memory_low(wv, true);
            if let Ok(wv3) = windows::core::Interface::cast::<ICoreWebView2_3>(wv) {
                let handler = TrySuspendCompletedHandler::create(Box::new(|_hr, _is_suspended| {
                    trim_working_set_async();
                    Ok(())
                }));
                if unsafe { wv3.TrySuspend(&handler) }.is_err() {
                    trim_working_set_async();
                }
            } else {
                trim_working_set_async();
            }
        } else {
            trim_working_set_async();
        }
        if state.idle_detach_ms > 0 {
            unsafe {
                let _ = SetTimer(Some(hwnd), TIMER_IDLE_DETACH, state.idle_detach_ms, None);
            }
        }
    }
    let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
}

fn attach_webview_controller(
    hwnd: HWND,
    environment: &ICoreWebView2Environment,
    state: &WindowState,
    visible: bool,
    preload_override: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let controller = {
        let (tx, rx) = mpsc::channel();
        let env_ref = environment.clone();
        CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| unsafe {
                env_ref
                    .CreateCoreWebView2Controller(hwnd, &handler)
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |error_code, ctrl| {
                error_code?;
                let _ = tx.send(ctrl.ok_or_else(|| windows::core::Error::from(E_POINTER)));
                Ok(())
            }),
        )?;
        rx.recv()
            .map_err(|_| webview2_com::Error::SendError)??
    };

    unsafe {
        if let Ok(ctrl2) = windows::core::Interface::cast::<ICoreWebView2Controller2>(&controller) {
            let _ = ctrl2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
                A: 255,
                R: 30,
                G: 30,
                B: 30,
            });
        }
        controller.SetBounds(client_rect_of(hwnd))?;
        controller.SetIsVisible(visible)?;
    }
    *state.controller.borrow_mut() = Some(controller.clone());

    let webview = unsafe { controller.CoreWebView2()? };
    *state.webview.borrow_mut() = Some(webview.clone());

    if !state.debug {
        unsafe {
            let settings = webview.Settings()?;
            settings.SetAreDefaultContextMenusEnabled(false)?;
            settings.SetAreDevToolsEnabled(false)?;
            if let Ok(settings3) = settings.cast::<ICoreWebView2Settings3>() {
                let _ = settings3.SetAreBrowserAcceleratorKeysEnabled(false);
            }
        }
    }

    let filter_patterns = [
        format!("{}*", state.virtual_origin),
        "http://*".to_string(),
        "https://*".to_string(),
    ];
    for pat in &filter_patterns {
        let filter_pattern = CoTaskMemPWSTR::from(pat.as_str());
        unsafe {
            webview.AddWebResourceRequestedFilter(
                *filter_pattern.as_ref().as_pcwstr(),
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            )?;
        }
    }

    let env_for_res = environment.clone();
    let dyn_for_res = state.dynamic_handler.clone();
    let startup_t0_for_res = state.startup_t0;
    let mut res_token: i64 = 0;
    unsafe {
        webview.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |_sender, args| {
                let Some(args) = args else {
                    return Ok(());
                };
                let req = args.Request()?;
                let mut uri_raw = PWSTR::null();
                req.Uri(&mut uri_raw)?;
                let uri = CoTaskMemPWSTR::from(uri_raw).to_string();

                let (status, mime, dyn_body, arc_body): (u16, &'static str, Vec<u8>, Option<std::sync::Arc<Vec<u8>>>) =
                    if !uri.starts_with(VIRTUAL_ORIGIN) {
                        (403, "text/plain; charset=utf-8", b"Forbidden".to_vec(), None)
                    } else {
                        let rel = &uri[VIRTUAL_ORIGIN.len() - 1..];
                        if let Some(hit) = dyn_for_res.as_deref().and_then(|h| h(rel)) {
                            (hit.0, hit.1, hit.2, None)
                        } else if let Some(a) = crate::assets::resolve_static_asset(rel) {
                            (200, a.mime, Vec::new(), Some(crate::assets::decode_asset_arc(a)))
                        } else {
                            (404, "text/plain; charset=utf-8", b"Not Found".to_vec(), None)
                        }
                    };

                let stream: Option<IStream> = if let Some(ref arc) = arc_body {
                    SHCreateMemStream(Some(arc.as_slice()))
                } else {
                    SHCreateMemStream(Some(&dyn_body))
                };
                let rel_path = uri
                    .strip_prefix(VIRTUAL_ORIGIN)
                    .unwrap_or("")
                    .split('?')
                    .next()
                    .unwrap_or("");
                if rel_path.is_empty() || rel_path == "index.html" {
                    write_perf_trace_event("html_req", startup_t0_for_res.elapsed().as_secs_f64() * 1000.0);
                } else if rel_path == "__boot_bundle.js" {
                    write_perf_trace_event("js_req", startup_t0_for_res.elapsed().as_secs_f64() * 1000.0);
                }
                let headers = build_response_headers(status, mime, rel_path);
                let reason_w = CoTaskMemPWSTR::from(reason_phrase_for(status));
                let headers_w = CoTaskMemPWSTR::from(headers.as_str());
                let response = env_for_res.CreateWebResourceResponse(
                    stream.as_ref(),
                    status as i32,
                    *reason_w.as_ref().as_pcwstr(),
                    *headers_w.as_ref().as_pcwstr(),
                )?;
                args.SetResponse(&response)?;
                Ok(())
            })),
            &mut res_token,
        )?;
    }

    let rpc_for_msg = Arc::clone(&state.rpc_handler);
    let q_for_msg = Arc::clone(&state.event_queue);
    let hwnd_for_msg = hwnd.0 as isize;
    // Persistent 4-thread RPC worker pool eliminates per-message OS thread spawn overhead
    // during startup RPC bursts, typing, and voice transcription handoffs.
    let (rpc_tx, rpc_rx) = mpsc::channel::<String>();
    let rpc_rx = Arc::new(Mutex::new(rpc_rx));
    for _ in 0..4 {
        let rx = Arc::clone(&rpc_rx);
        let rpc = Arc::clone(&rpc_for_msg);
        let q = Arc::clone(&q_for_msg);
        std::thread::spawn(move || loop {
            let raw_json = {
                let guard = rx.lock().expect("rpc worker rx mutex");
                match guard.recv() {
                    Ok(msg) => msg,
                    Err(_) => break,
                }
            };
            let reply_json = dispatch_rpc_message(&raw_json, rpc.as_ref());
            q.lock().expect("event_queue mutex").push_back(reply_json);
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(hwnd_for_msg as *mut _)),
                    WM_APP_PUSH_EVENT,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        });
    }
    let rpc_tx = Arc::new(Mutex::new(rpc_tx));
    let startup_t0_for_msg = state.startup_t0;
    let mut msg_token: i64 = 0;
    unsafe {
        webview.add_WebMessageReceived(
            &WebMessageReceivedEventHandler::create(Box::new(move |_sender, args| {
                let Some(args) = args else {
                    return Ok(());
                };
                let mut raw_pwstr = PWSTR::null();
                if args.WebMessageAsJson(&mut raw_pwstr).is_ok() {
                    let raw_json = CoTaskMemPWSTR::from(raw_pwstr).to_string();
                    if raw_json.contains("\"__perf_trace\"") {
                        for phase in ["js_exec_start", "init_app_start", "app_ready"] {
                            if raw_json.contains(phase) {
                                write_perf_trace_event(
                                    phase,
                                    startup_t0_for_msg.elapsed().as_secs_f64() * 1000.0,
                                );
                                return Ok(());
                            }
                        }
                    }
                    if let Ok(tx) = rpc_tx.lock() {
                        let _ = tx.send(raw_json);
                    }
                }
                Ok(())
            })),
            &mut msg_token,
        )?;
    }

    let preload_script = preload_override.unwrap_or_else(|| build_preload_script(&state.data_dir));
    let preload_w = CoTaskMemPWSTR::from(preload_script.as_str());
    let origin_w = CoTaskMemPWSTR::from(state.virtual_origin);
    unsafe {
        let _ = webview.AddScriptToExecuteOnDocumentCreated(*preload_w.as_ref().as_pcwstr(), None);
        webview.Navigate(*origin_w.as_ref().as_pcwstr())?;
        if visible {
            let _ = SetForegroundWindow(hwnd);
            let _ = controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
        }
    }
    Ok(())
}

extern "system" fn host_wnd_proc(
    hwnd: HWND,
    msg: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match msg {
        WM_SETFOCUS => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
            if !ptr.is_null() {
                let state = unsafe { &*ptr };
                if let Some(ctrl) = state.controller.borrow().as_ref() {
                    let _ = unsafe { ctrl.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC) };
                }
            }
            LRESULT(0)
        }
        WM_SIZE => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
            if !ptr.is_null() {
                let state = unsafe { &*ptr };
                if let Some(ctrl) = state.controller.borrow().as_ref() {
                    let bounds = client_rect_of(hwnd);
                    let _ = unsafe { ctrl.SetBounds(bounds) };
                }
            }
            LRESULT(0)
        }
        WM_APP_PUSH_EVENT => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
            if !ptr.is_null() {
                let state = unsafe { &*ptr };
                if let Some(wv) = state.webview.borrow().as_ref() {
                    let drained: Vec<String> = {
                        let mut q = state.event_queue.lock().expect("event_queue mutex");
                        q.drain(..).collect()
                    };
                    for json in drained {
                        let w = CoTaskMemPWSTR::from(json.as_str());
                        let _ = unsafe { wv.PostWebMessageAsJson(*w.as_ref().as_pcwstr()) };
                    }
                }
            }
            LRESULT(0)
        }
        WM_APP_ACTIVATE => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
            if !ptr.is_null() {
                let state = unsafe { &*ptr };
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_IDLE_DETACH);
                    let _ = KillTimer(Some(hwnd), TIMER_DETACH_COMMIT);
                }
                state.canvas.borrow_mut().wake();
                if state.webview.borrow().is_none() {
                    if let Some(env) = state.environment.borrow().as_ref() {
                        let _ = attach_webview_controller(hwnd, env, state, true, None);
                    }
                } else {
                    if let Some(wv) = state.webview.borrow().as_ref() {
                        set_webview_memory_low(wv, false);
                        if let Ok(wv3) = windows::core::Interface::cast::<ICoreWebView2_3>(wv) {
                            let _ = unsafe { wv3.Resume() };
                        }
                    }
                    if let Some(ctrl) = state.controller.borrow().as_ref() {
                        let _ = unsafe { ctrl.SetIsVisible(true) };
                    }
                }
            }
            unsafe {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
            if !ptr.is_null() {
                let state = unsafe { &*ptr };
                if let Some(ctrl) = state.controller.borrow().as_ref() {
                    let _ = unsafe { ctrl.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC) };
                }
                if let Some(wv) = state.webview.borrow().as_ref() {
                    let msg_w = CoTaskMemPWSTR::from(r#"{"topic":"window:restored","payload":{}}"#);
                    let _ = unsafe { wv.PostWebMessageAsJson(*msg_w.as_ref().as_pcwstr()) };
                }
            }
            LRESULT(0)
        }
        WM_APP_ENSURE_ATTACHED => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
            if !ptr.is_null() {
                let state = unsafe { &*ptr };
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_IDLE_DETACH);
                    let _ = KillTimer(Some(hwnd), TIMER_DETACH_COMMIT);
                }
                if state.webview.borrow().is_none() {
                    state.canvas.borrow_mut().wake();
                    if let Some(env) = state.environment.borrow().as_ref() {
                        let _ = attach_webview_controller(hwnd, env, state, false, None);
                    }
                } else if let Some(wv) = state.webview.borrow().as_ref() {
                    set_webview_memory_low(wv, false);
                    if let Ok(wv3) = windows::core::Interface::cast::<ICoreWebView2_3>(wv) {
                        let _ = unsafe { wv3.Resume() };
                    }
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
            if !ptr.is_null() {
                let state = unsafe { &*ptr };
                if w_param.0 == TIMER_IDLE_DETACH {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_IDLE_DETACH);
                    }
                    let busy = state
                        .detach_guard
                        .as_ref()
                        .map(|g| g())
                        .unwrap_or(false);
                    if busy {
                        if state.idle_detach_ms > 0 {
                            unsafe {
                                let _ = SetTimer(
                                    Some(hwnd),
                                    TIMER_IDLE_DETACH,
                                    state.idle_detach_ms,
                                    None,
                                );
                            }
                        }
                    } else if let Some(wv) = state.webview.borrow().as_ref() {
                        let flush_w =
                            CoTaskMemPWSTR::from(r#"{"topic":"host:flushSession","payload":{}}"#);
                        let _ = unsafe { wv.PostWebMessageAsJson(*flush_w.as_ref().as_pcwstr()) };
                        unsafe {
                            let _ = SetTimer(
                                Some(hwnd),
                                TIMER_DETACH_COMMIT,
                                state.detach_commit_ms,
                                None,
                            );
                        }
                    }
                } else if w_param.0 == TIMER_DETACH_COMMIT {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_DETACH_COMMIT);
                    }
                    let busy = state
                        .detach_guard
                        .as_ref()
                        .map(|g| g())
                        .unwrap_or(false);
                    if !busy {
                        *state.webview.borrow_mut() = None;
                        if let Some(ctrl) = state.controller.borrow_mut().take() {
                            let _ = unsafe { ctrl.Close() };
                        }
                        state.canvas.borrow_mut().detach();
                        crate::host_lifecycle::trim_working_set_sync();
                    }
                }
            }
            LRESULT(0)
        }
        WM_APP_WINDOW_CMD => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
            match w_param.0 {
                WIN_CMD_MINIMIZE => {
                    let _ = unsafe { ShowWindow(hwnd, SW_MINIMIZE) };
                }
                WIN_CMD_TOGGLE_MAX => unsafe {
                    if IsZoomed(hwnd).as_bool() {
                        let _ = ShowWindow(hwnd, SW_RESTORE);
                    } else {
                        let _ = ShowWindow(hwnd, SW_MAXIMIZE);
                    }
                },
                WIN_CMD_HIDE => {
                    let state = if !ptr.is_null() {
                        Some(unsafe { &*ptr })
                    } else {
                        None
                    };
                    hide_and_suspend_window(hwnd, state);
                }
                WIN_CMD_FORCE_QUIT => {
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                WIN_CMD_QUICK_CAPTURE => {
                    if !ptr.is_null() {
                        let state = unsafe { &*ptr };
                        let sender = state.event_sender.borrow().clone();
                        crate::quick_capture::open_quick_capture_popup(
                            state.data_dir.clone(),
                            sender,
                        );
                    }
                }
                10 => {
                    if !ptr.is_null() {
                        let state = unsafe { &*ptr };
                        let spec = state.hotkey_main.lock().expect("hotkey_main mutex").clone();
                        let _ = apply_hotkey(hwnd, HOTKEY_ID_MAIN, &spec);
                    }
                }
                11 => {
                    if !ptr.is_null() {
                        let state = unsafe { &*ptr };
                        let spec = state.hotkey_qc.lock().expect("hotkey_qc mutex").clone();
                        let _ = apply_hotkey(hwnd, HOTKEY_ID_QUICKCAPTURE, &spec);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_DEVTOOLS_CALL => {
            let req = unsafe { Box::from_raw(l_param.0 as *mut DevToolsRequest) };
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
            let wv_opt = if !ptr.is_null() {
                unsafe { &*ptr }.webview.borrow().clone()
            } else {
                None
            };
            let Some(wv) = wv_opt else {
                let _ = req.reply_tx.send(Err("webview not initialized".to_string()));
                return LRESULT(0);
            };
            let method_w = CoTaskMemPWSTR::from(req.method.as_str());
            let params_w = CoTaskMemPWSTR::from(req.params_json.as_str());
            let tx = req.reply_tx;
            let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
                move |hr, json_pwstr| {
                    if hr.is_err() {
                        let _ = tx.send(Err(format!("CDP error: {hr:?}")));
                    } else {
                        let out = json_pwstr.to_string();
                        let _ = tx.send(Ok(out));
                    }
                    Ok(())
                },
            ));
            if let Err(e) = unsafe {
                wv.CallDevToolsProtocolMethod(
                    *method_w.as_ref().as_pcwstr(),
                    *params_w.as_ref().as_pcwstr(),
                    &handler,
                )
            } {
                // Note: if CallDevToolsProtocolMethod fails synchronously, handler isn't called.
                eprintln!("CallDevToolsProtocolMethod error: {e}");
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            let id = w_param.0 as i32;
            if id == HOTKEY_ID_MAIN {
                let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_ACTIVATE, WPARAM(0), LPARAM(0)) };
            } else if id == HOTKEY_ID_QUICKCAPTURE {
                let _ = unsafe {
                    PostMessageW(
                        Some(hwnd),
                        WM_APP_WINDOW_CMD,
                        WPARAM(WIN_CMD_QUICK_CAPTURE),
                        LPARAM(0),
                    )
                };
            }
            LRESULT(0)
        }
        WM_TRAY_ICON => {
            let kind = (l_param.0 as u32) & 0xFFFF;
            if kind == WM_LBUTTONUP || kind == WM_LBUTTONDBLCLK {
                let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_ACTIVATE, WPARAM(0), LPARAM(0)) };
            } else if kind == WM_RBUTTONUP {
                match show_tray_menu(hwnd) {
                    Some(TRAY_CMD_SHOW) => {
                        let _ = unsafe {
                            PostMessageW(Some(hwnd), WM_APP_ACTIVATE, WPARAM(0), LPARAM(0))
                        };
                    }
                    Some(TRAY_CMD_QUICK_CAPTURE) => {
                        let _ = unsafe {
                            PostMessageW(
                                Some(hwnd),
                                WM_APP_WINDOW_CMD,
                                WPARAM(WIN_CMD_QUICK_CAPTURE),
                                LPARAM(0),
                            )
                        };
                    }
                    Some(TRAY_CMD_EXIT) => {
                        let _ = unsafe { DestroyWindow(hwnd) };
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
            let state_opt = if !ptr.is_null() {
                Some(unsafe { &*ptr })
            } else {
                None
            };
            if let Some(state) = state_opt {
                if let Some(wv) = state.webview.borrow().as_ref() {
                    let flush_w = CoTaskMemPWSTR::from(r#"{"topic":"host:flushSession","payload":{}}"#);
                    let _ = unsafe { wv.PostWebMessageAsJson(*flush_w.as_ref().as_pcwstr()) };
                }
                if is_tray_resident_enabled(&state.data_dir) {
                    hide_and_suspend_window(hwnd, Some(state));
                    return LRESULT(0);
                }
            }
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            remove_tray_icon(hwnd);
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    let _ = Box::from_raw(ptr);
                }
            }
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w_param, l_param) },
    }
}

/// Launches the single Win32 frame + WebView2 controller loop against `https://app.syki.local/`.
pub fn run_webview_host(
    config: WebViewHostConfig,
    rpc_handler: Arc<dyn Fn(&RpcRequest) -> RpcResponse + Send + Sync>,
    dynamic_handler: Option<Arc<DynamicRouteHandler>>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_webview_host_with_events(config, rpc_handler, dynamic_handler, None)
}

/// Launches the single Win32 frame + WebView2 controller loop with an [`EventSender`] callback
/// for pushing host-to-JS [`RpcEvent`] notifications onto the UI STA thread.
pub fn run_webview_host_with_events(
    config: WebViewHostConfig,
    rpc_handler: Arc<dyn Fn(&RpcRequest) -> RpcResponse + Send + Sync>,
    dynamic_handler: Option<Arc<DynamicRouteHandler>>,
    on_ready: Option<Box<dyn FnOnce(EventSender)>>,
) -> Result<(), Box<dyn std::error::Error>> {
    run_webview_host_with_handle(
        config,
        rpc_handler,
        dynamic_handler,
        on_ready.map(|cb| -> Box<dyn FnOnce(HostHandle)> { Box::new(move |h| cb(h.event_sender)) }),
    )
}

/// Launches the single Win32 frame + WebView2 controller loop with a [`HostHandle`] callback.
pub fn run_webview_host_with_handle(
    config: WebViewHostConfig,
    rpc_handler: Arc<dyn Fn(&RpcRequest) -> RpcResponse + Send + Sync>,
    dynamic_handler: Option<Arc<DynamicRouteHandler>>,
    on_ready: Option<Box<dyn FnOnce(HostHandle)>>,
) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    let _ = unsafe { SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE) };

    let class_name = CoTaskMemPWSTR::from("SykiLiteWindowClass");
    let title_w = CoTaskMemPWSTR::from(config.title.as_str());
    let hinstance = unsafe { GetModuleHandleW(None) }
        .ok()
        .map(|h| HINSTANCE(h.0));

    let app_icon = crate::host_lifecycle::load_app_icon();
    let wnd_class = WNDCLASSW {
        lpfnWndProc: Some(host_wnd_proc),
        hInstance: hinstance.unwrap_or_default(),
        hIcon: app_icon,
        hbrBackground: unsafe { CreateSolidBrush(COLORREF(0x001E_1E1E)) },
        lpszClassName: *class_name.as_ref().as_pcwstr(),
        ..Default::default()
    };
    let atom = unsafe { RegisterClassW(&wnd_class) };
    if atom == 0 {
        return Err(windows::core::Error::from_thread().into());
    }

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_APPWINDOW,
            *class_name.as_ref().as_pcwstr(),
            *title_w.as_ref().as_pcwstr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1280,
            820,
            None,
            None,
            hinstance,
            None,
        )?
    };

    let controller_slot: Rc<RefCell<Option<ICoreWebView2Controller>>> = Rc::new(RefCell::new(None));
    let webview_slot: Rc<RefCell<Option<ICoreWebView2>>> = Rc::new(RefCell::new(None));
    let env_slot: Rc<RefCell<Option<ICoreWebView2Environment>>> = Rc::new(RefCell::new(None));
    let event_queue: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    let startup_t0 = std::time::Instant::now();
    crate::assets::prewarm_boot_assets();
    let perf_params = load_perf_params_value();
    let browser_args = perf_params
        .as_ref()
        .and_then(|v| v.pointer("/webview/browserArgs"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(DEFAULT_BROWSER_ARGS)
        .to_string();
    let detach_commit_ms = perf_params
        .as_ref()
        .and_then(|v| v.pointer("/webview/detachCommitMs"))
        .and_then(|v| v.as_u64())
        .map(|ms| ms.clamp(80, 1000) as u32)
        .unwrap_or(200);
    let data_dir = config
        .user_data_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| syki_core::rpc::RuntimeEnv::from_env().data_dir);
    let (init_main_hk, init_qc_hk, mut idle_detach_ms) = read_startup_hotkeys_and_detach(&data_dir);
    if std::env::var("SYKI_IDLE_DETACH_MS").is_err() && idle_detach_ms == 300_000 {
        if let Some(p_ms) = perf_params
            .as_ref()
            .and_then(|v| v.pointer("/webview/idleDetachMs"))
            .and_then(|v| v.as_u64())
        {
            idle_detach_ms = p_ms.clamp(60_000, 600_000) as u32;
        }
    }
    let hotkey_main = Arc::new(Mutex::new(init_main_hk.clone()));
    let hotkey_qc = Arc::new(Mutex::new(init_qc_hk.clone()));
    let event_sender_slot: Rc<RefCell<Option<EventSender>>> = Rc::new(RefCell::new(None));
    let boxed_state = Box::new(WindowState {
        controller: Rc::clone(&controller_slot),
        webview: Rc::clone(&webview_slot),
        environment: Rc::clone(&env_slot),
        rpc_handler,
        dynamic_handler,
        virtual_origin: config.virtual_origin,
        debug: config.debug,
        event_queue: Arc::clone(&event_queue),
        canvas: RefCell::new(CanvasHostState::new()),
        hotkey_main: Arc::clone(&hotkey_main),
        hotkey_qc: Arc::clone(&hotkey_qc),
        data_dir,
        idle_detach_ms,
        detach_commit_ms,
        detach_guard: None,
        event_sender: Rc::clone(&event_sender_slot),
        startup_t0,
    });
    let state_ptr = Box::into_raw(boxed_state);
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
    }
    let _ = apply_hotkey(hwnd, HOTKEY_ID_MAIN, &init_main_hk);
    let _ = apply_hotkey(hwnd, HOTKEY_ID_QUICKCAPTURE, &init_qc_hk);
    let _ = add_tray_icon(hwnd, &config.title);
    unsafe {
        let dark_mode: i32 = 1;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_mode as *const _ as *const _,
            std::mem::size_of::<i32>() as u32,
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);
    }
    write_perf_trace_event("window_shown", startup_t0.elapsed().as_secs_f64() * 1000.0);

    let user_data_str = config.user_data_dir.to_string_lossy().into_owned();
    let browser_args_for_env = browser_args;
    let preload_rx = {
        let preload_dir = unsafe { &*state_ptr }.data_dir.clone();
        let (ptx, prx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = ptx.send(build_preload_script(&preload_dir));
        });
        prx
    };
    let environment = {
        let (tx, rx) = mpsc::channel();
        let user_data_for_env = user_data_str;
        CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| unsafe {
                let user_data_w = CoTaskMemPWSTR::from(user_data_for_env.as_str());
                let opts = webview2_com::CoreWebView2EnvironmentOptions::default();
                opts.set_additional_browser_arguments(browser_args_for_env.clone());
                let opts_iface: ICoreWebView2EnvironmentOptions = opts.into();
                CreateCoreWebView2EnvironmentWithOptions(
                    PCWSTR::null(),
                    *user_data_w.as_ref().as_pcwstr(),
                    &opts_iface,
                    &handler,
                )
                .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |error_code, env| {
                error_code?;
                let _ = tx.send(env.ok_or_else(|| windows::core::Error::from(E_POINTER)));
                Ok(())
            }),
        )?;
        rx.recv()
            .map_err(|_| webview2_com::Error::SendError)??
    };
    write_perf_trace_event("env_ready", startup_t0.elapsed().as_secs_f64() * 1000.0);

    *env_slot.borrow_mut() = Some(environment.clone());
    let state_ref = unsafe { &*state_ptr };
    let prebuilt_preload = preload_rx.recv().ok();
    attach_webview_controller(hwnd, &environment, state_ref, true, prebuilt_preload)?;
    write_perf_trace_event("controller_ready", startup_t0.elapsed().as_secs_f64() * 1000.0);

    if let Some(cb) = on_ready {
        let hwnd_raw = hwnd.0 as isize;
        let q_for_sender = Arc::clone(&event_queue);
        let sender: EventSender = Arc::new(move |ev: RpcEvent| {
            if let Ok(json) = serde_json::to_string(&ev) {
                q_for_sender.lock().expect("event_queue mutex").push_back(json);
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(hwnd_raw as *mut _)),
                        WM_APP_PUSH_EVENT,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
        });
        *event_sender_slot.borrow_mut() = Some(sender.clone());
        let qc_data_dir = state_ref.data_dir.clone();
        let qc_sender = sender.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(800));
            crate::quick_capture::prewarm_quick_capture_popup(qc_data_dir, Some(qc_sender));
        });
        cb(HostHandle {
            event_sender: sender,
            hwnd_raw,
            hotkey_main,
            hotkey_qc,
        });
    }

    let mut msg = MSG::default();
    loop {
        let res = unsafe { GetMessageW(&mut msg, None, 0, 0).0 };
        match res {
            -1 => return Err(windows::core::Error::from_thread().into()),
            0 => break,
            _ => unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            },
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_rpc_message_handles_tracer_methods_and_errors() {
        let version_reply = dispatch_rpc_message(
            r#"{"id":"1","method":"getAppVersion","params":{}}"#,
            &default_tracer_handler,
        );
        let parsed: RpcResponse = serde_json::from_str(&version_reply).expect("valid response");
        assert!(parsed.ok);
        assert_eq!(parsed.id, "1");
        assert_eq!(parsed.result, Some(serde_json::json!(syki_core::APP_VERSION)));

        let ready_reply = dispatch_rpc_message(
            r#"{"id":"2","method":"uiReady","params":{}}"#,
            &default_tracer_handler,
        );
        let ready: RpcResponse = serde_json::from_str(&ready_reply).expect("valid response");
        assert!(ready.ok);
        assert_eq!(ready.result, Some(serde_json::json!(true)));

        let unknown_reply = dispatch_rpc_message(
            r#"{"id":"3","method":"nope","params":{}}"#,
            &default_tracer_handler,
        );
        let unknown: RpcResponse = serde_json::from_str(&unknown_reply).expect("valid response");
        assert!(!unknown.ok);
        assert_eq!(unknown.error.as_deref(), Some("unknown method: nope"));

        let bad_json = dispatch_rpc_message("{not json", &default_tracer_handler);
        let bad: RpcResponse = serde_json::from_str(&bad_json).expect("valid error envelope");
        assert!(!bad.ok);
        assert!(bad
            .error
            .as_deref()
            .unwrap_or("")
            .starts_with("invalid rpc request:"));
    }

    #[test]
    fn resolve_virtual_request_enforces_origin_and_routes_dynamic_paths() {
        let dynamic: DynamicRouteHandler = Box::new(|path| {
            if path == "/api/print/preview.pdf" {
                Some((200, "application/pdf", b"%PDF-1.4".to_vec()))
            } else {
                None
            }
        });

        let root = resolve_virtual_request("https://app.syki.local/", Some(&dynamic))
            .expect("root index serves");
        assert_eq!(root.0, 200);
        assert_eq!(root.1, "text/html; charset=utf-8");

        let proxy = resolve_virtual_request(
            "https://app.syki.local/js/backend_proxy.js?v=1.0.0",
            Some(&dynamic),
        )
        .expect("backend_proxy.js serves");
        assert_eq!(proxy.0, 200);
        assert_eq!(proxy.1, "application/javascript; charset=utf-8");

        let pdf = resolve_virtual_request(
            "https://app.syki.local/api/print/preview.pdf",
            Some(&dynamic),
        )
        .expect("dynamic pdf route serves");
        assert_eq!(pdf, (200, "application/pdf", b"%PDF-1.4".to_vec()));

        assert!(
            resolve_virtual_request("https://evil.example/index.html", Some(&dynamic)).is_none(),
            "external origins must never fall through"
        );
        assert!(
            resolve_virtual_request("http://app.syki.local/index.html", Some(&dynamic)).is_none(),
            "non-https origin must be rejected"
        );
    }

    #[test]
    fn tray_resident_defaults_true_and_respects_false() {
        let dir = std::env::temp_dir().join(format!("syki_tray_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");

        assert!(is_tray_resident_enabled(&dir));

        std::fs::write(dir.join("config.json"), r#"{"general":{"language":"ja"}}"#).unwrap();
        assert!(is_tray_resident_enabled(&dir));

        std::fs::write(dir.join("config.json"), r#"{"general":{"trayResident":false}}"#).unwrap();
        assert!(!is_tray_resident_enabled(&dir));

        std::fs::write(dir.join("config.json"), r#"{"general":{"trayResident":true}}"#).unwrap();
        assert!(is_tray_resident_enabled(&dir));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn response_headers_apply_csp_to_index_and_no_store_to_ext() {
        let root_hdr = build_response_headers(200, "text/html; charset=utf-8", "");
        assert!(root_hdr.contains("Cache-Control: no-store"));
        assert!(root_hdr.contains(&format!(
            "Content-Security-Policy: {}",
            crate::ext_store::content_security_policy()
        )));

        let idx_hdr = build_response_headers(200, "text/html; charset=utf-8", "index.html");
        assert!(idx_hdr.contains("Content-Security-Policy:"));

        let ext_hdr = build_response_headers(200, "text/css; charset=utf-8", "__ext/r/1/user.css");
        assert!(ext_hdr.contains("Cache-Control: no-store"));
        assert!(!ext_hdr.contains("Content-Security-Policy:"));

        let js_hdr =
            build_response_headers(200, "application/javascript; charset=utf-8", "js/app.js");
        assert!(js_hdr.contains("Cache-Control: public, max-age=31536000, immutable"));
        assert!(!js_hdr.contains("Content-Security-Policy:"));
    }

    #[test]
    fn preload_script_includes_ext_flags_and_preserves_existing_keys() {
        let dir = std::env::temp_dir().join(format!("syki_preload_ext_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join("user.css"), b"body {}").unwrap();

        let script = build_preload_script(&dir);
        let json_str = script
            .strip_prefix("window.__sykiPreload = ")
            .and_then(|s| s.strip_suffix(';'))
            .expect("valid preload assignment");
        let parsed: serde_json::Value = serde_json::from_str(json_str).expect("valid json");
        assert!(parsed.get("config").is_some());
        assert!(parsed.get("session").is_some());
        assert_eq!(parsed["caps"]["os"], "win32");
        assert_eq!(parsed["ext"]["hasUserCss"], true);
        assert!(parsed["ext"]["hasPlugins"].is_boolean());
        assert!(parsed["ext"]["safeMode"].is_boolean());
        assert!(parsed["ext"]["isPortableCss"].is_boolean());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
