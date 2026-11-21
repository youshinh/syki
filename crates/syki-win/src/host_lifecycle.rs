//! Win32 Tray (`Shell_NotifyIconW`), Global Hotkey (`RegisterHotKey`), Working-Set Trim,
//! and 3-State Hybrid Memory `CanvasHost` state machine (#08).

use windows::core::w;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::ProcessStatus::{EmptyWorkingSet, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, AppendMenuW, DestroyMenu, GetCursorPos, LoadIconW, SetForegroundWindow,
    TrackPopupMenu, IDI_APPLICATION, MF_SEPARATOR, MF_STRING, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
    TPM_RETURNCMD, WM_USER,
};

pub const WM_TRAY_ICON: u32 = WM_USER + 100;
pub const HOTKEY_ID_MAIN: i32 = 1;
pub const HOTKEY_ID_QUICKCAPTURE: i32 = 2;

pub const TRAY_CMD_SHOW: u32 = 1001;
pub const TRAY_CMD_QUICK_CAPTURE: u32 = 1002;
pub const TRAY_CMD_EXIT: u32 = 1003;

/// 3-state hybrid memory lifecycle for the WebView2 canvas (`#08`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanvasState {
    Attached,
    Suspended,
    Detached,
}

#[derive(Debug, Clone)]
pub struct CanvasHostState {
    state: CanvasState,
}

impl Default for CanvasHostState {
    fn default() -> Self {
        Self::new()
    }
}

impl CanvasHostState {
    pub fn new() -> Self {
        Self {
            state: CanvasState::Attached,
        }
    }

    pub fn state(&self) -> CanvasState {
        self.state
    }

    pub fn suspend(&mut self) -> bool {
        if self.state == CanvasState::Attached {
            self.state = CanvasState::Suspended;
            true
        } else {
            false
        }
    }

    pub fn detach(&mut self) -> bool {
        if self.state != CanvasState::Detached {
            self.state = CanvasState::Detached;
            true
        } else {
            false
        }
    }

    pub fn wake(&mut self) -> CanvasState {
        let prev = self.state;
        self.state = CanvasState::Attached;
        prev
    }
}

/// Parses a human-readable shortcut like `"Ctrl+Alt+S"` or `"Alt+Space"` into `(mods, vk)`.
pub fn parse_shortcut(spec: &str) -> Option<(u32, u32)> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut mods = 0u32;
    let mut vk: Option<u32> = None;

    for raw_part in trimmed.split('+') {
        let part = raw_part.trim();
        if part.is_empty() {
            return None;
        }
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL.0,
            "alt" | "option" => mods |= MOD_ALT.0,
            "shift" => mods |= MOD_SHIFT.0,
            "win" | "meta" | "super" | "cmd" => mods |= MOD_WIN.0,
            _ => {
                if vk.is_some() {
                    return None;
                }
                vk = parse_vk(&lower);
            }
        }
    }

    let key = vk?;
    if mods == 0 && !(0x70..=0x7B).contains(&key) {
        // Require at least one modifier for non-function keys.
        return None;
    }
    Some((mods, key))
}

fn parse_vk(lower: &str) -> Option<u32> {
    if lower.len() == 1 {
        let b = lower.as_bytes()[0];
        if b.is_ascii_lowercase() {
            return Some((b - b'a' + 0x41) as u32);
        }
        if b.is_ascii_digit() {
            return Some(b as u32);
        }
    }
    if let Some(rest) = lower.strip_prefix('f') {
        if let Ok(n) = rest.parse::<u32>() {
            if (1..=12).contains(&n) {
                return Some(0x70 + n - 1);
            }
        }
    }
    match lower {
        "space" => Some(0x20),
        "enter" | "return" => Some(0x0D),
        "tab" => Some(0x09),
        "esc" | "escape" => Some(0x1B),
        "backspace" => Some(0x08),
        "delete" | "del" => Some(0x2E),
        "insert" | "ins" => Some(0x2D),
        "home" => Some(0x24),
        "end" => Some(0x23),
        "pageup" | "pgup" => Some(0x21),
        "pagedown" | "pgdn" => Some(0x22),
        "up" => Some(0x26),
        "down" => Some(0x28),
        "left" => Some(0x25),
        "right" => Some(0x27),
        _ => None,
    }
}

