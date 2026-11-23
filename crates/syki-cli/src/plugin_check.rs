//! Static contract checker for Syki Lite plugins (`syki-cli plugin check <path>`).
//!
//! Uses only the Rust standard library and `serde_json` so `syki-cli` stays lean.

use std::ffi::OsStr;
use std::path::Path;

use serde_json::{json, Value};

const ALLOWED_EXACT_PERMISSIONS: &[&str] = &[
    "llm", "decision", "voice", "meeting", "vision", "image", "semantic", "pty",
];

/// Structured result returned by [`check_plugin_file`] and serialized as JSON by the CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    pub ok: bool,
    pub path: String,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl CheckReport {
    pub fn to_json(&self) -> Value {
        json!({
            "ok": self.ok,
            "path": self.path,
            "errors": self.errors,
            "warnings": self.warnings,
        })
    }
}

/// Validates a plugin entry file against the Syki Lite static plugin contract.
pub fn check_plugin_file(path: &Path) -> CheckReport {
    let path_str = path.to_string_lossy().into_owned();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            errors.push(format!("failed to read plugin file: {e}"));
            return CheckReport {
                ok: false,
                path: path_str,
                errors,
                warnings,
            };
        }
    };

    if source.trim().is_empty() {
        errors.push("plugin file is empty".to_string());
        return CheckReport {
            ok: false,
            path: path_str,
            errors,
            warnings,
        };
    }

    if !has_api_version_1(&source) {
        errors.push("missing required `apiVersion: 1` declaration".to_string());
    }

    for perm in extract_permissions(&source) {
        if !is_allowed_permission(&perm) {
            errors.push(format!(
                "unknown permission `{perm}` (allowed: llm, decision, voice, meeting, vision, image, semantic, pty, shell:<prog>)"
            ));
        }
    }

    if source.contains("chrome.webview") {
        errors.push(
            "forbidden pattern `chrome.webview`: direct WebView2 IPC access is prohibited; use `window.syki` APIs"
                .to_string(),
        );
    }
    if source.contains("window.backend") {
        errors.push(
            "forbidden pattern `window.backend`: direct backend proxy access is prohibited; use `window.syki` APIs"
                .to_string(),
        );
    }
    if source.contains("https://") || source.contains("http://") {
        errors.push(
            "forbidden pattern `http://` or `https://`: external CDN or network loading is prohibited"
                .to_string(),
        );
    }

    for hex in find_raw_hex_colors(&source) {
        warnings.push(format!(
            "hardcoded CSS color `{hex}` detected; prefer semantic variables from tokens.css"
        ));
    }

    // If checking `plugin.js` and a sibling `style.css` exists, also check it for raw hex colors.
    if path.file_name() == Some(OsStr::new("plugin.js")) {
        if let Some(parent) = path.parent() {
            let sibling_css = parent.join("style.css");
            if let Ok(css_src) = std::fs::read_to_string(&sibling_css) {
                for hex in find_raw_hex_colors(&css_src) {
                    warnings.push(format!(
                        "hardcoded CSS color `{hex}` detected in sibling style.css; prefer semantic variables from tokens.css"
                    ));
                }
            }
        }
    }

    let ok = errors.is_empty();
    CheckReport {
        ok,
        path: path_str,
        errors,
        warnings,
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn has_api_version_1(source: &str) -> bool {
    let bytes = source.as_bytes();
    let needle = b"apiVersion";
    let mut start = 0;

    while let Some(rel) = source[start..].find("apiVersion") {
        let idx = start + rel;
        let prev_ok = idx == 0 || !is_ident_byte(bytes[idx - 1]);
        let mut cursor = idx + needle.len();
        let next_ok = cursor >= bytes.len() || !is_ident_byte(bytes[cursor]);

        if prev_ok && next_ok {
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor < bytes.len() && bytes[cursor] == b':' {
                cursor += 1;
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                if cursor < bytes.len() && bytes[cursor] == b'1' {
                    let after = cursor + 1;
                    let boundary_ok = after >= bytes.len()
                        || (!bytes[after].is_ascii_alphanumeric()
                            && bytes[after] != b'.'
                            && bytes[after] != b'_'
                            && bytes[after] != b'$');
                    if boundary_ok {
                        return true;
                    }
                }
            }
        }
        start = idx + needle.len();
    }

    false
}

fn extract_permissions(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let needle = b"permissions";
    let mut results = Vec::new();
    let mut start = 0;

    while let Some(rel) = source[start..].find("permissions") {
        let idx = start + rel;
        let prev_ok = idx == 0 || !is_ident_byte(bytes[idx - 1]);
        let mut cursor = idx + needle.len();
        let next_ok = cursor >= bytes.len() || !is_ident_byte(bytes[cursor]);

        if prev_ok && next_ok {
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor < bytes.len() && bytes[cursor] == b':' {
                cursor += 1;
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                if cursor < bytes.len() && bytes[cursor] == b'[' {
                    cursor += 1;
                    while cursor < bytes.len() && bytes[cursor] != b']' {
                        let b = bytes[cursor];
                        if b == b'\'' || b == b'"' {
                            let quote = b;
                            cursor += 1;
                            let str_start = cursor;
                            while cursor < bytes.len() && bytes[cursor] != quote {
                                if bytes[cursor] == b'\\' && cursor + 1 < bytes.len() {
                                    cursor += 2;
                                } else {
                                    cursor += 1;
                                }
                            }
                            if cursor < bytes.len() {
                                if let Ok(s) = std::str::from_utf8(&bytes[str_start..cursor]) {
                                    results.push(s.to_string());
                                }
                                cursor += 1;
                            }
                        } else {
                            cursor += 1;
                        }
                    }
                }
            }
        }
        start = idx + needle.len();
    }

    results
}

fn is_allowed_permission(perm: &str) -> bool {
    if ALLOWED_EXACT_PERMISSIONS.contains(&perm) {
        return true;
    }
    if let Some(prog) = perm.strip_prefix("shell:") {
        return !prog.is_empty()
            && prog
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    }
    false
}

fn find_raw_hex_colors(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'#' {
            let prev_ok = i == 0 || (!bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_');
            if prev_ok {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
                    j += 1;
                }
                let hex_len = j - (i + 1);
                let next_ok =
                    j >= bytes.len() || (!bytes[j].is_ascii_alphanumeric() && bytes[j] != b'_');
                if (3..=6).contains(&hex_len) && next_ok {
                    if let Ok(token) = std::str::from_utf8(&bytes[i..j]) {
                        let s = token.to_string();
                        if !found.contains(&s) {
                            found.push(s);
                        }
                    }
                    i = j;
                    continue;
                }
            }
        }
        i += 1;
    }

    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn write_temp_plugin(filename: &str, content: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "syki_plugin_check_test_{}_{}",
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let file = dir.join(filename);
        std::fs::write(&file, content).expect("write temp plugin");
        file
    }

    #[test]
    fn valid_plugin_passes_cleanly() {
        let path = write_temp_plugin(
            "plugin.js",
            r#"
export default {
  id: 'valid-sample',
  name: 'Valid Sample',
  apiVersion: 1,
  permissions: ['llm', 'decision', 'semantic', 'shell:git'],
  init(syki) {
    syki.ui.toast('ready', 'ok');
  },
};
"#,
        );
        let report = check_plugin_file(&path);
        assert!(report.ok, "expected ok, got errors: {:?}", report.errors);
        assert!(report.errors.is_empty());
        assert!(report.warnings.is_empty());
        let json = report.to_json();
        assert_eq!(json["ok"], true);
    }

    #[test]
    fn missing_and_empty_files_fail() {
        let missing = std::env::temp_dir().join("syki_definitely_missing_plugin_12345.js");
        let r1 = check_plugin_file(&missing);
        assert!(!r1.ok);
        assert!(!r1.errors.is_empty());

        let empty = write_temp_plugin("empty.js", "   \n\t  ");
        let r2 = check_plugin_file(&empty);
        assert!(!r2.ok);
        assert!(r2.errors.iter().any(|e| e.contains("empty")));
    }

    #[test]
    fn missing_api_version_fails() {
        let path = write_temp_plugin(
            "plugin.js",
            r#"
export default {
  id: 'no-ver',
  name: 'No Version',
  apiVersion: 2,
  init() {}
};
"#,
        );
        let report = check_plugin_file(&path);
        assert!(!report.ok);
        assert!(report.errors.iter().any(|e| e.contains("apiVersion: 1")));
    }

    #[test]
    fn forbidden_patterns_are_rejected() {
        let path = write_temp_plugin(
            "plugin.js",
            r#"
export default {
  id: 'bad-patterns',
  name: 'Bad Patterns',
  apiVersion: 1,
  init() {
    window.chrome.webview.postMessage({});
    window.backend.call('x');
    fetch('https://example.invalid/cdn.js');
  }
};
"#,
        );
        let report = check_plugin_file(&path);
        assert!(!report.ok);
        assert!(report.errors.iter().any(|e| e.contains("chrome.webview")));
        assert!(report.errors.iter().any(|e| e.contains("window.backend")));
        assert!(report.errors.iter().any(|e| e.contains("https://")));
    }

    #[test]
    fn unknown_permissions_are_rejected() {
        let path = write_temp_plugin(
            "plugin.js",
            r#"
export default {
  id: 'bad-perms',
  name: 'Bad Perms',
  apiVersion: 1,
  permissions: ['llm', 'shell:git', 'shell:', 'network:all', 'fs:root'],
  init() {}
};
"#,
        );
        let report = check_plugin_file(&path);
        assert!(!report.ok);
        assert!(report.errors.iter().any(|e| e.contains("shell:")));
        assert!(report.errors.iter().any(|e| e.contains("network:all")));
        assert!(report.errors.iter().any(|e| e.contains("fs:root")));
        assert!(!report.errors.iter().any(|e| e.contains("`llm`")));
        assert!(!report.errors.iter().any(|e| e.contains("`shell:git`")));
    }

    #[test]
    fn raw_hex_color_warns_without_failing() {
        let path = write_temp_plugin(
            "plugin.js",
            r#"
export default {
  id: 'warn-color',
  name: 'Warn Color',
  apiVersion: 1,
  permissions: ['llm'],
  init() {
    const css = 'color: #ff00aa; background: #1e1e1e;';
    return css;
  }
};
"#,
        );
        let report = check_plugin_file(&path);
        assert!(report.ok, "warnings must not flip ok to false");
        assert!(report.errors.is_empty());
        assert!(report.warnings.iter().any(|w| w.contains("#ff00aa")));
        assert!(report.warnings.iter().any(|w| w.contains("#1e1e1e")));
    }
}
