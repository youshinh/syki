//! Pure Win32 Native Quick Capture Popup (`F10a` / `#09`) — zero WebView2 overhead.
//! Supports multi-image screen snapshots (`imgcap`), WASAPI mic voice input (`speech`),
//! dark GDI surface (`#252526`), `WM_CTLCOLOREDIT`, and owner-drawn flat buttons.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::json;
use syki_core::rpc::RpcEvent;
use syki_core::storage::atomic::write_atomic_bytes;
use syki_core::storage::note::cyrb53_utf16_sig;
use syki_core::storage::{ConfigStore, JsonConfigStore, NoteFileStore, ScrapStore};
use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint,
    FillRect, GetStockObject, InvalidateRect, RoundRect, SelectObject, SetBkColor, SetBkMode,
    SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_GUI_FONT,
    DEFAULT_PITCH, DT_CENTER, DT_SINGLELINE, DT_VCENTER, FF_SWISS, HGDIOBJ, NULL_BRUSH,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, WAVEFORMATEX,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_CONTROL, VK_ESCAPE, VK_RETURN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetDlgItem,
    GetParent, GetSystemMetrics, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW,
    LoadCursorW, RegisterClassW, SendMessageW, SetForegroundWindow, SetLayeredWindowAttributes,
    SetWindowLongPtrW, SetWindowTextW, ShowWindow, GWLP_USERDATA, GWLP_WNDPROC, HMENU, IDC_ARROW,
    LWA_ALPHA, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT, WM_DESTROY, WM_DRAWITEM, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_NCHITTEST, WM_PAINT, WM_SETFONT, WNDCLASSW, WNDPROC, WS_CHILD,
    WS_CLIPCHILDREN, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_TABSTOP,
    WS_VISIBLE,
};

use crate::assets_store;
use crate::imgcap;
use crate::rpc_dispatch::EventSender;
use crate::speech;

const ID_EDIT: usize = 101;
const ID_BTN_SAVE: usize = 102;
const ID_BTN_CANCEL: usize = 103;
const ID_BTN_CAPTURE: usize = 104;
const ID_BTN_MIC: usize = 105;

const ES_MULTILINE: u32 = 0x0004;
const ES_AUTOVSCROLL: u32 = 0x0040;
const ES_WANTRETURN: u32 = 0x1000;
const BS_OWNERDRAW: u32 = 0x000B;
const BN_CLICKED: u32 = 0;
const ODS_SELECTED: u32 = 0x0001;
const HTCAPTION: isize = 2;

const COL_BG: u32 = 0x0026_2525;
const COL_FIELD: u32 = 0x003c_3c3c;
const COL_BORDER: u32 = 0x004a_4646;
const COL_BTN: u32 = 0x0033_3333;
const COL_BTN_SAVE: u32 = 0x00eb_6325;
const COL_BTN_REC: u32 = 0x003c_3ce7;
const COL_TEXT: u32 = 0x00d4_d4d4;
const COL_TEXT_WHITE: u32 = 0x00ff_ffff;
const COL_HINT: u32 = 0x0088_8888;

static POPUP_OPEN: AtomicBool = AtomicBool::new(false);
static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

struct MicCaptureState {
    stop: Arc<AtomicBool>,
    samples: Arc<Mutex<Vec<f32>>>,
    rate: Arc<Mutex<u32>>,
    channels: Arc<Mutex<u16>>,
    join: Option<std::thread::JoinHandle<()>>,
}

