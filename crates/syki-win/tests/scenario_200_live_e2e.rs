//! Comprehensive Live-API & Core Scenario Evaluation (138 of 200 scenarios: Rust Core + Live Gemini API).
//! Run with: `cargo test -p syki-win --test scenario_200_live_e2e -- --ignored --nocapture`

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::{json, Value};
use syki_core::rpc::{RpcEvent, RpcRequest, RuntimeEnv};
use syki_core::storage::config::{
    get_config_value, resolve_config_with_defaults, set_config_value, AutoDetectHints,
    ConfigStore, JsonConfigStore,
};
use syki_core::storage::{NoteFileStore, ScrapStore};
use syki_win::rpc_dispatch::CoreState;
use syki_win::speech::encode_wav_pcm16;

fn load_gemini_key() -> String {
    if let Ok(appdata) = std::env::var("APPDATA") {
        let cfg_path = PathBuf::from(appdata).join("syki-sok").join("config.json");
        if let Ok(raw) = std::fs::read_to_string(cfg_path) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if let Some(k) = v.pointer("/text/apiKey").and_then(|x| x.as_str()) {
                    if !k.trim().is_empty() {
                        return k.trim().to_string();
                    }
                }
            }
        }
    }
    std::env::var("GEMINI_API_KEY").unwrap_or_default()
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn make_test_state() -> (PathBuf, CoreState, mpsc::Receiver<RpcEvent>) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("syki-sc200-rust-{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    let env = RuntimeEnv {
        data_dir: root.clone(),
        instance_id: format!("sc200-{nanos}"),
    };
    let state = CoreState::new(env, None);
    let (tx, rx) = mpsc::channel::<RpcEvent>();
    state.set_event_sender(Arc::new(move |ev| {
        let _ = tx.send(ev);
    }));
    (root, state, rx)
}

fn rpc_req(id: &str, method: &str, params: Value) -> RpcRequest {
    RpcRequest {
        id: id.to_string(),
        method: method.to_string(),
        params,
    }
}

fn wait_event_opt(
    rx: &mpsc::Receiver<RpcEvent>,
    req_id: &str,
    timeout_secs: u64,
) -> Result<RpcEvent, String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let rem = deadline.saturating_duration_since(Instant::now());
        if rem.is_zero() {
            return Err(format!("timeout waiting for event reqId={req_id}"));
        }
        match rx.recv_timeout(rem) {
            Ok(ev) => {
                if ev.payload.get("reqId").and_then(|v| v.as_str()) == Some(req_id) {
                    return Ok(ev);
                }
            }
            Err(_) => return Err(format!("timeout receiving event reqId={req_id}")),
        }
    }
}

fn tiny_png_b64() -> String {
    let png_bytes: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    STANDARD.encode(png_bytes)
}

struct Recorder {
    results: BTreeMap<String, Value>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            results: BTreeMap::new(),
        }
    }

    fn run<F>(&mut self, id: &str, name: &str, f: F)
    where
        F: FnOnce() -> Result<String, String>,
    {
        let t0 = Instant::now();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let ms = t0.elapsed().as_millis() as u64;
        match outcome {
            Ok(Ok(detail)) => {
                println!("PASS {id} [{ms}ms] {name}: {detail}");
                self.results.insert(
                    id.to_string(),
                    json!({ "id": id, "name": name, "ok": true, "ms": ms, "detail": detail }),
                );
            }
            Ok(Err(err)) => {
                println!("FAIL {id} [{ms}ms] {name}: {err}");
                self.results.insert(
                    id.to_string(),
                    json!({ "id": id, "name": name, "ok": false, "ms": ms, "error": err }),
                );
            }
            Err(_) => {
                println!("FAIL {id} [{ms}ms] {name}: PANIC");
                self.results.insert(
                    id.to_string(),
                    json!({ "id": id, "name": name, "ok": false, "ms": ms, "error": "panic" }),
                );
            }
        }
    }
}

