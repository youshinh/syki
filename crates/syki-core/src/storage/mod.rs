//! Ring 0 storage layer for syki: the only code allowed to touch disk.
//!
//! This module owns every persistence primitive the CLI, the GUI shell and the
//! native runtime share: atomic temp-rename writes, backward-compatible
//! `config.json` access with an mtime/size cache, session state, encoding-aware
//! note I/O (UTF-8, Shift_JIS and EUC-JP), the append-only Scrap journal and a
//! single active file watcher that survives both in-place clears and atomic
//! renames.

pub mod atomic;
pub mod config;
pub mod note;
pub mod scrap;
pub mod session;
pub mod watch;

pub use config::{ConfigStore, JsonConfigStore};
pub use note::NoteFileStore;
pub use scrap::ScrapStore;
pub use session::SessionStore;
pub use watch::ActiveFileWatcher;

/// Content fingerprint of a byte blob: lowercase SHA-256 hex plus byte length.
///
/// Every note read through [`NoteFileStore`] carries one, and every
/// conflict-checked save compares against the signature the caller originally
/// loaded, so an external edit is detected instead of silently clobbered.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextSignature {
    /// Lowercase, 64-character hex digest of the raw bytes.
    pub sha256_hex: String,
    /// Number of raw bytes that were hashed.
    pub byte_len: u64,
}

impl TextSignature {
    /// Hashes `raw` and reports the digest together with its byte length.
    pub fn from_bytes(raw: &[u8]) -> Self {
        use std::fmt::Write as _;
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(raw);
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(64);
        for byte in digest.iter() {
            let _ = write!(hex, "{byte:02x}");
        }
        Self {
            sha256_hex: hex,
            byte_len: raw.len() as u64,
        }
    }
}

/// A note as it exists on disk: where it came from, how it was decoded and
/// the signature of the exact bytes that were read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedNote {
    /// Path the bytes were read from.
    pub path: std::path::PathBuf,
    /// Text produced by the strict, lossless decoding pass.
    pub content: String,
    /// Canonical name of the encoding used (`"UTF-8"` or `"Shift_JIS"`).
    pub encoding_name: String,
    /// Signature of the raw bytes, used as the optimistic concurrency token.
    pub sig: TextSignature,
}

/// Everything that can go wrong while reading or writing Ring 0 state.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The file on disk no longer matches the signature the caller loaded.
    #[error("conflict: file changed on disk")]
    Conflict {
        /// Signature of the bytes currently on disk.
        current_sig: TextSignature,
    },
    /// Underlying filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Malformed JSON handed to a store or found inside a config document.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// File-watcher registration or notification failure.
    #[error("watch error: {0}")]
    Watch(String),
}

#[cfg(test)]
pub(crate) fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../evals/fixtures")
        .join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_signature_is_lowercase_64_hex_and_length_sensitive() {
        let sig = TextSignature::from_bytes(b"hello");
        assert_eq!(sig.sha256_hex.len(), 64);
        assert_eq!(
            sig.sha256_hex,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(sig.byte_len, 5);
        assert_ne!(sig, TextSignature::from_bytes(b"hello!"));
        assert!(sig.sha256_hex.chars().all(|c| !c.is_ascii_uppercase()));
    }

    #[test]
    fn storage_error_variants_render_expected_messages() {
        let conflict = StorageError::Conflict {
            current_sig: TextSignature::from_bytes(b"x"),
        };
        assert_eq!(conflict.to_string(), "conflict: file changed on disk");
        let watch_err = StorageError::Watch("registration refused".to_string());
        assert_eq!(watch_err.to_string(), "watch error: registration refused");
        let io_err = StorageError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no such file",
        ));
        assert_eq!(io_err.to_string(), "io error: no such file");
        let json_err = serde_json::from_str::<serde_json::Value>("{not json")
            .expect_err("malformed json must fail");
        let storage_json = StorageError::Json(json_err);
        assert!(storage_json.to_string().starts_with("json error: "));
    }

    #[test]
    fn signature_round_trips_through_json() {
        let sig = TextSignature::from_bytes(b"round trip");
        let json = serde_json::to_string(&sig).expect("serialize");
        let back: TextSignature = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, sig);
    }
}