struct QuickCaptureContext {
    data_dir: PathBuf,
    event_sender: Option<EventSender>,
    edit_hwnd: isize,
    orig_edit_proc: isize,
    bg_brush: isize,
    field_brush: isize,
    font: isize,
    hint_font: isize,
    snapshots: Vec<Vec<u8>>,
    mic: Option<MicCaptureState>,
    status_hint: String,
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

/// Saves text + optional PNG snapshots to today's daily scrap and emits `host:openFile`.
pub fn submit_quick_capture_with_snapshots(
    data_dir: &Path,
    raw_text: &str,
    snapshots: &[Vec<u8>],
    event_sender: Option<&EventSender>,
) -> Result<Option<PathBuf>, String> {
    let trimmed = raw_text.trim();
    if trimmed.is_empty() && snapshots.is_empty() {
        return Ok(None);
    }

    let cfg = JsonConfigStore::new(data_dir.join("config.json"));
    let scrap_dir = ScrapStore::resolve_scrap_dir(&cfg.scrap_dir());
    let scrap_dir_str = scrap_dir.to_string_lossy().into_owned();

    let mut body = String::from(trimmed);
    for png_bytes in snapshots {
        let b64 = STANDARD.encode(png_bytes);
        let saved = assets_store::save_asset(data_dir, &scrap_dir_str, "png", &b64)?;
        if !body.is_empty() {
            body.push('\n');
        }
        let link = if saved.rel_path.is_empty() {
            saved.file_url
        } else {
            saved.rel_path
        };
        body.push_str(&format!("![capture]({link})"));
    }

    let scrap_path = ScrapStore::append_formatted(&scrap_dir, &body, "Quick Capture")
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

/// Backward-compatible text-only Quick Capture submit helper.
pub fn submit_quick_capture_text(
    data_dir: &PathBuf,
    raw_text: &str,
    event_sender: Option<&EventSender>,
) -> Result<Option<PathBuf>, String> {
    submit_quick_capture_with_snapshots(data_dir, raw_text, &[], event_sender)
}

fn start_mic_capture() -> MicCaptureState {
    let stop = Arc::new(AtomicBool::new(false));
    let samples = Arc::new(Mutex::new(Vec::new()));
    let rate = Arc::new(Mutex::new(48_000u32));
    let channels = Arc::new(Mutex::new(1u16));

    let s_bg = Arc::clone(&stop);
    let buf_bg = Arc::clone(&samples);
    let rate_bg = Arc::clone(&rate);
    let ch_bg = Arc::clone(&channels);

    let join = std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(enumerator): Result<IMMDeviceEnumerator, _> =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        else {
            return;
        };
        let Ok(device) = enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) else {
            return;
        };
        let Ok(client): Result<IAudioClient, _> = device.Activate(CLSCTX_ALL, None) else {
            return;
        };
        let Ok(fmt_ptr) = client.GetMixFormat() else {
            return;
        };
        let fmt: WAVEFORMATEX = *fmt_ptr;
        *rate_bg.lock().expect("rate") = fmt.nSamplesPerSec;
        *ch_bg.lock().expect("channels") = fmt.nChannels;
        let bits = fmt.wBitsPerSample;

        let init_ok = client
            .Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 10_000_000, 0, fmt_ptr, None)
            .is_ok();
        CoTaskMemFree(Some(fmt_ptr as *const _));
        if !init_ok {
            return;
        }
        let Ok(capture): Result<IAudioCaptureClient, _> = client.GetService() else {
            return;
        };
        if client.Start().is_err() {
            return;
        }
        while !s_bg.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(20));
            while let Ok(pkt) = capture.GetNextPacketSize() {
                if pkt == 0 {
                    break;
                }
                let mut data_ptr: *mut u8 = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                if capture
                    .GetBuffer(&mut data_ptr, &mut frames, &mut flags, None, None)
                    .is_err()
                {
                    break;
                }
                let total = (frames as usize) * (fmt.nChannels as usize);
                if !data_ptr.is_null() && total > 0 {
                    let mut g = buf_bg.lock().expect("mic samples");
                    if bits == 32 {
                        g.extend_from_slice(std::slice::from_raw_parts(data_ptr as *const f32, total));
                    } else if bits == 16 {
                        for &v in std::slice::from_raw_parts(data_ptr as *const i16, total) {
                            g.push((v as f32) / 32768.0);
                        }
                    }
                }
                let _ = capture.ReleaseBuffer(frames);
            }
        }
        let _ = client.Stop();
    });

    MicCaptureState {
        stop,
        samples,
        rate,
        channels,
        join: Some(join),
    }
}

