//! Native Win32 Japanese IME mode switching (`imm32`) and romaji retype (#05).

use std::time::Duration;

use serde_json::json;
use syki_core::rpc::RpcEvent;
use windows::Win32::UI::Input::Ime::{
    ImmGetContext, ImmReleaseContext, ImmSetConversionStatus, ImmSetOpenStatus,
    IME_CMODE_FULLSHAPE, IME_CMODE_NATIVE, IME_CONVERSION_MODE, IME_SENTENCE_MODE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, MapVirtualKeyW, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, GUITHREADINFO,
};

use crate::rpc_dispatch::EventSender;

const VK_IME_ON: u8 = 0x16;
const VK_IME_OFF: u8 = 0x1A;
const IME_RETYPE_MAX_KEYS: usize = 32;

/// Validates `romaji` (`1..=32` ASCII lowercase letters) and maps each to `VK_A..VK_Z`.
pub fn ime_retype_keys(romaji: &str) -> Option<Vec<u8>> {
    if romaji.is_empty() || romaji.len() > IME_RETYPE_MAX_KEYS {
        return None;
    }
    let mut keys = Vec::with_capacity(romaji.len());
    for b in romaji.bytes() {
        if !b.is_ascii_lowercase() {
            return None;
        }
        keys.push(b - b'a' + 0x41);
    }
    Some(keys)
}

/// Switches the OS IME state (`imm32` + `VK_IME_ON`/`VK_IME_OFF` key assist for WebView2).
pub fn set_ime_mode(enable_japanese: bool) {
    unsafe {
        let mut gui = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let target = if GetGUIThreadInfo(0, &mut gui).is_ok() && !gui.hwndFocus.is_invalid() {
            gui.hwndFocus
        } else {
            GetForegroundWindow()
        };

        if !target.is_invalid() {
            let himc = ImmGetContext(target);
            if !himc.is_invalid() {
                if enable_japanese {
                    let _ = ImmSetOpenStatus(himc, true);
                    let _ = ImmSetConversionStatus(
                        himc,
                        IME_CONVERSION_MODE(IME_CMODE_NATIVE.0 | IME_CMODE_FULLSHAPE.0),
                        IME_SENTENCE_MODE(0),
                    );
                } else {
                    let _ = ImmSetOpenStatus(himc, false);
                }
                let _ = ImmReleaseContext(target, himc);
            }
        }

        let vk = if enable_japanese { VK_IME_ON } else { VK_IME_OFF };
        keybd_event(vk, 0, KEYBD_EVENT_FLAGS(0), 0);
        keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
    }
}

fn owns_foreground() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid == std::process::id()
    }
}

fn send_key(vk: u8) {
    unsafe {
        let scan = MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) as u8;
        keybd_event(vk, scan, KEYBD_EVENT_FLAGS(0), 0);
        std::thread::sleep(Duration::from_millis(8));
        keybd_event(vk, scan, KEYEVENTF_KEYUP, 0);
        std::thread::sleep(Duration::from_millis(8));
    }
}

/// Spawns a background thread to retype `romaji` into the active IME composition and emits `ime:retype`.
pub fn retype_with_ime_async(req_id: String, romaji: String, sender: Option<EventSender>) {
    std::thread::spawn(move || {
        let err_msg = match ime_retype_keys(&romaji) {
            None => "only a-z can be retyped".to_string(),
            Some(keys) => {
                std::thread::sleep(Duration::from_millis(70));
                if !owns_foreground() {
                    "the window is no longer in front".to_string()
                } else {
                    send_key(VK_IME_ON);
                    std::thread::sleep(Duration::from_millis(60));
                    if !owns_foreground() {
                        "the window is no longer in front".to_string()
                    } else {
                        for vk in keys {
                            send_key(vk);
                        }
                        String::new()
                    }
                }
            }
        };

        if let Some(tx) = sender {
            tx(RpcEvent {
                topic: "ime:retype".to_string(),
                payload: json!({ "reqId": req_id, "error": err_msg }),
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ime_retype_keys_accepts_lowercase_ascii_and_rejects_others() {
        assert_eq!(ime_retype_keys("iro"), Some(vec![0x49, 0x52, 0x4F]));
        assert_eq!(ime_retype_keys(""), None);
        assert_eq!(ime_retype_keys("iro-ha"), None);
        assert_eq!(ime_retype_keys("Iro"), None);
        assert_eq!(ime_retype_keys(&"a".repeat(33)), None);
    }
}