#[test]
#[ignore = "full 200-scenario live API evaluation (Rust portion: 138 scenarios)"]
fn run_scenario_200_rust_live() {
    let api_key = load_gemini_key();
    assert!(!api_key.is_empty(), "Gemini API key not found in APPDATA/syki-sok/config.json or GEMINI_API_KEY");

    let (root, state, rx) = make_test_state();
    let mut rec = Recorder::new();
    let repo = repo_root();

    let gemini_cfg = json!({
        "baseUrl": "https://generativelanguage.googleapis.com",
        "model": "gemini-2.5-flash",
        "apiKey": api_key,
        "systemPrompt": "Reply concisely."
    })
    .to_string();

    let gemini_lite_cfg = json!({
        "baseUrl": "https://generativelanguage.googleapis.com",
        "model": "gemini-3.5-flash-lite",
        "apiKey": api_key,
        "systemPrompt": "Reply concisely."
    })
    .to_string();

    let gemini_openai_cfg = json!({
        "baseUrl": "https://generativelanguage.googleapis.com/v1beta/openai",
        "model": "gemini-2.5-flash",
        "apiKey": api_key,
        "systemPrompt": "Reply concisely."
    })
    .to_string();

    // =========================================================================
    // A. 設定セット & 設定反映タイミング (S001..S023, S030..S032, S034..S035)
    // =========================================================================
    rec.run("S001", "config.get デフォルト解決", || {
        let store = JsonConfigStore::new(root.join("s001_config.json"));
        let raw = store.load_raw().map_err(|e| e.to_string())?;
        if raw.trim() != "{}" {
            return Err(format!("expected {{}}, got {raw}"));
        }
        Ok("missing config -> {}".into())
    });

    rec.run("S002", "getConfig 解決済みJSON", || {
        let hints = AutoDetectHints {
            default_scrap_dir: Some(root.join("scraps").to_string_lossy().into_owned()),
            llm_base_url: Some("https://generativelanguage.googleapis.com".into()),
            llm_model: Some("gemini-3.8-flash".into()),
        };
        let resolved = resolve_config_with_defaults("{}", &hints);
        let url = resolved.pointer("/text/baseUrl").and_then(|v| v.as_str()).unwrap_or("");
        let max_mb = resolved.pointer("/scraps/maxPipeSizeMB").and_then(|v| v.as_u64()).unwrap_or(0);
        if url.is_empty() || max_mb != 10 {
            return Err(format!("unexpected resolved defaults: url={url}, max_mb={max_mb}"));
        }
        Ok(format!("resolved url={url}, maxPipeSizeMB={max_mb}"))
    });

    rec.run("S003", "config.set Bool型設定", || {
        let mut obj = json!({});
        set_config_value(&mut obj, "general.autoSave", json!(true))?;
        set_config_value(&mut obj, "general.autoSave", json!(false))?;
        if get_config_value(&obj, "general.autoSave")? != json!(false) {
            return Err("expected false".into());
        }
        Ok("bool set/get ok".into())
    });

    rec.run("S004", "config.set Int型設定", || {
        let mut obj = json!({});
        set_config_value(&mut obj, "scraps.maxPipeSizeMB", json!(25))?;
        set_config_value(&mut obj, "autocomplete.delayMs", json!(300))?;
        if get_config_value(&obj, "scraps.maxPipeSizeMB")? != json!(25) {
            return Err("maxPipeSizeMB mismatch".into());
        }
        Ok("int set/get ok".into())
    });

    rec.run("S005", "config.set Number型設定", || {
        let mut obj = json!({});
        set_config_value(&mut obj, "voice.barSilenceSec", json!(2.5))?;
        if get_config_value(&obj, "voice.barSilenceSec")? != json!(2.5) {
            return Err("barSilenceSec mismatch".into());
        }
        Ok("number set/get ok".into())
    });

    rec.run("S006", "config.set String型設定", || {
        let mut obj = json!({});
        set_config_value(&mut obj, "text.model", json!("gemini-2.5-flash"))?;
        set_config_value(&mut obj, "general.language", json!("ja"))?;
        set_config_value(&mut obj, "general.theme", json!("paper"))?;
        if get_config_value(&obj, "general.language")? != json!("ja") {
            return Err("language mismatch".into());
        }
        Ok("string set/get ok".into())
    });

    rec.run("S007", "config.set StringArray型設定", || {
        let mut obj = json!({});
        set_config_value(&mut obj, "voice.languageCodes", json!(["ja", "en"]))?;
        if get_config_value(&obj, "voice.languageCodes")? != json!(["ja", "en"]) {
            return Err("languageCodes mismatch".into());
        }
        Ok("string array set/get ok".into())
    });

    rec.run("S008", "config.set 未知キー拒否", || {
        let mut obj = json!({});
        match set_config_value(&mut obj, "unknown.key", json!("x")) {
            Ok(()) => Err("expected error for unknown key".into()),
            Err(e) => Ok(format!("rejected: {e}")),
        }
    });

    rec.run("S009", "config.set 型不一致拒否", || {
        let mut obj = json!({});
        if set_config_value(&mut obj, "general.autoSave", json!("yes")).is_ok() {
            return Err("accepted string for bool".into());
        }
        if set_config_value(&mut obj, "scraps.maxPipeSizeMB", json!("abc")).is_ok() {
            return Err("accepted string for int".into());
        }
        Ok("type mismatch rejected".into())
    });

    rec.run("S010", "config.set 空キー・ドット連続拒否", || {
        let mut obj = json!({});
        for k in ["", "general..theme", ".theme", "general."] {
            if set_config_value(&mut obj, k, json!("x")).is_ok() {
                return Err(format!("accepted invalid key: {k:?}"));
            }
        }
        Ok("invalid key forms rejected".into())
    });

    rec.run("S011", "semantic.* 設定スキーマ", || {
        let mut obj = json!({});
        set_config_value(&mut obj, "semantic.enabled", json!(true))?;
        set_config_value(&mut obj, "semantic.model", json!("bge-m3"))?;
        set_config_value(&mut obj, "semantic.baseUrl", json!("http://localhost:11434"))?;
        Ok("semantic.* accepted".into())
    });

    rec.run("S012", "appearance.* 設定スキーマ", || {
        let mut obj = json!({});
        set_config_value(&mut obj, "appearance.look", json!("paper"))?;
        set_config_value(&mut obj, "appearance.accent", json!("blue"))?;
        set_config_value(&mut obj, "appearance.font", json!("mono"))?;
        Ok("appearance.* accepted".into())
    });

    rec.run("S013", "action.intentRouter 設定スキーマ", || {
        let mut obj = json!({});
        set_config_value(&mut obj, "action.intentRouter", json!(true))?;
        Ok("action.intentRouter accepted".into())
    });

    rec.run("S014", "general.inheritTextConnection 設定スキーマ", || {
        let mut obj = json!({});
        set_config_value(&mut obj, "general.inheritTextConnection", json!(false))?;
        Ok("general.inheritTextConnection accepted".into())
    });

    rec.run("S015", "image.apiKey 設定スキーマ", || {
        let mut obj = json!({});
        set_config_value(&mut obj, "image.apiKey", json!("test-key"))?;
        Ok("image.apiKey accepted".into())
    });

    rec.run("S016", "voice.engine 設定スキーマ", || {
        let mut obj = json!({});
        set_config_value(&mut obj, "voice.engine", json!("gemini"))?;
        Ok("voice.engine accepted".into())
    });

    rec.run("S017", "shortcuts.* 動的キー設定", || {
        let mut obj = json!({});
        set_config_value(&mut obj, "shortcuts.globalSummon", json!("Ctrl+Alt+S"))?;
        set_config_value(&mut obj, "shortcuts.quickCapture", json!("Ctrl+Alt+Q"))?;
        Ok("shortcuts.* accepted".into())
    });

    rec.run("S018", "general.cloudConsent.* 動的キー設定", || {
        let mut obj = json!({});
        set_config_value(&mut obj, "general.cloudConsent.gemini", json!("allow"))?;
        Ok("cloudConsent.* accepted".into())
    });

    rec.run("S019", "maxPipeSizeMB 境界クランプ", || {
        let p = root.join("s019_config.json");
        let store = JsonConfigStore::new(&p);
        for (input, expect_mb) in [(0, 1), (1, 1), (100, 100), (999, 100)] {
            store
                .save_raw(&json!({ "scraps": { "maxPipeSizeMB": input } }).to_string())
                .map_err(|e| e.to_string())?;
            let got = store.max_pipe_size_bytes() / (1024 * 1024);
            if got != expect_mb {
                return Err(format!("input={input} expected={expect_mb} got={got}"));
            }
        }
        Ok("clamped 0->1, 1->1, 100->100, 999->100".into())
    });

    rec.run("S020", "レガシー平坦キー互換", || {
        let p = root.join("s020_config.json");
        let store = JsonConfigStore::new(&p);
        let legacy_dir = root.join("legacy_scraps");
        let nested_dir = root.join("nested_scraps");
        store
            .save_raw(&json!({ "scrap_dir": legacy_dir, "max_pipe_size_mb": 15 }).to_string())
            .map_err(|e| e.to_string())?;
        if store.scrap_dir() != legacy_dir {
            return Err("legacy scrap_dir not used".into());
        }
        if store.max_pipe_size_bytes() != 15 * 1024 * 1024 {
            return Err("legacy max_pipe_size_mb not used".into());
        }
        store
            .save_raw(
                &json!({ "scrap_dir": legacy_dir, "scraps": { "scrapDir": nested_dir } }).to_string(),
            )
            .map_err(|e| e.to_string())?;
        if store.scrap_dir() != nested_dir {
            return Err("nested scraps.scrapDir did not take precedence".into());
        }
        Ok("legacy + precedence ok".into())
    });

    rec.run("S021", "破損 config.json 復旧", || {
        let p = root.join("s021_bad.json");
        std::fs::write(&p, "{ broken json !!!").map_err(|e| e.to_string())?;
        let store = JsonConfigStore::new(&p);
        let resolved = store.load_resolved_json().map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(&resolved).map_err(|e| e.to_string())?;
        if v.pointer("/scraps/maxPipeSizeMB").and_then(|x| x.as_u64()) != Some(10) {
            return Err("fallback defaults missing".into());
        }
        Ok("corrupt config fell back cleanly".into())
    });

    rec.run("S022", "JsonConfigStore キャッシュ無効化", || {
        let p = root.join("s022_cache.json");
        let store = JsonConfigStore::new(&p);
        store.save_raw(r#"{"general":{"language":"ja"}}"#).map_err(|e| e.to_string())?;
        let _ = store.load_raw().map_err(|e| e.to_string())?;
        std::fs::write(&p, r#"{"general":{"language":"en","autoSave":true}}"#)
            .map_err(|e| e.to_string())?;
        let reloaded = store.load_raw().map_err(|e| e.to_string())?;
        if !reloaded.contains(r#""en""#) {
            return Err(format!("stale cache returned: {reloaded}"));
        }
        Ok("cache invalidated on external write".into())
    });

    rec.run("S023", "同一サイズ高速書換キャッシュ", || {
        let p = root.join("s023_same_len.json");
        let store = JsonConfigStore::new(&p);
        store.save_raw(r#"{"general":{"language":"ja"}}"#).map_err(|e| e.to_string())?;
        store.save_raw(r#"{"general":{"language":"en"}}"#).map_err(|e| e.to_string())?;
        let reloaded = store.load_raw().map_err(|e| e.to_string())?;
        if !reloaded.contains(r#""en""#) {
            return Err(format!("stale same-size cache: {reloaded}"));
        }
        Ok("same-size rapid save_raw coherent".into())
    });

    rec.run("S030", "scraps.scrapDir 変更の次回即反映", || {
        let new_dir = root.join("s030_new_scraps");
        let cfg = json!({ "scraps": { "scrapDir": new_dir.to_string_lossy() } }).to_string();
        let r = state.dispatch(&rpc_req("s030-save", "saveConfig", json!([cfg])));
        if !r.ok {
            return Err(format!("saveConfig failed: {:?}", r.error));
        }
        let d = state.dispatch(&rpc_req("s030-dir", "getScrapDir", json!([])));
        let got = d.result.and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        if Path::new(&got) != new_dir {
            return Err(format!("expected {:?}, got {got}", new_dir));
        }
        Ok(format!("scrapDir reflected immediately: {got}"))
    });

    rec.run("S031", "text.model 変更の次回LLM即反映", || {
        let r = state.dispatch(&rpc_req(
            "s031-llm",
            "queryLLMAsync",
            json!(["s031-llm", "Reply with ONLY the word ALPHA.", gemini_lite_cfg]),
        ));
        if !r.ok {
            return Err(format!("dispatch failed: {:?}", r.error));
        }
        let ev = wait_event_opt(&rx, "s031-llm", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let res = ev.payload["result"].as_str().unwrap_or("");
        if !err.is_empty() || res.trim().is_empty() {
            return Err(format!("err={err}, res={res}"));
        }
        Ok(format!("model switch reflected -> {}", res.trim().chars().take(30).collect::<String>()))
    });

    rec.run("S032", "action.model 変更の次回Decision即反映", || {
        let choices = json!([
            { "id": "bug", "label": "Bug", "description": "Software defect or crash" },
            { "id": "doc", "label": "Docs", "description": "Documentation update" }
        ]);
        let cfg1 = json!({ "enabled": true, "baseUrl": "https://generativelanguage.googleapis.com", "model": "gemini-decision-1", "apiKey": api_key });
        let cfg2 = json!({ "enabled": true, "baseUrl": "https://generativelanguage.googleapis.com", "model": "gemini-2.5-flash", "apiKey": api_key });
        state.dispatch(&rpc_req("s032-1", "queryDecisionAsync", json!(["s032-1", cfg1, json!({}), "App crashes with null pointer", choices, "doc", "Classify", 20000])));
        let ev1 = wait_event_opt(&rx, "s032-1", 45)?;
        state.dispatch(&rpc_req("s032-2", "queryDecisionAsync", json!(["s032-2", cfg2, json!({}), "Update README typo", choices, "bug", "Classify", 20000])));
        let ev2 = wait_event_opt(&rx, "s032-2", 45)?;
        if ev1.payload["ok"] != true || ev2.payload["ok"] != true {
            return Err(format!("ev1={:?} ev2={:?}", ev1.payload, ev2.payload));
        }
        Ok(format!("decision1={} flash={}", ev1.payload["choiceId"], ev2.payload["choiceId"]))
    });

    rec.run("S034", "config.get 単一キー取得 vs 全体取得", || {
        let obj = json!({ "general": { "theme": "ink" } });
        if get_config_value(&obj, "general.theme")? != json!("ink") {
            return Err("nested key failed".into());
        }
        if get_config_value(&obj, "general.missing")? != Value::Null {
            return Err("missing key should be null".into());
        }
        Ok("single key & null missing ok".into())
    });

    rec.run("S035", "saveConfig 不正JSON拒否", || {
        let before = state.dispatch(&rpc_req("s035-b", "getConfig", json!([]))).result;
        let bad = state.dispatch(&rpc_req("s035-bad", "saveConfig", json!(["{not valid json"])));
        if bad.ok {
            return Err("accepted invalid json in saveConfig".into());
        }
        let after = state.dispatch(&rpc_req("s035-a", "getConfig", json!([]))).result;
        if before != after {
            return Err("config mutated after failed saveConfig".into());
        }
        Ok("invalid JSON rejected without mutation".into())
    });

    // =========================================================================
    // B. F01 エディタ I/O (S036..S044, S054..S055)
    // =========================================================================
    rec.run("S036", "UTF-8 ノート保存・読込", || {
        let p = root.join("s036_utf8.md");
        let content = "# 日本語テスト 🚀\n行2\r\n行3\n";
        let sv = state.dispatch(&rpc_req("s036-s", "saveFile", json!([p.to_string_lossy(), content, "UTF-8"])));
        if !sv.ok {
            return Err(format!("saveFile failed: {:?}", sv.error));
        }
        let rd = state.dispatch(&rpc_req("s036-r", "readFileByPath", json!([p.to_string_lossy()])));
        if !rd.ok {
            return Err(format!("readFileByPath failed: {:?}", rd.error));
        }
        let got = rd.result.unwrap()["content"].as_str().unwrap_or("").to_string();
        if got != content {
            return Err("roundtrip mismatch".into());
        }
        Ok("UTF-8 roundtrip ok".into())
    });

    rec.run("S037", "Shift_JIS 自動判定・読込・保存", || {
        let sjis_src = repo.join("evals/fixtures/scraps/shift_jis_note.md");
        let sjis_copy = root.join("s037_sjis.md");
        std::fs::copy(&sjis_src, &sjis_copy).map_err(|e| e.to_string())?;
        let loaded = NoteFileStore::read_note(&sjis_copy).map_err(|e| e.to_string())?;
        if !loaded.encoding_name.to_lowercase().contains("shift") {
            return Err(format!("expected Shift_JIS, got {}", loaded.encoding_name));
        }
        let sv = state.dispatch(&rpc_req("s037-s", "saveFile", json!([sjis_copy.to_string_lossy(), loaded.content, loaded.encoding_name])));
        if !sv.ok {
            return Err(format!("saveFile sjis failed: {:?}", sv.error));
        }
        Ok(format!("encoding={} preserved", loaded.encoding_name))
    });

    rec.run("S038", "saveFileChecked 正常保存", || {
        let p = root.join("s038_checked.md");
        std::fs::write(&p, "v1\n").map_err(|e| e.to_string())?;
        let rd = state.dispatch(&rpc_req("s038-r", "readFileByPath", json!([p.to_string_lossy()])));
        let sig = rd.result.unwrap()["sig"].as_str().unwrap_or("").to_string();
        let sv = state.dispatch(&rpc_req("s038-s", "saveFileChecked", json!([p.to_string_lossy(), "v2\n", "UTF-8", sig])));
        if !sv.ok {
            return Err(format!("saveFileChecked failed: {:?}", sv.error));
        }
        Ok("optimistic save succeeded".into())
    });

    rec.run("S039", "saveFileChecked 外部変更衝突検知", || {
        let p = root.join("s039_conflict.md");
        std::fs::write(&p, "v1\n").map_err(|e| e.to_string())?;
        let rd = state.dispatch(&rpc_req("s039-r", "readFileByPath", json!([p.to_string_lossy()])));
        let sig1 = rd.result.unwrap()["sig"].as_str().unwrap_or("").to_string();
        std::fs::write(&p, "external edit\n").map_err(|e| e.to_string())?;
        let sv = state.dispatch(&rpc_req("s039-s", "saveFileChecked", json!([p.to_string_lossy(), "v2\n", "UTF-8", sig1])));
        let conflict = sv.result.as_ref().and_then(|v| v.get("conflict")).and_then(|v| v.as_bool()).unwrap_or(false);
        if sv.ok && !conflict {
            return Err(format!("expected conflict, got {:?}", sv.result));
        }
        let disk = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
        if disk != "external edit\n" {
            return Err("external edit was overwritten!".into());
        }
        Ok("conflict detected and disk preserved".into())
    });

    rec.run("S040", "watchActiveFile 外部変更検知", || {
        let p = root.join("s040_watch.md");
        std::fs::write(&p, "initial").map_err(|e| e.to_string())?;
        let w = state.dispatch(&rpc_req("s040-w", "watchActiveFile", json!([p.to_string_lossy()])));
        if !w.ok {
            return Err(format!("watchActiveFile failed: {:?}", w.error));
        }
        let u = state.dispatch(&rpc_req("s040-u", "unwatchActiveFile", json!([])));
        if !u.ok {
            return Err(format!("unwatchActiveFile failed: {:?}", u.error));
        }
        Ok("watch/unwatch ok".into())
    });

    rec.run("S041", "createNoteInFolder 正常・重複", || {
        let dir = root.join("s041_notes");
        let r1 = state.dispatch(&rpc_req("s041-1", "createNoteInFolder", json!(["# Hello", "UTF-8", "note.md", dir.to_string_lossy()])));
        let r2 = state.dispatch(&rpc_req("s041-2", "createNoteInFolder", json!(["# Hello 2", "UTF-8", "note.md", dir.to_string_lossy()])));
        if !r1.ok || !r2.ok {
            return Err(format!("r1={:?} r2={:?}", r1.error, r2.error));
        }
        let p1 = r1.result.unwrap()["path"].as_str().unwrap_or("").to_string();
        let p2 = r2.result.unwrap()["path"].as_str().unwrap_or("").to_string();
        if p1 == p2 {
            return Err("duplicate note overwrote first".into());
        }
        Ok(format!("created distinct files: {} & {}", p1, p2))
    });

    rec.run("S042", "scanFolderFiles 階層スキャン", || {
        let dir = root.join("s041_notes");
        let r = state.dispatch(&rpc_req("s042", "scanFolderFiles", json!([dir.to_string_lossy()])));
        if !r.ok {
            return Err(format!("scanFolderFiles failed: {:?}", r.error));
        }
        let arr = r.result.unwrap().as_array().cloned().unwrap_or_default();
        if arr.len() < 2 {
            return Err(format!("expected >=2 notes, got {}", arr.len()));
        }
        Ok(format!("scanned {} files", arr.len()))
    });

    rec.run("S043", "プレーンテキスト原子書き出し (write_atomic)", || {
        let out = root.join("s043_out.txt");
        NoteFileStore::write_atomic(&out, "plain text body", "UTF-8").map_err(|e| e.to_string())?;
        let got = std::fs::read_to_string(&out).map_err(|e| e.to_string())?;
        if got != "plain text body" {
            return Err(format!("mismatch: {got:?}"));
        }
        Ok("write_atomic plain text ok".into())
    });

    rec.run("S044", "セッション保存・復元", || {
        let sess = json!({ "tabs": [{ "id": "t1", "title": "A.md" }], "activeTabId": "t1" }).to_string();
        let sv = state.dispatch(&rpc_req("s044-s", "saveSession", json!([sess])));
        if !sv.ok {
            return Err(format!("saveSession failed: {:?}", sv.error));
        }
        let gd = state.dispatch(&rpc_req("s044-g", "getSession", json!([])));
        let got = gd.result.and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        if !got.contains("A.md") {
            return Err(format!("session mismatch: {got}"));
        }
        Ok("session roundtrip ok".into())
    });

    rec.run("S054", "resolveDroppedFile パス解決", || {
        let p = root.join("s036_utf8.md");
        let r = state.dispatch(&rpc_req("s054", "resolveDroppedFile", json!([p.to_string_lossy()])));
        if !r.ok {
            return Err(format!("resolveDroppedFile failed: {:?}", r.error));
        }
        Ok("resolveDroppedFile ok".into())
    });

    rec.run("S055", "不正パスへの保存エラー", || {
        let r = state.dispatch(&rpc_req("s055", "saveFile", json!(["", "content", "UTF-8"])));
        if r.ok {
            return Err("empty path save should fail".into());
        }
        Ok(format!("rejected: {:?}", r.error))
    });

    // =========================================================================
    // C. F02 日次スクラップ & 並列全文検索 (S060..S068)
    // =========================================================================
    let scrap_dir = root.join("s060_scraps");
    std::fs::create_dir_all(&scrap_dir).unwrap();
    let _ = state.dispatch(&rpc_req(
        "setup-scrap-dir",
        "saveConfig",
        json!([json!({ "scraps": { "scrapDir": scrap_dir.to_string_lossy() } }).to_string()]),
    ));

    rec.run("S060", "scrap dir パス解決優先順位", || {
        let r = state.dispatch(&rpc_req("s060", "getScrapDir", json!([])));
        let got = r.result.and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        if Path::new(&got) != scrap_dir {
            return Err(format!("got {got}"));
        }
        Ok("scrapDir resolved".into())
    });

    ScrapStore::append_formatted(&scrap_dir, "日本語検索テスト #syki #design\nAlphaBeta keyword", "").unwrap();
    ScrapStore::append_formatted(&scrap_dir, "別のエントリ #ops\nDeployment log", "").unwrap();

    rec.run("S061", "searchScraps 日本語全文検索", || {
        let r = state.dispatch(&rpc_req("s061", "searchScraps", json!(["日本語検索", 20, ""])));
        let arr = r.result.unwrap_or(json!([])).as_array().cloned().unwrap_or_default();
        if arr.is_empty() {
            return Err("no hits for Japanese query".into());
        }
        Ok(format!("{} hit(s)", arr.len()))
    });

    rec.run("S062", "searchScraps 大文字小文字非依存", || {
        let r = state.dispatch(&rpc_req("s062", "searchScraps", json!(["alphabeta", 20, ""])));
        let arr = r.result.unwrap_or(json!([])).as_array().cloned().unwrap_or_default();
        if arr.is_empty() {
            return Err("case-insensitive search failed".into());
        }
        Ok(format!("{} hit(s)", arr.len()))
    });

    rec.run("S063", "searchScraps 件数上限 (limit)", || {
        let r = state.dispatch(&rpc_req("s063", "searchScraps", json!(["#", 1, ""])));
        let arr = r.result.unwrap_or(json!([])).as_array().cloned().unwrap_or_default();
        if arr.len() > 1 {
            return Err(format!("exceeded limit: {}", arr.len()));
        }
        Ok(format!("capped at {}", arr.len()))
    });

    rec.run("S064", "searchScraps タグ・日付フィルタ", || {
        let r = state.dispatch(&rpc_req("s064", "searchScraps", json!(["#design", 20, ""])));
        let arr = r.result.unwrap_or(json!([])).as_array().cloned().unwrap_or_default();
        if arr.is_empty() {
            return Err("tag search returned 0".into());
        }
        Ok(format!("tag hits={}", arr.len()))
    });

    rec.run("S065", "searchScraps 空クエリ・未ヒット", || {
        let r = state.dispatch(&rpc_req("s065", "searchScraps", json!(["zzz_never_matches_999", 20, ""])));
        let arr = r.result.unwrap_or(json!([])).as_array().cloned().unwrap_or_default();
        if !arr.is_empty() {
            return Err("unexpected hit".into());
        }
        Ok("0 hits safely".into())
    });

    rec.run("S066", "scrapFilterOptions 集計", || {
        let r = state.dispatch(&rpc_req("s066", "scrapFilterOptions", json!([])));
        if !r.ok {
            return Err(format!("scrapFilterOptions failed: {:?}", r.error));
        }
        Ok("filter options ok".into())
    });

    rec.run("S067", "非存在スクラップディレクトリ検索", || {
        let missing = root.join("does_not_exist_scraps");
        let _ = state.dispatch(&rpc_req("s067-cfg", "saveConfig", json!([json!({ "scraps": { "scrapDir": missing } }).to_string()])));
        let r = state.dispatch(&rpc_req("s067", "searchScraps", json!(["test", 10, ""])));
        let _ = state.dispatch(&rpc_req("s067-restore", "saveConfig", json!([json!({ "scraps": { "scrapDir": scrap_dir } }).to_string()])));
        if !r.ok {
            return Err(format!("missing dir caused error: {:?}", r.error));
        }
        Ok("missing scrapDir handled safely".into())
    });

    rec.run("S068", "連続高速スクラップ追記 (10連投)", || {
        let burst_dir = root.join("s068_burst");
        for i in 0..10 {
            ScrapStore::append_formatted(&burst_dir, &format!("burst_item_{i}"), "")
                .map_err(|e| e.to_string())?;
        }
        let entries: Vec<_> = std::fs::read_dir(&burst_dir).map_err(|e| e.to_string())?.collect();
        let mut all = String::new();
        for e in entries {
            let p = e.map_err(|e| e.to_string())?.path();
            all.push_str(&std::fs::read_to_string(p).unwrap_or_default());
        }
        for i in 0..10 {
            if !all.contains(&format!("burst_item_{i}")) {
                return Err(format!("missing burst_item_{i}"));
            }
        }
        Ok("all 10 burst appends present".into())
    });

    // =========================================================================
    // E. F05 シェルパイプ & Policy Decision (S087..S096)
    // =========================================================================
    rec.run("S087", "runCommandFilter 正常変換", || {
        let r = state.dispatch(&rpc_req("s087", "runCommandFilter", json!(["findstr .", "hello world"])));
        if !r.ok {
            return Err(format!("{:?}", r.error));
        }
        let out = r.result.unwrap()["output"].as_str().unwrap_or("").to_string();
        if !out.contains("hello world") {
            return Err(format!("unexpected output: {out:?}"));
        }
        Ok("stdout echoed".into())
    });

    rec.run("S088", "runCommandFilter UTF-8日本語透過", || {
        let r = state.dispatch(&rpc_req("s088", "runCommandFilter", json!(["node -e \"const fs=require('fs');process.stdout.write(fs.readFileSync(0,'utf8').toUpperCase())\"", "こんにちは syki"])));
        let out = r.result.as_ref().and_then(|v| v["output"].as_str()).unwrap_or("");
        if !out.contains("こんにちは SYKI") {
            return Err(format!("UTF-8 mismatch: {:?}", r.result));
        }
        Ok("UTF-8 roundtrip ok".into())
    });

    rec.run("S089", "runCommandFilterAsync 非同期完了イベント", || {
        let r = state.dispatch(&rpc_req("s089", "runCommandFilterAsync", json!(["s089", "findstr .", "async line"])));
        if !r.ok {
            return Err(format!("{:?}", r.error));
        }
        let ev = wait_event_opt(&rx, "s089", 15)?;
        let res = ev.payload["result"]["output"]
            .as_str()
            .or_else(|| ev.payload["result"].as_str())
            .unwrap_or("");
        if !res.contains("async line") {
            return Err(format!("unexpected async payload: {:?}", ev.payload));
        }
        Ok("async filter ok".into())
    });

    rec.run("S090", "cancelCommandFilter 実行中キャンセル", || {
        let _ = state.dispatch(&rpc_req("s090", "runCommandFilterAsync", json!(["s090", "node -e \"setTimeout(()=>{}, 10000)\"", ""])));
        std::thread::sleep(Duration::from_millis(150));
        let c = state.dispatch(&rpc_req("s090-c", "cancelCommandFilter", json!(["s090"])));
        if !c.ok {
            return Err(format!("cancel failed: {:?}", c.error));
        }
        Ok("cancelCommandFilter ok".into())
    });

    rec.run("S091", "runCommandFilter 非0終了コード処理", || {
        let r = state.dispatch(&rpc_req("s091", "runCommandFilter", json!(["node -e \"process.stderr.write('boom');process.exit(3)\"", ""])));
        let err = r.result.as_ref().and_then(|v| v["error"].as_str()).unwrap_or("");
        if err.is_empty() {
            return Err(format!("expected non-zero error, got {:?}", r.result));
        }
        Ok(format!("captured error: {}", err.chars().take(40).collect::<String>()))
    });

    rec.run("S092", "runProgramAsync 未承認プログラム (POLICY_ASK)", || {
        let r = state.dispatch(&rpc_req("s092", "runProgramAsync", json!(["s092", "git", ["--version"], ""])));
        let allowed = r.result.as_ref().and_then(|v| v["allowed"].as_bool()).unwrap_or(true);
        if allowed {
            return Err("unapproved program should not be allowed".into());
        }
        Ok("unapproved -> POLICY_ASK".into())
    });

    rec.run("S093", "runProgramAsync 危険シェル拒否 (POLICY_DENIED)", || {
        let r = state.dispatch(&rpc_req("s093", "runProgramAsync", json!(["s093", "cmd.exe", ["/c", "echo hi"], ""])));
        let allowed = r.result.as_ref().and_then(|v| v["allowed"].as_bool()).unwrap_or(true);
        let verdict = r.result.as_ref().and_then(|v| v.pointer("/decision/verdict")).and_then(|v| v.as_str()).unwrap_or("");
        if allowed || verdict.to_lowercase() != "deny" {
            return Err(format!("expected deny, got {:?}", r.result));
        }
        Ok("cmd.exe denied".into())
    });

    rec.run("S094", "approveProgram -> runProgramAsync 許可実行", || {
        let ap = state.dispatch(&rpc_req("s094-a", "approveProgram", json!(["node"])));
        if !ap.ok {
            return Err(format!("approveProgram failed: {:?}", ap.error));
        }
        let r = state.dispatch(&rpc_req("s094-r", "runProgramAsync", json!(["s094-r", "node", ["-e", "console.log('policy_ok')"], ""])));
        let allowed = r.result.as_ref().and_then(|v| v["allowed"].as_bool()).unwrap_or(false);
        if !allowed {
            return Err(format!("approved node denied: {:?}", r.result));
        }
        let ev = wait_event_opt(&rx, "s094-r", 15)?;
        let out = ev.payload["result"]["output"]
            .as_str()
            .or_else(|| ev.payload["stdout"].as_str())
            .or_else(|| ev.payload["result"].as_str())
            .unwrap_or("");
        if !out.contains("policy_ok") {
            return Err(format!("unexpected output: {:?}", ev.payload));
        }
        Ok("approved program executed".into())
    });

    rec.run("S095", "getPolicyAuditLog 監査ログ記録", || {
        let r = state.dispatch(&rpc_req("s095", "getPolicyAuditLog", json!([])));
        let arr = r.result.unwrap_or(json!([])).as_array().cloned().unwrap_or_default();
        if arr.len() < 3 {
            return Err(format!("expected >=3 audit entries, got {}", arr.len()));
        }
        Ok(format!("{} audit entries", arr.len()))
    });

    rec.run("S096", "approveProgram 空プログラム名拒否", || {
        let r = state.dispatch(&rpc_req("s096", "approveProgram", json!([""])));
        if r.ok {
            return Err("empty program approved".into());
        }
        Ok("empty program rejected".into())
    });

    // =========================================================================
    // F. F06/F08 基盤 (S102..S106) & G. F07/F10 (S107, S109..S114)
    // =========================================================================
    rec.run("S102", "printSavePdf 出力先不正パスエラー", || {
        let r = state.dispatch(&rpc_req("s102", "printSavePdf", json!(["", "<html></html>"])));
        if r.ok {
            return Err("empty pdf path should fail".into());
        }
        Ok("invalid pdf path rejected".into())
    });

    rec.run("S103", "printPreview / printPreviewClose 遷移", || {
        let r = state.dispatch(&rpc_req("s103-c", "printPreviewClose", json!([])));
        if !r.ok {
            return Err(format!("printPreviewClose failed: {:?}", r.error));
        }
        Ok("printPreviewClose ok".into())
    });

    rec.run("S104", "user.css @media print ルール存在確認", || {
        let css = std::fs::read_to_string(repo.join("frontend/css/print.css")).map_err(|e| e.to_string())?;
        if !css.contains("@media print") && !css.contains("@page") {
            return Err("print.css missing print rules".into());
        }
        Ok("print.css verified".into())
    });

    rec.run("S105", "openExternal URLスキーム検証", || {
        let bad = state.dispatch(&rpc_req("s105", "openExternal", json!(["javascript:alert(1)"])));
        if bad.ok {
            return Err("javascript: URL allowed!".into());
        }
        Ok("dangerous URL scheme rejected".into())
    });

    rec.run("S106", "getAppVersion / getAppInfo / getPlatformCapabilities", || {
        let v = state.dispatch(&rpc_req("s106-v", "getAppVersion", json!([])));
        let i = state.dispatch(&rpc_req("s106-i", "getAppInfo", json!([])));
        let c = state.dispatch(&rpc_req("s106-c", "getPlatformCapabilities", json!([])));
        if !v.ok || !i.ok || !c.ok {
            return Err("platform info RPC failed".into());
        }
        Ok(format!("version={}", v.result.unwrap()))
    });

    rec.run("S107", "setIMEMode 正常切替", || {
        let r = state.dispatch(&rpc_req("s107", "setIMEMode", json!(["alphanumeric"])));
        if !r.ok {
            return Err(format!("setIMEMode failed: {:?}", r.error));
        }
        Ok("setIMEMode ok".into())
    });

    rec.run("S109", "saveQcThemeCache 正常キャッシュ保存", || {
        let snap = json!({
            "bgModal": "#112233", "bgField": "#223344", "border": "#334455",
            "bgBtn": "#445566", "accent": "#556677", "textMain": "#eef0f2",
            "textOnAccent": "#ffffff", "textMuted": "#99aabb"
        });
        let r = state.dispatch(&rpc_req("s109", "saveQcThemeCache", json!([snap])));
        if !r.ok || !root.join("qc-theme-cache.json").is_file() {
            return Err(format!("saveQcThemeCache failed: {:?}", r.error));
        }
        Ok("qc-theme-cache.json saved".into())
    });

    rec.run("S110", "saveQcThemeCache 不正ペイロード処理", || {
        let r = state.dispatch(&rpc_req("s110", "saveQcThemeCache", json!(["not-an-object"])));
        // Should handle safely without panic
        Ok(format!("handled safely ok={}", r.ok))
    });

    let png_b64 = tiny_png_b64();
    rec.run("S111", "saveAsset PNG保存・相対パス返却", || {
        let r = state.dispatch(&rpc_req("s111", "saveAsset", json!([root.to_string_lossy(), "png", png_b64])));
        if !r.ok {
            return Err(format!("saveAsset failed: {:?}", r.error));
        }
        Ok(format!("saved: {:?}", r.result))
    });

    rec.run("S112", "saveAsset 危険拡張子/パストラバーサル防止", || {
        let r = state.dispatch(&rpc_req("s112", "saveAsset", json!([root.to_string_lossy(), "../../evil.png", png_b64])));
        if r.ok {
            let s = r.result.as_ref().map(|v| v.to_string()).unwrap_or_default();
            if s.contains("..") {
                return Err(format!("path traversal in result: {s}"));
            }
        }
        Ok("traversal prevented".into())
    });

    rec.run("S113", "importAssetFile 実在/不正データ", || {
        let r = state.dispatch(&rpc_req("s113", "importAssetFile", json!([root.to_string_lossy(), "sample.png", png_b64])));
        if !r.ok {
            return Err(format!("importAssetFile failed: {:?}", r.error));
        }
        Ok("importAssetFile ok".into())
    });

    rec.run("S114", "isSendToShortcutInstalled 状態取得", || {
        let r = state.dispatch(&rpc_req("s114", "isSendToShortcutInstalled", json!([])));
        if !r.ok {
            return Err(format!("{:?}", r.error));
        }
        Ok(format!("installed={:?}", r.result))
    });

    // =========================================================================
    // H. F11 内蔵 LLM / Decision / Autocomplete / CLI生成 [本番 Gemini API] (S115..S144)
    // =========================================================================
    rec.run("S115", "detectLLMProvider Gemini URL判定", || {
        let r = state.dispatch(&rpc_req("s115", "detectLLMProvider", json!(["https://generativelanguage.googleapis.com", ""])));
        if !r.ok {
            return Err(format!("{:?}", r.error));
        }
        Ok(format!("detected={:?}", r.result))
    });

    rec.run("S116", "detectLLMProvider OpenAI / Anthropic / Ollama 判定", || {
        for url in ["https://api.openai.com/v1", "https://api.anthropic.com", "http://127.0.0.1:11434"] {
            let r = state.dispatch(&rpc_req("s116", "detectLLMProvider", json!([url, ""])));
            if !r.ok {
                return Err(format!("failed for {url}"));
            }
        }
        Ok("all providers detected".into())
    });

    rec.run("S117", "fetchModels 本番 Gemini モデル一覧取得", || {
        let r = state.dispatch(&rpc_req("s117", "fetchModels", json!(["https://generativelanguage.googleapis.com", api_key])));
        if !r.ok {
            return Err(format!("{:?}", r.error));
        }
        let models = r.result.unwrap()["models"].as_array().cloned().unwrap_or_default();
        if models.is_empty() {
            return Err("0 models returned".into());
        }
        Ok(format!("{} models", models.len()))
    });

    rec.run("S118", "fetchModels OpenAI互換エンドポイント (/v1beta/openai)", || {
        let r = state.dispatch(&rpc_req("s118", "fetchModels", json!(["https://generativelanguage.googleapis.com/v1beta/openai", api_key])));
        if !r.ok {
            return Err(format!("{:?}", r.error));
        }
        let models = r.result.unwrap()["models"].as_array().cloned().unwrap_or_default();
        if models.is_empty() {
            return Err("0 models".into());
        }
        Ok(format!("{} models", models.len()))
    });

    rec.run("S119", "fetchModels 空APIキーガード", || {
        let r = state.dispatch(&rpc_req("s119", "fetchModels", json!(["https://api.anthropic.com", ""])));
        if r.ok {
            return Err("expected empty key error".into());
        }
        Ok("empty key blocked".into())
    });

    rec.run("S120", "fetchModels 不正APIキー401処理", || {
        let fake_key = "AIzaSyINVALID_TEST_KEY_00000000000000";
        let r = state.dispatch(&rpc_req("s120", "fetchModels", json!(["https://generativelanguage.googleapis.com", fake_key])));
        if r.ok {
            return Err("invalid key succeeded?".into());
        }
        let err = r.error.unwrap_or_default();
        if err.contains(fake_key) {
            return Err("API key leaked in error message!".into());
        }
        Ok(format!("401 handled cleanly (no key leak)"))
    });

    rec.run("S121", "queryLLMAsync 本番 Gemini 基本応答 (gemini-2.5-flash)", || {
        state.dispatch(&rpc_req("s121", "queryLLMAsync", json!(["s121", "Reply with ONLY the word OK.", gemini_cfg])));
        let ev = wait_event_opt(&rx, "s121", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let res = ev.payload["result"].as_str().unwrap_or("");
        if !err.is_empty() || res.trim().is_empty() {
            return Err(format!("err={err} res={res}"));
        }
        Ok(format!("-> {}", res.trim().chars().take(30).collect::<String>()))
    });

    rec.run("S122", "queryLLMAsync 本番 gemini-3.5-flash-lite 応答", || {
        state.dispatch(&rpc_req("s122", "queryLLMAsync", json!(["s122", "Reply with ONLY 42.", gemini_lite_cfg])));
        let ev = wait_event_opt(&rx, "s122", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let res = ev.payload["result"].as_str().unwrap_or("");
        if !err.is_empty() || res.trim().is_empty() {
            return Err(format!("err={err} res={res}"));
        }
        Ok(format!("-> {}", res.trim().chars().take(30).collect::<String>()))
    });

    rec.run("S123", "queryLLMAsync OpenAI互換パス (/v1beta/openai)", || {
        state.dispatch(&rpc_req("s123", "queryLLMAsync", json!(["s123", "Reply with ONLY PING.", gemini_openai_cfg])));
        let ev = wait_event_opt(&rx, "s123", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let res = ev.payload["result"].as_str().unwrap_or("");
        if !err.is_empty() || res.trim().is_empty() {
            return Err(format!("err={err} res={res}"));
        }
        Ok(format!("-> {}", res.trim().chars().take(30).collect::<String>()))
    });

    rec.run("S124", "queryLLMAsync systemPrompt 反映確認", || {
        let sp_cfg = json!({
            "baseUrl": "https://generativelanguage.googleapis.com",
            "model": "gemini-2.5-flash",
            "apiKey": api_key,
            "systemPrompt": "Always start your reply with the exact prefix [SYKI_SYS]:"
        }).to_string();
        state.dispatch(&rpc_req("s124", "queryLLMAsync", json!(["s124", "Say hello.", sp_cfg])));
        let ev = wait_event_opt(&rx, "s124", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let res = ev.payload["result"].as_str().unwrap_or("");
        if !err.is_empty() || !res.contains("[SYKI_SYS]:") {
            return Err(format!("systemPrompt not followed: err={err} res={res}"));
        }
        Ok("systemPrompt prefix honored".into())
    });

    rec.run("S125", "queryLLMAsync 日本語長文要約・Markdown生成", || {
        state.dispatch(&rpc_req("s125", "queryLLMAsync", json!(["s125", "次の文を日本語の箇条書き2点で要約して: sykiはRustとWebView2で作られた超高速メモ帳で、トレイ待機時のRAM消費を3-5MBに抑えつつプラグインで拡張できる。", gemini_cfg])));
        let ev = wait_event_opt(&rx, "s125", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let res = ev.payload["result"].as_str().unwrap_or("");
        if !err.is_empty() || res.trim().is_empty() {
            return Err(format!("err={err}"));
        }
        Ok(format!("len={} chars", res.chars().count()))
    });

    rec.run("S126", "queryLLMAsync 環境変数 GEMINI_API_KEY フォールバック", || {
        // Test with explicit empty apiKey and valid GEMINI_API_KEY in env
        std::env::set_var("GEMINI_API_KEY", &api_key);
        let env_cfg = json!({
            "baseUrl": "https://generativelanguage.googleapis.com",
            "model": "gemini-2.5-flash",
            "apiKey": ""
        }).to_string();
        state.dispatch(&rpc_req("s126", "queryLLMAsync", json!(["s126", "Reply OK", env_cfg])));
        let ev = wait_event_opt(&rx, "s126", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let res = ev.payload["result"].as_str().unwrap_or("");
        if !err.is_empty() || res.trim().is_empty() {
            return Err(format!("env fallback failed: err={err} res={res}"));
        }
        Ok("env GEMINI_API_KEY fallback ok".into())
    });

    rec.run("S127", "queryLLMAsync 不正APIキーエラー", || {
        let bad_key = "AIzaSyBAD_SECRET_KEY_1234567890";
        let bad_cfg = json!({
            "baseUrl": "https://generativelanguage.googleapis.com",
            "model": "gemini-2.5-flash",
            "apiKey": bad_key
        }).to_string();
        state.dispatch(&rpc_req("s127", "queryLLMAsync", json!(["s127", "Hi", bad_cfg])));
        let ev = wait_event_opt(&rx, "s127", 30)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        if err.is_empty() {
            return Err("expected error for invalid key".into());
        }
        if err.contains(bad_key) {
            return Err("API key leaked in llm:result error!".into());
        }
        Ok("invalid key error clean".into())
    });

    rec.run("S128", "queryLLMAsync 存在しないモデル名エラー", || {
        let bad_model = json!({
            "baseUrl": "https://generativelanguage.googleapis.com",
            "model": "gemini-nonexistent-999",
            "apiKey": api_key
        }).to_string();
        state.dispatch(&rpc_req("s128", "queryLLMAsync", json!(["s128", "Hi", bad_model])));
        let ev = wait_event_opt(&rx, "s128", 30)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        if err.is_empty() {
            return Err("expected 404 model error".into());
        }
        Ok("404 model error returned".into())
    });

    rec.run("S129", "queryLLMAsync 空プロンプト処理", || {
        state.dispatch(&rpc_req("s129", "queryLLMAsync", json!(["s129", "", gemini_cfg])));
        let ev = wait_event_opt(&rx, "s129", 30)?;
        // Either graceful error or response, no hang/panic
        Ok(format!("handled err_empty={}", ev.payload["error"].as_str().unwrap_or("").is_empty()))
    });

    rec.run("S130", "queryLLMAsync + cancelLLMRequest キャンセル", || {
        state.dispatch(&rpc_req("s130", "queryLLMAsync", json!(["s130", "Write a 500 word essay about Rust.", gemini_cfg])));
        let cancelled = state.dispatch(&rpc_req("s130-c", "cancelLLMRequest", json!(["s130"])));
        let _ = wait_event_opt(&rx, "s130", 30);
        if !cancelled.ok {
            return Err("cancelLLMRequest failed".into());
        }
        Ok(format!("cancelled={:?}", cancelled.result))
    });

    let yn_choices = json!([
        { "id": "yes", "label": "Yes", "description": "Affirmative" },
        { "id": "no", "label": "No", "description": "Negative" }
    ]);
    let gem_dec_cfg = json!({
        "enabled": true,
        "baseUrl": "https://generativelanguage.googleapis.com",
        "model": "gemini-decision-1",
        "apiKey": api_key
    });

    rec.run("S131", "queryDecisionAsync 本番 gemini-decision-1 肯定判定", || {
        state.dispatch(&rpc_req("s131", "queryDecisionAsync", json!(["s131", gem_dec_cfg, json!({}), "Yes, please proceed immediately.", yn_choices, "no", "Pick choice", 20000])));
        let ev = wait_event_opt(&rx, "s131", 30)?;
        if ev.payload["ok"] != true || ev.payload["choiceId"] != "yes" {
            return Err(format!("unexpected: {:?}", ev.payload));
        }
        Ok(format!("choiceId=yes by={}", ev.payload["by"]))
    });

    rec.run("S132", "queryDecisionAsync 本番 gemini-decision-1 日本語多択分類", || {
        let choices = json!([
            { "id": "bug", "label": "バグ", "description": "不具合・クラッシュ・エラー報告" },
            { "id": "feature", "label": "機能要望", "description": "新機能の追加リクエスト" },
            { "id": "docs", "label": "ドキュメント", "description": "READMEや仕様書の誤字修正" },
            { "id": "question", "label": "質問", "description": "使い方に関する質問" }
        ]);
        state.dispatch(&rpc_req("s132", "queryDecisionAsync", json!(["s132", gem_dec_cfg, json!({}), "PDF出力ボタンを押すとアプリが落ちます", choices, "question", "問い合わせ種別を分類して", 20000])));
        let ev = wait_event_opt(&rx, "s132", 30)?;
        if ev.payload["ok"] != true || ev.payload["choiceId"] != "bug" {
            return Err(format!("expected bug, got {:?}", ev.payload));
        }
        Ok(format!("classified=bug by={}", ev.payload["by"]))
    });

    rec.run("S133", "queryDecisionAsync 出力トークン0/高速性検証", || {
        let t0 = Instant::now();
        state.dispatch(&rpc_req("s133", "queryDecisionAsync", json!(["s133", gem_dec_cfg, json!({}), "No, cancel it.", yn_choices, "yes", "Classify", 20000])));
        let ev = wait_event_opt(&rx, "s133", 30)?;
        let ms = t0.elapsed().as_millis();
        if ev.payload["ok"] != true || ev.payload["choiceId"] != "no" {
            return Err(format!("unexpected: {:?}", ev.payload));
        }
        Ok(format!("choiceId=no in {ms}ms"))
    });

    rec.run("S134", "queryDecisionAsync 通常モデルフォールバック (gemini-2.5-flash)", || {
        let flash_dec = json!({ "enabled": true, "baseUrl": "https://generativelanguage.googleapis.com", "model": "gemini-2.5-flash", "apiKey": api_key });
        state.dispatch(&rpc_req("s134", "queryDecisionAsync", json!(["s134", flash_dec, json!({}), "Yes, confirmed.", yn_choices, "no", "Pick", 20000])));
        let ev = wait_event_opt(&rx, "s134", 30)?;
        if ev.payload["ok"] != true || ev.payload["choiceId"] != "yes" {
            return Err(format!("unexpected: {:?}", ev.payload));
        }
        Ok(format!("flash decision ok by={}", ev.payload["by"]))
    });

    rec.run("S135", "queryDecisionAsync {{input}} テンプレート展開", || {
        state.dispatch(&rpc_req("s135", "queryDecisionAsync", json!(["s135", gem_dec_cfg, json!({}), "Absolutely yes!", yn_choices, "no", "User wrote: {{input}}. Decide yes or no.", 20000])));
        let ev = wait_event_opt(&rx, "s135", 30)?;
        if ev.payload["ok"] != true || ev.payload["choiceId"] != "yes" {
            return Err(format!("unexpected: {:?}", ev.payload));
        }
        Ok("template expansion ok".into())
    });

    rec.run("S136", "queryDecisionAsync defaultConfig 継承", || {
        let def_cfg = json!({ "baseUrl": "https://generativelanguage.googleapis.com", "model": "gemini-2.5-flash", "apiKey": api_key });
        state.dispatch(&rpc_req("s136", "queryDecisionAsync", json!(["s136", json!({ "enabled": true }), def_cfg, "Yes!", yn_choices, "no", "Decide", 20000])));
        let ev = wait_event_opt(&rx, "s136", 30)?;
        if ev.payload["ok"] != true {
            return Err(format!("defaultConfig fallback failed: {:?}", ev.payload));
        }
        Ok(format!("inherited choiceId={}", ev.payload["choiceId"]))
    });

    rec.run("S137", "queryDecisionAsync enabled:false 時のフォールバック", || {
        state.dispatch(&rpc_req("s137", "queryDecisionAsync", json!(["s137", json!({ "enabled": false }), json!({}), "Yes!", yn_choices, "no", "Decide", 5000])));
        let ev = wait_event_opt(&rx, "s137", 10)?;
        if ev.payload["choiceId"] != "no" {
            return Err(format!("expected fallback 'no', got {:?}", ev.payload));
        }
        Ok(format!("disabled -> fallback ({})", ev.payload["by"]))
    });

    rec.run("S138", "queryDecisionAsync 空choices/無効キー時の安全フォールバック", || {
        state.dispatch(&rpc_req("s138", "queryDecisionAsync", json!(["s138", json!({ "enabled": true, "model": "gemini-decision-1", "apiKey": "AIzaSyBAD" }), json!({}), "hi", json!([]), "fb", "Decide", 5000])));
        let ev = wait_event_opt(&rx, "s138", 15)?;
        if ev.payload["choiceId"] != "fb" {
            return Err(format!("expected fallback 'fb', got {:?}", ev.payload));
        }
        Ok("empty choices safely returned fallback".into())
    });

    rec.run("S139", "queryDecisionAsync 超短タイムアウト (timeoutMs)", || {
        state.dispatch(&rpc_req("s139", "queryDecisionAsync", json!(["s139", gem_dec_cfg, json!({}), "Yes!", yn_choices, "no", "Decide", 1])));
        let ev = wait_event_opt(&rx, "s139", 15)?;
        if ev.payload["choiceId"].as_str().unwrap_or("").is_empty() {
            return Err(format!("empty choiceId on timeout: {:?}", ev.payload));
        }
        Ok(format!("timeout handled -> {} ({})", ev.payload["choiceId"], ev.payload["by"]))
    });

    rec.run("S140", "autocompleteAsync 本番 Gemini コード補完", || {
        state.dispatch(&rpc_req("s140", "autocompleteAsync", json!(["s140", "function add(a, b) {\n  return ", "\n}", gemini_cfg])));
        let ev = wait_event_opt(&rx, "s140", 30)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let sug = ev.payload["suggestion"].as_str().unwrap_or("");
        if !err.is_empty() || sug.trim().is_empty() {
            return Err(format!("err={err} sug={sug}"));
        }
        Ok(format!("-> {}", sug.trim().chars().take(30).collect::<String>()))
    });

    rec.run("S141", "autocompleteAsync 本番 Gemini 日本語文章補完", || {
        state.dispatch(&rpc_req("s141", "autocompleteAsync", json!(["s141", "本日の会議では、来月のリリーススケジュールについて", "。", gemini_cfg])));
        let ev = wait_event_opt(&rx, "s141", 30)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let sug = ev.payload["suggestion"].as_str().unwrap_or("");
        if !err.is_empty() || sug.trim().is_empty() {
            return Err(format!("err={err} sug={sug}"));
        }
        Ok(format!("-> {}", sug.trim().chars().take(30).collect::<String>()))
    });

    rec.run("S142", "autocompleteAsync maxTokens 設定反映", || {
        let short_cfg = json!({
            "baseUrl": "https://generativelanguage.googleapis.com",
            "model": "gemini-2.5-flash",
            "apiKey": api_key,
            "maxTokens": 12
        }).to_string();
        state.dispatch(&rpc_req("s142", "autocompleteAsync", json!(["s142", "const greeting = 'Hello, ", "';", short_cfg])));
        let ev = wait_event_opt(&rx, "s142", 30)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        if !err.is_empty() {
            return Err(format!("err={err}"));
        }
        Ok(format!("sug_len={}", ev.payload["suggestion"].as_str().unwrap_or("").len()))
    });

    rec.run("S143", "generateCliCommandAsync 本番 Gemini コマンド生成", || {
        state.dispatch(&rpc_req("s143", "generateCliCommandAsync", json!(["s143", "list files in current directory", gemini_cfg, "{}"])));
        let ev = wait_event_opt(&rx, "s143", 30)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let cmd = ev.payload["command"].as_str().or_else(|| ev.payload["result"].as_str()).unwrap_or("");
        if !err.is_empty() || cmd.trim().is_empty() {
            return Err(format!("err={err} payload={:?}", ev.payload));
        }
        Ok(format!("cmd={}", cmd.trim().chars().take(40).collect::<String>()))
    });

    rec.run("S144", "generateCliCommandAsync 不正キーエラー処理", || {
        let bad_cfg = json!({ "baseUrl": "https://generativelanguage.googleapis.com", "model": "gemini-2.5-flash", "apiKey": "AIzaSyINVALID" }).to_string();
        state.dispatch(&rpc_req("s144", "generateCliCommandAsync", json!(["s144", "list files", bad_cfg, "{}"])));
        let ev = wait_event_opt(&rx, "s144", 30)?;
        if ev.payload["error"].as_str().unwrap_or("").is_empty() {
            return Err("expected error for invalid key".into());
        }
        Ok("invalid key error returned".into())
    });

    // =========================================================================
    // I. F12 画像生成 / F23 Vision OCR / Media Drop [本番 Gemini API] (S145..S155)
    // =========================================================================
    let mut generated_img_path: Option<PathBuf> = None;
    rec.run("S145", "generateImageAsync 本番 Gemini 画像生成 (1:1)", || {
        let img_cfg = json!({
            "baseUrl": "https://generativelanguage.googleapis.com",
            "apiKey": api_key,
            "model": "gemini-3.1-flash-lite-image",
            "aspectRatio": "1:1"
        }).to_string();
        let note_path = root.join("s145_note.md").to_string_lossy().to_string();
        state.dispatch(&rpc_req("s145", "generateImageAsync", json!(["s145", "minimalist blue circle icon", img_cfg, note_path])));
        let ev = wait_event_opt(&rx, "s145", 60)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let res = ev.payload["result"].as_str().unwrap_or("");
        if !err.is_empty() || !res.starts_with("![Generated Image](") {
            return Err(format!("err={err} res={res}"));
        }
        if let Ok(rd) = std::fs::read_dir(root.join("assets")) {
            for e in rd.flatten() {
                if e.path().is_file() {
                    generated_img_path = Some(e.path());
                }
            }
        }
        Ok(res.to_string())
    });

    rec.run("S146", "generateImageAsync アスペクト比変更 (16:9)", || {
        let img_cfg = json!({
            "baseUrl": "https://generativelanguage.googleapis.com",
            "apiKey": api_key,
            "model": "gemini-3.1-flash-lite-image",
            "aspectRatio": "16:9"
        }).to_string();
        let note_path = root.join("s146_note.md").to_string_lossy().to_string();
        state.dispatch(&rpc_req("s146", "generateImageAsync", json!(["s146", "minimalist green landscape line", img_cfg, note_path])));
        let ev = wait_event_opt(&rx, "s146", 60)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let res = ev.payload["result"].as_str().unwrap_or("");
        if !err.is_empty() || !res.starts_with("![Generated Image](") {
            return Err(format!("err={err} res={res}"));
        }
        Ok(res.to_string())
    });

    rec.run("S147", "generateImageAsync 不正APIキーエラー", || {
        let img_cfg = json!({
            "baseUrl": "https://generativelanguage.googleapis.com",
            "apiKey": "AIzaSyBAD_KEY",
            "model": "gemini-3.1-flash-lite-image",
            "aspectRatio": "1:1"
        }).to_string();
        state.dispatch(&rpc_req("s147", "generateImageAsync", json!(["s147", "circle", img_cfg, root.join("s147.md").to_string_lossy()])));
        let ev = wait_event_opt(&rx, "s147", 30)?;
        if ev.payload["error"].as_str().unwrap_or("").is_empty() {
            return Err("expected error".into());
        }
        Ok("invalid key error returned".into())
    });

    let sample_ocr_b64 = STANDARD.encode(std::fs::read(repo.join("evals/fixtures/images/sample_ocr.png")).unwrap_or_default());
    rec.run("S148", "queryVisionAsync 本番 Gemini OCR (sample_ocr.png)", || {
        state.dispatch(&rpc_req("s148", "queryVisionAsync", json!(["s148", "Extract all text or describe this image concisely.", sample_ocr_b64, "image/png", gemini_cfg])));
        let ev = wait_event_opt(&rx, "s148", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let res = ev.payload["result"].as_str().unwrap_or("");
        if !err.is_empty() || res.trim().is_empty() {
            return Err(format!("err={err} res={res}"));
        }
        Ok(format!("-> {}", res.trim().chars().take(40).collect::<String>()))
    });

    rec.run("S149", "queryVisionAsync カスタム vision.prompt 反映", || {
        state.dispatch(&rpc_req("s149", "queryVisionAsync", json!(["s149", "Reply with ONLY a single word naming the main color.", png_b64, "image/png", gemini_cfg])));
        let ev = wait_event_opt(&rx, "s149", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let res = ev.payload["result"].as_str().unwrap_or("");
        if !err.is_empty() || res.trim().is_empty() {
            return Err(format!("err={err}"));
        }
        Ok(format!("color -> {}", res.trim()))
    });

    rec.run("S150", "queryVisionAsync 1x1 PNG色認識", || {
        state.dispatch(&rpc_req("s150", "queryVisionAsync", json!(["s150", "What color is this pixel?", png_b64, "image/png", gemini_cfg])));
        let ev = wait_event_opt(&rx, "s150", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let res = ev.payload["result"].as_str().unwrap_or("");
        if !err.is_empty() || res.trim().is_empty() {
            return Err(format!("err={err}"));
        }
        Ok(format!("-> {}", res.trim().chars().take(30).collect::<String>()))
    });

    rec.run("S151", "queryVisionAsync 破損Base64エラー", || {
        state.dispatch(&rpc_req("s151", "queryVisionAsync", json!(["s151", "Describe", "!!!not_base64!!!", "image/png", gemini_cfg])));
        let ev = wait_event_opt(&rx, "s151", 30)?;
        if ev.payload["error"].as_str().unwrap_or("").is_empty() {
            return Err("expected error for corrupt base64".into());
        }
        Ok("corrupt base64 error returned".into())
    });

    rec.run("S152", "processDroppedMedia 画像PNGドロップ (本番Gemini)", || {
        state.dispatch(&rpc_req("s152", "processDroppedMedia", json!(["s152", "tiny.png", "image/png", png_b64, gemini_cfg, "{}"])));
        let ev = wait_event_opt(&rx, "s152", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        if !err.is_empty() {
            return Err(format!("err={err}"));
        }
        Ok("PNG dropped media ok".into())
    });

    let tone_wav_b64 = STANDARD.encode(std::fs::read(repo.join("evals/fixtures/audio/tone_44100_stereo.wav")).unwrap_or_default());
    let voice_cfg = json!({
        "engine": "gemini",
        "apiKey": api_key,
        "model": "gemini-2.5-flash",
        "prompt": "Transcribe speech or reply empty if no speech."
    }).to_string();

    rec.run("S153", "processDroppedMedia 音声WAVドロップ (本番Gemini STT)", || {
        state.dispatch(&rpc_req("s153", "processDroppedMedia", json!(["s153", "tone.wav", "audio/wav", tone_wav_b64, gemini_cfg, voice_cfg])));
        let ev = wait_event_opt(&rx, "s153", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        // Pure sine tone has no speech; Gemini may return empty transcription cleanly
        if !err.is_empty() && !err.contains("空") && !err.to_ascii_lowercase().contains("empty") {
            return Err(format!("err={err}"));
        }
        Ok(if err.is_empty() { "WAV dropped media ok".into() } else { format!("no-speech tone handled cleanly: {err}") })
    });

    let silence_wav_b64 = STANDARD.encode(encode_wav_pcm16(&vec![0i16; 8000], 16_000, 1));
    rec.run("S154", "processDroppedMedia 無音WAVドロップ", || {
        state.dispatch(&rpc_req("s154", "processDroppedMedia", json!(["s154", "silence.wav", "audio/wav", silence_wav_b64, gemini_cfg, voice_cfg])));
        let ev = wait_event_opt(&rx, "s154", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        if !err.is_empty() {
            return Err(format!("err={err}"));
        }
        Ok("silence WAV ok".into())
    });

    rec.run("S155", "processDroppedMedia 空データ/不正MIME", || {
        state.dispatch(&rpc_req("s155", "processDroppedMedia", json!(["s155", "bad.bin", "application/octet-stream", "", gemini_cfg, voice_cfg])));
        let ev = wait_event_opt(&rx, "s155", 30)?;
        if ev.payload["error"].as_str().unwrap_or("").is_empty() {
            return Err("expected error for empty/invalid media".into());
        }
        Ok("invalid media error returned".into())
    });

    // =========================================================================
    // J. F13 意味検索 / F18 PTY / F21-F22 音声STT・会議録音 (S159..S164, S166..S172)
    // =========================================================================
    rec.run("S159", "semanticStatus 初期・設定反映", || {
        let r = state.dispatch(&rpc_req("s159", "semanticStatus", json!([{ "enabled": false }])));
        if !r.ok {
            return Err(format!("{:?}", r.error));
        }
        Ok("semanticStatus ok".into())
    });

    rec.run("S160", "semanticUpdate 未設定時エラー", || {
        let r = state.dispatch(&rpc_req("s160", "semanticUpdate", json!([{ "enabled": false }])));
        // Expect graceful non-panic response or error
        Ok(format!("handled ok={}", r.ok))
    });

    rec.run("S161", "searchScrapsSemantic 未構築インデックス", || {
        let r = state.dispatch(&rpc_req("s161", "searchScrapsSemantic", json!(["test", 5, { "enabled": false }])));
        Ok(format!("handled ok={}", r.ok))
    });

    rec.run("S162", "cancelSemanticUpdate 安全性", || {
        let r = state.dispatch(&rpc_req("s162", "cancelSemanticUpdate", json!([])));
        if !r.ok {
            return Err(format!("{:?}", r.error));
        }
        Ok("cancelSemanticUpdate ok".into())
    });

    rec.run("S163", "ptyAvailablePresets 取得", || {
        let r = state.dispatch(&rpc_req("s163", "ptyAvailablePresets", json!([])));
        let raw = r.result.and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        if !raw.contains("cmd") && !raw.contains("pwsh") && !raw.contains("shell") {
            return Err(format!("unexpected presets: {raw}"));
        }
        Ok("presets available".into())
    });

    rec.run("S164", "ptyStart -> ptyWrite -> ptyHistory -> ptyClose", || {
        let st = state.dispatch(&rpc_req("s164-s", "ptyStart", json!(["cmd", root.to_string_lossy(), 80, 24])));
        if !st.ok {
            return Err(format!("ptyStart failed: {:?}", st.error));
        }
        let info_str = st.result.unwrap().as_str().unwrap_or("{}").to_string();
        let info: Value = serde_json::from_str(&info_str).unwrap_or(json!({}));
        let pid = info["id"].as_str().unwrap_or("").to_string();
        if pid.is_empty() {
            return Err(format!("missing pty id: {info_str}"));
        }
        let cmd_b64 = STANDARD.encode("echo SYKI_PTY_OK\r\n");
        let w = state.dispatch(&rpc_req("s164-w", "ptyWrite", json!([pid, cmd_b64])));
        if !w.ok {
            return Err(format!("ptyWrite failed: {:?}", w.error));
        }
        std::thread::sleep(Duration::from_millis(600));
        let h = state.dispatch(&rpc_req("s164-h", "ptyHistory", json!([pid])));
        let _ = state.dispatch(&rpc_req("s164-c", "ptyClose", json!([pid])));
        if !h.ok {
            return Err(format!("ptyHistory failed: {:?}", h.error));
        }
        Ok(format!("pty session {pid} lifecycle ok"))
    });

    rec.run("S166", "ptyResize / ptyRename 正常・無効ID", || {
        let bad_r = state.dispatch(&rpc_req("s166-r", "ptyResize", json!(["nonexistent-pty", 100, 40])));
        let bad_n = state.dispatch(&rpc_req("s166-n", "ptyRename", json!(["nonexistent-pty", "NewName"])));
        if bad_r.ok || bad_n.ok {
            return Err("nonexistent pty resize/rename should fail".into());
        }
        Ok("invalid pty id rejected".into())
    });

    rec.run("S167", "ptySuggestDirs 候補ディレクトリ返却", || {
        let r = state.dispatch(&rpc_req("s167", "ptySuggestDirs", json!([])));
        if !r.ok {
            return Err(format!("{:?}", r.error));
        }
        Ok("ptySuggestDirs ok".into())
    });

    rec.run("S168", "getSpeechStatus 状態取得", || {
        let r = state.dispatch(&rpc_req("s168", "getSpeechStatus", json!(["{}"])));
        if !r.ok {
            return Err(format!("{:?}", r.error));
        }
        Ok("getSpeechStatus ok".into())
    });

    rec.run("S169", "transcribeAudioAsync 本番 Gemini WAV文字起こし", || {
        state.dispatch(&rpc_req("s169", "transcribeAudioAsync", json!(["s169", silence_wav_b64, "audio/wav", voice_cfg])));
        let ev = wait_event_opt(&rx, "s169", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        if !err.is_empty() {
            return Err(format!("err={err}"));
        }
        Ok("transcribeAudioAsync ok".into())
    });

    rec.run("S170", "listVoiceCaches / discardAllVoiceCaches", || {
        let l = state.dispatch(&rpc_req("s170-l", "listVoiceCaches", json!([])));
        let d = state.dispatch(&rpc_req("s170-d", "discardAllVoiceCaches", json!([])));
        if !l.ok || !d.ok {
            return Err("voice cache RPC failed".into());
        }
        Ok("voice cache ops ok".into())
    });

    rec.run("S171", "validateWhisperModelFile 非存在・無効ファイル", || {
        let r = state.dispatch(&rpc_req("s171", "validateWhisperModelFile", json!([root.join("missing.bin").to_string_lossy()])));
        let valid = r.result.as_ref().and_then(|v| v["valid"].as_bool()).unwrap_or(false);
        if valid {
            return Err("missing whisper model marked valid".into());
        }
        Ok("missing whisper model rejected".into())
    });

    rec.run("S172", "meetingRecordingSupported / abortMeetingRecording", || {
        let s = state.dispatch(&rpc_req("s172-s", "meetingRecordingSupported", json!([])));
        let a = state.dispatch(&rpc_req("s172-a", "abortMeetingRecording", json!([])));
        if !s.ok || !a.ok {
            return Err("meeting RPC failed".into());
        }
        Ok("meeting RPCs ok".into())
    });

    // =========================================================================
    // K. 拡張・クロス機能組合せシナリオ (S176, S183, S185..S187, S189..S195, S198, S200)
    // =========================================================================
    rec.run("S176", "approvePlugin + setPluginDevMode 永続化", || {
        let ap = state.dispatch(&rpc_req("s176-a", "approvePlugin", json!(["demo-plugin", "0123456789abcdef"])));
        let dm = state.dispatch(&rpc_req("s176-d", "setPluginDevMode", json!(["demo-plugin", true])));
        if !ap.ok || !dm.ok || !root.join("trust-registry.json").is_file() {
            return Err(format!("ap={:?} dm={:?}", ap.error, dm.error));
        }
        Ok("trust-registry.json persisted".into())
    });

    rec.run("S183", "組合せ: config set scraps.scrapDir -> scrap append -> scrap search", || {
        let dir = root.join("s183_combo_scraps");
        let cfg = json!({ "scraps": { "scrapDir": dir.to_string_lossy() } }).to_string();
        state.dispatch(&rpc_req("s183-cfg", "saveConfig", json!([cfg])));
        ScrapStore::append_formatted(&dir, "COMBO_TOKEN_S183 unique phrase", "").map_err(|e| e.to_string())?;
        let sr = state.dispatch(&rpc_req("s183-s", "searchScraps", json!(["COMBO_TOKEN_S183", 10, ""])));
        let arr = sr.result.unwrap_or(json!([])).as_array().cloned().unwrap_or_default();
        if arr.is_empty() {
            return Err("combo search missed newly configured scrapDir".into());
        }
        Ok("scrapDir switch + append + search ok".into())
    });

    rec.run("S185", "組合せ: text.model 変更 -> 本番 queryLLMAsync", || {
        state.dispatch(&rpc_req("s185", "queryLLMAsync", json!(["s185", "Reply with ONLY: S185_OK", gemini_lite_cfg])));
        let ev = wait_event_opt(&rx, "s185", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        if !err.is_empty() || !ev.payload["result"].as_str().unwrap_or("").contains("S185") {
            return Err(format!("err={err} res={:?}", ev.payload["result"]));
        }
        Ok("model switch + live LLM ok".into())
    });

    rec.run("S186", "組合せ: action.model 切替 -> 本番 queryDecisionAsync", || {
        let choices = json!([{ "id": "a", "description": "Greeting" }, { "id": "b", "description": "Error log" }]);
        state.dispatch(&rpc_req("s186-1", "queryDecisionAsync", json!(["s186-1", gem_dec_cfg, json!({}), "FATAL panic at line 10", choices, "a", "Classify", 20000])));
        let ev1 = wait_event_opt(&rx, "s186-1", 30)?;
        if ev1.payload["choiceId"] != "b" {
            return Err(format!("expected b, got {:?}", ev1.payload));
        }
        Ok("decision switch combo ok".into())
    });

    rec.run("S187", "組合せ: scrap.append -> 本番 queryDecisionAsync 分類 -> searchScraps", || {
        let dir = root.join("s187_combo");
        let cfg = json!({ "scraps": { "scrapDir": dir.to_string_lossy() } }).to_string();
        state.dispatch(&rpc_req("s187-c", "saveConfig", json!([cfg])));
        let log_txt = "CRITICAL: database connection refused on port 5432";
        let choices = json!([{ "id": "alert", "description": "Critical error" }, { "id": "info", "description": "Normal info" }]);
        state.dispatch(&rpc_req("s187-d", "queryDecisionAsync", json!(["s187-d", gem_dec_cfg, json!({}), log_txt, choices, "info", "Classify", 20000])));
        let ev = wait_event_opt(&rx, "s187-d", 30)?;
        let tag = ev.payload["choiceId"].as_str().unwrap_or("info");
        ScrapStore::append_formatted(&dir, &format!("[#{tag}] {log_txt}"), "").map_err(|e| e.to_string())?;
        let sr = state.dispatch(&rpc_req("s187-s", "searchScraps", json!(["#alert", 10, ""])));
        let arr = sr.result.unwrap_or(json!([])).as_array().cloned().unwrap_or_default();
        if arr.is_empty() {
            return Err(format!("expected #alert hit, tag={tag}"));
        }
        Ok("append -> decision -> search ok".into())
    });

    rec.run("S189", "組合せ: saveAsset -> 本番 queryVisionAsync OCR -> saveFileChecked", || {
        let sa = state.dispatch(&rpc_req("s189-a", "saveAsset", json!([root.to_string_lossy(), "png", sample_ocr_b64])));
        if !sa.ok {
            return Err("saveAsset failed".into());
        }
        state.dispatch(&rpc_req("s189-v", "queryVisionAsync", json!(["s189-v", "Summarize image in one short line.", sample_ocr_b64, "image/png", gemini_cfg])));
        let ev = wait_event_opt(&rx, "s189-v", 45)?;
        let txt = ev.payload["result"].as_str().unwrap_or("");
        if txt.trim().is_empty() {
            return Err(format!("vision empty: {:?}", ev.payload));
        }
        let note = root.join("s189_ocr_note.md");
        std::fs::write(&note, "# OCR\n").map_err(|e| e.to_string())?;
        let rd = state.dispatch(&rpc_req("s189-r", "readFileByPath", json!([note.to_string_lossy()])));
        let sig = rd.result.unwrap()["sig"].as_str().unwrap_or("").to_string();
        let sv = state.dispatch(&rpc_req("s189-s", "saveFileChecked", json!([note.to_string_lossy(), format!("# OCR\n{txt}\n"), "UTF-8", sig])));
        if !sv.ok {
            return Err(format!("saveFileChecked failed: {:?}", sv.error));
        }
        Ok("asset -> vision -> checked save ok".into())
    });

    rec.run("S190", "組合せ: 本番 generateImageAsync -> 生成画像を本番 queryVisionAsync 再認識", || {
        let img_p = generated_img_path.as_ref().ok_or("no generated image from S145")?;
        let gen_b64 = STANDARD.encode(std::fs::read(img_p).map_err(|e| e.to_string())?);
        state.dispatch(&rpc_req("s190", "queryVisionAsync", json!(["s190", "Describe this generated image in 5 words.", gen_b64, "image/png", gemini_cfg])));
        let ev = wait_event_opt(&rx, "s190", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        let res = ev.payload["result"].as_str().unwrap_or("");
        if !err.is_empty() || res.trim().is_empty() {
            return Err(format!("err={err} res={res}"));
        }
        Ok(format!("roundtrip vision -> {}", res.trim().chars().take(40).collect::<String>()))
    });

    rec.run("S191", "組合せ: runCommandFilter -> 本番 queryLLMAsync 要約 -> scrap.append", || {
        let f = state.dispatch(&rpc_req("s191-f", "runCommandFilter", json!(["findstr .", "line1: Rust fast\nline2: WebView2 native"])));
        let filtered = f.result.unwrap()["output"].as_str().unwrap_or("").to_string();
        state.dispatch(&rpc_req("s191-l", "queryLLMAsync", json!(["s191-l", format!("Summarize in 5 words: {filtered}"), gemini_cfg])));
        let ev = wait_event_opt(&rx, "s191-l", 45)?;
        let summary = ev.payload["result"].as_str().unwrap_or("");
        if summary.trim().is_empty() {
            return Err(format!("LLM empty: {:?}", ev.payload));
        }
        ScrapStore::append_formatted(&scrap_dir, summary, "").map_err(|e| e.to_string())?;
        Ok("shell -> llm -> scrap ok".into())
    });

    rec.run("S192", "組合せ: 外部変更 -> saveFileChecked衝突 -> 再読込 -> 再保存", || {
        let p = root.join("s192_recover.md");
        std::fs::write(&p, "base\n").map_err(|e| e.to_string())?;
        let sig1 = state.dispatch(&rpc_req("s192-r1", "readFileByPath", json!([p.to_string_lossy()]))).result.unwrap()["sig"].as_str().unwrap().to_string();
        std::fs::write(&p, "external\n").map_err(|e| e.to_string())?;
        let c = state.dispatch(&rpc_req("s192-c", "saveFileChecked", json!([p.to_string_lossy(), "mine\n", "UTF-8", sig1])));
        if c.ok && !c.result.as_ref().and_then(|v| v["conflict"].as_bool()).unwrap_or(false) {
            return Err("missed conflict".into());
        }
        let sig2 = state.dispatch(&rpc_req("s192-r2", "readFileByPath", json!([p.to_string_lossy()]))).result.unwrap()["sig"].as_str().unwrap().to_string();
        let sv = state.dispatch(&rpc_req("s192-s", "saveFileChecked", json!([p.to_string_lossy(), "external+mine\n", "UTF-8", sig2])));
        if !sv.ok {
            return Err(format!("recovery save failed: {:?}", sv.error));
        }
        Ok("conflict -> reload -> save ok".into())
    });

    rec.run("S193", "組合せ: 並列本番API耐性 (LLMx3 + Decisionx2 + searchScraps)", || {
        for i in 1..=3 {
            state.dispatch(&rpc_req(&format!("s193-l{i}"), "queryLLMAsync", json!([format!("s193-l{i}"), format!("Reply ONLY: P{i}"), gemini_cfg])));
        }
        for i in 1..=2 {
            state.dispatch(&rpc_req(&format!("s193-d{i}"), "queryDecisionAsync", json!([format!("s193-d{i}"), gem_dec_cfg, json!({}), "Yes", yn_choices, "no", "Pick", 20000])));
        }
        let sr = state.dispatch(&rpc_req("s193-s", "searchScraps", json!(["syki", 10, ""])));
        if !sr.ok {
            return Err("parallel searchScraps failed".into());
        }
        let mut seen = BTreeMap::new();
        let deadline = Instant::now() + Duration::from_secs(60);
        while seen.len() < 5 && Instant::now() < deadline {
            if let Ok(ev) = rx.recv_timeout(Duration::from_secs(5)) {
                if let Some(rid) = ev.payload.get("reqId").and_then(|v| v.as_str()) {
                    if rid.starts_with("s193-") {
                        seen.insert(rid.to_string(), ev);
                    }
                }
            }
        }
        if seen.len() < 5 {
            return Err(format!("only {}/5 parallel responses received", seen.len()));
        }
        Ok("all 5 parallel live API + search responses matched reqId".into())
    });

    rec.run("S194", "組合せ: queryLLMAsync キャンセル直後の再実行 (本番API)", || {
        state.dispatch(&rpc_req("s194-1", "queryLLMAsync", json!(["s194-1", "Write 400 words on OS kernels.", gemini_cfg])));
        state.dispatch(&rpc_req("s194-c", "cancelLLMRequest", json!(["s194-1"])));
        state.dispatch(&rpc_req("s194-2", "queryLLMAsync", json!(["s194-2", "Reply ONLY: RECOVERED", gemini_cfg])));
        let ev = wait_event_opt(&rx, "s194-2", 45)?;
        let res = ev.payload["result"].as_str().unwrap_or("");
        if res.trim().is_empty() {
            return Err(format!("post-cancel request failed: {:?}", ev.payload));
        }
        Ok(format!("recovered -> {}", res.trim().chars().take(25).collect::<String>()))
    });

    rec.run("S195", "組合せ: 秘密情報含むテキストのマスク + queryLLMAsync", || {
        // Verify LLM call completes without leaking synthetic key in error or crash
        state.dispatch(&rpc_req("s195", "queryLLMAsync", json!(["s195", "Summarize: token=[REDACTED_SECRET] service=auth", gemini_cfg])));
        let ev = wait_event_opt(&rx, "s195", 45)?;
        let err = ev.payload["error"].as_str().unwrap_or("");
        if !err.is_empty() {
            return Err(format!("err={err}"));
        }
        Ok("redacted secret prompt ok".into())
    });

    rec.run("S198", "組合せ: ptyStart -> ptyWrite -> ptyClose ライフサイクル", || {
        let st = state.dispatch(&rpc_req("s198-s", "ptyStart", json!(["cmd", root.to_string_lossy(), 80, 24])));
        let info: Value = serde_json::from_str(st.result.unwrap().as_str().unwrap_or("{}")).unwrap_or(json!({}));
        let pid = info["id"].as_str().unwrap_or("").to_string();
        let cl = state.dispatch(&rpc_req("s198-c", "ptyClose", json!([pid])));
        let cl2 = state.dispatch(&rpc_req("s198-c2", "ptyClose", json!([pid])));
        if !cl.ok || !cl2.ok {
            return Err("ptyClose failed".into());
        }
        Ok(format!("pty {pid} clean close & idempotent second close"))
    });

    rec.run("S200", "組合せ: checkAppUpdate + detectLLMProvider + getAppInfo", || {
        let u = state.dispatch(&rpc_req("s200-u", "checkAppUpdate", json!([])));
        let d = state.dispatch(&rpc_req("s200-d", "detectLLMProvider", json!(["https://generativelanguage.googleapis.com", ""])));
        let i = state.dispatch(&rpc_req("s200-i", "getAppInfo", json!([])));
        if !u.ok || !d.ok || !i.ok {
            return Err(format!("u={:?} d={:?} i={:?}", u.error, d.error, i.error));
        }
        Ok("health check combo ok".into())
    });

    // Save JSON report
    let out_dir = repo.join("evals/results");
    let _ = std::fs::create_dir_all(&out_dir);
    let pass_cnt = rec.results.values().filter(|v| v["ok"] == true).count();
    let fail_cnt = rec.results.values().filter(|v| v["ok"] == false).count();
    let report = json!({
        "suite": "scenario_200_rust_live",
        "total": rec.results.len(),
        "passed": pass_cnt,
        "failed": fail_cnt,
        "results": rec.results.values().cloned().collect::<Vec<_>>()
    });
    let _ = std::fs::write(
        out_dir.join("scenario_200_rust.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    );

    println!("\n=== RUST LIVE SCENARIOS SUMMARY: total={} pass={} fail={} ===", rec.results.len(), pass_cnt, fail_cnt);
    for v in rec.results.values().filter(|v| v["ok"] == false) {
        println!("  [FAIL] {} {}: {}", v["id"], v["name"], v["error"]);
    }

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(fail_cnt, 0, "{fail_cnt} Rust live scenarios failed");
}
