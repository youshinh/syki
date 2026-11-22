//! Pure Win32 Native Quick Capture Popup (`F10a` / `#09`) — zero WebView2 overhead.
//! Supports multi-image screen snapshots (`imgcap`), WASAPI mic voice input (`speech`),
//! dark GDI surface (`#252526`), `WM_CTLCOLOREDIT`, and owner-drawn flat buttons.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
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
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE, DWMWA_USE_IMMERSIVE_DARK_MODE};
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
    IsWindow, LoadCursorW, RegisterClassW, SendMessageW, SetForegroundWindow,
    SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowTextW, ShowWindow, GWLP_USERDATA,
    GWLP_WNDPROC, HMENU, IDC_ARROW, LWA_ALPHA, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT, WM_DESTROY, WM_DRAWITEM, WM_ERASEBKGND, WM_KEYDOWN,
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

#[derive(Debug, Clone, Copy)]
struct QcTheme {
    is_dark: bool,
    bg: u32,
    field: u32,
    border: u32,
    btn: u32,
    btn_pressed: u32,
    btn_save: u32,
    btn_save_pressed: u32,
    btn_rec: u32,
    text: u32,
    text_on_save: u32,
    hint: u32,
}

fn parse_hex_bgr(hex: &str) -> Option<u32> {
    let s = hex.trim().strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let r = u32::from_str_radix(&s[0..2], 16).ok()?;
    let g = u32::from_str_radix(&s[2..4], 16).ok()?;
    let b = u32::from_str_radix(&s[4..6], 16).ok()?;
    Some((b << 16) | (g << 8) | r)
}

fn darken_bgr(bgr: u32, pct: u32) -> u32 {
    let r = ((bgr & 0xFF) * pct / 100).min(255);
    let g = (((bgr >> 8) & 0xFF) * pct / 100).min(255);
    let b = (((bgr >> 16) & 0xFF) * pct / 100).min(255);
    (b << 16) | (g << 8) | r
}

