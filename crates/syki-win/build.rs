use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::write::GzEncoder;
use flate2::Compression;

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

fn extract_attr_href_or_src(line: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    let raw = &rest[..end];
    Some(raw.split('?').next().unwrap_or(raw).to_string())
}

fn build_bundled_assets(frontend_root: &Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let index_path = frontend_root.join("index.html");
    let raw_html = fs::read_to_string(&index_path).ok()?;

    let mut bundled_css = String::with_capacity(64 * 1024);
    let mut bundled_js = String::with_capacity(1024 * 1024);
    let mut out_html = String::with_capacity(raw_html.len());
    let mut css_slot_inserted = false;
    let mut js_slot_inserted = false;

    for line in raw_html.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("<link ")
            && trimmed.contains("rel=\"stylesheet\"")
            && !trimmed.contains("media=\"print\"")
        {
            if let Some(rel) = extract_attr_href_or_src(trimmed, "href") {
                if rel.starts_with("css/") {
                    if let Ok(css) = fs::read_to_string(frontend_root.join(&rel)) {
                        bundled_css.push_str(&css);
                        bundled_css.push('\n');
                        if !css_slot_inserted {
                            out_html.push_str("  <!--SYKI_BUNDLED_CSS-->\n");
                            css_slot_inserted = true;
                        }
                        continue;
                    }
                }
            }
        }

        if trimmed.starts_with("<script src=\"js/") && trimmed.ends_with("</script>") {
            if let Some(rel) = extract_attr_href_or_src(trimmed, "src") {
                if let Ok(js) = fs::read_to_string(frontend_root.join(&rel)) {
                    bundled_js.push_str(&js);
                    bundled_js.push_str("\n;\n");
                    if !js_slot_inserted {
                        out_html.push_str("  <script defer src=\"js/app.bundle.js\"></script>\n");
                        js_slot_inserted = true;
                    }
                    continue;
                }
            }
        }

        out_html.push_str(line);
        out_html.push('\n');
    }

    let style_tag = format!("<style>\n{bundled_css}</style>");
    let final_html = out_html.replace("  <!--SYKI_BUNDLED_CSS-->\n", &style_tag);
    Some((final_html.into_bytes(), bundled_js.into_bytes()))
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

    let bundled = build_bundled_assets(&frontend_root);
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let gz_dir = out_dir.join("gz_assets");
    fs::create_dir_all(&gz_dir).expect("create gz_assets dir");

    let dest = out_dir.join("embedded_assets.rs");
    let mut file = fs::File::create(&dest).expect("create embedded_assets.rs");

    writeln!(
        file,
        "static EMBEDDED_ASSETS: &[(&str, &str, bool, &[u8])] = &["
    )
    .unwrap();
    let mut emit_asset = |idx: usize, url_path: &str, mime: &str, raw_bytes: &[u8], fallback_path: Option<&str>| {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(raw_bytes).expect("gzip encode");
        let gz_bytes = encoder.finish().expect("gzip finish");

        if gz_bytes.len() + 32 < raw_bytes.len() || fallback_path.is_none() {
            let gz_path = gz_dir.join(format!("asset_{idx}.gz"));
            fs::write(&gz_path, &gz_bytes).expect("write compressed asset");
            let gz_str = gz_path.to_string_lossy();
            let clean_gz = gz_str.strip_prefix(r"\\?\").unwrap_or(&gz_str);
            writeln!(
                file,
                "    ({url_path:?}, {mime:?}, true, include_bytes!(r#\"{clean_gz}\"#)),"
            )
            .unwrap();
        } else if let Some(abs_path) = fallback_path {
            writeln!(
                file,
                "    ({url_path:?}, {mime:?}, false, include_bytes!(r#\"{abs_path}\"#)),"
            )
            .unwrap();
        }
    };

    let mut next_idx = 0usize;
    if let Some((_, ref bundle_js)) = bundled {
        emit_asset(
            next_idx,
            "/js/app.bundle.js",
            "application/javascript; charset=utf-8",
            bundle_js,
            None,
        );
        next_idx += 1;
    }

    for (url_path, mime, abs_path) in entries {
        assert!(!abs_path.contains('"'), "unexpected quote in path: {abs_path}");
        if url_path == "/index.html" {
            if let Some((ref html_bytes, _)) = bundled {
                emit_asset(next_idx, &url_path, mime, html_bytes, None);
                next_idx += 1;
                continue;
            }
        }
        let raw_bytes = fs::read(&abs_path).expect("read asset bytes");
        emit_asset(next_idx, &url_path, mime, &raw_bytes, Some(&abs_path));
        next_idx += 1;
    }
    writeln!(file, "];").unwrap();
}
