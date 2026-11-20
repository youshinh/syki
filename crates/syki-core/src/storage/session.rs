//! Editor session persistence (`session.json`) backed by atomic writes.

use std::path::{Path, PathBuf};

use crate::storage::{atomic, StorageError};

/// Reads and atomically persists editor tab/pane session JSON.
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_raw(&self) -> Result<String, StorageError> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(err) => Err(StorageError::Io(err)),
        }
    }

    pub fn save_raw(&self, json: &str) -> Result<(), StorageError> {
        let _: serde_json::Value = serde_json::from_str(json)?;
        atomic::write_atomic_bytes(&self.path, json.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn missing_session_returns_empty_string_and_saves_round_trip() {
        let dir = tempdir().expect("tempdir");
        let store = SessionStore::new(dir.path().join("session.json"));
        assert_eq!(store.load_raw().expect("missing session"), "");

        let payload = r#"{"tabs":[{"id":"t1","title":"note.md"}],"activeTabId":"t1"}"#;
        store.save_raw(payload).expect("save session");
        assert_eq!(store.load_raw().expect("reload session"), payload);
    }

    #[test]
    fn malformed_session_json_is_rejected() {
        let dir = tempdir().expect("tempdir");
        let store = SessionStore::new(dir.path().join("session.json"));
        assert!(matches!(store.save_raw("{bad"), Err(StorageError::Json(_))));
        assert!(!store.path().exists());
    }
}
