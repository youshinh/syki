//! Win32 Screen Region Capture (`imgcap` / `#10`) via GDI `BitBlt` + `GetDIBits` -> PNG.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, GetDIBits,
    GetStockObject, InvalidateRect, LineTo, MoveToEx, Rectangle, ReleaseDC, RoundRect,
    SelectObject, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE,
    DT_VCENTER, FF_SWISS, HBRUSH, HGDIOBJ, NULL_BRUSH, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID,
    SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetSystemMetrics, LoadCursorW, PostQuitMessage, RegisterClassW, SetLayeredWindowAttributes,
    ShowWindow, TranslateMessage, IDC_CROSS, LWA_ALPHA, MSG, SM_CXSCREEN, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOW, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

static OVERLAY_CLASS_REG: AtomicBool = AtomicBool::new(false);

struct OverlayDragState {
    dragging: bool,
    start_x: i32,
    start_y: i32,
    end_x: i32,
    end_y: i32,
    cursor_x: i32,
    cursor_y: i32,
    has_cursor: bool,
    accent_bgr: u32,
    confirmed: bool,
    cancelled: bool,
}

static OVERLAY_STATE: Mutex<Option<OverlayDragState>> = Mutex::new(None);

/// Normalizes drag endpoints `(x0, y0) -> (x1, y1)` into `(x, y, width, height)`.
/// Returns `None` if the selection is smaller than `4x4` pixels.
pub fn normalize_selection_rect(x0: i32, y0: i32, x1: i32, y1: i32) -> Option<(i32, i32, i32, i32)> {
    let left = x0.min(x1);
    let top = y0.min(y1);
    let w = (x0 - x1).abs();
    let h = (y0 - y1).abs();
    if w < 4 || h < 4 {
        return None;
    }
    Some((left, top, w, h))
}

/// Encodes top-down 32-bit `BGRA` pixels into PNG bytes.
pub fn bgra_top_down_to_png(bgra: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("invalid image dimensions".to_string());
    }
    let expected = (width as usize) * (height as usize) * 4;
    if bgra.len() < expected {
        return Err("bgra buffer too small".to_string());
    }
    let mut rgba = Vec::with_capacity(expected);
    for px in bgra[..expected].chunks_exact(4) {
        rgba.push(px[2]); // R
        rgba.push(px[1]); // G
        rgba.push(px[0]); // B
        rgba.push(255);   // opaque alpha
    }

    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("png header failed: {e}"))?;
        writer
            .write_image_data(&rgba)
            .map_err(|e| format!("png encode failed: {e}"))?;
    }
    Ok(out)
}