/// Registers (or unregisters if empty) a global hotkey on `hwnd` (must be called on HWND thread).
pub fn apply_hotkey(hwnd: HWND, id: i32, spec: &str) -> Result<bool, String> {
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), id);
    }
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Ok(false);
    }
    let (mods, vk) = parse_shortcut(trimmed)
        .ok_or_else(|| format!("invalid shortcut specification: {trimmed}"))?;
    unsafe {
        RegisterHotKey(
            Some(hwnd),
            id,
            HOT_KEY_MODIFIERS(mods | MOD_NOREPEAT.0),
            vk,
        )
        .map_err(|e| format!("RegisterHotKey failed: {e}"))?;
    }
    Ok(true)
}

/// Registers the Win32 system tray icon for `hwnd`.
pub fn add_tray_icon(hwnd: HWND, tooltip: &str) -> bool {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY_ICON,
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() },
        ..Default::default()
    };
    let wide: Vec<u16> = tooltip.encode_utf16().take(127).collect();
    for (i, &c) in wide.iter().enumerate() {
        nid.szTip[i] = c;
    }
    unsafe { Shell_NotifyIconW(NIM_ADD, &nid).as_bool() }
}

/// Removes the Win32 system tray icon for `hwnd`.
pub fn remove_tray_icon(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

/// Displays the native Win32 tray context menu and returns the selected command id.
pub fn show_tray_menu(hwnd: HWND) -> Option<u32> {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return None;
        };
        let _ = AppendMenuW(menu, MF_STRING, TRAY_CMD_SHOW as usize, w!("Show syki-lite"));
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            TRAY_CMD_QUICK_CAPTURE as usize,
            w!("Quick Capture"),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, TRAY_CMD_EXIT as usize, w!("Exit"));

        let mut pt = windows::Win32::Foundation::POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        if cmd.0 > 0 {
            Some(cmd.0 as u32)
        } else {
            None
        }
    }
}

/// Releases unused working-set pages on a background thread.
pub fn trim_working_set_async() {
    std::thread::spawn(|| unsafe {
        let proc_h = GetCurrentProcess();
        let _ = EmptyWorkingSet(proc_h);
    });
}

/// Returns current process working set in bytes.
pub fn current_working_set_bytes() -> u64 {
    unsafe {
        let mut pmc = PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        if GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut pmc,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
        .is_ok()
        {
            pmc.WorkingSetSize as u64
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_shortcut_handles_modifiers_and_rejects_invalid() {
        assert_eq!(parse_shortcut("Ctrl+Alt+S"), Some((MOD_CONTROL.0 | MOD_ALT.0, 0x53)));
        assert_eq!(parse_shortcut("Alt+Space"), Some((MOD_ALT.0, 0x20)));
        assert_eq!(parse_shortcut("Ctrl+Shift+F9"), Some((MOD_CONTROL.0 | MOD_SHIFT.0, 0x78)));
        assert_eq!(parse_shortcut("F12"), Some((0, 0x7B)));
        assert_eq!(parse_shortcut("S"), None);
        assert_eq!(parse_shortcut(""), None);
        assert_eq!(parse_shortcut("Ctrl+"), None);
    }

    #[test]
    fn canvas_host_state_transitions_cleanly() {
        let mut host = CanvasHostState::new();
        assert_eq!(host.state(), CanvasState::Attached);
        assert!(host.suspend());
        assert_eq!(host.state(), CanvasState::Suspended);
        assert!(!host.suspend());
        assert!(host.detach());
        assert_eq!(host.state(), CanvasState::Detached);
        assert_eq!(host.wake(), CanvasState::Detached);
        assert_eq!(host.state(), CanvasState::Attached);
    }

    #[test]
    fn current_working_set_reports_nonzero_for_running_process() {
        assert!(current_working_set_bytes() > 0);
    }
}
