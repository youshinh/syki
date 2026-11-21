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

#[derive(Debug, Clone, Default)]
pub struct AutoDetectHints {
    pub default_scrap_dir: Option<String>,
    pub llm_base_url: Option<String>,
    pub llm_model: Option<String>,
}

impl AutoDetectHints {
    pub fn from_system() -> Self {
        let (llm_base_url, llm_model) = if std::env::var("GEMINI_API_KEY")
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            (
                Some("https://generativelanguage.googleapis.com".to_string()),
                Some("gemini-3.8-flash".to_string()),
            )
        } else if std::env::var("OPENAI_API_KEY")
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            (
                Some("https://api.openai.com/v1".to_string()),
                Some("gpt-6.1-sol".to_string()),
            )
        } else {
            (None, None)
        };
        Self {
            default_scrap_dir: Some("~/Documents/syki-sok/scraps".to_string()),
            llm_base_url,
            llm_model,
        }
    }
}

fn ensure_object<'a>(
    parent: &'a mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> &'a mut serde_json::Map<String, serde_json::Value> {
    if !parent.get(key).map(|v| v.is_object()).unwrap_or(false) {
        parent.insert(key.to_string(), serde_json::Value::Object(Default::default()));
    }
    parent
        .get_mut(key)
        .and_then(|v| v.as_object_mut())
        .expect("object inserted")
}

fn set_default_if_missing(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    default: serde_json::Value,
) {
    let missing = match obj.get(key) {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(s)) => s.trim().is_empty(),
        _ => false,
    };
    if missing {
        obj.insert(key.to_string(), default);
    }
}

pub fn resolve_config_with_defaults(raw_json: &str, hints: &AutoDetectHints) -> serde_json::Value {
    let mut root = match serde_json::from_str::<serde_json::Value>(raw_json) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };

    let legacy_scrap_dir = root
        .get("scrap_dir")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let legacy_pipe_mb = root.get("max_pipe_size_mb").cloned();

    {
        let general = ensure_object(&mut root, "general");
        set_default_if_missing(general, "trimWebViewWhenHidden", serde_json::json!(true));
        set_default_if_missing(general, "webViewInProcessGpu", serde_json::json!(false));
        set_default_if_missing(general, "tabDockFollowOs", serde_json::json!(true));
        set_default_if_missing(general, "tabDockStrength", serde_json::json!(100));
        set_default_if_missing(general, "tabStripWidth", serde_json::json!(24));
        set_default_if_missing(general, "tabHideDelayMs", serde_json::json!(300));
        set_default_if_missing(general, "tabHideDurationMs", serde_json::json!(250));
        set_default_if_missing(general, "rewriteHistoryDir", serde_json::json!("history"));
    }

    {
        let scraps = ensure_object(&mut root, "scraps");
        let default_scrap = legacy_scrap_dir
            .or_else(|| hints.default_scrap_dir.clone())
            .unwrap_or_else(|| "~/Documents/syki-sok/scraps".to_string());
        set_default_if_missing(scraps, "scrapDir", serde_json::json!(default_scrap));
        set_default_if_missing(
            scraps,
            "maxPipeSizeMB",
            legacy_pipe_mb.unwrap_or_else(|| serde_json::json!(10)),
        );
    }

    {
        let autocomplete = ensure_object(&mut root, "autocomplete");
        set_default_if_missing(autocomplete, "delayMs", serde_json::json!(500));
        set_default_if_missing(autocomplete, "maxTokens", serde_json::json!(30));
    }

    {
        let voice = ensure_object(&mut root, "voice");
        set_default_if_missing(voice, "silence_timeout_sec", serde_json::json!(5));
        set_default_if_missing(voice, "barSilenceSec", serde_json::json!(1.5));
        let refine = ensure_object(voice, "refine");
        set_default_if_missing(refine, "timeoutSec", serde_json::json!(5));
        let rt = ensure_object(voice, "realtime");
        set_default_if_missing(rt, "apiStyle", serde_json::json!("auto"));
        let bt = ensure_object(voice, "batch");
        set_default_if_missing(bt, "apiStyle", serde_json::json!("auto"));
    }

    {
        let whisper = ensure_object(&mut root, "whisper");
        set_default_if_missing(whisper, "threads", serde_json::json!(0));
    }

    if hints.llm_base_url.is_some() || hints.llm_model.is_some() {
        let text = ensure_object(&mut root, "text");
        if let Some(url) = &hints.llm_base_url {
            set_default_if_missing(text, "baseUrl", serde_json::json!(url));
        }
        if let Some(model) = &hints.llm_model {
            set_default_if_missing(text, "model", serde_json::json!(model));
        }
    }

    serde_json::Value::Object(root)
}