fn stop_mic_and_transcribe(data_dir: &Path, mut mic: MicCaptureState) -> Result<String, String> {
    mic.stop.store(true, Ordering::Release);
    if let Some(j) = mic.join.take() {
        let _ = j.join();
    }
    let samples = mic.samples.lock().expect("mic samples").clone();
    if samples.is_empty() {
        return Err("no microphone audio captured".to_string());
    }
    let rate = *mic.rate.lock().expect("rate");
    let ch = *mic.channels.lock().expect("channels");
    let pcm16 = speech::resample_f32_to_mono_16k_i16(&samples, rate, ch);
    let wav = speech::encode_wav_pcm16(&pcm16, 16_000, 1);

    let cache_dir = speech::voice_cache_dir(data_dir);
    let _ = std::fs::create_dir_all(&cache_dir);
    let wav_path = cache_dir.join("qc-voice.wav");
    write_atomic_bytes(&wav_path, &wav).map_err(|e| e.to_string())?;
    speech::transcribe_wav_file(data_dir, &wav_path, "ja")
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
    let id = dis.ctl_id as usize;
    let pressed = (dis.item_state & ODS_SELECTED) != 0;

    let (label, is_save, is_rec) = if let Ok(guard) = ACTIVE_CTX.lock() {
        let ctx = guard.as_ref();
        match id {
            ID_BTN_SAVE => ("Save".to_string(), true, false),
            ID_BTN_CANCEL => ("Cancel".to_string(), false, false),
            ID_BTN_CAPTURE => {
                let cnt = ctx.map(|c| c.snapshots.len()).unwrap_or(0);
                let txt = if cnt > 0 {
                    format!("Capture ({cnt})")
                } else {
                    "Capture".to_string()
                };
                (txt, false, false)
            }
            ID_BTN_MIC => {
                let rec = ctx.map(|c| c.mic.is_some()).unwrap_or(false);
                (if rec { "Stop".to_string() } else { "Mic".to_string() }, false, rec)
            }
            _ => (String::new(), false, false),
        }
    } else {
        (String::new(), false, false)
    };

    let fill_col = if is_save {
        if pressed { 0x00d4_531d } else { COL_BTN_SAVE }
    } else if is_rec {
        COL_BTN_REC
    } else if pressed {
        0x0044_4444
    } else {
        COL_BTN
    };
    let border_col = if is_save {
        COL_BTN_SAVE
    } else if is_rec {
        COL_BTN_REC
    } else {
        COL_BORDER
    };

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
        COLORREF(if is_save || is_rec { COL_TEXT_WHITE } else { COL_TEXT }),
    );
    let mut text: Vec<u16> = label.encode_utf16().collect();
    let mut rc = dis.rc_item;
    let _ = DrawTextW(
        dis.hdc,
        &mut text,
        &mut rc,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
}