/// Captures a screen rectangle `(x, y, width, height)` in virtual-screen coordinates as PNG bytes.
pub fn capture_virtual_screen_rect(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<Vec<u8>, String> {
    if width <= 0 || height <= 0 {
        return Err("capture width/height must be positive".to_string());
    }
    unsafe {
        let hdc_screen = GetDC(None);
        if hdc_screen.is_invalid() {
            return Err("GetDC(screen) failed".to_string());
        }
        let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
        if hdc_mem.is_invalid() {
            let _ = ReleaseDC(None, hdc_screen);
            return Err("CreateCompatibleDC failed".to_string());
        }
        let hbm = CreateCompatibleBitmap(hdc_screen, width, height);
        if hbm.is_invalid() {
            let _ = DeleteDC(hdc_mem);
            let _ = ReleaseDC(None, hdc_screen);
            return Err("CreateCompatibleBitmap failed".to_string());
        }

        let old_obj = SelectObject(hdc_mem, hbm.into());
        let blt_ok = BitBlt(hdc_mem, 0, 0, width, height, Some(hdc_screen), x, y, SRCCOPY).is_ok();

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down DIB
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut buf = vec![0u8; (width as usize) * (height as usize) * 4];
        let lines = if blt_ok {
            GetDIBits(
                hdc_mem,
                hbm,
                0,
                height as u32,
                Some(buf.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };

        SelectObject(hdc_mem, old_obj);
        let _ = DeleteObject(hbm.into());
        let _ = DeleteDC(hdc_mem);
        let _ = ReleaseDC(None, hdc_screen);

        if !blt_ok || lines == 0 {
            return Err("screen BitBlt/GetDIBits failed".to_string());
        }
        bgra_top_down_to_png(&buf, width as u32, height as u32)
    }
}

unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    let lx = (l_param.0 & 0xFFFF) as i16 as i32;
    let ly = ((l_param.0 >> 16) & 0xFFFF) as i16 as i32;

    match msg {
        WM_LBUTTONDOWN => {
            if let Ok(mut guard) = OVERLAY_STATE.lock() {
                if let Some(st) = guard.as_mut() {
                    st.dragging = true;
                    st.start_x = lx;
                    st.start_y = ly;
                    st.end_x = lx;
                    st.end_y = ly;
                }
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            if let Ok(mut guard) = OVERLAY_STATE.lock() {
                if let Some(st) = guard.as_mut() {
                    st.cursor_x = lx;
                    st.cursor_y = ly;
                    st.has_cursor = true;
                    if st.dragging {
                        st.end_x = lx;
                        st.end_y = ly;
                    }
                }
            }
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let cw = (rc.right - rc.left).max(1);
            let ch = (rc.bottom - rc.top).max(1);

            let mem_dc = CreateCompatibleDC(Some(hdc));
            let mem_bmp = CreateCompatibleBitmap(hdc, cw, ch);
            let old_bmp = SelectObject(mem_dc, HGDIOBJ(mem_bmp.0));

            let bg_brush = CreateSolidBrush(COLORREF(0x0014_1615));
            let _ = FillRect(mem_dc, &rc, bg_brush);
            let _ = DeleteObject(HGDIOBJ(bg_brush.0));

            let (dragging, has_cursor, cx, cy, sel, accent_bgr) = OVERLAY_STATE
                .lock()
                .ok()
                .and_then(|g| {
                    g.as_ref().map(|st| {
                        (
                            st.dragging,
                            st.has_cursor,
                            st.cursor_x,
                            st.cursor_y,
                            if st.dragging {
                                normalize_selection_rect(st.start_x, st.start_y, st.end_x, st.end_y)
                            } else {
                                None
                            },
                            st.accent_bgr,
                        )
                    })
                })
                .unwrap_or((false, false, 0, 0, None, 0x00C2_7400));

            if !dragging && has_cursor {
                let guide_pen = CreatePen(PS_SOLID, 1, COLORREF(0x0058_5E5A));
                let old_p = SelectObject(mem_dc, HGDIOBJ(guide_pen.0));
                let _ = MoveToEx(mem_dc, 0, cy, None);
                let _ = LineTo(mem_dc, cw, cy);
                let _ = MoveToEx(mem_dc, cx, 0, None);
                let _ = LineTo(mem_dc, cx, ch);
                SelectObject(mem_dc, old_p);
                let _ = DeleteObject(HGDIOBJ(guide_pen.0));
            }

            let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let prim_w = GetSystemMetrics(SM_CXSCREEN).max(640);
            let pill_w = 430;
            let pill_h = 36;
            let pill_x = ((prim_w - pill_w) / 2) - vx;
            let pill_y = 28 - vy;
            let pill_brush = CreateSolidBrush(COLORREF(0x001C_1E1D));
            let pill_pen = CreatePen(PS_SOLID, 1, COLORREF(accent_bgr));
            let old_b = SelectObject(mem_dc, HGDIOBJ(pill_brush.0));
            let old_p = SelectObject(mem_dc, HGDIOBJ(pill_pen.0));
            let _ = RoundRect(mem_dc, pill_x, pill_y, pill_x + pill_w, pill_y + pill_h, 18, 18);
            SelectObject(mem_dc, old_b);
            SelectObject(mem_dc, old_p);
            let _ = DeleteObject(HGDIOBJ(pill_brush.0));
            let _ = DeleteObject(HGDIOBJ(pill_pen.0));

            let font = CreateFontW(
                -14,
                0,
                0,
                0,
                500,
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
            let old_f = if !font.is_invalid() {
                SelectObject(mem_dc, HGDIOBJ(font.0))
            } else {
                HGDIOBJ::default()
            };
            SetBkMode(mem_dc, TRANSPARENT);
            SetTextColor(mem_dc, COLORREF(0x00F2_F4F3));
            let mut hint_rc = RECT {
                left: pill_x,
                top: pill_y,
                right: pill_x + pill_w,
                bottom: pill_y + pill_h,
            };
            let mut hint_w: Vec<u16> = "ドラッグして範囲を選択   ·   Esc / 右クリックでキャンセル"
                .encode_utf16()
                .collect();
            let _ = DrawTextW(mem_dc, &mut hint_w, &mut hint_rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);

            if let Some((rx, ry, rw, rh)) = sel {
                let acc_pen = CreatePen(PS_SOLID, 2, COLORREF(accent_bgr));
                let old_p = SelectObject(mem_dc, HGDIOBJ(acc_pen.0));
                let old_b = SelectObject(mem_dc, GetStockObject(NULL_BRUSH));
                let _ = Rectangle(mem_dc, rx, ry, rx + rw, ry + rh);
                SelectObject(mem_dc, old_b);
                SelectObject(mem_dc, old_p);
                let _ = DeleteObject(HGDIOBJ(acc_pen.0));

                let white_pen = CreatePen(PS_SOLID, 1, COLORREF(0x00FF_FFFF));
                let old_p = SelectObject(mem_dc, HGDIOBJ(white_pen.0));
                let old_b = SelectObject(mem_dc, GetStockObject(NULL_BRUSH));
                let _ = Rectangle(mem_dc, rx + 2, ry + 2, (rx + rw - 2).max(rx + 2), (ry + rh - 2).max(ry + 2));
                SelectObject(mem_dc, old_b);
                SelectObject(mem_dc, old_p);
                let _ = DeleteObject(HGDIOBJ(white_pen.0));

                let corner_pen = CreatePen(PS_SOLID, 2, COLORREF(0x00FF_FFFF));
                let old_p = SelectObject(mem_dc, HGDIOBJ(corner_pen.0));
                let cl = 10.min(rw / 2).min(rh / 2);
                let (x0, y0, x1, y1) = (rx, ry, rx + rw, ry + rh);
                let _ = MoveToEx(mem_dc, x0, y0 + cl, None); let _ = LineTo(mem_dc, x0, y0); let _ = LineTo(mem_dc, x0 + cl, y0);
                let _ = MoveToEx(mem_dc, x1 - cl, y0, None); let _ = LineTo(mem_dc, x1, y0); let _ = LineTo(mem_dc, x1, y0 + cl);
                let _ = MoveToEx(mem_dc, x0, y1 - cl, None); let _ = LineTo(mem_dc, x0, y1); let _ = LineTo(mem_dc, x0 + cl, y1);
                let _ = MoveToEx(mem_dc, x1 - cl, y1, None); let _ = LineTo(mem_dc, x1, y1); let _ = LineTo(mem_dc, x1, y1 - cl);
                SelectObject(mem_dc, old_p);
                let _ = DeleteObject(HGDIOBJ(corner_pen.0));

                let bw = 108;
                let bh = 24;
                let bx = (x1 + 8).min(cw - bw - 8).max(8);
                let by = if y1 + 8 + bh < ch { y1 + 8 } else { (y0 - bh - 8).max(8) };
                let b_brush = CreateSolidBrush(COLORREF(0x001C_1E1D));
                let b_pen = CreatePen(PS_SOLID, 1, COLORREF(accent_bgr));
                let old_b = SelectObject(mem_dc, HGDIOBJ(b_brush.0));
                let old_p = SelectObject(mem_dc, HGDIOBJ(b_pen.0));
                let _ = RoundRect(mem_dc, bx, by, bx + bw, by + bh, 10, 10);
                SelectObject(mem_dc, old_b);
                SelectObject(mem_dc, old_p);
                let _ = DeleteObject(HGDIOBJ(b_brush.0));
                let _ = DeleteObject(HGDIOBJ(b_pen.0));

                let mut dim_rc = RECT { left: bx, top: by, right: bx + bw, bottom: by + bh };
                let mut dim_w: Vec<u16> = format!("{rw} × {rh}").encode_utf16().collect();
                let _ = DrawTextW(mem_dc, &mut dim_w, &mut dim_rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            }

            if !font.is_invalid() {
                SelectObject(mem_dc, old_f);
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
            let _ = BitBlt(hdc, 0, 0, cw, ch, Some(mem_dc), 0, 0, SRCCOPY);
            SelectObject(mem_dc, old_bmp);
            let _ = DeleteObject(HGDIOBJ(mem_bmp.0));
            let _ = DeleteDC(mem_dc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Ok(mut guard) = OVERLAY_STATE.lock() {
                if let Some(st) = guard.as_mut() {
                    st.dragging = false;
                    st.end_x = lx;
                    st.end_y = ly;
                    st.confirmed = true;
                }
            }
            let _ = DestroyWindow(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            if let Ok(mut guard) = OVERLAY_STATE.lock() {
                if let Some(st) = guard.as_mut() {
                    st.cancelled = true;
                }
            }
            let _ = DestroyWindow(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_KEYDOWN if (w_param.0 as u16) == VK_ESCAPE.0 => {
            if let Ok(mut guard) = OVERLAY_STATE.lock() {
                if let Some(st) = guard.as_mut() {
                    st.cancelled = true;
                }
            }
            let _ = DestroyWindow(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w_param, l_param),
    }
}

/// Launches a transparent Win32 region-selection overlay across the virtual screen
/// and captures the selected rectangle as PNG bytes (`Ok(None)` if cancelled).
pub fn capture_region_interactive() -> Result<Option<Vec<u8>>, String> {
    capture_region_interactive_with_accent(0x00C2_7400)
}

/// Launches the screen-region capture overlay tinted with the given Win32 BGR accent color.
pub fn capture_region_interactive_with_accent(accent_bgr: u32) -> Result<Option<Vec<u8>>, String> {
    unsafe {
        if !OVERLAY_CLASS_REG.swap(true, Ordering::AcqRel) {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(overlay_wnd_proc),
                hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
                lpszClassName: w!("SykiImgCapOverlayClass"),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);
        }

        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);

        *OVERLAY_STATE.lock().expect("overlay state mutex") = Some(OverlayDragState {
            dragging: false,
            start_x: 0,
            start_y: 0,
            end_x: 0,
            end_y: 0,
            cursor_x: 0,
            cursor_y: 0,
            has_cursor: false,
            accent_bgr,
            confirmed: false,
            cancelled: false,
        });

        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            w!("SykiImgCapOverlayClass"),
            w!("Select Screen Region"),
            WS_POPUP,
            vx,
            vy,
            vw,
            vh,
            None,
            None,
            None,
            None,
        )
        .map_err(|e| format!("overlay CreateWindowExW failed: {e}"))?;

        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 115, LWA_ALPHA);
        let _ = ShowWindow(hwnd, SW_SHOW);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let st = OVERLAY_STATE
            .lock()
            .expect("overlay state mutex")
            .take()
            .ok_or_else(|| "missing overlay state".to_string())?;
        if st.cancelled || !st.confirmed {
            return Ok(None);
        }
        let Some((rx, ry, rw, rh)) =
            normalize_selection_rect(st.start_x, st.start_y, st.end_x, st.end_y)
        else {
            return Ok(None);
        };
        let _ = RECT::default();
        capture_virtual_screen_rect(vx + rx, vy + ry, rw, rh).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_selection_rect_handles_reverse_drag_and_tiny_clicks() {
        assert_eq!(normalize_selection_rect(100, 200, 50, 120), Some((50, 120, 50, 80)));
        assert_eq!(normalize_selection_rect(10, 10, 12, 12), None);
    }

    #[test]
    fn bgra_top_down_to_png_produces_valid_png_signature() {
        // 2x1 pixels: red (B=0,G=0,R=255) and blue (B=255,G=0,R=0)
        let bgra = [0u8, 0, 255, 255, 255, 0, 0, 255];
        let png_bytes = bgra_top_down_to_png(&bgra, 2, 1).expect("png");
        assert!(png_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
}
