//! In-memory frontend asset registry bundled at compile time via `include_bytes!`.

/// Static asset resolved from the virtual origin `https://app.syki.local/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddedAsset {
    pub mime: &'static str,
    pub bytes: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/embedded_assets.rs"));

/// Normalizes a virtual request path and looks it up via binary search.
pub fn resolve_static_asset(path: &str) -> Option<EmbeddedAsset> {
    let base = path.split(['?', '#']).next().unwrap_or("");
    if base.contains('\\') {
        return None;
    }

    let normalized = if base.is_empty() || base == "/" {
        "/index.html".to_string()
    } else if base.starts_with('/') {
        base.to_string()
    } else {
        format!("/{base}")
    };

    if normalized.split('/').any(|seg| seg == "..") {
        return None;
    }

    let idx = EMBEDDED_ASSETS
        .binary_search_by_key(&normalized.as_str(), |(k, _, _)| *k)
        .ok()?;
    let (_, mime, bytes) = EMBEDDED_ASSETS[idx];
    Some(EmbeddedAsset { mime, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_resolves_to_index_html() {
        let root = resolve_static_asset("/").expect("root index.html exists");
        assert_eq!(root.mime, "text/html; charset=utf-8");
        assert!(!root.bytes.is_empty());

        let empty = resolve_static_asset("").expect("empty path maps to index.html");
        assert_eq!(empty.mime, root.mime);
        assert_eq!(empty.bytes.len(), root.bytes.len());
    }

    #[test]
    fn backend_proxy_with_query_resolves_and_tests_are_excluded() {
        let proxy =
            resolve_static_asset("/js/backend_proxy.js?v=1.0.0").expect("backend_proxy.js bundled");
        assert_eq!(proxy.mime, "application/javascript; charset=utf-8");
        assert!(std::str::from_utf8(proxy.bytes)
            .expect("utf8 js")
            .contains("createPostMessageBackendProxy"));

        assert!(
            resolve_static_asset("/js/backend_proxy_test.js").is_none(),
            "*_test.js must not be bundled into release binary"
        );
    }

    #[test]
    fn path_traversal_and_backslashes_are_rejected() {
        assert!(resolve_static_asset("/../Cargo.toml").is_none());
        assert!(resolve_static_asset("/js/../../Cargo.toml").is_none());
        assert!(resolve_static_asset(r"\js\app.js").is_none());
    }
}
