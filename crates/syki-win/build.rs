use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

fn mime_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("json") => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn should_skip(name: &str) -> bool {
    name.ends_with("_test.js") || name == ".DS_Store" || name == "Thumbs.db"
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, &'static str, String)>) {
    let entries = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(err) => panic!("failed to read frontend dir {}: {err}", dir.display()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if should_skip(&file_name) {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, out);
            continue;
        }
        if !path.is_file() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        let rel = path
            .strip_prefix(root)
            .expect("asset inside frontend root")
            .to_string_lossy()
            .replace('\\', "/");
        let url_path = format!("/{rel}");
        let abs = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let abs_str = abs.to_string_lossy();
        let clean_abs = abs_str.strip_prefix(r"\\?\").unwrap_or(&abs_str).to_string();
        out.push((url_path, mime_for_path(&path), clean_abs));
    }
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let raw_frontend = manifest_dir.join("../../frontend");
    let frontend_root = fs::canonicalize(&raw_frontend).unwrap_or(raw_frontend);
    println!("cargo:rerun-if-changed={}", frontend_root.display());

    let mut entries = Vec::new();
    collect_files(&frontend_root, &frontend_root, &mut entries);
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let dest = out_dir.join("embedded_assets.rs");
    let mut file = fs::File::create(&dest).expect("create embedded_assets.rs");

    writeln!(file, "static EMBEDDED_ASSETS: &[(&str, &str, &[u8])] = &[").unwrap();
    for (url_path, mime, abs_path) in entries {
        assert!(!abs_path.contains('"'), "unexpected quote in path: {abs_path}");
        writeln!(
            file,
            "    ({url_path:?}, {mime:?}, include_bytes!(r#\"{abs_path}\"#)),"
        )
        .unwrap();
    }
    writeln!(file, "];").unwrap();
}
