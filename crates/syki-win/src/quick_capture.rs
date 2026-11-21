//! Pure Win32 Native Quick Capture Popup (`F10a` / `#09`) — zero WebView2 overhead.
//! Styled to match syki-lite's dark theme (`#252526` surface, `#3c3c3c` input, owner-drawn buttons).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde_json::json;
use syki_core::rpc::RpcEvent;
use syki_core::storage::note::cyrb53_utf16_sig;
use syki_core::storage::{ConfigStore, JsonConfigStore, NoteFileStore, ScrapStore};
use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint,
    FillRect, GetStockObject, InvalidateRect, RoundRect, SelectObject, SetBkColor, SetBkMode,
    SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_GUI_FONT,
    DEFAULT_PITCH, DT_CENTER, DT_SINGLELINE, DT_VCENTER, FF_SWISS, HGDIOBJ,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_CONTROL, VK_ESCAPE, VK_RETURN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetParent,
    GetSystemMetrics, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, LoadCursorW,
    RegisterClassW, SendMessageW, SetForegroundWindow, SetLayeredWindowAttributes,
    SetWindowLongPtrW, ShowWindow, GWLP_USERDATA, GWLP_WNDPROC, HMENU, IDC_ARROW, LWA_ALPHA,
    SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLOREDIT, WM_DESTROY, WM_DRAWITEM, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_NCHITTEST, WM_PAINT, WM_SETFONT, WNDCLASSW, WNDPROC, WS_CHILD, WS_CLIPCHILDREN,
    WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_TABSTOP, WS_VISIBLE,
};

use crate::rpc_dispatch::EventSender;

const ID_EDIT: usize = 101;
const ID_BTN_SAVE: usize = 102;
const ID_BTN_CANCEL: usize = 103;

const ES_MULTILINE: u32 = 0x0004;
const ES_AUTOVSCROLL: u32 = 0x0040;
const ES_WANTRETURN: u32 = 0x1000;
const BS_OWNERDRAW: u32 = 0x000B;
const BN_CLICKED: u32 = 0;
const ODS_SELECTED: u32 = 0x0001;
const HTCAPTION: isize = 2;

// Dark palette matching frontend/css/style.css (0x00BBGGRR)
const COL_BG: u32 = 0x0026_2525;
const COL_FIELD: u32 = 0x003c_3c3c;
const COL_BORDER: u32 = 0x004a_4646;
const COL_BTN_CANCEL: u32 = 0x0033_3333;
const COL_BTN_SAVE: u32 = 0x00eb_6325;
const COL_TEXT: u32 = 0x00d4_d4d4;
const COL_TEXT_WHITE: u32 = 0x00ff_ffff;
const COL_HINT: u32 = 0x0088_8888;

static POPUP_OPEN: AtomicBool = AtomicBool::new(false);
static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

struct QuickCaptureContext {
    data_dir: PathBuf,
    event_sender: Option<EventSender>,
    edit_hwnd: isize,
    orig_edit_proc: isize,
    bg_brush: isize,
    field_brush: isize,
    font: isize,
    hint_font: isize,
}

static ACTIVE_CTX: Mutex<Option<Box<QuickCaptureContext>>> = Mutex::new(None);

