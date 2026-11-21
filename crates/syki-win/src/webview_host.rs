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
    ICoreWebView2Controller2, ICoreWebView2EnvironmentOptions, ICoreWebView2_3,
    COREWEBVIEW2_COLOR, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
};
use webview2_com::{
    CallDevToolsProtocolMethodCompletedHandler, CoTaskMemPWSTR,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    TrySuspendCompletedHandler, WebMessageReceivedEventHandler, WebResourceRequestedEventHandler,
};
use windows::core::{PCWSTR, PWSTR};
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
    GetWindowLongPtrW, IsZoomed, PostMessageW, PostQuitMessage, RegisterClassW,
    SetForegroundWindow, SetWindowLongPtrW, ShowWindow, TranslateMessage, CW_USEDEFAULT,
    GWLP_USERDATA, MSG, SW_HIDE, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOW,
    WM_APP, WM_CLOSE, WM_DESTROY, WM_HOTKEY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP,
    WM_SIZE, WNDCLASSW, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW,
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

struct WindowState {
    controller: Rc<RefCell<Option<ICoreWebView2Controller>>>,
    webview: Rc<RefCell<Option<ICoreWebView2>>>,
    event_queue: Arc<Mutex<VecDeque<String>>>,
    canvas: RefCell<CanvasHostState>,
    hotkey_main: Arc<Mutex<String>>,
    hotkey_qc: Arc<Mutex<String>>,
    data_dir: PathBuf,
    event_sender: Rc<RefCell<Option<EventSender>>>,
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
        }
    });
    format!("window.__sykiPreload = {payload};")
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

fn hide_and_suspend_window(hwnd: HWND, state: Option<&WindowState>) {
    if let Some(state) = state {
        state.canvas.borrow_mut().suspend();
        if let Some(ctrl) = state.controller.borrow().as_ref() {
            let _ = unsafe { ctrl.SetIsVisible(false) };
        }
        if let Some(wv) = state.webview.borrow().as_ref() {
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
    }
    let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
}

extern "system" fn host_wnd_proc(
    hwnd: HWND,
    msg: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match msg {
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
                state.canvas.borrow_mut().wake();
                if let Some(wv) = state.webview.borrow().as_ref() {
                    if let Ok(wv3) = windows::core::Interface::cast::<ICoreWebView2_3>(wv) {
                        let _ = unsafe { wv3.Resume() };
                    }
                }
                if let Some(ctrl) = state.controller.borrow().as_ref() {
                    let _ = unsafe { ctrl.SetIsVisible(true) };
                }
            }
            unsafe {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
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
    let event_queue: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    let hotkey_main = Arc::new(Mutex::new(String::new()));
    let hotkey_qc = Arc::new(Mutex::new(String::new()));
    let event_sender_slot: Rc<RefCell<Option<EventSender>>> = Rc::new(RefCell::new(None));
    let data_dir = config
        .user_data_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| syki_core::rpc::RuntimeEnv::from_env().data_dir);
    let data_dir_for_preload = data_dir.clone();
    let boxed_state = Box::new(WindowState {
        controller: Rc::clone(&controller_slot),
        webview: Rc::clone(&webview_slot),
        event_queue: Arc::clone(&event_queue),
        canvas: RefCell::new(CanvasHostState::new()),
        hotkey_main: Arc::clone(&hotkey_main),
        hotkey_qc: Arc::clone(&hotkey_qc),
        data_dir,
        event_sender: Rc::clone(&event_sender_slot),
    });
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(boxed_state) as isize);
    }
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

    let user_data_str = config.user_data_dir.to_string_lossy().into_owned();
    let environment = {
        let (tx, rx) = mpsc::channel();
        let user_data_for_env = user_data_str;
        CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| unsafe {
                let user_data_w = CoTaskMemPWSTR::from(user_data_for_env.as_str());
                let opts = webview2_com::CoreWebView2EnvironmentOptions::default();
                opts.set_additional_browser_arguments(
                    "--disable-background-networking --disable-component-update --no-pings"
                        .to_string(),
                );
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
        controller.SetIsVisible(true)?;
    }
    *controller_slot.borrow_mut() = Some(controller.clone());

    let webview = unsafe { controller.CoreWebView2()? };
    *webview_slot.borrow_mut() = Some(webview.clone());
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
        cb(HostHandle {
            event_sender: sender,
            hwnd_raw,
            hotkey_main,
            hotkey_qc,
        });
    }
    if !config.debug {
        unsafe {
            let settings = webview.Settings()?;
            settings.SetAreDefaultContextMenusEnabled(false)?;
            settings.SetAreDevToolsEnabled(false)?;
        }
    }

    let filter_pattern = CoTaskMemPWSTR::from(format!("{}*", config.virtual_origin).as_str());
    unsafe {
        webview.AddWebResourceRequestedFilter(
            *filter_pattern.as_ref().as_pcwstr(),
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
        )?;
    }

    let env_for_res = environment.clone();
    let dyn_for_res = dynamic_handler.clone();
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

                let (status, mime, body) = match resolve_virtual_request(
                    &uri,
                    dyn_for_res.as_deref(),
                ) {
                    Some(hit) => hit,
                    None if !uri.starts_with(VIRTUAL_ORIGIN) => {
                        (403, "text/plain; charset=utf-8", b"Forbidden".to_vec())
                    }
                    None => (404, "text/plain; charset=utf-8", b"Not Found".to_vec()),
                };

                let stream: Option<IStream> = SHCreateMemStream(Some(&body));
                let headers = format!("Content-Type: {mime}\r\nCache-Control: no-store");
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

    let rpc_for_msg = rpc_handler.clone();
    let q_for_msg = Arc::clone(&event_queue);
    let hwnd_for_msg = hwnd.0 as isize;
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
                    let rpc = Arc::clone(&rpc_for_msg);
                    let q = Arc::clone(&q_for_msg);
                    std::thread::spawn(move || {
                        let reply_json = dispatch_rpc_message(&raw_json, rpc.as_ref());
                        q.lock().expect("event_queue mutex").push_back(reply_json);
                        let _ = PostMessageW(
                            Some(HWND(hwnd_for_msg as *mut _)),
                            WM_APP_PUSH_EVENT,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    });
                }
                Ok(())
            })),
            &mut msg_token,
        )?;
    }

    let preload_script = build_preload_script(&data_dir_for_preload);
    let preload_w = CoTaskMemPWSTR::from(preload_script.as_str());
    let origin_w = CoTaskMemPWSTR::from(config.virtual_origin);
    unsafe {
        let _ = webview.AddScriptToExecuteOnDocumentCreated(*preload_w.as_ref().as_pcwstr(), None);
        webview.Navigate(*origin_w.as_ref().as_pcwstr())?;
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
}
