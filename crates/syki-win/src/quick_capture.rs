//! Pure Win32 Native Quick Capture Popup (`F10a` / `#09`) — zero WebView2 overhead.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde_json::json;
use syki_core::rpc::RpcEvent;
use syki_core::storage::{ConfigStore, JsonConfigStore, NoteFileStore, ScrapStore};
use syki_core::storage::note::cyrb53_utf16_sig;
use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, GetStockObject, CLEARTYPE_QUALITY, DEFAULT_CHARSET, DEFAULT_GUI_FONT,
    OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, DEFAULT_PITCH, FF_SWISS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, VK_CONTROL, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect,
    GetSystemMetrics, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, RegisterClassW,
    SetForegroundWindow, SetWindowLongPtrW, ShowWindow, SendMessageW, GWLP_USERDATA, GWLP_WNDPROC,
    SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND,
    WM_DESTROY, WM_KEYDOWN, WM_SETFONT, WM_SIZE, WNDCLASSW, WNDPROC, WS_CAPTION, WS_CHILD,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::rpc_dispatch::EventSender;

const ID_EDIT: usize = 101;
const ID_BTN_SAVE: usize = 102;
const ID_BTN_CANCEL: usize = 103;

const ES_MULTILINE: u32 = 0x0004;
const ES_AUTOVSCROLL: u32 = 0x0040;
const ES_WANTRETURN: u32 = 0x1000;
const BS_DEFPUSHBUTTON: u32 = 0x0001;
const BN_CLICKED: u32 = 0;

static POPUP_OPEN: AtomicBool = AtomicBool::new(false);
static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

struct QuickCaptureContext {
    data_dir: PathBuf,
    event_sender: Option<EventSender>,
    edit_hwnd: isize,
    btn_save: isize,
    btn_cancel: isize,
    orig_edit_proc: isize,
}

static ACTIVE_CTX: Mutex<Option<Box<QuickCaptureContext>>> = Mutex::new(None);