#[repr(C)]
struct DrawItemStruct {
    ctl_type: u32,
    ctl_id: u32,
    item_id: u32,
    item_action: u32,
    item_state: u32,
    hwnd_item: HWND,
    hdc: windows::Win32::Graphics::Gdi::HDC,
    rc_item: RECT,
    item_data: usize,
}

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
            if let Ok(parent) = GetParent(hwnd) {
                let _ = DestroyWindow(parent);
                return LRESULT(0);
            }
        } else if vk == VK_RETURN.0 && (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0 {
            if let Ok(parent) = GetParent(hwnd) {
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

unsafe fn draw_round_button(dis: &DrawItemStruct) {
    let is_save = dis.ctl_id as usize == ID_BTN_SAVE;
    let pressed = (dis.item_state & ODS_SELECTED) != 0;
    let fill_col = if is_save {
        if pressed { 0x00d4_531d } else { COL_BTN_SAVE }
    } else if pressed {
        0x0044_4444
    } else {
        COL_BTN_CANCEL
    };
    let border_col = if is_save { COL_BTN_SAVE } else { COL_BORDER };

    let bg_brush = CreateSolidBrush(COLORREF(COL_BG));
    FillRect(dis.hdc, &dis.rc_item, bg_brush);
    let _ = DeleteObject(HGDIOBJ(bg_brush.0));

    let brush = CreateSolidBrush(COLORREF(fill_col));
    let pen = CreatePen(PS_SOLID, 1, COLORREF(border_col));
    let old_b = SelectObject(dis.hdc, HGDIOBJ(brush.0));
    let old_p = SelectObject(dis.hdc, HGDIOBJ(pen.0));
    let _ = RoundRect(
        dis.hdc,
        dis.rc_item.left,
        dis.rc_item.top,
        dis.rc_item.right,
        dis.rc_item.bottom,
        8,
        8,
    );
    SelectObject(dis.hdc, old_b);
    SelectObject(dis.hdc, old_p);
    let _ = DeleteObject(HGDIOBJ(brush.0));
    let _ = DeleteObject(HGDIOBJ(pen.0));

    SetBkMode(dis.hdc, TRANSPARENT);
    SetTextColor(
        dis.hdc,
        COLORREF(if is_save { COL_TEXT_WHITE } else { COL_TEXT }),
    );
    let mut text: Vec<u16> = if is_save {
        "Save\0".encode_utf16().collect()
    } else {
        "Cancel\0".encode_utf16().collect()
    };
    let mut rc = dis.rc_item;
    let n = text.len() - 1;
    let _ = DrawTextW(
        dis.hdc,
        &mut text[..n],
        &mut rc,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
}

unsafe extern "system" fn quick_capture_wnd_proc(
    hwnd: HWND,
    msg: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_NCHITTEST => {
            let hit = DefWindowProcW(hwnd, msg, w_param, l_param);
            if hit.0 == 1 {
                LRESULT(HTCAPTION)
            } else {
                hit
            }
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);

            let bg = CreateSolidBrush(COLORREF(COL_BG));
            FillRect(hdc, &rc, bg);
            let _ = DeleteObject(HGDIOBJ(bg.0));

            let border_pen = CreatePen(PS_SOLID, 1, COLORREF(COL_BORDER));
            let old_p = SelectObject(hdc, HGDIOBJ(border_pen.0));
            let old_b = SelectObject(hdc, GetStockObject(windows::Win32::Graphics::Gdi::NULL_BRUSH));
            let _ = RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, 10, 10);

            // Paint rounded dark field backing around the inset EDIT control
            let field_brush = CreateSolidBrush(COLORREF(COL_FIELD));
            SelectObject(hdc, HGDIOBJ(field_brush.0));
            let _ = RoundRect(hdc, 14, 14, rc.right - 14, rc.bottom - 52, 8, 8);
            SelectObject(hdc, old_b);
            SelectObject(hdc, old_p);
            let _ = DeleteObject(HGDIOBJ(field_brush.0));
            let _ = DeleteObject(HGDIOBJ(border_pen.0));

            // Hint label on bottom-left
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(COL_HINT));
            if let Ok(guard) = ACTIVE_CTX.lock() {
                if let Some(ctx) = guard.as_ref() {
                    if ctx.hint_font != 0 {
                        SelectObject(hdc, HGDIOBJ(ctx.hint_font as *mut _));
                    }
                }
            }
            let mut hint_rc = RECT {
                left: 16,
                top: rc.bottom - 40,
                right: rc.right - 200,
                bottom: rc.bottom - 12,
            };
            let mut hint_w: Vec<u16> = "Ctrl+Enter: Save   Esc: Cancel"
                .encode_utf16()
                .collect();
            let _ = DrawTextW(hdc, &mut hint_w, &mut hint_rc, DT_VCENTER | DT_SINGLELINE);

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_CTLCOLOREDIT => {
            let hdc = windows::Win32::Graphics::Gdi::HDC(w_param.0 as *mut _);
            SetTextColor(hdc, COLORREF(COL_TEXT));
            SetBkColor(hdc, COLORREF(COL_FIELD));
            if let Ok(guard) = ACTIVE_CTX.lock() {
                if let Some(ctx) = guard.as_ref() {
                    return LRESULT(ctx.field_brush);
                }
            }
            DefWindowProcW(hwnd, msg, w_param, l_param)
        }
        WM_DRAWITEM => {
            if l_param.0 != 0 {
                let dis = &*(l_param.0 as *const DrawItemStruct);
                draw_round_button(dis);
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, w_param, l_param)
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
        WM_LBUTTONDOWN => {
            let _ = InvalidateRect(Some(hwnd), None, false);
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
                    if ctx.bg_brush != 0 {
                        let _ = DeleteObject(HGDIOBJ(ctx.bg_brush as *mut _));
                    }
                    if ctx.field_brush != 0 {
                        let _ = DeleteObject(HGDIOBJ(ctx.field_brush as *mut _));
                    }
                    if ctx.font != 0 {
                        let _ = DeleteObject(HGDIOBJ(ctx.font as *mut _));
                    }
                    if ctx.hint_font != 0 {
                        let _ = DeleteObject(HGDIOBJ(ctx.hint_font as *mut _));
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
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: w!("SykiQuickCaptureClass"),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);
        }

        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        let width = 600;
        let height = 180;
        let x = ((sw - width) / 2).max(0);
        let y = ((sh - height) / 4).max(0);

        let title = HSTRING::from("Quick Capture");
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            w!("SykiQuickCaptureClass"),
            &title,
            WINDOW_STYLE(WS_POPUP.0 | WS_CLIPCHILDREN.0),
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

        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 240, LWA_ALPHA);

        let edit_hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("EDIT"),
            w!(""),
            WINDOW_STYLE(
                WS_CHILD.0
                    | WS_VISIBLE.0
                    | WS_TABSTOP.0
                    | ES_MULTILINE
                    | ES_AUTOVSCROLL
                    | ES_WANTRETURN,
            ),
            24,
            22,
            width - 48,
            height - 82,
            Some(hwnd),
            Some(HMENU(ID_EDIT as *mut _)),
            None,
            None,
        )
        .unwrap_or_default();

        let btn_cancel = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("Cancel"),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW),
            width - 196,
            height - 42,
            84,
            28,
            Some(hwnd),
            Some(HMENU(ID_BTN_CANCEL as *mut _)),
            None,
            None,
        )
        .unwrap_or_default();

        let btn_save = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("Save"),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW),
            width - 102,
            height - 42,
            88,
            28,
            Some(hwnd),
            Some(HMENU(ID_BTN_SAVE as *mut _)),
            None,
            None,
        )
        .unwrap_or_default();

        let hfont = CreateFontW(
            -16,
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
        let hint_font = CreateFontW(
            -13,
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
        let font_handle: HGDIOBJ = if hfont.is_invalid() {
            GetStockObject(DEFAULT_GUI_FONT)
        } else {
            HGDIOBJ(hfont.0)
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

        let bg_brush = CreateSolidBrush(COLORREF(COL_BG));
        let field_brush = CreateSolidBrush(COLORREF(COL_FIELD));

        if let Ok(mut guard) = ACTIVE_CTX.lock() {
            *guard = Some(Box::new(QuickCaptureContext {
                data_dir,
                event_sender,
                edit_hwnd: edit_hwnd.0 as isize,
                orig_edit_proc,
                bg_brush: bg_brush.0 as isize,
                field_brush: field_brush.0 as isize,
                font: if hfont.is_invalid() { 0 } else { hfont.0 as isize },
                hint_font: if hint_font.is_invalid() { 0 } else { hint_font.0 as isize },
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
