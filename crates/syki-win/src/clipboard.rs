//! Native Win32 clipboard PNG writer (`CF_DIB` + registered `"PNG"` format) (#05).

use std::io::Cursor;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use windows::core::w;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

const CF_DIB_FMT: u32 = 8;

/// Decodes a `data:image/png;base64,...` URL (or raw base64) into PNG bytes.
pub fn decode_png_data_url(data_url: &str) -> Result<Vec<u8>, String> {
    let mut s = data_url.trim();
    if let Some(idx) = s.find(";base64,") {
        s = &s[idx + ";base64,".len()..];
    }
    let cleaned: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if cleaned.is_empty() {
        return Err("empty PNG payload".to_string());
    }
    STANDARD
        .decode(cleaned.as_bytes())
        .map_err(|e| format!("decode base64 PNG: {e}"))
}

/// Converts PNG bytes into a 24-bit bottom-up BGR `CF_DIB` composited over white.
pub fn png_to_dib24(png_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let decoder = png::Decoder::new(Cursor::new(png_bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("decode PNG header: {e}"))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("decode PNG frame: {e}"))?;

    let w = info.width as usize;
    let h = info.height as usize;
    if w == 0 || h == 0 {
        return Err("invalid PNG dimensions".to_string());
    }

    const HEADER_SIZE: usize = 40;
    let row_stride = (w * 3 + 3) & !3;
    let img_size = row_stride * h;
    let mut dib = vec![0u8; HEADER_SIZE + img_size];

    dib[0..4].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
    dib[4..8].copy_from_slice(&(w as u32).to_le_bytes());
    dib[8..12].copy_from_slice(&(h as u32).to_le_bytes());
    dib[12..14].copy_from_slice(&1u16.to_le_bytes());
    dib[14..16].copy_from_slice(&24u16.to_le_bytes());
    dib[16..20].copy_from_slice(&0u32.to_le_bytes());
    dib[20..24].copy_from_slice(&(img_size as u32).to_le_bytes());

    let src = &buf[..info.buffer_size()];
    for y in 0..h {
        let src_y = h - 1 - y;
        let dst_row = HEADER_SIZE + y * row_stride;
        for x in 0..w {
            let (r, g, b, a) = sample_rgba(src, info.color_type, src_y * w + x);
            let inv_a = 255u16 - a as u16;
            let comp = |c: u8| -> u8 { ((c as u16 * a as u16 + 255 * inv_a) / 255).min(255) as u8 };
            let px = dst_row + x * 3;
            dib[px] = comp(b);
            dib[px + 1] = comp(g);
            dib[px + 2] = comp(r);
        }
    }
    Ok(dib)
}

fn sample_rgba(src: &[u8], color_type: png::ColorType, idx: usize) -> (u8, u8, u8, u8) {
    match color_type {
        png::ColorType::Rgba => {
            let i = idx * 4;
            (src[i], src[i + 1], src[i + 2], src[i + 3])
        }
        png::ColorType::Rgb => {
            let i = idx * 3;
            (src[i], src[i + 1], src[i + 2], 255)
        }
        png::ColorType::GrayscaleAlpha => {
            let i = idx * 2;
            (src[i], src[i], src[i], src[i + 1])
        }
        png::ColorType::Grayscale => {
            let v = src[idx];
            (v, v, v, 255)
        }
        png::ColorType::Indexed => (0, 0, 0, 255),
    }
}

unsafe fn alloc_global_bytes(data: &[u8]) -> Result<HGLOBAL, String> {
    if data.is_empty() {
        return Err("empty clipboard data".to_string());
    }
    let hmem = GlobalAlloc(GMEM_MOVEABLE, data.len())
        .map_err(|e| format!("GlobalAlloc failed: {e}"))?;
    let ptr = GlobalLock(hmem);
    if ptr.is_null() {
        let _ = GlobalFree(Some(hmem));
        return Err("GlobalLock returned null".to_string());
    }
    std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len());
    let _ = GlobalUnlock(hmem);
    Ok(hmem)
}

/// Copies a PNG data URL to the Windows clipboard as both `CF_DIB` and `"PNG"`.
pub fn copy_png_to_clipboard(data_url: &str) -> Result<(), String> {
    let png_bytes = decode_png_data_url(data_url)?;
    let dib = png_to_dib24(&png_bytes)?;

    unsafe {
        OpenClipboard(None).map_err(|e| format!("OpenClipboard failed: {e}"))?;
        let _ = EmptyClipboard();

        let h_dib = match alloc_global_bytes(&dib) {
            Ok(h) => h,
            Err(e) => {
                let _ = CloseClipboard();
                return Err(e);
            }
        };
        if SetClipboardData(CF_DIB_FMT, Some(HANDLE(h_dib.0))).is_err() {
            let _ = GlobalFree(Some(h_dib));
            let _ = CloseClipboard();
            return Err("SetClipboardData(CF_DIB) failed".to_string());
        }

        let png_fmt = RegisterClipboardFormatW(w!("PNG"));
        if png_fmt != 0 {
            if let Ok(h_png) = alloc_global_bytes(&png_bytes) {
                if SetClipboardData(png_fmt, Some(HANDLE(h_png.0))).is_err() {
                    let _ = GlobalFree(Some(h_png));
                }
            }
        }
        let _ = CloseClipboard();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_to_dib24_encodes_bottom_up_bgr_with_white_alpha_composite() {
        let mut png_buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_buf, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            // Pure transparent pixel -> composites over white to (255, 255, 255)
            writer.write_image_data(&[255, 0, 0, 0]).unwrap();
        }
        let dib = png_to_dib24(&png_buf).expect("dib");
        assert_eq!(dib.len(), 40 + 4); // 1px 24bpp row padded to 4 bytes
        assert_eq!(&dib[40..43], &[255, 255, 255]);
    }
}