/// Saves a quick-capture note to today's daily scrap and returns the scrap path.
pub fn submit_quick_capture_text(
    data_dir: &PathBuf,
    raw_text: &str,
    event_sender: Option<&EventSender>,
) -> Result<Option<PathBuf>, String> {
    let trimmed = raw_text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let cfg = JsonConfigStore::new(data_dir.join("config.json"));
    let scrap_dir = cfg.scrap_dir();
    let scrap_path = ScrapStore::append_formatted(&scrap_dir, trimmed, "Quick Capture")
        .map_err(|e| format!("quick capture append failed: {e}"))?;

    if let Some(sender) = event_sender {
        if let Ok(loaded) = NoteFileStore::read_note(&scrap_path) {
            sender(RpcEvent {
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
    }
    Ok(Some(scrap_path))
}

unsafe extern "system" fn edit_subclass_proc(
    hwnd: HWND,
    msg: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if msg == WM_KEYDOWN {
        let vk = w_param.0 as u16;
        if vk == VK_ESCAPE.0 {
            if let Ok(guard) = ACTIVE_CTX.lock() {
                if let Some(ctx) = guard.as_ref() {
                    let parent = windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd).unwrap_or_default();
                    let _ = ctx;
                    let _ = DestroyWindow(parent);
                    return LRESULT(0);
                }
            }
        } else if vk == VK_RETURN.0 && (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0 {
            if let Ok(parent) = windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd) {
                let _ = SendMessageW(
                    parent,
                    WM_COMMAND,
                    Some(WPARAM(ID_BTN_SAVE)),
                    Some(LPARAM(0)),
                );
                return LRESULT(0);
            }
        }
    }
    let orig = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if orig != 0 {
        let proc_fn: WNDPROC = Some(std::mem::transmute(orig));
        return CallWindowProcW(proc_fn, hwnd, msg, w_param, l_param);
    }
    DefWindowProcW(hwnd, msg, w_param, l_param)
}

unsafe extern "system" fn quick_capture_wnd_proc(
    hwnd: HWND,
    msg: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match msg {
        WM_SIZE => {
            if let Ok(guard) = ACTIVE_CTX.lock() {
                if let Some(ctx) = guard.as_ref() {
                    let mut rc = windows::Win32::Foundation::RECT::default();
                    let _ = GetClientRect(hwnd, &mut rc);
                    let w = (rc.right - rc.left).max(200);
                    let h = (rc.bottom - rc.top).max(120);
                    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                        HWND(ctx.edit_hwnd as *mut _),
                        12,
                        12,
                        w - 24,
                        h - 56,
                        true,
                    );
                    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                        HWND(ctx.btn_cancel as *mut _),
                        w - 196,
                        h - 36,
                        84,
                        26,
                        true,
                    );
                    let _ = windows::Win32::UI::WindowsAndMessaging::MoveWindow(
                        HWND(ctx.btn_save as *mut _),
                        w - 104,
                        h - 36,
                        92,
                        26,
                        true,
                    );
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let cmd_id = w_param.0 & 0xFFFF;
            let notify = (w_param.0 >> 16) as u32;
            if cmd_id == ID_BTN_SAVE && (notify == BN_CLICKED || l_param.0 == 0) {
                let text = if let Ok(guard) = ACTIVE_CTX.lock() {
                    if let Some(ctx) = guard.as_ref() {
                        let edit = HWND(ctx.edit_hwnd as *mut _);
                        let len = GetWindowTextLengthW(edit);
                        let mut buf = vec![0u16; (len as usize) + 1];
                        let copied = GetWindowTextW(edit, &mut buf);
                        let s = String::from_utf16_lossy(&buf[..copied as usize]);
                        Some((s, ctx.data_dir.clone(), ctx.event_sender.clone()))
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some((body, dir, sender)) = text {
                    let _ = submit_quick_capture_text(&dir, &body, sender.as_ref());
                }
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            } else if cmd_id == ID_BTN_CANCEL {
                let _ = DestroyWindow(hwnd);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, w_param, l_param)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Ok(mut guard) = ACTIVE_CTX.lock() {
                if let Some(ctx) = guard.take() {
                    if ctx.orig_edit_proc != 0 {
                        let _ = SetWindowLongPtrW(
                            HWND(ctx.edit_hwnd as *mut _),
                            GWLP_WNDPROC,
                            ctx.orig_edit_proc,
                        );
                    }
                }
            }
            POPUP_OPEN.store(false, Ordering::Release);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w_param, l_param),
    }
}

/// Opens the native Win32 Quick Capture popup on the UI thread (no-op if already open).
pub fn open_quick_capture_popup(data_dir: PathBuf, event_sender: Option<EventSender>) {
    if POPUP_OPEN.swap(true, Ordering::AcqRel) {
        return;
    }

    unsafe {
        if !CLASS_REGISTERED.swap(true, Ordering::AcqRel) {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(quick_capture_wnd_proc),
                lpszClassName: w!("SykiQuickCaptureClass"),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);
        }

        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        let width = 540;
        let height = 220;
        let x = ((sw - width) / 2).max(0);
        let y = ((sh - height) / 3).max(0);

        let title = HSTRING::from("Quick Capture (Ctrl+Enter to Save, Esc to Cancel)");
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            w!("SykiQuickCaptureClass"),
            &title,
            WINDOW_STYLE(WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0),
            x,
            y,
            width,
            height,
            None,
            None,
            None,
            None,
        ) else {
            POPUP_OPEN.store(false, Ordering::Release);
            return;
        };

        let edit_hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("EDIT"),
            w!(""),
            WINDOW_STYLE(
                WS_CHILD.0
                    | WS_VISIBLE.0
                    | WS_VSCROLL.0
                    | WS_TABSTOP.0
                    | ES_MULTILINE
                    | ES_AUTOVSCROLL
                    | ES_WANTRETURN,
            ),
            12,
            12,
            width - 36,
            height - 96,
            Some(hwnd),
            Some(windows::Win32::UI::WindowsAndMessaging::HMENU(ID_EDIT as *mut _)),
            None,
            None,
        )
        .unwrap_or_default();

        let btn_cancel = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("Cancel"),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0),
            width - 210,
            height - 74,
            84,
            26,
            Some(hwnd),
            Some(windows::Win32::UI::WindowsAndMessaging::HMENU(ID_BTN_CANCEL as *mut _)),
            None,
            None,
        )
        .unwrap_or_default();

        let btn_save = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("Save"),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_DEFPUSHBUTTON),
            width - 118,
            height - 74,
            92,
            26,
            Some(hwnd),
            Some(windows::Win32::UI::WindowsAndMessaging::HMENU(ID_BTN_SAVE as *mut _)),
            None,
            None,
        )
        .unwrap_or_default();

        let hfont = CreateFontW(
            -15,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (DEFAULT_PITCH.0 | FF_SWISS.0) as u32,
            w!("Segoe UI"),
        );
        let font_handle = if hfont.is_invalid() {
            GetStockObject(DEFAULT_GUI_FONT)
        } else {
            windows::Win32::Graphics::Gdi::HGDIOBJ(hfont.0)
        };
        for ctrl in [edit_hwnd, btn_cancel, btn_save] {
            let _ = SendMessageW(
                ctrl,
                WM_SETFONT,
                Some(WPARAM(font_handle.0 as usize)),
                Some(LPARAM(1)),
            );
        }

        let orig_edit_proc = SetWindowLongPtrW(
            edit_hwnd,
            GWLP_WNDPROC,
            edit_subclass_proc as *const () as isize,
        );
        SetWindowLongPtrW(edit_hwnd, GWLP_USERDATA, orig_edit_proc);

        if let Ok(mut guard) = ACTIVE_CTX.lock() {
            *guard = Some(Box::new(QuickCaptureContext {
                data_dir,
                event_sender,
                edit_hwnd: edit_hwnd.0 as isize,
                btn_save: btn_save.0 as isize,
                btn_cancel: btn_cancel.0 as isize,
                orig_edit_proc,
            }));
        }

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(edit_hwnd));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn submit_quick_capture_appends_to_scrap_and_emits_open_file_event() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("syki-qc-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(submit_quick_capture_text(&dir, "   ", None).unwrap(), None);

        let (tx, rx) = std::sync::mpsc::channel::<RpcEvent>();
        let sender: EventSender = Arc::new(move |ev| {
            let _ = tx.send(ev);
        });
        let saved = submit_quick_capture_text(&dir, "quick thought", Some(&sender))
            .unwrap()
            .expect("saved scrap");
        assert!(saved.is_file());
        let content = std::fs::read_to_string(&saved).unwrap();
        assert!(content.contains("Quick Capture"));
        assert!(content.contains("quick thought"));

        let ev = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(ev.topic, "host:openFile");

        let _ = std::fs::remove_dir_all(dir);
    }
}
