//! In-memory frontend asset registry bundled at compile time (gzip-compressed when smaller).

/// Static asset resolved from the virtual origin `https://app.syki.local/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddedAsset {
    pub mime: &'static str,
    pub is_gzip: bool,
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
        .binary_search_by_key(&normalized.as_str(), |(k, _, _, _)| *k)
        .ok()?;
    let (_, mime, is_gzip, bytes) = EMBEDDED_ASSETS[idx];
    Some(EmbeddedAsset {
        mime,
        is_gzip,
        bytes,
    })
}

/// Returns the uncompressed bytes of an [`EmbeddedAsset`] (decompressing gzip on the fly
/// and caching large boot assets in memory).
pub fn decode_asset_bytes(asset: EmbeddedAsset) -> Vec<u8> {
    if !asset.is_gzip {
        return asset.bytes.to_vec();
    }
    use std::collections::HashMap;
    use std::io::Read;
    use std::sync::{Mutex, OnceLock};

    static CACHE: OnceLock<Mutex<HashMap<usize, Vec<u8>>>> = OnceLock::new();
    let cache_key = asset.bytes.as_ptr() as usize;
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(hit) = guard.get(&cache_key) {
            return hit.clone();
        }
    }

    let mut dec = flate2::read::GzDecoder::new(asset.bytes);
    let mut out = Vec::with_capacity(asset.bytes.len().saturating_mul(3));
    if dec.read_to_end(&mut out).is_ok() {
        if asset.bytes.len() > 16 * 1024 {
            if let Ok(mut guard) = cache.lock() {
                guard.insert(cache_key, out.clone());
            }
        }
        out
    } else {
        asset.bytes.to_vec()
    }
}

/// Pre-decompresses `/index.html` and `/__boot_deferred.js` on a background thread
/// so they are already in memory when WebView2 finishes initializing.
pub fn prewarm_boot_assets() {
    std::thread::spawn(|| {
        for path in [
            "/index.html",
            "/__boot_deferred.js",
            "/js/app.js",
            "/js/i18n.js",
            "/css/tokens.css",
            "/css/style.css",
            "/css/chrome.css",
        ] {
            if let Some(asset) = resolve_static_asset(path) {
                let _ = decode_asset_bytes(asset);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn decode_asset(asset: EmbeddedAsset) -> String {
        if asset.is_gzip {
            let mut dec = flate2::read::GzDecoder::new(asset.bytes);
            let mut out = String::new();
            dec.read_to_string(&mut out).expect("valid gzip utf8");
            out
        } else {
            std::str::from_utf8(asset.bytes).expect("utf8").to_string()
        }
    }

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
        assert!(decode_asset(proxy).contains("createPostMessageBackendProxy"));

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
