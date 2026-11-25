//! Safe extension storage and `/__ext/` virtual route resolver for syki.
//!
//! Uses only the Rust standard library (plus `serde_json` already in `syki-win`)
//! to resolve user CSS and plugin static assets with portable precedence,
//! strict path-traversal protection, and safe-mode gating.

use std::path::{Component, Path, PathBuf};

pub const EXT_ROUTE_PREFIX: &str = "/__ext/";

const MIME_PLAIN: &str = "text/plain; charset=utf-8";

#[derive(Debug, Clone)]
pub struct ExtRoots {
    pub portable_dir: Option<PathBuf>,
    pub data_dir: PathBuf,
    pub safe_mode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFile {
    pub path: PathBuf,
    pub is_portable: bool,
}

/// Returns the executable's parent directory as the portable root, if available.
pub fn portable_dir_from_exe() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

/// Returns `true` when safe mode is enabled via `--safe-mode` CLI flag or `SYKI_SAFE_MODE=1|true`.
pub fn safe_mode_from_args() -> bool {
    if std::env::args().any(|arg| arg == "--safe-mode") {
        return true;
    }
    std::env::var("SYKI_SAFE_MODE")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

impl ExtRoots {
    pub fn new(portable_dir: Option<PathBuf>, data_dir: PathBuf, safe_mode: bool) -> Self {
        Self {
            portable_dir,
            data_dir,
            safe_mode,
        }
    }

    pub fn has_user_css(&self) -> bool {
        self.resolve_user_css().is_some()
    }

    pub fn has_plugins_dir(&self) -> bool {
        if self.safe_mode {
            return false;
        }
        if let Some(ref portable) = self.portable_dir {
            if portable.join("plugins").is_dir() {
                return true;
            }
        }
        self.data_dir.join("plugins").is_dir()
    }

    pub fn resolve_user_css(&self) -> Option<ResolvedFile> {
        if self.safe_mode {
            return None;
        }
        if let Some(ref portable) = self.portable_dir {
            let p = portable.join("user.css");
            if p.is_file() {
                return Some(ResolvedFile {
                    path: p,
                    is_portable: true,
                });
            }
        }
        let d = self.data_dir.join("user.css");
        if d.is_file() {
            return Some(ResolvedFile {
                path: d,
                is_portable: false,
            });
        }
        None
    }

    /// Discovers installed plugins (`plugins/<id>/plugin.js`) in portable_dir (precedence)
    /// and data_dir, returning deterministic preload descriptors.
    pub fn list_plugins(&self) -> Vec<serde_json::Value> {
        if self.safe_mode {
            return Vec::new();
        }
        let mut seen = std::collections::BTreeMap::<String, bool>::new();
        let mut scan_root = |root: &Path, is_portable: bool| {
            let plugins_dir = root.join("plugins");
            let Ok(entries) = std::fs::read_dir(plugins_dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if id.is_empty()
                    || id == "."
                    || id == ".."
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                {
                    continue;
                }
                if path.join("plugin.js").is_file() {
                    seen.entry(id).or_insert(is_portable);
                }
            }
        };
        if let Some(ref portable) = self.portable_dir {
            scan_root(portable, true);
        }
        scan_root(&self.data_dir, false);
        seen.into_iter()
            .map(|(id, is_portable)| {
                serde_json::json!({
                    "id": id,
                    "name": id,
                    "url": format!("/__ext/r/1/plugins/{id}/plugin.js"),
                    "isPortable": is_portable
                })
            })
            .collect()
    }

    /// Lightweight preload metadata for `window.__sykiPreload.ext`.
    pub fn preload_ext_flags(&self) -> serde_json::Value {
        if self.safe_mode {
            return serde_json::json!({
                "hasUserCss": false,
                "hasPlugins": false,
                "safeMode": true,
                "isPortableCss": false,
                "plugins": []
            });
        }
        let resolved_css = self.resolve_user_css();
        let has_user_css = resolved_css.is_some();
        let is_portable_css = resolved_css.is_some_and(|f| f.is_portable);
        let plugins = self.list_plugins();
        let has_plugins = self.has_plugins_dir() || !plugins.is_empty();
        serde_json::json!({
            "hasUserCss": has_user_css,
            "hasPlugins": has_plugins,
            "safeMode": false,
            "isPortableCss": is_portable_css,
            "plugins": plugins
        })
    }

    /// Handles `/__ext/user.css`, `/__ext/r/<rev>/user.css`,
    /// `/__ext/plugins/<id>/<file>`, `/__ext/r/<rev>/plugins/<id>/<file>`.
    pub fn resolve_ext_request(&self, rel_path: &str) -> Option<(u16, &'static str, Vec<u8>)> {
        let stripped = rel_path.split(['?', '#']).next().unwrap_or(rel_path);
        let raw_rest = stripped.strip_prefix(EXT_ROUTE_PREFIX)?;

        if self.safe_mode {
            return Some(not_found());
        }

        let decoded = match percent_decode_strict(raw_rest) {
            Some(s) => s,
            None => return Some(forbidden()),
        };

        if has_forbidden_chars(&decoded) {
            return Some(forbidden());
        }

        let segments: Vec<&str> = decoded.split('/').collect();
        if segments.is_empty()
            || segments
                .iter()
                .any(|s| s.is_empty() || *s == "." || *s == "..")
        {
            return Some(forbidden());
        }

        let mut slice = segments.as_slice();
        if slice.len() >= 3 && slice[0] == "r" {
            let rev = slice[1];
            if rev.is_empty()
                || !rev
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Some(forbidden());
            }
            slice = &slice[2..];
        }

        let last_seg = match slice.last() {
            Some(s) => *s,
            None => return Some(forbidden()),
        };
        let mime = match mime_for_filename(last_seg) {
            Some(m) => m,
            None => return Some(forbidden()),
        };

        let (base_dir, candidate) = match slice {
            ["user.css"] => {
                if let Some(ref portable) = self.portable_dir {
                    let p = portable.join("user.css");
                    if std::fs::symlink_metadata(&p).is_ok() {
                        (portable.clone(), p)
                    } else {
                        (self.data_dir.clone(), self.data_dir.join("user.css"))
                    }
                } else {
                    (self.data_dir.clone(), self.data_dir.join("user.css"))
                }
            }
            ["plugins", plugin_id, rest @ ..] if !rest.is_empty() => {
                if !is_safe_segment(plugin_id) || rest.iter().any(|s| !is_safe_segment(s)) {
                    return Some(forbidden());
                }
                let base = self.resolve_plugin_base_dir(plugin_id);
                let mut candidate = base.clone();
                for seg in rest {
                    candidate.push(seg);
                }
                (base, candidate)
            }
            _ => return Some(not_found()),
        };

        if std::fs::symlink_metadata(&candidate).is_err() {
            return Some(not_found());
        }

        let Ok(base_canonical) = std::fs::canonicalize(&base_dir) else {
            return Some(not_found());
        };
        let Ok(canonical) = std::fs::canonicalize(&candidate) else {
            return Some(not_found());
        };

        if !canonical.starts_with(&base_canonical) {
            return Some(forbidden());
        }
        if !canonical.is_file() {
            return Some(not_found());
        }

        match std::fs::read(&canonical) {
            Ok(bytes) => Some((200, mime, bytes)),
            Err(_) => Some(not_found()),
        }
    }

    fn resolve_plugin_base_dir(&self, plugin_id: &str) -> PathBuf {
        if let Some(ref portable) = self.portable_dir {
            let p = portable.join("plugins").join(plugin_id);
            if p.is_dir() {
                return p;
            }
        }
        self.data_dir.join("plugins").join(plugin_id)
    }
}

pub fn content_security_policy() -> &'static str {
    "default-src 'self' https://app.syki.local; script-src 'self' https://app.syki.local 'unsafe-inline'; style-src 'self' https://app.syki.local 'unsafe-inline'; img-src 'self' https://app.syki.local data: blob:; font-src 'self' https://app.syki.local data:; connect-src 'self' https://app.syki.local; object-src 'self' https://app.syki.local; frame-src 'self' https://app.syki.local; base-uri 'self';"
}