fn load_qc_theme(data_dir: &Path) -> QcTheme {
    let val = std::fs::read_to_string(data_dir.join("config.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
    let look = val
        .as_ref()
        .and_then(|v| v.pointer("/appearance/look"))
        .and_then(|v| v.as_str())
        .unwrap_or("ink");
    let is_dark = look != "paper";
    let accent_name = val
        .as_ref()
        .and_then(|v| {
            v.pointer("/appearance/accent")
                .and_then(|a| a.as_str())
                .or_else(|| v.pointer("/general/theme").and_then(|a| a.as_str()))
        })
        .unwrap_or("olive");
    let accent = match accent_name {
        "blue" => {
            if is_dark { 0x00C2_7400 } else { 0x00B0_6900 }
        }
        "forest" => 0x0056_662E,
        "charcoal" => {
            if is_dark { 0x0050_5050 } else { 0x004A_4A4A }
        }
        "vermilion" => 0x002C_47B8,
        "custom" => val
            .as_ref()
            .and_then(|v| v.pointer("/appearance/accentCustom"))
            .and_then(|v| v.as_str())
            .and_then(parse_hex_bgr)
            .unwrap_or(0x002F_6B55),
        _ => 0x002F_6B55, // olive #556b2f
    };
    let r = accent & 0xFF;
    let g = (accent >> 8) & 0xFF;
    let b = (accent >> 16) & 0xFF;
    let luma = (299 * r + 587 * g + 114 * b) / 1000;
    let text_on_save = if luma > 155 { 0x0016_1815 } else { 0x00FF_FFFF };

    if is_dark {
        QcTheme {
            is_dark: true,
            bg: 0x0019_1A18,
            field: 0x0021_2320,
            border: 0x0036_3934,
            btn: 0x0028_2A26,
            btn_pressed: 0x0037_3A34,
            btn_save: accent,
            btn_save_pressed: darken_bgr(accent, 82),
            btn_rec: 0x0046_46D9,
            text: 0x00EB_EEEC,
            text_on_save,
            hint: 0x0090_958E,
        }
    } else {
        QcTheme {
            is_dark: false,
            bg: 0x00EF_F5F7,
            field: 0x00FF_FFFF,
            border: 0x00CE_D8DD,
            btn: 0x00E0_E9EC,
            btn_pressed: 0x00D0_DBDF,
            btn_save: accent,
            btn_save_pressed: darken_bgr(accent, 82),
            btn_rec: 0x0046_46D9,
            text: 0x0025_2623,
            text_on_save,
            hint: 0x0078_756E,
        }
    }
}

static POPUP_OPEN: AtomicBool = AtomicBool::new(false);
static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);
static POPUP_HWND: AtomicIsize = AtomicIsize::new(0);

unsafe fn hide_quick_capture_popup(hwnd: HWND) {
    if let Ok(mut guard) = ACTIVE_CTX.lock() {
        if let Some(ctx) = guard.as_mut() {
            if let Some(mic) = ctx.mic.take() {
                mic.stop.store(true, Ordering::Release);
            }
            ctx.snapshots.clear();
            ctx.status_hint.clear();
            let _ = SetWindowTextW(HWND(ctx.edit_hwnd as *mut _), w!(""));
        }
    }
    POPUP_OPEN.store(false, Ordering::Release);
    let _ = ShowWindow(hwnd, SW_HIDE);
}

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
    icon_font: isize,
    theme: QcTheme,
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
fn load_vision_ocr_config(data_dir: &Path) -> Option<(crate::llm::LlmConfig, String)> {
    let raw = std::fs::read_to_string(data_dir.join("config.json")).ok()?;
    let val: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let vision = val.get("vision")?;
    let base_url = vision
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let model = vision
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let mut api_key = vision
        .get("apiKey")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if api_key.is_empty() {
        api_key = val
            .pointer("/text/apiKey")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
    }
    let is_local = base_url.contains("localhost") || base_url.contains("127.0.0.1");
    if api_key.is_empty() && !is_local {
        return None;
    }
    let prompt = vision
        .get("prompt")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Transcribe the content of this image into structured Markdown.")
        .to_string();
    let cfg = crate::llm::LlmConfig {
        provider: if base_url.contains("googleapis.com") || model.starts_with("gemini") {
            "gemini".to_string()
        } else if is_local && base_url.contains("11434") {
            "ollama".to_string()
        } else {
            String::new()
        },
        base_url,
        model,
        api_key,
        ..Default::default()
    };
    Some((cfg, prompt))
}

fn emit_open_scrap(
    scrap_path: &Path,
    sender: &EventSender,
    appended_entry: Option<&str>,
    ocr_replace: Option<(&str, &str)>,
) {
    if let Ok(loaded) = NoteFileStore::read_note(scrap_path) {
        let mut payload = json!({
            "path": loaded.path.to_string_lossy(),
            "filePath": loaded.path.to_string_lossy(),
            "title": loaded.path.file_name().and_then(|n| n.to_str()).unwrap_or("scrap.md"),
            "fileName": loaded.path.file_name().and_then(|n| n.to_str()).unwrap_or("scrap.md"),
            "content": loaded.content,
            "encoding": loaded.encoding_name,
            "sig": cyrb53_utf16_sig(&loaded.content),
            "command": "Quick Capture"
        });
        if let Some(entry) = appended_entry {
            payload["appendedEntry"] = json!(entry);
        }
        if let Some((marker, replacement)) = ocr_replace {
            payload["ocrReplace"] = json!({ "marker": marker, "replacement": replacement });
        }
        sender(RpcEvent {
            topic: "host:openFile".to_string(),
            payload,
        });
    }
}

/// Appends Quick Capture text and optional PNG snapshots (`./assets/...`) to today's daily scrap
/// and triggers background Vision AI OCR for snapshots when a vision provider is configured.
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
    let vision_cfg = if !snapshots.is_empty() {
        load_vision_ocr_config(data_dir)
    } else {
        None
    };

    let now = chrono::Local::now();
    let stamp = now.format("%Y%m%d%H%M%S%3f").to_string();
    let mut body = String::from(trimmed);
    let mut ocr_jobs: Vec<(String, String)> = Vec::new();

    for (idx, png_bytes) in snapshots.iter().enumerate() {
        let b64 = STANDARD.encode(png_bytes);
        let saved = assets_store::save_asset(data_dir, &scrap_dir_str, "png", &b64)?;
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        let link = if saved.rel_path.is_empty() {
            saved.file_url
        } else {
            saved.rel_path
        };
        body.push_str(&format!("![capture]({link})"));
        if vision_cfg.is_some() {
            let marker = format!("> ⏳ 画像を文字起こし中... ({stamp}-{idx})");
            body.push('\n');
            body.push_str(&marker);
            ocr_jobs.push((marker, b64));
        }
    }

    let entry = format!(
        "---\n## [{}] Quick Capture\n\n{}\n",
        now.format("%H:%M:%S"),
        body.trim_end_matches(['\r', '\n'])
    );
    let scrap_path = ScrapStore::append_entry(&scrap_dir, &entry, now)
        .map_err(|e| format!("quick capture append failed: {e}"))?;

    if let Some(sender) = event_sender {
        emit_open_scrap(&scrap_path, sender, Some(&entry), None);
    }

    if let (Some((llm_cfg, prompt)), false) = (vision_cfg, ocr_jobs.is_empty()) {
        let target_path = scrap_path.clone();
        let sender_cloned = event_sender.cloned();
        std::thread::spawn(move || {
            for (marker, b64) in ocr_jobs {
                let replacement = match crate::llm::query_vision_sync(&prompt, &b64, "image/png", &llm_cfg) {
                    Ok(text) => text
                        .lines()
                        .map(|l| format!("> {l}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Err(e) => format!("> (画像OCR失敗: {e})"),
                };
                if let Ok(content) = std::fs::read_to_string(&target_path) {
                    if content.contains(&marker) {
                        let updated = content.replacen(&marker, &replacement, 1);
                        if syki_core::storage::atomic::write_atomic_bytes(&target_path, updated.as_bytes()).is_ok() {
                            if let Some(ref tx) = sender_cloned {
                                emit_open_scrap(&target_path, tx, None, Some((&marker, &replacement)));
                            }
                        }
                    }
                }
            }
        });
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
    let res = speech::transcribe_wav_file(data_dir, &wav_path, "ja");
    if res.is_ok() {
        let _ = std::fs::remove_file(&wav_path);
    }
    res
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
                hide_quick_capture_popup(parent);
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

    let (theme, icon_font, hint_font, snap_cnt, is_rec) = if let Ok(guard) = ACTIVE_CTX.lock() {
        if let Some(ctx) = guard.as_ref() {
            (
                ctx.theme,
                ctx.icon_font,
                ctx.hint_font,
                ctx.snapshots.len(),
                ctx.mic.is_some(),
            )
        } else {
            return;
        }
    } else {
        return;
    };

    let (glyph, badge, is_save, active_rec) = match id {
        ID_BTN_SAVE => ("\u{E724}", String::new(), true, false),
        ID_BTN_CANCEL => ("\u{E711}", String::new(), false, false),
        ID_BTN_CAPTURE => (
            "\u{E722}",
            if snap_cnt > 0 { snap_cnt.to_string() } else { String::new() },
            false,
            false,
        ),
        ID_BTN_MIC => (if is_rec { "\u{E71A}" } else { "\u{E720}" }, String::new(), false, is_rec),
        _ => ("", String::new(), false, false),
    };

    let fill_col = if is_save {
        if pressed { theme.btn_save_pressed } else { theme.btn_save }
    } else if active_rec {
        theme.btn_rec
    } else if pressed {
        theme.btn_pressed
    } else {
        theme.btn
    };
    let border_col = if is_save {
        theme.btn_save
    } else if active_rec {
        theme.btn_rec
    } else {
        theme.border
    };
    let txt_col = if is_save {
        theme.text_on_save
    } else if active_rec {
        0x00FF_FFFF
    } else {
        theme.text
    };

    let bg_brush = CreateSolidBrush(COLORREF(theme.bg));
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
        12,
        12,
    );
    SelectObject(dis.hdc, old_b);
    SelectObject(dis.hdc, old_p);
    let _ = DeleteObject(HGDIOBJ(brush.0));
    let _ = DeleteObject(HGDIOBJ(pen.0));

    SetBkMode(dis.hdc, TRANSPARENT);
    SetTextColor(dis.hdc, COLORREF(txt_col));
    if icon_font != 0 {
        SelectObject(dis.hdc, HGDIOBJ(icon_font as *mut _));
    }
    let mut wide: Vec<u16> = glyph.encode_utf16().collect();
    let mut rc = dis.rc_item;
    if !badge.is_empty() {
        rc.right -= 12;
        let _ = DrawTextW(dis.hdc, &mut wide, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        if hint_font != 0 {
            SelectObject(dis.hdc, HGDIOBJ(hint_font as *mut _));
        }
        let mut bwide: Vec<u16> = badge.encode_utf16().collect();
        let mut brc = dis.rc_item;
        brc.left = brc.right - 18;
        let _ = DrawTextW(dis.hdc, &mut bwide, &mut brc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    } else {
        let _ = DrawTextW(dis.hdc, &mut wide, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
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

            let (theme, hint, hint_font) = if let Ok(guard) = ACTIVE_CTX.lock() {
                if let Some(ctx) = guard.as_ref() {
                    (ctx.theme, ctx.status_hint.clone(), ctx.hint_font)
                } else {
                    return LRESULT(0);
                }
            } else {
                return LRESULT(0);
            };

            let bg = CreateSolidBrush(COLORREF(theme.bg));
            FillRect(hdc, &rc, bg);
            let _ = DeleteObject(HGDIOBJ(bg.0));

            let border_pen = CreatePen(PS_SOLID, 1, COLORREF(theme.border));
            let old_p = SelectObject(hdc, HGDIOBJ(border_pen.0));
            let old_b = SelectObject(hdc, GetStockObject(NULL_BRUSH));
            let _ = RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, 16, 16);
            SelectObject(hdc, old_b);
            SelectObject(hdc, old_p);
            let _ = DeleteObject(HGDIOBJ(border_pen.0));

            if !hint.is_empty() {
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, COLORREF(theme.hint));
                if hint_font != 0 {
                    SelectObject(hdc, HGDIOBJ(hint_font as *mut _));
                }
                let mut hint_rc = RECT {
                    left: 20,
                    top: rc.bottom - 48,
                    right: rc.right - 208,
                    bottom: rc.bottom - 14,
                };
                let mut hint_w: Vec<u16> = hint.encode_utf16().collect();
                let _ = DrawTextW(hdc, &mut hint_w, &mut hint_rc, DT_VCENTER | DT_SINGLELINE);
            }

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_CTLCOLOREDIT => {
            let hdc = windows::Win32::Graphics::Gdi::HDC(w_param.0 as *mut _);
            if let Ok(guard) = ACTIVE_CTX.lock() {
                if let Some(ctx) = guard.as_ref() {
                    SetTextColor(hdc, COLORREF(ctx.theme.text));
                    SetBkColor(hdc, COLORREF(ctx.theme.bg));
                    return LRESULT(ctx.bg_brush);
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
                let accent_bgr = ACTIVE_CTX
                    .lock()
                    .ok()
                    .and_then(|g| g.as_ref().map(|c| c.theme.btn_save))
                    .unwrap_or(0x00C2_7400);
                let _ = ShowWindow(hwnd, SW_HIDE);
                std::thread::sleep(std::time::Duration::from_millis(80));
                if let Ok(Some(png)) = imgcap::capture_region_interactive_with_accent(accent_bgr) {
                    if let Ok(mut guard) = ACTIVE_CTX.lock() {
                        if let Some(ctx) = guard.as_mut() {
                            ctx.snapshots.push(png);
                            ctx.status_hint =
                                format!("画像 {} 枚", ctx.snapshots.len());
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
                            ctx.status_hint = "変換中...".to_string();
                            Some((mic, ctx.data_dir.clone(), HWND(ctx.edit_hwnd as *mut _)))
                        } else {
                            speech::prefetch_model_async(ctx.data_dir.clone());
                            ctx.mic = Some(start_mic_capture());
                            ctx.status_hint = "録音中...".to_string();
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
                                    ctx.status_hint = "音声入力完了".to_string();
                                }
                            }
                        }
                        Ok(_) => {
                            if let Ok(mut guard) = ACTIVE_CTX.lock() {
                                if let Some(ctx) = guard.as_mut() {
                                    ctx.status_hint = "音声が検出されませんでした".to_string();
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
                        let active_mic = ctx.mic.take();
                        let body = read_edit_text(HWND(ctx.edit_hwnd as *mut _));
                        let snaps = std::mem::take(&mut ctx.snapshots);
                        ctx.status_hint.clear();
                        let _ = SetWindowTextW(HWND(ctx.edit_hwnd as *mut _), w!(""));
                        Some((body, snaps, active_mic, ctx.data_dir.clone(), ctx.event_sender.clone()))
                    } else {
                        None
                    }
                } else {
                    None
                };
                POPUP_HWND.store(0, Ordering::Release);
            POPUP_OPEN.store(false, Ordering::Release);
                let _ = ShowWindow(hwnd, SW_HIDE);
                if let Some((mut body, snaps, active_mic, dir, sender)) = payload {
                    std::thread::spawn(move || {
                        if let Some(mic) = active_mic {
                            if let Ok(txt) = stop_mic_and_transcribe(&dir, mic) {
                                if !txt.is_empty() {
                                    if body.trim().is_empty() {
                                        body = txt;
                                    } else {
                                        body = format!("{}\r\n{}", body.trim_end(), txt);
                                    }
                                }
                            }
                        }
                        let _ = submit_quick_capture_with_snapshots(
                            &dir,
                            &body,
                            &snaps,
                            sender.as_ref(),
                        );
                    });
                }
                return LRESULT(0);
            } else if cmd_id == ID_BTN_CANCEL {
                hide_quick_capture_popup(hwnd);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, w_param, l_param)
        }
        WM_LBUTTONDOWN => {
            let _ = InvalidateRect(Some(hwnd), None, false);
            DefWindowProcW(hwnd, msg, w_param, l_param)
        }
        WM_CLOSE => {
            hide_quick_capture_popup(hwnd);
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
                    if ctx.icon_font != 0 {
                        let _ = DeleteObject(HGDIOBJ(ctx.icon_font as *mut _));
                    }
                }
            }
            POPUP_HWND.store(0, Ordering::Release);
            POPUP_OPEN.store(false, Ordering::Release);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w_param, l_param),
    }
}

/// Pre-creates the Win32 Quick Capture popup in hidden state so the first hotkey wake takes 1-3 ms.
pub fn prewarm_quick_capture_popup(data_dir: PathBuf, event_sender: Option<EventSender>) {
    let existing = POPUP_HWND.load(Ordering::Acquire);
    if existing != 0 && unsafe { IsWindow(Some(HWND(existing as *mut _))).as_bool() } {
        return;
    }
    create_quick_capture_window(data_dir, event_sender, false);
}

/// Opens the native Win32 Quick Capture popup on the UI thread (no-op if already open).
pub fn open_quick_capture_popup(data_dir: PathBuf, event_sender: Option<EventSender>) {
    if POPUP_OPEN.swap(true, Ordering::AcqRel) {
        let existing = POPUP_HWND.load(Ordering::Acquire);
        if existing != 0 {
            let hwnd = HWND(existing as *mut _);
            unsafe {
                let _ = SetForegroundWindow(hwnd);
            }
        }
        return;
    }

    let existing = POPUP_HWND.load(Ordering::Acquire);
    if existing != 0 && unsafe { IsWindow(Some(HWND(existing as *mut _))).as_bool() } {
        let hwnd = HWND(existing as *mut _);
        let edit_hwnd = if let Ok(mut guard) = ACTIVE_CTX.lock() {
            if let Some(ctx) = guard.as_mut() {
                ctx.data_dir = data_dir;
                ctx.event_sender = event_sender;
                ctx.snapshots.clear();
                ctx.status_hint.clear();
                let eh = HWND(ctx.edit_hwnd as *mut _);
                unsafe {
                    let _ = SetWindowTextW(eh, w!(""));
                }
                Some(eh)
            } else {
                None
            }
        } else {
            None
        };
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            if let Some(eh) = edit_hwnd {
                let _ = SetFocus(Some(eh));
            }
        }
        return;
    }

    create_quick_capture_window(data_dir, event_sender, true);
}

fn create_quick_capture_window(
    data_dir: PathBuf,
    event_sender: Option<EventSender>,
    show_now: bool,
) {

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
        let width = 640;
        let height = 240;
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
            POPUP_HWND.store(0, Ordering::Release);
            POPUP_OPEN.store(false, Ordering::Release);
            return;
        };

        let theme = load_qc_theme(&data_dir);

        let dark_val: i32 = if theme.is_dark { 1 } else { 0 };
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_val as *const _ as *const _,
            std::mem::size_of::<i32>() as u32,
        );
        let corner_val: i32 = 2; // DWMWCP_ROUND
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(33),
            &corner_val as *const _ as *const _,
            std::mem::size_of::<i32>() as u32,
        );
        let backdrop_val: i32 = 3; // DWMSBT_TRANSIENTWINDOW (Acrylic)
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(38),
            &backdrop_val as *const _ as *const _,
            std::mem::size_of::<i32>() as u32,
        );
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 238, LWA_ALPHA);

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
            height - 88,
            Some(hwnd),
            Some(HMENU(ID_EDIT as *mut _)),
            None,
            None,
        )
        .unwrap_or_default();

        let btn_capture = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!(""),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW),
            width - 192,
            height - 52,
            38,
            38,
            Some(hwnd),
            Some(HMENU(ID_BTN_CAPTURE as *mut _)),
            None,
            None,
        )
        .unwrap_or_default();

        let btn_mic = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!(""),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW),
            width - 146,
            height - 52,
            38,
            38,
            Some(hwnd),
            Some(HMENU(ID_BTN_MIC as *mut _)),
            None,
            None,
        )
        .unwrap_or_default();

        let btn_cancel = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!(""),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW),
            width - 100,
            height - 52,
            38,
            38,
            Some(hwnd),
            Some(HMENU(ID_BTN_CANCEL as *mut _)),
            None,
            None,
        )
        .unwrap_or_default();

        let btn_save = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!(""),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW),
            width - 54,
            height - 52,
            38,
            38,
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

        let icon_font = CreateFontW(
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
            w!("Segoe Fluent Icons"),
        );
        let bg_brush = CreateSolidBrush(COLORREF(theme.bg));
        let field_brush = CreateSolidBrush(COLORREF(theme.field));

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
                icon_font: if icon_font.is_invalid() { 0 } else { icon_font.0 as isize },
                theme,
                snapshots: Vec::new(),
                mic: None,
                status_hint: String::new(),
            }));
        }

        POPUP_HWND.store(hwnd.0 as isize, Ordering::Release);
        if show_now {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(edit_hwnd));
        }
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