unsafe fn read_edit_text(edit: HWND) -> String {
    let len = GetWindowTextLengthW(edit);
    let mut buf = vec![0u16; (len as usize) + 1];
    let copied = GetWindowTextW(edit, &mut buf);
    String::from_utf16_lossy(&buf[..copied as usize])
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
            let old_b = SelectObject(hdc, GetStockObject(NULL_BRUSH));
            let _ = RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, 10, 10);

            let field_brush = CreateSolidBrush(COLORREF(COL_FIELD));
            SelectObject(hdc, HGDIOBJ(field_brush.0));
            let _ = RoundRect(hdc, 14, 14, rc.right - 14, rc.bottom - 52, 8, 8);
            SelectObject(hdc, old_b);
            SelectObject(hdc, old_p);
            let _ = DeleteObject(HGDIOBJ(field_brush.0));
            let _ = DeleteObject(HGDIOBJ(border_pen.0));

            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(COL_HINT));
            let hint = if let Ok(guard) = ACTIVE_CTX.lock() {
                if let Some(ctx) = guard.as_ref() {
                    if ctx.hint_font != 0 {
                        SelectObject(hdc, HGDIOBJ(ctx.hint_font as *mut _));
                    }
                    ctx.status_hint.clone()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            let mut hint_rc = RECT {
                left: 16,
                top: rc.bottom - 40,
                right: rc.right - 375,
                bottom: rc.bottom - 12,
            };
            let mut hint_w: Vec<u16> = hint.encode_utf16().collect();
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
            if cmd_id == ID_BTN_CAPTURE && (notify == BN_CLICKED || l_param.0 == 0) {
                let _ = ShowWindow(hwnd, SW_HIDE);
                std::thread::sleep(std::time::Duration::from_millis(80));
                if let Ok(Some(png)) = imgcap::capture_region_interactive() {
                    if let Ok(mut guard) = ACTIVE_CTX.lock() {
                        if let Some(ctx) = guard.as_mut() {
                            ctx.snapshots.push(png);
                            ctx.status_hint =
                                format!("{} snapshot(s) attached", ctx.snapshots.len());
                        }
                    }
                }
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
                if let Ok(btn) = GetDlgItem(Some(hwnd), ID_BTN_CAPTURE as i32) {
                    let _ = InvalidateRect(Some(btn), None, true);
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
                return LRESULT(0);
            } else if cmd_id == ID_BTN_MIC && (notify == BN_CLICKED || l_param.0 == 0) {
                let stop_job = if let Ok(mut guard) = ACTIVE_CTX.lock() {
                    if let Some(ctx) = guard.as_mut() {
                        if let Some(mic) = ctx.mic.take() {
                            ctx.status_hint = "Transcribing...".to_string();
                            Some((mic, ctx.data_dir.clone(), HWND(ctx.edit_hwnd as *mut _)))
                        } else {
                            ctx.mic = Some(start_mic_capture());
                            ctx.status_hint = "Recording... click Stop".to_string();
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some((mic, dir, edit)) = stop_job {
                    match stop_mic_and_transcribe(&dir, mic) {
                        Ok(txt) if !txt.is_empty() => {
                            let existing = read_edit_text(edit);
                            let combined = if existing.trim().is_empty() {
                                txt
                            } else {
                                format!("{}\r\n{}", existing.trim_end(), txt)
                            };
                            let h = HSTRING::from(combined);
                            let _ = SetWindowTextW(edit, &h);
                            if let Ok(mut guard) = ACTIVE_CTX.lock() {
                                if let Some(ctx) = guard.as_mut() {
                                    ctx.status_hint = "Voice transcribed".to_string();
                                }
                            }
                        }
                        Ok(_) => {
                            if let Ok(mut guard) = ACTIVE_CTX.lock() {
                                if let Some(ctx) = guard.as_mut() {
                                    ctx.status_hint = "No speech detected".to_string();
                                }
                            }
                        }
                        Err(err) => {
                            if let Ok(mut guard) = ACTIVE_CTX.lock() {
                                if let Some(ctx) = guard.as_mut() {
                                    ctx.status_hint = err;
                                }
                            }
                        }
                    }
                }
                if let Ok(btn) = GetDlgItem(Some(hwnd), ID_BTN_MIC as i32) {
                    let _ = InvalidateRect(Some(btn), None, true);
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
                return LRESULT(0);
            } else if cmd_id == ID_BTN_SAVE && (notify == BN_CLICKED || l_param.0 == 0) {
                let payload = if let Ok(mut guard) = ACTIVE_CTX.lock() {
                    if let Some(ctx) = guard.as_mut() {
                        if let Some(mic) = ctx.mic.take() {
                            mic.stop.store(true, Ordering::Release);
                        }
                        let body = read_edit_text(HWND(ctx.edit_hwnd as *mut _));
                        let snaps = std::mem::take(&mut ctx.snapshots);
                        Some((body, snaps, ctx.data_dir.clone(), ctx.event_sender.clone()))
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some((body, snaps, dir, sender)) = payload {
                    let _ = submit_quick_capture_with_snapshots(
                        &dir,
                        &body,
                        &snaps,
                        sender.as_ref(),
                    );
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
                if let Some(mut ctx) = guard.take() {
                    if let Some(mic) = ctx.mic.take() {
                        mic.stop.store(true, Ordering::Release);
                    }
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
        let width = 660;
        let height = 185;
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

        let btn_capture = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("Capture"),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW),
            width - 360,
            height - 42,
            96,
            28,
            Some(hwnd),
            Some(HMENU(ID_BTN_CAPTURE as *mut _)),
            None,
            None,
        )
        .unwrap_or_default();

        let btn_mic = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("Mic"),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW),
            width - 256,
            height - 42,
            64,
            28,
            Some(hwnd),
            Some(HMENU(ID_BTN_MIC as *mut _)),
            None,
            None,
        )
        .unwrap_or_default();

        let btn_cancel = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("Cancel"),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW),
            width - 184,
            height - 42,
            76,
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
            width - 100,
            height - 42,
            86,
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
        for ctrl in [edit_hwnd, btn_capture, btn_mic, btn_cancel, btn_save] {
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
                snapshots: Vec::new(),
                mic: None,
                status_hint: "Ctrl+Enter: Save   Esc: Cancel".to_string(),
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
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn submit_quick_capture_appends_to_scrap_and_emits_open_file_event() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("syki-qc-{nanos}"));
        let scrap_dir = dir.join("scraps");
        std::fs::create_dir_all(&scrap_dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            serde_json::json!({ "general": { "scrapDir": scrap_dir.to_string_lossy() } }).to_string(),
        )
        .unwrap();

        assert_eq!(submit_quick_capture_text(&dir, "   ", None).unwrap(), None);

        let (tx, rx) = std::sync::mpsc::channel::<RpcEvent>();
        let sender: EventSender = Arc::new(move |ev| {
            let _ = tx.send(ev);
        });
        let saved = submit_quick_capture_with_snapshots(
            &dir,
            "quick thought",
            &[b"png1".to_vec(), b"png2".to_vec()],
            Some(&sender),
        )
        .unwrap()
        .expect("saved scrap");
        assert!(saved.is_file());
        let content = std::fs::read_to_string(&saved).unwrap();
        assert!(content.contains("Quick Capture"));
        assert!(content.contains("quick thought"));
        assert_eq!(content.matches("![capture](").count(), 2);

        let ev = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(ev.topic, "host:openFile");

        let _ = std::fs::remove_dir_all(dir);
    }
}