impl JsonConfigStore {
    pub fn load_resolved_json(&self) -> Result<String, StorageError> {
        let raw = self.load_raw()?;
        let resolved = resolve_config_with_defaults(&raw, &AutoDetectHints::from_system());
        Ok(serde_json::to_string(&resolved)?)
    }
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

    #[test]
    fn resolve_config_with_defaults_populates_zero_config_fields() {
        let hints = AutoDetectHints {
            default_scrap_dir: Some("~/Documents/syki-sok/scraps".to_string()),
            llm_base_url: Some("http://127.0.0.1:11434".to_string()),
            llm_model: Some("qwen3:latest".to_string()),
        };
        let resolved = resolve_config_with_defaults("{}", &hints);

        assert_eq!(resolved["general"]["trimWebViewWhenHidden"], true);
        assert_eq!(resolved["general"]["webViewInProcessGpu"], false);
        assert_eq!(resolved["general"]["tabDockFollowOs"], true);
        assert_eq!(resolved["general"]["tabDockStrength"], 100);
        assert_eq!(resolved["general"]["tabStripWidth"], 24);
        assert_eq!(resolved["general"]["tabHideDelayMs"], 300);
        assert_eq!(resolved["general"]["tabHideDurationMs"], 250);
        assert_eq!(resolved["general"]["rewriteHistoryDir"], "history");

        assert_eq!(resolved["scraps"]["scrapDir"], "~/Documents/syki-sok/scraps");
        assert_eq!(resolved["scraps"]["maxPipeSizeMB"], 10);

        assert_eq!(resolved["autocomplete"]["delayMs"], 500);
        assert_eq!(resolved["autocomplete"]["maxTokens"], 30);

        assert_eq!(resolved["voice"]["silence_timeout_sec"], 5);
        assert_eq!(resolved["voice"]["barSilenceSec"], 1.5);
        assert_eq!(resolved["voice"]["refine"]["timeoutSec"], 5);
        assert_eq!(resolved["voice"]["realtime"]["apiStyle"], "auto");
        assert_eq!(resolved["voice"]["batch"]["apiStyle"], "auto");
        assert_eq!(resolved["whisper"]["threads"], 0);

        assert_eq!(resolved["text"]["baseUrl"], "http://127.0.0.1:11434");
        assert_eq!(resolved["text"]["model"], "qwen3:latest");
    }

    #[test]
    fn resolve_config_with_defaults_preserves_existing_user_and_legacy_values() {
        let legacy = r#"{
            "scrap_dir": "E:\\custom_scraps",
            "max_pipe_size_mb": 25,
            "cfBridge": {"enabled": true},
            "general": {"tabStripWidth": 36, "trimWebViewWhenHidden": false},
            "autocomplete": {"delayMs": 900},
            "whisper": {"threads": 4},
            "text": {"baseUrl": "https://api.example.com", "model": "custom-model"}
        }"#;
        let hints = AutoDetectHints {
            default_scrap_dir: Some("~/Documents/syki-sok/scraps".to_string()),
            llm_base_url: Some("http://127.0.0.1:11434".to_string()),
            llm_model: Some("qwen3:latest".to_string()),
        };
        let resolved = resolve_config_with_defaults(legacy, &hints);

        assert_eq!(resolved["cfBridge"]["enabled"], true);
        assert_eq!(resolved["scraps"]["scrapDir"], "E:\\custom_scraps");
        assert_eq!(resolved["scraps"]["maxPipeSizeMB"], 25);
        assert_eq!(resolved["general"]["tabStripWidth"], 36);
        assert_eq!(resolved["general"]["trimWebViewWhenHidden"], false);
        assert_eq!(resolved["general"]["rewriteHistoryDir"], "history");
        assert_eq!(resolved["autocomplete"]["delayMs"], 900);
        assert_eq!(resolved["autocomplete"]["maxTokens"], 30);
        assert_eq!(resolved["whisper"]["threads"], 4);
        assert_eq!(resolved["text"]["baseUrl"], "https://api.example.com");
        assert_eq!(resolved["text"]["model"], "custom-model");
    }

    #[test]
    fn load_resolved_json_fills_defaults_without_mutating_disk() {
        let dir = tempdir().expect("tempdir");
        let store = JsonConfigStore::new(dir.path().join("config.json"));
        store.save_raw(r#"{"general":{"language":"ja"}}"#).expect("save");

        let resolved_raw = store.load_resolved_json().expect("load resolved");
        let resolved: serde_json::Value = serde_json::from_str(&resolved_raw).expect("parse");
        assert_eq!(resolved["general"]["language"], "ja");
        assert_eq!(resolved["scraps"]["maxPipeSizeMB"], 10);
        assert_eq!(store.load_raw().expect("raw unchanged"), r#"{"general":{"language":"ja"}}"#);
    }
}
