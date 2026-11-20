//! Asset persistence (`saveAsset` / `importAssetFile`) for pasted images & dropped files (#05).

use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use chrono::Local;
use serde::Serialize;

pub const MAX_ASSET_BYTES: usize = 25 * 1024 * 1024;

const ALLOWED_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "webm", "ogg", "m4a", "mp3", "wav", "pdf", "txt", "md",
];

const RESERVED_WINDOWS_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AssetResult {
    pub abs_path: String,
    pub rel_path: String,
    pub file_url: String,
}

/// Strips an optional `data:...;base64,` prefix, validates encoded length, and decodes up to `cap_bytes`.
pub fn decode_capped_base64(input: &str, cap_bytes: usize) -> Result<Vec<u8>, String> {
    let mut s = input.trim();
    if s.starts_with("data:") {
        if let Some(idx) = s.find(',') {
            s = &s[idx + 1..];
        }
    }
    let cleaned: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let max_encoded_len = ((cap_bytes + 2) / 3) * 4 + 8;
    if cleaned.len() > max_encoded_len {
        return Err("asset payload exceeds maximum size".to_string());
    }
    let bytes = STANDARD
        .decode(cleaned.as_bytes())
        .map_err(|e| format!("base64 decode failed: {e}"))?;
    if bytes.len() > cap_bytes {
        return Err("asset payload exceeds maximum size".to_string());
    }
    Ok(bytes)
}

/// Sanitizes an arbitrary filename into a safe single-component Windows filename.
pub fn sanitize_asset_filename(name: &str) -> String {
    let normalized = name.replace('\\', "/");
    let base_raw = normalized.rsplit('/').next().unwrap_or("");
    let mut out = String::with_capacity(base_raw.len());
    for ch in base_raw.chars() {
        if (ch as u32) < 0x20 || ch as u32 == 0x7f {
            continue;
        }
        match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => out.push('_'),
            _ => out.push(ch),
        }
    }
    let trimmed = out.trim_end_matches([' ', '.']);
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return "file".to_string();
    }
    let (stem, ext) = match trimmed.rfind('.') {
        Some(idx) if idx > 0 => (&trimmed[..idx], Some(&trimmed[idx + 1..])),
        _ => (trimmed, None),
    };
    let safe_stem = if RESERVED_WINDOWS_NAMES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(stem))
    {
        format!("_{stem}")
    } else {
        stem.to_string()
    };
    match ext {
        Some(e) => format!("{safe_stem}.{e}"),
        None => safe_stem,
    }
}

/// Converts an absolute filesystem path into a `file://` URL.
pub fn file_url_from_path(abs: &Path) -> String {
    let slashed = abs.to_string_lossy().replace('\\', "/");
    let encoded: String = slashed
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'/'
            | b':' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    if encoded.starts_with("//") {
        format!("file:{encoded}")
    } else if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    }
}

fn resolve_asset_root(base_dir: &str, app_data_dir: &Path) -> Result<(PathBuf, bool), String> {
    let trimmed = base_dir.trim();
    if trimmed.is_empty() {
        return Ok((app_data_dir.to_path_buf(), false));
    }
    let p = Path::new(trimmed);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("current_dir failed: {e}"))?
            .join(p)
    };
    Ok((abs, true))
}

fn unique_asset_path(dir: &Path, base: &str, ext: &str) -> (PathBuf, String) {
    let name_for = |b: &str| -> String {
        if ext.is_empty() {
            b.to_string()
        } else {
            format!("{b}.{ext}")
        }
    };
    let first = name_for(base);
    let first_path = dir.join(&first);
    if !first_path.exists() {
        return (first_path, first);
    }
    for i in 2..10_000 {
        let candidate = name_for(&format!("{base}-{i}"));
        let p = dir.join(&candidate);
        if !p.exists() {
            return (p, candidate);
        }
    }
    let fallback = name_for(&format!("{base}-copy"));
    (dir.join(&fallback), fallback)
}

