//! Backward-compatible `config.json` access with `(exists, mtime, size)` cache.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::storage::{atomic, scrap::ScrapStore, StorageError};

/// Persistence contract for application configuration (`config.json`).
pub trait ConfigStore: Send + Sync {
    fn load_raw(&self) -> Result<String, StorageError>;
    fn save_raw(&self, json: &str) -> Result<(), StorageError>;
    fn scrap_dir(&self) -> PathBuf;
}

#[derive(Debug, Clone)]
struct ConfigCacheEntry {
    exists: bool,
    modified: Option<SystemTime>,
    len: u64,
    content: String,
}

/// Thread-safe JSON configuration store with metadata cache.
pub struct JsonConfigStore {
    path: PathBuf,
    cache: Mutex<Option<ConfigCacheEntry>>,
}

impl JsonConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            cache: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn stat_meta(&self) -> (bool, Option<SystemTime>, u64) {
        match std::fs::metadata(&self.path) {
            Ok(meta) if !meta.is_dir() => (true, meta.modified().ok(), meta.len()),
            _ => (false, None, 0),
        }
    }
}

impl ConfigStore for JsonConfigStore {
    fn load_raw(&self) -> Result<String, StorageError> {
        let (exists, modified, len) = self.stat_meta();
        {
            let guard = self.cache.lock().expect("config cache mutex");
            if let Some(cached) = guard.as_ref() {
                if cached.exists == exists && cached.modified == modified && cached.len == len {
                    return Ok(cached.content.clone());
                }
            }
        }

        let content = if !exists {
            "{}".to_string()
        } else {
            std::fs::read_to_string(&self.path)?
        };

        let mut guard = self.cache.lock().expect("config cache mutex");
        *guard = Some(ConfigCacheEntry {
            exists,
            modified,
            len,
            content: content.clone(),
        });
        Ok(content)
    }

    fn save_raw(&self, json: &str) -> Result<(), StorageError> {
        let _: serde_json::Value = serde_json::from_str(json)?;
        atomic::write_atomic_bytes(&self.path, json.as_bytes())?;

        let (exists, modified, len) = self.stat_meta();
        let mut guard = self.cache.lock().expect("config cache mutex");
        *guard = Some(ConfigCacheEntry {
            exists,
            modified,
            len,
            content: json.to_string(),
        });
        Ok(())
    }

    fn scrap_dir(&self) -> PathBuf {
        let raw = self.load_raw().ok().unwrap_or_else(|| "{}".to_string());
        let parsed: serde_json::Value =
            serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
        let configured = non_empty_str(parsed.pointer("/scraps/scrapDir"))
            .or_else(|| non_empty_str(parsed.get("scrap_dir")))
            .or_else(|| non_empty_str(parsed.pointer("/general/scrapDir")))
            .unwrap_or("~/Documents/syki-sok/scraps");
        ScrapStore::resolve_scrap_dir(Path::new(configured))
    }
}

fn non_empty_str(val: Option<&serde_json::Value>) -> Option<&str> {
    val.and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fixture_path;
    use tempfile::tempdir;

    #[test]
    fn missing_config_returns_empty_object_and_default_scrap_dir() {
        let dir = tempdir().expect("tempdir");
        let store = JsonConfigStore::new(dir.path().join("config.json"));
        assert_eq!(store.load_raw().expect("load missing"), "{}");
        let resolved = store.scrap_dir();
        assert!(resolved.ends_with(Path::new("Documents").join("syki-sok").join("scraps")));
    }

    #[test]
    fn legacy_v1_config_round_trips_with_unknown_keys_intact() {
        let dir = tempdir().expect("tempdir");
        let dst = dir.path().join("config.json");
        let fixture = std::fs::read_to_string(fixture_path("config_v1_legacy.json"))
            .expect("read v1 fixture");

        let store = JsonConfigStore::new(&dst);
        store.save_raw(&fixture).expect("save v1 fixture");

        let reloaded = store.load_raw().expect("reload");
        let before: serde_json::Value = serde_json::from_str(&fixture).expect("json");
        let after: serde_json::Value = serde_json::from_str(&reloaded).expect("json");
        assert_eq!(before, after);
        assert!(after.get("cfBridge").is_some());
        assert!(after.get("gitSync").is_some());
        assert!(after.get("ocr").is_some());
        assert!(store
            .scrap_dir()
            .ends_with(Path::new("Documents").join("syki-sok").join("scraps")));
    }

    #[test]
    fn invalid_json_is_rejected_and_cache_updates_on_save() {
        let dir = tempdir().expect("tempdir");
        let store = JsonConfigStore::new(dir.path().join("config.json"));
        assert!(store.save_raw("{broken").is_err());

        store
            .save_raw(r#"{"scraps":{"scrapDir":"C:\\notes\\daily"}}"#)
            .expect("save valid");
        assert_eq!(store.scrap_dir(), PathBuf::from(r"C:\notes\daily"));

        store
            .save_raw(r#"{"scraps":{"scrapDir":"D:\\other"}}"#)
            .expect("overwrite valid");
        assert_eq!(store.scrap_dir(), PathBuf::from(r"D:\other"));
    }
}
