//! Static contract checker for syki plugins (`syki-cli plugin check <path>`).
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

/// Validates a plugin entry file against the syki static plugin contract.
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

    let (stripped, syntax_errors) = lex_and_strip_comments(&source);
    errors.extend(syntax_errors);

    match check_api_version(&stripped) {
        ApiVersionStatus::Valid => {}
        ApiVersionStatus::Missing => {
            errors.push("missing required `apiVersion: 1` declaration".to_string());
        }
        ApiVersionStatus::Unsupported => {
            errors.push("unsupported apiVersion: expected 1".to_string());
        }
    }

    if stripped.contains("module.exports") {
        errors.push(
            "forbidden CommonJS `module.exports`: plugins must use ES module `export default`"
                .to_string(),
        );
    }
    if !stripped.contains("export default") {
        errors.push("missing required `export default` declaration".to_string());
    }
    if !has_decl(&stripped, "id", false) {
        errors.push("missing required `id` declaration in plugin export".to_string());
    }
    if !has_decl(&stripped, "init", true) {
        errors.push("missing required `init` declaration in plugin export".to_string());
    }

    let perms = extract_permissions(&stripped);
    for perm in &perms {
        if !is_allowed_permission(perm) {
            errors.push(format!(
                "unknown permission `{perm}` (allowed: llm, decision, voice, meeting, vision, image, semantic, pty, shell:<prog>)"
            ));
        } else if perm == "shell:cmd.exe"
            || perm == "shell:pwsh"
            || perm == concat!("shell:power", "shell")
        {
            warnings.push(format!(
                "broad shell permission `{perm}` detected; prefer narrower command permissions"
            ));
        }
    }

    if stripped.contains("exec.shell") && !perms.iter().any(|p| p.starts_with("shell:")) {
        errors.push(
            "undeclared `exec.shell` usage: requires a `shell:<prog>` permission".to_string(),
        );
    }
    if stripped.contains("exec.llm") && !perms.iter().any(|p| p == "llm") {
        errors.push("undeclared `exec.llm` usage: requires `llm` permission".to_string());
    }

    let folded = fold_string_concatenations(&stripped);
    if stripped.contains("chrome.webview")
        || folded.contains("'chrome'")
        || folded.contains("\"chrome\"")
        || folded.contains("'webview'")
        || folded.contains("\"webview\"")
    {
        errors.push(
            "forbidden pattern `chrome.webview`: direct WebView2 IPC access is prohibited; use `window.syki` APIs"
                .to_string(),
        );
    }
    if stripped.contains("window.backend")
        || folded.contains("'backend'")
        || folded.contains("\"backend\"")
    {
        errors.push(
            "forbidden pattern `window.backend`: direct backend proxy access is prohibited; use `window.syki` APIs"
                .to_string(),
        );
    }
    if stripped.contains("https://")
        || stripped.contains("http://")
        || stripped.contains("wss://")
        || stripped.contains("ws://")
    {
        errors.push(
            "forbidden pattern `http://`, `https://`, `ws://`, or `wss://`: external CDN or network loading is prohibited"
                .to_string(),
        );
    }

    let dom_targets = ["data-syki-part", "#header", ".status-bar", "statusbar"];
    let dom_mutations = [
        "appendChild",
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
    ];
    let lower_stripped = stripped.to_ascii_lowercase();
    let has_dom_target = dom_targets.iter().any(|t| lower_stripped.contains(t));
    let has_dom_mutation = dom_mutations.iter().any(|m| stripped.contains(m));
    if has_dom_target && has_dom_mutation {
        errors.push(
            "forbidden direct DOM manipulation of toolbar/statusbar: use `window.syki.ui` APIs"
                .to_string(),
        );
    }

    for color in find_raw_css_colors(&stripped) {
        warnings.push(format!(
            "hardcoded CSS color `{color}` detected; prefer semantic variables from tokens.css"
        ));
    }

    // If checking `plugin.js` and a sibling `style.css` exists, also check it for raw CSS colors.
    if path.file_name() == Some(OsStr::new("plugin.js")) {
        if let Some(parent) = path.parent() {
            let sibling_css = parent.join("style.css");
            if let Ok(css_src) = std::fs::read_to_string(&sibling_css) {
                for color in find_raw_css_colors(&css_src) {
                    warnings.push(format!(
                        "hardcoded CSS color `{color}` detected in sibling style.css; prefer semantic variables from tokens.css"
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

enum ApiVersionStatus {
    Valid,
    Missing,
    Unsupported,
}

fn check_api_version(source: &str) -> ApiVersionStatus {
    let bytes = source.as_bytes();
    let needle = b"apiVersion";
    let mut start = 0;
    let mut saw_any = false;

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
                saw_any = true;
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
                        return ApiVersionStatus::Valid;
                    }
                }
            }
        }
        start = idx + needle.len();
    }

    if saw_any {
        ApiVersionStatus::Unsupported
    } else {
        ApiVersionStatus::Missing
    }
}

fn has_decl(source: &str, name: &str, allow_method: bool) -> bool {
    let bytes = source.as_bytes();
    let needle = name.as_bytes();
    let mut start = 0;

    while let Some(rel) = source[start..].find(name) {
        let idx = start + rel;
        let prev_ok = idx == 0 || !is_ident_byte(bytes[idx - 1]);
        let mut cursor = idx + needle.len();
        let next_ok = cursor >= bytes.len() || !is_ident_byte(bytes[cursor]);

        if prev_ok && next_ok {
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor < bytes.len()
                && (bytes[cursor] == b':' || (allow_method && bytes[cursor] == b'('))
            {
                return true;
            }
        }
        start = idx + needle.len();
    }

    false
}

/// Strips JS line (`//`) and block (`/* */`) comments while preserving string/template literals
/// and checking for basic syntax errors (unclosed literals/comments, unbalanced `{}`, `()`, `[]`).
fn lex_and_strip_comments(source: &str) -> (String, Vec<String>) {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut errors = Vec::new();
    let mut stack: Vec<(char, bool)> = Vec::new(); // (opening_delim, is_template_expr)
    let mut i = 0;
    let mut in_template = false;

    while i < bytes.len() {
        if in_template {
            let b = bytes[i];
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(b as char);
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if b == b'`' {
                out.push('`');
                in_template = false;
                i += 1;
                continue;
            }
            if b == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                out.push_str("${");
                stack.push(('{', true));
                in_template = false;
                i += 2;
                continue;
            }
            let ch = source[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }

        let b = bytes[i];
        // Line comment
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            out.push(' ');
            continue;
        }
        // Block comment
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            let mut closed = false;
            while i + 1 < bytes.len() {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    closed = true;
                    break;
                }
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            if !closed {
                // Check if last byte wasn't consumed
                if i < bytes.len() && bytes[i] == b'\n' {
                    out.push('\n');
                }
                errors.push("syntax error: unclosed block comment `/*`".to_string());
                break;
            }
            out.push(' ');
            continue;
        }
        // Single or double quoted string
        if b == b'\'' || b == b'"' {
            let quote = b;
            out.push(quote as char);
            i += 1;
            let mut closed = false;
            while i < bytes.len() {
                let c = bytes[i];
                if c == b'\\' && i + 1 < bytes.len() {
                    out.push('\\');
                    let next_ch = source[i + 1..].chars().next().unwrap();
                    out.push(next_ch);
                    i += 1 + next_ch.len_utf8();
                    continue;
                }
                if c == quote {
                    out.push(quote as char);
                    i += 1;
                    closed = true;
                    break;
                }
                if c == b'\n' {
                    break;
                }
                let ch = source[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
            if !closed {
                errors.push(format!("syntax error: unclosed string literal `{}`", quote as char));
            }
            continue;
        }
        // Template literal start
        if b == b'`' {
            out.push('`');
            in_template = true;
            i += 1;
            continue;
        }
        // Delimiters
        match b {
            b'{' | b'(' | b'[' => {
                stack.push((b as char, false));
                out.push(b as char);
                i += 1;
            }
            b'}' | b')' | b']' => {
                let expected = match b {
                    b'}' => '{',
                    b')' => '(',
                    b']' => '[',
                    _ => unreachable!(),
                };
                match stack.pop() {
                    Some((open, is_tmpl)) if open == expected => {
                        if is_tmpl {
                            in_template = true;
                        }
                    }
                    _ => {
                        errors.push(format!("syntax error: unexpected `{}`", b as char));
                    }
                }
                out.push(b as char);
                i += 1;
            }
            _ => {
                let ch = source[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }

    if in_template {
        errors.push("syntax error: unclosed template literal `` ` ``".to_string());
    }
    if let Some((open, _)) = stack.last() {
        errors.push(format!("syntax error: unclosed delimiter `{open}`"));
    }

    (out, errors)
}

/// Folds adjacent string literal concatenations (`'a' + 'b'` -> `'ab'`, `"a" + "b"` -> `"ab"`)
/// so obfuscated property/API access can be detected.
fn fold_string_concatenations(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\'' || b == b'"' {
            let quote = b;
            let mut combined = String::new();
            let mut cursor = i;

            loop {
                if cursor >= bytes.len() || bytes[cursor] != quote {
                    break;
                }
                cursor += 1;
                let str_start = cursor;
                while cursor < bytes.len() && bytes[cursor] != quote {
                    if bytes[cursor] == b'\\' && cursor + 1 < bytes.len() {
                        cursor += 2;
                    } else {
                        cursor += 1;
                    }
                }
                if cursor >= bytes.len() {
                    combined.push_str(&source[str_start..]);
                    break;
                }
                combined.push_str(&source[str_start..cursor]);
                cursor += 1; // skip closing quote

                // Look ahead for `+` followed by another string literal with the same quote
                let mut look = cursor;
                while look < bytes.len() && bytes[look].is_ascii_whitespace() {
                    look += 1;
                }
                if look < bytes.len() && bytes[look] == b'+' {
                    look += 1;
                    while look < bytes.len() && bytes[look].is_ascii_whitespace() {
                        look += 1;
                    }
                    if look < bytes.len() && bytes[look] == quote {
                        cursor = look;
                        continue;
                    }
                }
                break;
            }

            out.push(quote as char);
            out.push_str(&combined);
            out.push(quote as char);
            i = cursor;
            continue;
        }
        let ch = source[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    out
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

fn find_raw_css_colors(source: &str) -> Vec<String> {
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
        if !source.is_char_boundary(i) {
            i += 1;
            continue;
        }
        for prefix in ["rgba(", "rgb(", "hsl("] {
            if source[i..].starts_with(prefix) {
                let prev_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
                if prev_ok {
                    if let Some(end_rel) = source[i..].find(')') {
                        let token = &source[i..=i + end_rel];
                        if !token.contains('\n') {
                            let s = token.to_string();
                            if !found.contains(&s) {
                                found.push(s);
                            }
                            i += end_rel + 1;
                            continue;
                        }
                    }
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
    fn missing_and_unsupported_api_version_fail_distinctly() {
        let unsupported = write_temp_plugin(
            "plugin.js",
            r#"
export default {
  id: 'bad-ver',
  name: 'Bad Version',
  apiVersion: 2,
  init() {}
};
"#,
        );
        let r1 = check_plugin_file(&unsupported);
        assert!(!r1.ok);
        assert!(r1
            .errors
            .iter()
            .any(|e| e == "unsupported apiVersion: expected 1"));

        let missing = write_temp_plugin(
            "plugin.js",
            r#"
export default {
  id: 'no-ver',
  name: 'No Version',
  init() {}
};
"#,
        );
        let r2 = check_plugin_file(&missing);
        assert!(!r2.ok);
        assert!(r2.errors.iter().any(|e| e.contains("missing required `apiVersion: 1`")));
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

    #[test]
    fn comments_do_not_trigger_false_positives() {
        let path = write_temp_plugin(
            "plugin.js",
            r#"
// https://example.com/docs
/*
  window.backend.call('x');
  chrome.webview.postMessage({});
  ws://localhost:8080
*/
export default {
  id: 'comment-safe',
  apiVersion: 1,
  permissions: ['llm'],
  init(syki) {
    // http://ignored.invalid
    syki.ui.toast('ok');
  }
};
"#,
        );
        let report = check_plugin_file(&path);
        assert!(report.ok, "expected ok, got errors: {:?}", report.errors);
    }

    #[test]
    fn syntax_errors_are_rejected() {
        let unbalanced = write_temp_plugin(
            "plugin.js",
            "export default { id: 'x', apiVersion: 1, init() { if (true) { } };",
        );
        let r1 = check_plugin_file(&unbalanced);
        assert!(!r1.ok);
        assert!(r1.errors.iter().any(|e| e.contains("syntax error")));

        let unclosed_str = write_temp_plugin(
            "plugin.js",
            "export default { id: 'x, apiVersion: 1, init() {} };",
        );
        let r2 = check_plugin_file(&unclosed_str);
        assert!(!r2.ok);
        assert!(r2.errors.iter().any(|e| e.contains("unclosed string")));

        let unclosed_tmpl = write_temp_plugin(
            "plugin.js",
            "export default { id: 'x', apiVersion: 1, init() { const s = `hello; } };",
        );
        let r3 = check_plugin_file(&unclosed_tmpl);
        assert!(!r3.ok);
        assert!(r3.errors.iter().any(|e| e.contains("unclosed template")));

        let unclosed_comment = write_temp_plugin(
            "plugin.js",
            "export default { id: 'x', apiVersion: 1, init() {} }; /* unclosed",
        );
        let r4 = check_plugin_file(&unclosed_comment);
        assert!(!r4.ok);
        assert!(r4.errors.iter().any(|e| e.contains("unclosed block comment")));
    }

    #[test]
    fn string_concat_evasion_is_rejected() {
        let path = write_temp_plugin(
            "plugin.js",
            r#"
export default {
  id: 'evade',
  apiVersion: 1,
  init() {
    const a = window['chr' + 'ome'];
    const b = 'web' + 'view';
    const c = window['back' + 'end'];
    return [a, b, c];
  }
};
"#,
        );
        let report = check_plugin_file(&path);
        assert!(!report.ok);
        assert!(report.errors.iter().any(|e| e.contains("chrome.webview")));
        assert!(report.errors.iter().any(|e| e.contains("window.backend")));
    }

    #[test]
    fn dom_manipulation_and_websocket_schemes_are_rejected() {
        let path = write_temp_plugin(
            "plugin.js",
            r#"
export default {
  id: 'dom-ws',
  apiVersion: 1,
  init() {
    document.querySelector('#header').innerHTML = '<x/>';
    const s = new WebSocket('wss://evil.invalid/ws');
    return s;
  }
};
"#,
        );
        let report = check_plugin_file(&path);
        assert!(!report.ok);
        assert!(report.errors.iter().any(|e| e.contains("DOM manipulation")));
        assert!(report.errors.iter().any(|e| e.contains("wss://")));
    }

    #[test]
    fn module_structure_and_undeclared_exec_are_rejected() {
        let cjs = write_temp_plugin(
            "plugin.js",
            r#"
module.exports = {
  apiVersion: 1,
  run() {
    syki.exec.shell('git', ['status']);
    syki.exec.llm('hi');
  }
};
"#,
        );
        let report = check_plugin_file(&cjs);
        assert!(!report.ok);
        assert!(report.errors.iter().any(|e| e.contains("module.exports")));
        assert!(report.errors.iter().any(|e| e.contains("export default")));
        assert!(report.errors.iter().any(|e| e.contains("`id`")));
        assert!(report.errors.iter().any(|e| e.contains("`init`")));
        assert!(report.errors.iter().any(|e| e.contains("exec.shell")));
        assert!(report.errors.iter().any(|e| e.contains("exec.llm")));
    }

    #[test]
    fn css_functions_and_broad_shell_permissions_warn() {
        let path = write_temp_plugin(
            "plugin.js",
            r#"
export default {
  id: 'warns',
  apiVersion: 1,
  permissions: ['shell:cmd.exe', 'shell:pwsh'],
  init() {
    return 'color: rgba(1, 2, 3, 0.5); background: hsl(120, 50%, 50%);';
  }
};
"#,
        );
        let report = check_plugin_file(&path);
        assert!(report.ok, "warnings must not fail: {:?}", report.errors);
        assert!(report.warnings.iter().any(|w| w.contains("rgba(1, 2, 3, 0.5)")));
        assert!(report.warnings.iter().any(|w| w.contains("hsl(120, 50%, 50%)")));
        assert!(report.warnings.iter().any(|w| w.contains("shell:cmd.exe")));
        assert!(report.warnings.iter().any(|w| w.contains("shell:pwsh")));
    }
}