/// Writes a whitelisted base64 payload to `<root>/assets/YYYY-MM-DD-HHmmss.<ext>`.
pub fn save_asset(
    app_data_dir: &Path,
    base_dir: &str,
    ext: &str,
    data_base64: &str,
) -> Result<AssetResult, String> {
    let clean_ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    if !ALLOWED_EXTS.contains(&clean_ext.as_str()) {
        return Err(format!("disallowed asset extension: {clean_ext}"));
    }
    let data = decode_capped_base64(data_base64, MAX_ASSET_BYTES)?;
    let (root, has_base_dir) = resolve_asset_root(base_dir, app_data_dir)?;
    let assets_dir = root.join("assets");
    std::fs::create_dir_all(&assets_dir)
        .map_err(|e| format!("failed to create assets dir: {e}"))?;

    let stamp = Local::now().format("%Y-%m-%d-%H%M%S").to_string();
    let (abs_path, name) = unique_asset_path(&assets_dir, &stamp, &clean_ext);
    std::fs::write(&abs_path, &data).map_err(|e| format!("failed to write asset: {e}"))?;

    Ok(AssetResult {
        abs_path: abs_path.to_string_lossy().into_owned(),
        rel_path: if has_base_dir {
            format!("./assets/{name}")
        } else {
            String::new()
        },
        file_url: file_url_from_path(&abs_path),
    })
}

/// Writes an imported file payload to `<root>/assets/<sanitized-name>`, deduplicating collisions.
pub fn import_asset_file(
    app_data_dir: &Path,
    base_dir: &str,
    file_name: &str,
    data_base64: &str,
) -> Result<AssetResult, String> {
    let data = decode_capped_base64(data_base64, MAX_ASSET_BYTES)?;
    let sanitized = sanitize_asset_filename(file_name);
    let (base, ext) = match sanitized.rfind('.') {
        Some(idx) if idx > 0 => (&sanitized[..idx], &sanitized[idx + 1..]),
        _ => (sanitized.as_str(), ""),
    };
    let (root, has_base_dir) = resolve_asset_root(base_dir, app_data_dir)?;
    let assets_dir = root.join("assets");
    std::fs::create_dir_all(&assets_dir)
        .map_err(|e| format!("failed to create assets dir: {e}"))?;

    let (abs_path, name) = unique_asset_path(&assets_dir, base, ext);
    std::fs::write(&abs_path, &data).map_err(|e| format!("failed to write asset: {e}"))?;

    Ok(AssetResult {
        abs_path: abs_path.to_string_lossy().into_owned(),
        rel_path: if has_base_dir {
            format!("./assets/{name}")
        } else {
            String::new()
        },
        file_url: file_url_from_path(&abs_path),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("syki-assets-{label}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn save_asset_writes_with_rel_path_and_rejects_bad_ext() {
        let app_dir = temp_dir("app");
        let note_dir = temp_dir("note");
        let b64 = STANDARD.encode(b"hello png");

        let res = save_asset(&app_dir, note_dir.to_str().unwrap(), "png", &b64).expect("saveAsset");
        assert!(res.rel_path.starts_with("./assets/"));
        assert!(res.rel_path.ends_with(".png"));
        assert!(Path::new(&res.abs_path).is_file());

        let fallback = save_asset(&app_dir, "", "txt", &b64).expect("fallback to app_dir");
        assert_eq!(fallback.rel_path, "");
        assert!(Path::new(&fallback.abs_path).starts_with(&app_dir));

        assert!(save_asset(&app_dir, "", "exe", &b64).is_err());

        let _ = std::fs::remove_dir_all(app_dir);
        let _ = std::fs::remove_dir_all(note_dir);
    }

    #[test]
    fn import_asset_file_sanitizes_traversal_reserved_names_and_collisions() {
        let app_dir = temp_dir("imp-app");
        let note_dir = temp_dir("imp-note");
        let b64 = STANDARD.encode(b"pdf-bytes");

        let r1 = import_asset_file(&app_dir, note_dir.to_str().unwrap(), "../../etc/report.pdf", &b64)
            .expect("import 1");
        assert_eq!(r1.rel_path, "./assets/report.pdf");

        let r2 = import_asset_file(&app_dir, note_dir.to_str().unwrap(), "report.pdf", &b64)
            .expect("import 2");
        assert_eq!(r2.rel_path, "./assets/report-2.pdf");

        assert_eq!(sanitize_asset_filename("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_asset_filename("../.."), "file");

        let _ = std::fs::remove_dir_all(app_dir);
        let _ = std::fs::remove_dir_all(note_dir);
    }

    #[test]
    fn decode_capped_base64_enforces_cap_and_strips_data_url() {
        let data_url = format!("data:image/png;base64,{}", STANDARD.encode(b"abc"));
        assert_eq!(decode_capped_base64(&data_url, 1024).unwrap(), b"abc");
        let oversized = "A".repeat(((MAX_ASSET_BYTES + 10) / 3) * 4 + 16);
        assert!(decode_capped_base64(&oversized, MAX_ASSET_BYTES).is_err());
    }
}