pub fn is_immutable_virtual_asset(status: u16, rel_path: &str) -> bool {
    status == 200
        && !rel_path.is_empty()
        && rel_path != "index.html"
        && !rel_path.starts_with("api/")
        && !rel_path.starts_with("__ext/")
        && (rel_path.ends_with(".js")
            || rel_path.ends_with(".css")
            || rel_path.ends_with(".png")
            || rel_path.ends_with(".svg")
            || rel_path.ends_with(".woff2")
            || rel_path.ends_with(".ico"))
}

fn forbidden() -> (u16, &'static str, Vec<u8>) {
    (403, MIME_PLAIN, b"Forbidden".to_vec())
}

fn not_found() -> (u16, &'static str, Vec<u8>) {
    (404, MIME_PLAIN, b"Not Found".to_vec())
}

fn has_forbidden_chars(s: &str) -> bool {
    s.bytes()
        .any(|b| b < 0x20 || b == 0x7f || b == b'\\' || b == b':')
}

fn is_safe_segment(seg: &str) -> bool {
    if seg.is_empty() || seg == "." || seg == ".." || has_forbidden_chars(seg) {
        return false;
    }
    let mut comps = Path::new(seg).components();
    matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none()
}

fn mime_for_filename(filename: &str) -> Option<&'static str> {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    match ext.as_str() {
        "css" => Some("text/css; charset=utf-8"),
        "js" => Some("application/javascript; charset=utf-8"),
        "json" => Some("application/json; charset=utf-8"),
        "svg" => Some("image/svg+xml"),
        "png" => Some("image/png"),
        "woff2" => Some("font/woff2"),
        _ => None,
    }
}

fn percent_decode_strict(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_val(bytes[i + 1])?;
            let lo = hex_val(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempRoots {
        root: PathBuf,
        portable: PathBuf,
        data: PathBuf,
    }

    impl TempRoots {
        fn new(name: &str) -> Self {
            let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "syki_ext_store_{}_{}_{}",
                name,
                std::process::id(),
                n
            ));
            let _ = std::fs::remove_dir_all(&root);
            let portable = root.join("portable");
            let data = root.join("data");
            std::fs::create_dir_all(&portable).unwrap();
            std::fs::create_dir_all(&data).unwrap();
            Self {
                root,
                portable,
                data,
            }
        }
    }

    impl Drop for TempRoots {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn portable_user_css_takes_precedence_over_data_dir() {
        let tmp = TempRoots::new("portable_css");
        std::fs::write(tmp.data.join("user.css"), b"body { color: red; }").unwrap();
        std::fs::write(tmp.portable.join("user.css"), b"body { color: green; }").unwrap();

        let roots = ExtRoots::new(Some(tmp.portable.clone()), tmp.data.clone(), false);
        assert!(roots.has_user_css());
        let resolved = roots.resolve_user_css().expect("resolved user.css");
        assert!(resolved.is_portable);
        assert_eq!(resolved.path, tmp.portable.join("user.css"));

        let flags = roots.preload_ext_flags();
        assert_eq!(flags["hasUserCss"], true);
        assert_eq!(flags["isPortableCss"], true);
        assert_eq!(flags["safeMode"], false);

        let (status, mime, body) = roots
            .resolve_ext_request("/__ext/user.css")
            .expect("handled");
        assert_eq!(status, 200);
        assert_eq!(mime, "text/css; charset=utf-8");
        assert_eq!(body, b"body { color: green; }");

        // Removing portable user.css falls back to data_dir.
        std::fs::remove_file(tmp.portable.join("user.css")).unwrap();
        let resolved2 = roots.resolve_user_css().expect("fallback user.css");
        assert!(!resolved2.is_portable);
        let (status2, _, body2) = roots
            .resolve_ext_request("/__ext/user.css?v=2")
            .expect("handled");
        assert_eq!(status2, 200);
        assert_eq!(body2, b"body { color: red; }");
    }

    #[test]
    fn safe_mode_disables_all_extensions() {
        let tmp = TempRoots::new("safe_mode");
        std::fs::write(tmp.data.join("user.css"), b"body {}").unwrap();
        std::fs::create_dir_all(tmp.data.join("plugins").join("demo")).unwrap();
        std::fs::write(
            tmp.data.join("plugins").join("demo").join("main.js"),
            b"console.log(1);",
        )
        .unwrap();

        let roots = ExtRoots::new(Some(tmp.portable.clone()), tmp.data.clone(), true);
        assert!(!roots.has_user_css());
        assert!(!roots.has_plugins_dir());
        assert!(roots.resolve_user_css().is_none());

        let flags = roots.preload_ext_flags();
        assert_eq!(flags["hasUserCss"], false);
        assert_eq!(flags["hasPlugins"], false);
        assert_eq!(flags["safeMode"], true);
        assert_eq!(flags["isPortableCss"], false);

        let (st1, _, _) = roots.resolve_ext_request("/__ext/user.css").unwrap();
        assert_eq!(st1, 404);
        let (st2, _, _) = roots
            .resolve_ext_request("/__ext/plugins/demo/main.js")
            .unwrap();
        assert_eq!(st2, 404);
        assert!(roots.resolve_ext_request("/js/app.js").is_none());
    }

    #[test]
    fn revision_prefix_and_plugin_files_resolve_with_correct_mime() {
        let tmp = TempRoots::new("rev_plugins");
        std::fs::write(tmp.data.join("user.css"), b"/* rev */").unwrap();
        let p_dir = tmp.portable.join("plugins").join("p1");
        let d_dir = tmp.data.join("plugins").join("p1");
        std::fs::create_dir_all(p_dir.join("assets")).unwrap();
        std::fs::create_dir_all(&d_dir).unwrap();
        std::fs::write(p_dir.join("main.js"), b"export const v = 'portable';").unwrap();
        std::fs::write(d_dir.join("main.js"), b"export const v = 'data';").unwrap();
        std::fs::write(d_dir.join("only_in_data.js"), b"// should not leak").unwrap();
        std::fs::write(p_dir.join("manifest.json"), b"{}").unwrap();
        std::fs::write(p_dir.join("assets").join("icon.svg"), b"<svg/>").unwrap();
        std::fs::write(p_dir.join("assets").join("pic.png"), b"\x89PNG").unwrap();
        std::fs::write(p_dir.join("assets").join("font.woff2"), b"wOF2").unwrap();

        let roots = ExtRoots::new(Some(tmp.portable.clone()), tmp.data.clone(), false);
        assert!(roots.has_plugins_dir());

        let (st, mime, body) = roots
            .resolve_ext_request("/__ext/r/42/user.css#hash")
            .unwrap();
        assert_eq!((st, mime, body), (200, "text/css; charset=utf-8", b"/* rev */".to_vec()));

        let (st, mime, body) = roots
            .resolve_ext_request("/__ext/r/7/plugins/p1/main.js?t=1")
            .unwrap();
        assert_eq!(st, 200);
        assert_eq!(mime, "application/javascript; charset=utf-8");
        assert_eq!(body, b"export const v = 'portable';");

        // Single-source precedence: portable/plugins/p1 exists, so data/plugins/p1/only_in_data.js is NOT served.
        let (st_leak, _, _) = roots
            .resolve_ext_request("/__ext/plugins/p1/only_in_data.js")
            .unwrap();
        assert_eq!(st_leak, 404);

        assert_eq!(
            roots.resolve_ext_request("/__ext/plugins/p1/manifest.json").unwrap().1,
            "application/json; charset=utf-8"
        );
        assert_eq!(
            roots.resolve_ext_request("/__ext/plugins/p1/assets/icon.svg").unwrap().1,
            "image/svg+xml"
        );
        assert_eq!(
            roots.resolve_ext_request("/__ext/plugins/p1/assets/pic.png").unwrap().1,
            "image/png"
        );
        assert_eq!(
            roots.resolve_ext_request("/__ext/plugins/p1/assets/font.woff2").unwrap().1,
            "font/woff2"
        );
    }

    #[test]
    fn traversal_and_unsafe_paths_return_403() {
        let tmp = TempRoots::new("traversal");
        let roots = ExtRoots::new(Some(tmp.portable.clone()), tmp.data.clone(), false);

        let unsafe_paths = [
            "/__ext/../secret.css",
            "/__ext/plugins/demo/../../secret.css",
            "/__ext/plugins/demo/%2e%2e/secret.css",
            "/__ext/plugins/demo/%2E%2E/secret.css",
            "/__ext/plugins/demo/a\\b.js",
            "/__ext/plugins/demo/a%5cb.js",
            "/__ext/C:/Windows/win.ini",
            "/__ext/plugins/demo/C:/Windows/win.ini",
            "/__ext/plugins/demo/run.exe",
            "/__ext/evil.exe",
            "/__ext/plugins/demo/%ZZ.js",
            "/__ext/plugins/demo/null%00.js",
        ];

        for path in unsafe_paths {
            let resp = roots
                .resolve_ext_request(path)
                .unwrap_or_else(|| panic!("expected Some for {path}"));
            assert_eq!(resp.0, 403, "expected 403 for {path}, got {}", resp.0);
        }
    }

    #[test]
    fn immutable_asset_excludes_ext_and_api() {
        assert!(is_immutable_virtual_asset(200, "js/app.js"));
        assert!(is_immutable_virtual_asset(200, "css/style.css"));
        assert!(is_immutable_virtual_asset(200, "img/logo.png"));
        assert!(is_immutable_virtual_asset(200, "img/icon.svg"));
        assert!(is_immutable_virtual_asset(200, "fonts/f.woff2"));
        assert!(is_immutable_virtual_asset(200, "favicon.ico"));

        assert!(!is_immutable_virtual_asset(200, ""));
        assert!(!is_immutable_virtual_asset(200, "index.html"));
        assert!(!is_immutable_virtual_asset(200, "api/v1/test.js"));
        assert!(!is_immutable_virtual_asset(200, "__ext/user.css"));
        assert!(!is_immutable_virtual_asset(200, "__ext/r/1/user.css"));
        assert!(!is_immutable_virtual_asset(200, "__ext/plugins/p1/main.js"));
        assert!(!is_immutable_virtual_asset(404, "js/app.js"));
        assert!(!is_immutable_virtual_asset(200, "data/readme.txt"));
    }

    #[test]
    fn list_plugins_discovers_portable_first_and_respects_safe_mode() {
        let tmp = TempRoots::new("list_plugins");
        std::fs::create_dir_all(tmp.portable.join("plugins").join("alpha")).unwrap();
        std::fs::write(
            tmp.portable.join("plugins").join("alpha").join("plugin.js"),
            "export default { id: 'alpha', apiVersion: 1, init() {} };",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.data.join("plugins").join("alpha")).unwrap();
        std::fs::write(
            tmp.data.join("plugins").join("alpha").join("plugin.js"),
            "// shadowed",
        )
        .unwrap();
        std::fs::create_dir_all(tmp.data.join("plugins").join("beta")).unwrap();
        std::fs::write(
            tmp.data.join("plugins").join("beta").join("plugin.js"),
            "export default { id: 'beta', apiVersion: 1, init() {} };",
        )
        .unwrap();
        // Folder without plugin.js is ignored
        std::fs::create_dir_all(tmp.data.join("plugins").join("empty-dir")).unwrap();

        let roots = ExtRoots::new(Some(tmp.portable.clone()), tmp.data.clone(), false);
        let list = roots.list_plugins();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["id"], "alpha");
        assert_eq!(list[0]["isPortable"], true);
        assert_eq!(list[1]["id"], "beta");
        assert_eq!(list[1]["isPortable"], false);

        let safe = ExtRoots::new(Some(tmp.portable.clone()), tmp.data.clone(), true);
        assert!(safe.list_plugins().is_empty());
    }
}
