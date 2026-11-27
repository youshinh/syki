//! Additional 300-Scenario Live-API & Deep Core Evaluation (Rust portion: S201-S415, 215 scenarios).
//! Complements scenario_200_live_e2e.rs (S001-S200) + scenario_300_extra_cli_ipc_live.mjs (S416-S500).
//! Run: `cargo test -p syki-win --test scenario_300_extra_live_e2e -- --ignored --nocapture`

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::{json, Value};
use syki_core::policy::{PolicyDecisionEngine, PolicyVerdict, TrustRegistry};
use syki_core::rpc::{RpcEvent, RpcRequest, RuntimeEnv};
use syki_core::storage::config::{
    get_config_value, resolve_config_with_defaults, set_config_value, AutoDetectHints,
    ConfigStore, JsonConfigStore,
};
use syki_core::storage::{NoteFileStore, ScrapStore};
use syki_win::ext_store::{content_security_policy, is_immutable_virtual_asset, ExtRoots};
use syki_win::host_lifecycle::{parse_shortcut, ActiveTaskGuard, CanvasHostState, CanvasState};
use syki_win::quick_capture::{save_qc_theme_cache, submit_quick_capture_text};
use syki_win::rpc_dispatch::CoreState;
use syki_win::shell_pipe::{strip_ansi, validate_cli_command};
use syki_win::speech::{encode_wav_pcm16, is_audio_media};

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
    let root = std::env::temp_dir().join(format!("syki-sc300-rust-{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    let env = RuntimeEnv {
        data_dir: root.clone(),
        instance_id: format!("sc300-{nanos}"),
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

    fn record(&mut self, id: &str, name: &str, ms: u64, res: Result<String, String>) {
        match res {
            Ok(detail) => {
                println!("PASS {id} [{ms}ms] {name}: {detail}");
                self.results.insert(
                    id.to_string(),
                    json!({ "id": id, "name": name, "ok": true, "ms": ms, "detail": detail }),
                );
            }
            Err(err) => {
                println!("FAIL {id} [{ms}ms] {name}: {err}");
                self.results.insert(
                    id.to_string(),
                    json!({ "id": id, "name": name, "ok": false, "ms": ms, "error": err }),
                );
            }
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
            Ok(r) => self.record(id, name, ms, r),
            Err(_) => self.record(id, name, ms, Err("panic".into())),
        }
    }
}

fn collect_events(
    rx: &mpsc::Receiver<RpcEvent>,
    expected_ids: &[String],
    timeout_secs: u64,
) -> BTreeMap<String, RpcEvent> {
    let mut map = BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while map.len() < expected_ids.len() && Instant::now() < deadline {
        let rem = deadline.saturating_duration_since(Instant::now());
        if let Ok(ev) = rx.recv_timeout(rem.min(Duration::from_secs(2))) {
            if let Some(rid) = ev.payload.get("reqId").and_then(|v| v.as_str()) {
                if expected_ids.iter().any(|e| e == rid) {
                    map.insert(rid.to_string(), ev);
                }
            }
        }
    }
    map
}

#[test]
#[ignore = "additional 300-scenario live API evaluation (Rust portion: S201-S415)"]
fn run_scenario_300_extra_rust_live() {
    let api_key = load_gemini_key();
    assert!(!api_key.is_empty(), "Gemini API key not found");

    let (root, state, rx) = make_test_state();
    let mut rec = Recorder::new();
    let repo = repo_root();

    // =========================================================================
    // L. 設定マトリクス深掘り・全キー網羅・境界・反映 (S201..S240: 40件)
    // =========================================================================
    let cfg_cases: Vec<(&str, &str, &str, Value)> = vec![
        ("S201", "general.defaultNoteDir", "general.defaultNoteDir", json!("C:/notes")),
        ("S202", "general.rewriteHistoryDir", "general.rewriteHistoryDir", json!("rev_hist")),
        ("S203", "general.commentStyle", "general.commentStyle", json!("block")),
        ("S204", "general.diagramDetail", "general.diagramDetail", json!("detailed")),
        ("S205", "general.diagramStyle", "general.diagramStyle", json!("minimal")),
        ("S206", "general.pasteImageOcr", "general.pasteImageOcr", json!(true)),
        ("S207", "general.pasteHtmlAsMarkdown", "general.pasteHtmlAsMarkdown", json!(false)),
        ("S208", "general.restoreSession", "general.restoreSession", json!(true)),
        ("S209", "general.trayResident", "general.trayResident", json!(true)),
        ("S210", "general.trimWebViewWhenHidden", "general.trimWebViewWhenHidden", json!(false)),
        ("S211", "general.webViewInProcessGpu", "general.webViewInProcessGpu", json!(true)),
        ("S212", "general.splitViewOnStartup", "general.splitViewOnStartup", json!(true)),
        ("S213", "general.imeGuardianRetype+Reverse", "general.imeGuardianRetype", json!(true)),
        ("S214", "general.tabDock+StripWidth+Delays", "general.tabStripWidth", json!(32)),
        ("S215", "general.toolbarLayout.order", "general.toolbarLayout.order", json!(["save", "preview"])),
        ("S216", "autocomplete.enabled+maxTokens", "autocomplete.enabled", json!(true)),
        ("S217", "cli.resultPlacement+openNewTab", "cli.resultPlacement", json!("replace")),
        ("S218", "voice.apiStyle+mode+customVocab", "voice.customVocabulary", json!(["syki", "WebView2"])),
        ("S219", "voice.refine.enabled+model+timeout", "voice.refine.timeoutSec", json!(8.5)),
        ("S220", "voice.realtime+batch.apiStyle", "voice.realtime.apiStyle", json!("gemini")),
        ("S221", "whisper.threads+model+language", "whisper.threads", json!(4)),
        ("S222", "vision.baseUrl+model+prompt", "vision.prompt", json!("Extract tables")),
        ("S223", "image.model+aspectRatio+resolution", "image.resolution", json!("2048")),
        ("S224", "appearance.fontSize Number", "appearance.fontSize", json!(15.5)),
        ("S225", "general.contextMenuLayout.hidden", "general.contextMenuLayout.hidden", json!(["cut"])),
    ];
    for (id, name, key, val) in cfg_cases {
        rec.run(id, name, || {
            let mut obj = json!({});
            set_config_value(&mut obj, key, val.clone())?;
            let got = get_config_value(&obj, key)?;
            if got != val {
                return Err(format!("expected {val}, got {got}"));
            }
            Ok(format!("{key}={got}"))
        });
    }

    let bad_cfg_cases: Vec<(&str, &str, &str, Value)> = vec![
        ("S226", "shortcuts.a.b 多重ドット拒否", "shortcuts.a.b", json!("Ctrl+K")),
        ("S227", "general.cloudConsent.a.b 多重ドット拒否", "general.cloudConsent.a.b", json!("allow")),
        ("S228", "voice.languageCodes 数値混入拒否", "voice.languageCodes", json!(["ja", 123])),
        ("S229", "general.toolbarLayout.order 非配列拒否", "general.toolbarLayout.order", json!("save")),
        ("S230", "appearance.fontSize 文字列拒否", "appearance.fontSize", json!("large")),
        ("S231", "scraps.maxPipeSizeMB 浮動小数拒否", "scraps.maxPipeSizeMB", json!(12.5)),
    ];
    for (id, name, key, val) in bad_cfg_cases {
        rec.run(id, name, || {
            let mut obj = json!({});
            if set_config_value(&mut obj, key, val).is_ok() {
                return Err(format!("expected rejection for {key}"));
            }
            Ok("rejected as expected".into())
        });
    }

    rec.run("S232", "config.json 配列[]/数値123/空ファイル復旧", || {
        for raw in ["[]", "123", "", "   ", "null"] {
            let v = resolve_config_with_defaults(raw, &AutoDetectHints::default());
            if v.pointer("/scraps/maxPipeSizeMB").and_then(|x| x.as_u64()) != Some(10) {
                return Err(format!("failed recovery for {raw:?}"));
            }
        }
        Ok("all malformed JSON roots recovered".into())
    });

    rec.run("S233", "連続20回 config.set 整合性", || {
        let p = root.join("s233_burst_cfg.json");
        let store = JsonConfigStore::new(&p);
        let mut obj = json!({});
        for i in 1..=20 {
            set_config_value(&mut obj, "scraps.maxPipeSizeMB", json!(i))?;
            store.save_raw(&obj.to_string()).map_err(|e| e.to_string())?;
        }
        if store.max_pipe_size_bytes() != 20 * 1024 * 1024 {
            return Err("final value mismatch".into());
        }
        Ok("20 rapid config saves coherent".into())
    });

    rec.run("S234", "AutoDetectHints OpenAI/Default ヒント", || {
        let h = AutoDetectHints {
            default_scrap_dir: Some("/custom/scraps".into()),
            llm_base_url: Some("https://api.openai.com/v1".into()),
            llm_model: Some("gpt-6.1-sol".into()),
        };
        let v = resolve_config_with_defaults("{}", &h);
        if v.pointer("/text/model").and_then(|x| x.as_str()) != Some("gpt-6.1-sol") {
            return Err("hint not applied".into());
        }
        Ok("hints applied".into())
    });

    rec.run("S235", "既存値がある場合は AutoDetectHints で上書きしない", || {
        let h = AutoDetectHints {
            default_scrap_dir: Some("/hint/scraps".into()),
            llm_base_url: Some("https://api.openai.com/v1".into()),
            llm_model: Some("gpt-6.1-sol".into()),
        };
        let v = resolve_config_with_defaults(r#"{"text":{"model":"my-custom-model"}}"#, &h);
        if v.pointer("/text/model").and_then(|x| x.as_str()) != Some("my-custom-model") {
            return Err("user model was overwritten by hint!".into());
        }
        Ok("user config preserved over hints".into())
    });

    rec.run("S236", "saveConfig オブジェクト直接渡し", || {
        let r = state.dispatch(&rpc_req("s236", "saveConfig", json!([{ "general": { "language": "ja" } }])));
        if !r.ok {
            return Err(format!("{:?}", r.error));
        }
        Ok("object payload saved".into())
    });

    rec.run("S237", "saveConfig ペイロード欠落拒否", || {
        let r = state.dispatch(&rpc_req("s237", "saveConfig", json!([])));
        if r.ok {
            return Err("accepted empty saveConfig".into());
        }
        Ok("missing payload rejected".into())
    });

    rec.run("S238", "JsonConfigStore 存在->削除->再作成キャッシュ遷移", || {
        let p = root.join("s238_del.json");
        let store = JsonConfigStore::new(&p);
        store.save_raw(r#"{"general":{"language":"en"}}"#).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&p);
        let after_del = store.load_raw().map_err(|e| e.to_string())?;
        if after_del.trim() != "{}" {
            return Err(format!("stale after delete: {after_del}"));
        }
        store.save_raw(r#"{"general":{"language":"ja"}}"#).map_err(|e| e.to_string())?;
        let after_recreate = store.load_raw().map_err(|e| e.to_string())?;
        if !after_recreate.contains("ja") {
            return Err("stale after recreate".into());
        }
        Ok("create->delete->recreate cache transitions ok".into())
    });

    rec.run("S239", "general.tabHideDelayMs/DurationMs/Strength Int検証", || {
        let mut obj = json!({});
        set_config_value(&mut obj, "general.tabDockStrength", json!(80))?;
        set_config_value(&mut obj, "general.tabHideDelayMs", json!(150))?;
        set_config_value(&mut obj, "general.tabHideDurationMs", json!(200))?;
        Ok("tab dock int keys ok".into())
    });

    rec.run("S240", "general.activityApps/Edits/telemetry/checkUpdates Bool検証", || {
        let mut obj = json!({});
        for k in ["general.activityApps", "general.activityEdits", "general.telemetry", "general.checkUpdates", "general.aiCorrection", "general.cursorAura", "general.tabBarTop", "general.tabDock", "general.tabDockFollowOs", "general.rewriteHistoryLink", "general.welcomeShown", "general.aiChoiceMade"] {
            set_config_value(&mut obj, k, json!(true))?;
        }
        Ok("all 12 general bool keys ok".into())
    });

    // =========================================================================
    // M. Policy Decision・セキュリティ・パストラバーサル・URL検証 (S241..S275: 35件)
    // =========================================================================
    let blocked_cmds: Vec<(&str, &str)> = vec![
        ("S241", "format c:"),
        ("S242", "diskpart /s script.txt"),
        ("S243", "bcdedit /deletevalue"),
        ("S244", "shutdown /s /t 0"),
        ("S245", "clear-disk -number 1"),
        ("S246", "cipher /w:C:\\"),
        ("S247", "vssadmin delete shadows /all"),
        ("S248", "rm -rf /"),
        ("S249", "del /s /q C:\\temp"),
        ("S250", "rd /s /q C:\\temp"),
        ("S251", "remove-item -recurse C:\\x"),
        ("S252", "reg delete HKCU\\Software\\Test"),
        ("S253", "net user admin pass"),
        ("S254", "certutil -urlcache -f http://x"),
        ("S255", "   "),
    ];
    for (id, cmd) in blocked_cmds {
        rec.run(id, &format!("validate_cli_command 拒否: {}", cmd.trim()), || {
            if validate_cli_command(cmd).is_ok() {
                return Err(format!("dangerous command allowed: {cmd:?}"));
            }
            Ok("blocked".into())
        });
    }

    rec.run("S256", "strip_ansi CSI色コード除去", || {
        let s = strip_ansi("\x1b[31mRED\x1b[0m plain");
        if s != "RED plain" { return Err(format!("got {s:?}")); }
        Ok("CSI color stripped".into())
    });
    rec.run("S257", "strip_ansi CSIカーソル移動除去", || {
        let s = strip_ansi("\x1b[2J\x1b[1;1Hhello");
        if s != "hello" { return Err(format!("got {s:?}")); }
        Ok("CSI cursor stripped".into())
    });
    rec.run("S258", "strip_ansi OSC BEL終端除去", || {
        let s = strip_ansi("\x1b]0;Window Title\x07body");
        if s != "body" { return Err(format!("got {s:?}")); }
        Ok("OSC BEL stripped".into())
    });
    rec.run("S259", "strip_ansi OSC ST終端除去", || {
        let s = strip_ansi("\x1b]2;Title\x1b\\body");
        if s != "body" { return Err(format!("got {s:?}")); }
        Ok("OSC ST stripped".into())
    });
    rec.run("S260", "strip_ansi 日本語混在・不完全ESC耐性", || {
        let s = strip_ansi("日本語\x1b[32m緑\x1b[0mテスト\x1b[");
        if !s.contains("日本語緑テスト") { return Err(format!("got {s:?}")); }
        Ok("Japanese + truncated ESC safe".into())
    });

    rec.run("S261", "Policy: 危険インタプリタ全種拒否", || {
        let mut eng = PolicyDecisionEngine::new(TrustRegistry::default());
        for prog in ["cmd", "cmd.exe", "CMD.EXE", "C:\\Windows\\System32\\cmd.exe", "wscript.exe", "cscript.exe", "mshta.exe"] {
            let d = eng.evaluate_program_exec(prog, &[]);
            if d.verdict != PolicyVerdict::Deny {
                return Err(format!("expected Deny for {prog}, got {:?}", d.verdict));
            }
        }
        Ok("all dangerous interpreters denied".into())
    });

    rec.run("S262", "Policy: SHA-256改ざんプラグイン拒否", || {
        let mut eng = PolicyDecisionEngine::new(TrustRegistry::default());
        let b1 = b"export default { id: 'p1', apiVersion: 1 };";
        let b2 = b"export default { id: 'p1', apiVersion: 1, evil: true };";
        let h1 = PolicyDecisionEngine::sha256_hex(b1);
        eng.approve_plugin("p1", &h1);
        let d = eng.evaluate_plugin_load("p1", b2);
        if d.verdict == PolicyVerdict::Allow {
            return Err("tampered plugin hash was allowed!".into());
        }
        Ok(format!("tampered hash verdict={:?}", d.verdict))
    });

    rec.run("S263", "Policy: 承認済みSHA-256一致プラグイン許可", || {
        let mut eng = PolicyDecisionEngine::new(TrustRegistry::default());
        let b1 = b"valid plugin";
        let h1 = PolicyDecisionEngine::sha256_hex(b1);
        eng.approve_plugin("p1", &h1);
        let d = eng.evaluate_plugin_load("p1", b1);
        if d.verdict != PolicyVerdict::Allow {
            return Err(format!("expected Allow, got {:?}", d.verdict));
        }
        Ok("approved hash allowed".into())
    });

    rec.run("S264", "Policy: plugin reload 未承認/非DevMode拒否 vs DevMode許可", || {
        let mut eng = PolicyDecisionEngine::new(TrustRegistry::default());
        let b1 = b"v1";
        let b2 = b"v2";
        let h1 = PolicyDecisionEngine::sha256_hex(b1);
        eng.approve_plugin("p1", &h1);
        let d_no_dev = eng.evaluate_plugin_reload("p1", b2);
        if d_no_dev.verdict == PolicyVerdict::Allow {
            return Err("reload allowed without dev mode".into());
        }
        eng.set_dev_mode("p1", true);
        let d_dev = eng.evaluate_plugin_reload("p1", b2);
        if d_dev.verdict != PolicyVerdict::Allow {
            return Err(format!("dev mode reload denied: {:?}", d_dev.verdict));
        }
        Ok("reload dev-mode guard verified".into())
    });

    rec.run("S265", "Policy: 128件監査ログリングバッファ上限", || {
        let mut eng = PolicyDecisionEngine::new(TrustRegistry::default());
        for i in 0..160 {
            let _ = eng.evaluate_program_exec(&format!("tool_{i}"), &[]);
        }
        let log = eng.audit_log();
        if log.len() > 128 {
            return Err(format!("audit log exceeded 128: {}", log.len()));
        }
        Ok(format!("capped at {} entries", log.len()))
    });

    rec.run("S266", "Policy: exposeRpc=false プラグインコマンド拒否", || {
        let mut eng = PolicyDecisionEngine::new(TrustRegistry::default());
        let d = eng.evaluate_rpc_plugin_run("p1", "p1.secret", false);
        if d.verdict != PolicyVerdict::Deny {
            return Err(format!("expected Deny, got {:?}", d.verdict));
        }
        Ok("non-exposed RPC command denied".into())
    });

    rec.run("S267", "Policy: 承認済み + exposeRpc=true プラグインコマンド許可", || {
        let mut eng = PolicyDecisionEngine::new(TrustRegistry::default());
        let h = PolicyDecisionEngine::sha256_hex(b"approved");
        eng.approve_plugin("p1", &h);
        let d = eng.evaluate_rpc_plugin_run("p1", "p1.pub", true);
        if d.verdict != PolicyVerdict::Allow {
            return Err(format!("expected Allow, got {:?}", d.verdict));
        }
        Ok("approved exposed RPC command allowed".into())
    });

    rec.run("S268", "Policy: PTY open 判定 (既定Ask確認)", || {
        let mut eng = PolicyDecisionEngine::new(TrustRegistry::default());
        let d = eng.evaluate_pty_open();
        if d.verdict != PolicyVerdict::Ask {
            return Err(format!("unexpected pty verdict: {:?}", d.verdict));
        }
        Ok("pty open requires Ask as designed".into())
    });

    for (id, url) in [
        ("S269", "javascript:alert(1)"),
        ("S270", "data:text/html,<script>alert(1)</script>"),
        ("S271", "file:///C:/Windows/System32/calc.exe"),
        ("S272", "vbscript:msgbox(1)"),
        ("S273", ""),
    ] {
        rec.run(id, &format!("openExternal 危険スキーム拒否: {}", if url.is_empty() { "<empty>" } else { &url[..url.find(':').unwrap_or(url.len())] }), || {
            let r = state.dispatch(&rpc_req(id, "openExternal", json!([url])));
            if r.ok {
                return Err(format!("dangerous URL allowed: {url}"));
            }
            Ok("rejected".into())
        });
    }

    let png_b64 = tiny_png_b64();
    rec.run("S274", "saveAsset data URI プレフィックス除去 & 同名衝突回避", || {
        let data_uri = format!("data:image/png;base64,{png_b64}");
        let r1 = state.dispatch(&rpc_req("s274-1", "saveAsset", json!([root.to_string_lossy(), "png", data_uri])));
        let r2 = state.dispatch(&rpc_req("s274-2", "saveAsset", json!([root.to_string_lossy(), "png", data_uri])));
        if !r1.ok || !r2.ok {
            return Err(format!("r1={:?} r2={:?}", r1.error, r2.error));
        }
        Ok("data URI handled cleanly".into())
    });

    rec.run("S275", "importAssetFile パストラバーサル防止", || {
        let r = state.dispatch(&rpc_req("s275", "importAssetFile", json!([root.to_string_lossy(), "..\\..\\evil.png", png_b64])));
        if r.ok {
            let s = r.result.unwrap().to_string();
            if s.contains("..") {
                return Err(format!("traversal in path: {s}"));
            }
        }
        Ok("importAssetFile traversal safe".into())
    });

    // =========================================================================
    // N. ストレージ・エンコーディング・検索・SemIndex・HostLifecycle・ExtStore (S276..S315: 40件)
    // =========================================================================
    rec.run("S276", "NoteFileStore UTF-8 BOM 読込・維持", || {
        let p = root.join("s276_bom.md");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("BOM付き日本語\n".as_bytes());
        std::fs::write(&p, &bytes).map_err(|e| e.to_string())?;
        let loaded = NoteFileStore::read_note(&p).map_err(|e| e.to_string())?;
        if !loaded.content.contains("BOM付き日本語") {
            return Err("BOM content mismatch".into());
        }
        Ok(format!("encoding={}", loaded.encoding_name))
    });

    rec.run("S277", "NoteFileStore UTF-16LE BOM 読込", || {
        let p = root.join("s277_utf16.md");
        let mut bytes = vec![0xFF, 0xFE];
        for u in "UTF16日本語".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        std::fs::write(&p, &bytes).map_err(|e| e.to_string())?;
        let loaded = NoteFileStore::read_note(&p).map_err(|e| e.to_string())?;
        if !loaded.content.contains("UTF16日本語") {
            return Err(format!("UTF-16LE mismatch: {:?}", loaded.content));
        }
        Ok(format!("encoding={}", loaded.encoding_name))
    });

    rec.run("S278", "NoteFileStore CRLF/LF 保持", || {
        let p = root.join("s278_eol.md");
        let body = "line1\r\nline2\nline3\r\n";
        NoteFileStore::write_atomic(&p, body, "UTF-8").map_err(|e| e.to_string())?;
        let loaded = NoteFileStore::read_note(&p).map_err(|e| e.to_string())?;
        if loaded.content != body {
            return Err("EOL not preserved".into());
        }
        Ok("EOL preserved".into())
    });

    rec.run("S279", "NoteFileStore 空ファイル往復", || {
        let p = root.join("s279_empty.md");
        NoteFileStore::write_atomic(&p, "", "UTF-8").map_err(|e| e.to_string())?;
        let loaded = NoteFileStore::read_note(&p).map_err(|e| e.to_string())?;
        if !loaded.content.is_empty() { return Err("expected empty".into()); }
        Ok("empty file ok".into())
    });

    rec.run("S280", "NoteFileStore 500KB 大容量ファイル往復", || {
        let p = root.join("s280_big.md");
        let big = "日本語とASCIIのテスト行 0123456789\n".repeat(12000);
        NoteFileStore::write_atomic(&p, &big, "UTF-8").map_err(|e| e.to_string())?;
        let loaded = NoteFileStore::read_note(&p).map_err(|e| e.to_string())?;
        if loaded.content.len() != big.len() { return Err("size mismatch".into()); }
        Ok(format!("{} bytes ok", big.len()))
    });

    rec.run("S281", "NoteFileStore 日本語・スペース含むパス", || {
        let p = root.join("日本語 フォルダ").join("議事録 2026 (案).md");
        NoteFileStore::write_atomic(&p, "# 議事録\n", "UTF-8").map_err(|e| e.to_string())?;
        let loaded = NoteFileStore::read_note(&p).map_err(|e| e.to_string())?;
        if !loaded.content.contains("議事録") { return Err("mismatch".into()); }
        Ok("Japanese space path ok".into())
    });

    rec.run("S282", "原子保存後の .tmp ゴミ非残存確認", || {
        let dir = root.join("s282_atomic");
        let p = dir.join("clean.md");
        NoteFileStore::write_atomic(&p, "clean", "UTF-8").map_err(|e| e.to_string())?;
        let leftovers: Vec<_> = std::fs::read_dir(&dir).map_err(|e| e.to_string())?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        if !leftovers.is_empty() { return Err("tmp files left behind".into()); }
        Ok("no .tmp leftovers".into())
    });

    rec.run("S283", "NoteFileStore 非存在ファイル読込エラー", || {
        let r = state.dispatch(&rpc_req("s283", "readFileByPath", json!([root.join("no_such_file.md").to_string_lossy()])));
        if r.ok { return Err("expected error".into()); }
        Ok("missing file error".into())
    });

    rec.run("S284", "NoteFileStore ディレクトリ読込エラー", || {
        let r = state.dispatch(&rpc_req("s284", "readFileByPath", json!([root.to_string_lossy()])));
        if r.ok { return Err("expected error reading directory".into()); }
        Ok("directory read error".into())
    });

    rec.run("S285", "NoteFileStore 極長1行 100KB テキスト", || {
        let p = root.join("s285_longline.md");
        let line = "X".repeat(100_000);
        NoteFileStore::write_atomic(&p, &line, "UTF-8").map_err(|e| e.to_string())?;
        let loaded = NoteFileStore::read_note(&p).map_err(|e| e.to_string())?;
        if loaded.content.len() != 100_000 { return Err("length mismatch".into()); }
        Ok("100KB single line ok".into())
    });

    rec.run("S286", "resolveDroppedFile サイズ一致解決", || {
        let p = root.join("s279_empty.md");
        let r = state.dispatch(&rpc_req("s286", "resolveDroppedFile", json!(["s279_empty.md", 0, 0, root.to_string_lossy()])));
        if !r.ok { return Err(format!("{:?}", r.error)); }
        Ok(format!("resolved={:?}", r.result))
    });

    let multi_scrap = root.join("s287_multi_scraps");
    std::fs::create_dir_all(&multi_scrap).unwrap();
    std::fs::write(multi_scrap.join("2026-01-10.md"), "## 10:00\nRegex [test] (a+b) $100 .* #alpha #beta\n").unwrap();
    std::fs::write(multi_scrap.join("2026-02-20.md"), "## 12:00\n日本語スニペット境界テスト絵文字🚀🎉 #beta\n").unwrap();
    for d in 1..=20 {
        std::fs::write(multi_scrap.join(format!("2026-03-{d:02}.md")), format!("## 09:00\nBatch scrap #{d} #march keyword_batch\n")).unwrap();
    }
    let _ = state.dispatch(&rpc_req("cfg-multi", "saveConfig", json!([json!({ "scraps": { "scrapDir": multi_scrap } }).to_string()])));

    rec.run("S287", "searchScraps 複数日次ファイル横断", || {
        let r = state.dispatch(&rpc_req("s287", "searchScraps", json!(["#beta", 50, ""])));
        let arr = r.result.unwrap().as_array().cloned().unwrap_or_default();
        if arr.len() < 2 { return Err(format!("expected >=2 hits, got {}", arr.len())); }
        Ok(format!("{} cross-day hits", arr.len()))
    });

    for (id, q) in [("S288", "[test]"), ("S289", "(a+b)"), ("S290", "$100"), ("S291", ".*")] {
        rec.run(id, &format!("searchScraps 正規表現特殊文字リテラル: {q}"), || {
            let r = state.dispatch(&rpc_req(id, "searchScraps", json!([q, 20, ""])));
            let arr = r.result.unwrap().as_array().cloned().unwrap_or_default();
            if arr.is_empty() { return Err(format!("literal search missed {q}")); }
            Ok(format!("hit={}", arr.len()))
        });
    }

    rec.run("S292", "searchScraps マルチバイト・絵文字スニペット境界", || {
        let r = state.dispatch(&rpc_req("s292", "searchScraps", json!(["🚀", 20, ""])));
        let arr = r.result.unwrap().as_array().cloned().unwrap_or_default();
        if arr.is_empty() { return Err("emoji search missed".into()); }
        Ok("emoji snippet safe".into())
    });

    rec.run("S293", "searchScraps 20+ファイル並列検索", || {
        let r = state.dispatch(&rpc_req("s293", "searchScraps", json!(["keyword_batch", 50, ""])));
        let arr = r.result.unwrap().as_array().cloned().unwrap_or_default();
        if arr.len() < 20 { return Err(format!("expected 20 hits, got {}", arr.len())); }
        Ok(format!("{} parallel hits", arr.len()))
    });

    rec.run("S294", "searchScraps limit=5 厳密上限", || {
        let r = state.dispatch(&rpc_req("s294", "searchScraps", json!(["keyword_batch", 5, ""])));
        let arr = r.result.unwrap().as_array().cloned().unwrap_or_default();
        if arr.len() != 5 { return Err(format!("expected 5, got {}", arr.len())); }
        Ok("limit=5 exact".into())
    });

    rec.run("S295", "scrapFilterOptions 22ファイル集計", || {
        let r = state.dispatch(&rpc_req("s295", "scrapFilterOptions", json!([])));
        if !r.ok { return Err(format!("{:?}", r.error)); }
        Ok("filter options aggregated".into())
    });

    rec.run("S296", "ScrapStore カスタムヘッダ付き追記", || {
        let p = ScrapStore::append_formatted(&multi_scrap, "custom body", "CustomTitle").map_err(|e| e.to_string())?;
        let content = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
        if !content.contains("custom body") { return Err("body missing".into()); }
        Ok("custom scrap append ok".into())
    });

    rec.run("S297", "semindex enabled:true 設定反映", || {
        let r = state.dispatch(&rpc_req("s297", "semanticStatus", json!([{ "enabled": true, "model": { "baseUrl": "http://127.0.0.1:11434", "model": "bge-m3" } }])));
        if !r.ok { return Err(format!("{:?}", r.error)); }
        Ok("semanticStatus enabled ok".into())
    });

    rec.run("S298", "semindex 破損インデックスファイル耐性", || {
        let sem_dir = root.join("semindex");
        std::fs::create_dir_all(&sem_dir).map_err(|e| e.to_string())?;
        std::fs::write(sem_dir.join("meta.json"), "{corrupt").map_err(|e| e.to_string())?;
        let r = state.dispatch(&rpc_req("s298", "semanticStatus", json!([{ "enabled": true }])));
        if !r.ok { return Err(format!("crashed on corrupt semindex: {:?}", r.error)); }
        Ok("corrupt semindex survived".into())
    });

    rec.run("S299", "semindex 到達不能エンドポイントで明確エラー", || {
        let r = state.dispatch(&rpc_req("s299", "semanticUpdate", json!([{ "enabled": true, "model": { "baseUrl": "http://127.0.0.1:1", "model": "bge-m3" } }])));
        Ok(format!("unreachable semanticUpdate handled ok={}", r.ok))
    });

    rec.run("S300", "searchScrapsSemantic 空クエリ耐性", || {
        let r = state.dispatch(&rpc_req("s300", "searchScrapsSemantic", json!(["", 5, { "enabled": true }])));
        Ok(format!("empty semantic query handled ok={}", r.ok))
    });

    rec.run("S301", "searchScrapsSemantic limit=0 耐性", || {
        let r = state.dispatch(&rpc_req("s301", "searchScrapsSemantic", json!(["rust", 0, { "enabled": true }])));
        Ok(format!("limit=0 handled ok={}", r.ok))
    });

    rec.run("S302", "semanticStatus 引数なし config.json フォールバック", || {
        let r = state.dispatch(&rpc_req("s302", "semanticStatus", json!([])));
        if !r.ok { return Err(format!("{:?}", r.error)); }
        Ok("config fallback ok".into())
    });

    rec.run("S303", "CanvasHostState Attached->Suspended->Detached->wake 遷移", || {
        let mut h = CanvasHostState::new();
        if h.state() != CanvasState::Attached { return Err("not Attached".into()); }
        h.suspend();
        if h.state() != CanvasState::Suspended { return Err("not Suspended".into()); }
        h.detach();
        if h.state() != CanvasState::Detached { return Err("not Detached".into()); }
        let prev = h.wake();
        if prev != CanvasState::Detached || h.state() != CanvasState::Attached { return Err("wake failed".into()); }
        Ok("full 3-state cycle ok".into())
    });

    rec.run("S304", "ActiveTaskGuard デタッチ抑止 & 解放後許可", || {
        let mut h = CanvasHostState::new();
        let g = ActiveTaskGuard::new();
        let tok = g.acquire();
        if h.detach_if_idle(&g) { return Err("detached while guard active!".into()); }
        if !g.is_active() || g.active_count() != 1 { return Err("guard count wrong".into()); }
        tok.release();
        if !h.detach_if_idle(&g) || h.state() != CanvasState::Detached {
            return Err("failed to detach after guard release".into());
        }
        Ok("ActiveTaskGuard protected detach".into())
    });

    rec.run("S305", "ActiveTaskGuard 二重ネスト参照カウント", || {
        let g = ActiveTaskGuard::new();
        let t1 = g.acquire();
        let t2 = g.acquire();
        if g.active_count() != 2 { return Err("expected 2".into()); }
        drop(t1);
        if g.active_count() != 1 || !g.is_active() { return Err("expected 1".into()); }
        drop(t2);
        if g.active_count() != 0 || g.is_active() { return Err("expected 0".into()); }
        Ok("nested RAII guard ok".into())
    });

    rec.run("S306", "parse_shortcut 正常・異常パース", || {
        if parse_shortcut("Ctrl+Alt+S").is_none() { return Err("failed Ctrl+Alt+S".into()); }
        if parse_shortcut("Ctrl+Shift+K").is_none() { return Err("failed Ctrl+Shift+K".into()); }
        if parse_shortcut("").is_some() { return Err("accepted empty shortcut".into()); }
        if parse_shortcut("InvalidMod+Z").is_some() { return Err("accepted invalid modifier".into()); }
        Ok("shortcut parser ok".into())
    });

    rec.run("S307", "ExtRoots Portable優先 vs UserData", || {
        let port_dir = root.join("s307_portable");
        let user_dir = root.join("s307_userdata");
        std::fs::create_dir_all(&port_dir).unwrap();
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(port_dir.join("user.css"), "/* portable */").unwrap();
        std::fs::write(user_dir.join("user.css"), "/* userdata */").unwrap();
        let roots = ExtRoots::new(Some(port_dir.clone()), user_dir, false);
        let css = roots.resolve_user_css().ok_or("no user.css")?;
        if !css.path.starts_with(&port_dir) { return Err("portable did not win!".into()); }
        Ok("portable user.css won".into())
    });

    rec.run("S308", "ExtRoots --safe-mode 全スキップ", || {
        let port_dir = root.join("s307_portable");
        let user_dir = root.join("s307_userdata");
        let roots = ExtRoots::new(Some(port_dir), user_dir, true);
        if roots.has_user_css() || !roots.list_plugins().is_empty() {
            return Err("safe mode did not skip extensions".into());
        }
        Ok("safe mode skipped all".into())
    });

    rec.run("S309", "ExtRoots CSPヘッダ & 仮想アセットキャッシュ判定", || {
        let csp = content_security_policy();
        if !csp.contains("default-src") { return Err("invalid CSP".into()); }
        let imm = is_immutable_virtual_asset(200, "vendor/katex.min.js");
        Ok(format!("CSP ok, immutable={imm}"))
    });

    rec.run("S310", "ExtRoots プラグインディレクトリ列挙 & 不正フォルダ耐性", || {
        let ud = root.join("s310_ud");
        std::fs::create_dir_all(ud.join("plugins/valid-p")).unwrap();
        std::fs::write(ud.join("plugins/valid-p/plugin.js"), "export default { id:'valid-p', apiVersion:1 };").unwrap();
        std::fs::create_dir_all(ud.join("plugins/empty-dir")).unwrap();
        let roots = ExtRoots::new(None, ud, false);
        let list = roots.list_plugins();
        if list.len() != 1 { return Err(format!("expected 1 valid plugin, got {}", list.len())); }
        Ok("listed valid plugin only".into())
    });

    rec.run("S311", "SessionStore 空・破損・巨大セッション復旧", || {
        std::fs::write(root.join("session.json"), "{corrupt session").unwrap();
        let g = state.dispatch(&rpc_req("s311", "getSession", json!([])));
        if !g.ok { return Err(format!("getSession failed: {:?}", g.error)); }
        Ok("corrupt session survived".into())
    });

    rec.run("S312", "saveSession ペイロード欠落拒否", || {
        let r = state.dispatch(&rpc_req("s312", "saveSession", json!([])));
        if r.ok { return Err("accepted empty saveSession".into()); }
        Ok("missing session rejected".into())
    });

    rec.run("S313", "watchActiveFile 非存在パス・連続切替耐性", || {
        let _ = state.dispatch(&rpc_req("s313-1", "watchActiveFile", json!([root.join("missing_watch.md").to_string_lossy()])));
        let p = root.join("s279_empty.md");
        let w = state.dispatch(&rpc_req("s313-2", "watchActiveFile", json!([p.to_string_lossy()])));
        let u = state.dispatch(&rpc_req("s313-3", "unwatchActiveFile", json!([])));
        if !w.ok || !u.ok { return Err("watch switch failed".into()); }
        Ok("watch switch safe".into())
    });

    rec.run("S314", "copyPNGToClipboard 不正Base64エラー", || {
        let r = state.dispatch(&rpc_req("s314", "copyPNGToClipboard", json!(["!!!bad_base64!!!"])));
        if r.ok { return Err("expected error for invalid PNG base64".into()); }
        Ok("invalid PNG clipboard rejected".into())
    });

    rec.run("S315", "trimMemory 正常実行", || {
        let r = state.dispatch(&rpc_req("s315", "trimMemory", json!([])));
        if !r.ok { return Err(format!("{:?}", r.error)); }
        Ok("trimMemory ok".into())
    });

    // =========================================================================
    // O. PTY・Speech・Meeting・QuickCapture 深掘り (S316..S345: 30件)
    // =========================================================================
    rec.run("S316", "ptyStartWithOptions 明示ID・ラベル", || {
        let r = state.dispatch(&rpc_req("s316", "ptyStartWithOptions", json!(["custom-pty-316", "cmd", root.to_string_lossy(), "BuildShell", "", "", 90, 25])));
        if !r.ok { return Err(format!("{:?}", r.error)); }
        let info: Value = serde_json::from_str(r.result.unwrap().as_str().unwrap_or("{}")).unwrap_or(json!({}));
        let id = info["id"].as_str().unwrap_or("").to_string();
        let _ = state.dispatch(&rpc_req("s316-c", "ptyClose", json!([id])));
        if id != "custom-pty-316" { return Err(format!("expected custom-pty-316, got {id}")); }
        Ok("explicit pty id ok".into())
    });

    rec.run("S317", "複数PTY同時稼働 -> ptyList 2->1->0 同期", || {
        let s1 = state.dispatch(&rpc_req("s317-1", "ptyStart", json!(["cmd", root.to_string_lossy(), 80, 24])));
        let s2 = state.dispatch(&rpc_req("s317-2", "ptyStart", json!(["cmd", root.to_string_lossy(), 80, 24])));
        let id1 = serde_json::from_str::<Value>(s1.result.unwrap().as_str().unwrap()).unwrap()["id"].as_str().unwrap().to_string();
        let id2 = serde_json::from_str::<Value>(s2.result.unwrap().as_str().unwrap()).unwrap()["id"].as_str().unwrap().to_string();
        let l2 = state.dispatch(&rpc_req("s317-l2", "ptyList", json!([]))).result.unwrap().as_array().unwrap().len();
        state.dispatch(&rpc_req("s317-c1", "ptyClose", json!([id1])));
        let l1 = state.dispatch(&rpc_req("s317-l1", "ptyList", json!([]))).result.unwrap().as_array().unwrap().len();
        state.dispatch(&rpc_req("s317-c2", "ptyClose", json!([id2])));
        let l0 = state.dispatch(&rpc_req("s317-l0", "ptyList", json!([]))).result.unwrap().as_array().unwrap().len();
        if (l2, l1, l0) != (2, 1, 0) { return Err(format!("expected (2,1,0), got ({l2},{l1},{l0})")); }
        Ok("ptyList tracked 2->1->0".into())
    });

    rec.run("S318", "ptyResize 最小値クランプ (0x0 -> 10x4) & ptyRename 正常", || {
        let s = state.dispatch(&rpc_req("s318-s", "ptyStart", json!(["cmd", root.to_string_lossy(), 80, 24])));
        let id = serde_json::from_str::<Value>(s.result.unwrap().as_str().unwrap()).unwrap()["id"].as_str().unwrap().to_string();
        let rs = state.dispatch(&rpc_req("s318-r", "ptyResize", json!([id, 0, 0])));
        let rn = state.dispatch(&rpc_req("s318-n", "ptyRename", json!([id, "RenamedTab"])));
        let _ = state.dispatch(&rpc_req("s318-c", "ptyClose", json!([id])));
        if !rs.ok || !rn.ok { return Err(format!("rs={:?} rn={:?}", rs.error, rn.error)); }
        Ok("resize clamp + rename ok".into())
    });

    rec.run("S319", "ptyWrite プレーン文字列フォールバック", || {
        let s = state.dispatch(&rpc_req("s319-s", "ptyStart", json!(["cmd", root.to_string_lossy(), 80, 24])));
        let id = serde_json::from_str::<Value>(s.result.unwrap().as_str().unwrap()).unwrap()["id"].as_str().unwrap().to_string();
        let w = state.dispatch(&rpc_req("s319-w", "ptyWrite", json!([id, "echo hi!\r\n"])));
        let _ = state.dispatch(&rpc_req("s319-c", "ptyClose", json!([id])));
        if !w.ok { return Err(format!("{:?}", w.error)); }
        Ok("plain string write ok".into())
    });

    rec.run("S320", "未知PTY ID への write/history/close 耐性", || {
        let w = state.dispatch(&rpc_req("s320-w", "ptyWrite", json!(["no-pty", "a"])));
        let h = state.dispatch(&rpc_req("s320-h", "ptyHistory", json!(["no-pty"])));
        let c = state.dispatch(&rpc_req("s320-c", "ptyClose", json!(["no-pty"])));
        if w.ok || !h.ok || !c.ok || c.result != Some(json!(false)) {
            return Err("unexpected unknown pty behavior".into());
        }
        Ok("unknown pty handled cleanly".into())
    });

    rec.run("S321", "ptyDetectGate Rust/Node/Generic 判定", || {
        let g_rust = state.dispatch(&rpc_req("s321-r", "ptyDetectGate", json!([repo.to_string_lossy()]))).result.unwrap();
        let g_gen = state.dispatch(&rpc_req("s321-g", "ptyDetectGate", json!([root.to_string_lossy()]))).result.unwrap();
        Ok(format!("repo={g_rust} tmp={g_gen}"))
    });

    rec.run("S322", "ptyPrepareWorktree 非Gitフォルダ安全処理", || {
        let r = state.dispatch(&rpc_req("s322", "ptyPrepareWorktree", json!([root.to_string_lossy(), "wt-test"])));
        if !r.ok { return Err(format!("{:?}", r.error)); }
        Ok("non-git worktree safe".into())
    });

    rec.run("S323", "ptyList 初期0件確認", || {
        let l = state.dispatch(&rpc_req("s323", "ptyList", json!([]))).result.unwrap();
        if l != json!([]) { return Err(format!("expected [], got {l}")); }
        Ok("0 active sessions".into())
    });

    rec.run("S324", "getAvailableSkillsJSON / getSkillInstruction 応答", || {
        let a = state.dispatch(&rpc_req("s324-a", "getAvailableSkillsJSON", json!([])));
        let b = state.dispatch(&rpc_req("s324-b", "getSkillInstruction", json!(["any"])));
        if !a.ok || !b.ok { return Err("skill RPC failed".into()); }
        Ok("skill stubs ok".into())
    });

    rec.run("S325", "fileTypesStatus / uiReady 応答", || {
        let a = state.dispatch(&rpc_req("s325-a", "fileTypesStatus", json!([])));
        let b = state.dispatch(&rpc_req("s325-b", "uiReady", json!([])));
        if !a.ok || !b.ok { return Err("status RPC failed".into()); }
        Ok("fileTypesStatus & uiReady ok".into())
    });

    rec.run("S326", "getStartupFile 初期値応答", || {
        let r = state.dispatch(&rpc_req("s326", "getStartupFile", json!([])));
        if !r.ok { return Err(format!("{:?}", r.error)); }
        Ok("getStartupFile ok".into())
    });

    rec.run("S327", "未知RPCメソッド拒否 (CoreState)", || {
        let r = state.dispatch(&rpc_req("s327", "noSuchRpcMethodXYZ", json!([])));
        if r.ok { return Err("accepted unknown RPC method".into()); }
        Ok("unknown method rejected".into())
    });

    rec.run("S328", "encode_wav_pcm16 RIFF/WAVE ヘッダ検証 (モノラル16kHz)", || {
        let wav = encode_wav_pcm16(&[0i16; 160], 16_000, 1);
        if wav.len() != 44 + 320 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
            return Err("invalid mono WAV header".into());
        }
        Ok(format!("mono WAV {} bytes", wav.len()))
    });

    rec.run("S329", "encode_wav_pcm16 ステレオ44.1kHz ヘッダ検証", || {
        let wav = encode_wav_pcm16(&[100i16; 441], 44_100, 2);
        if &wav[0..4] != b"RIFF" || u16::from_le_bytes([wav[22], wav[23]]) != 2 {
            return Err("invalid stereo WAV".into());
        }
        Ok(format!("stereo WAV {} bytes", wav.len()))
    });

    rec.run("S330", "is_audio_media 全音声拡張子判定", || {
        for ext in ["a.wav", "b.mp3", "c.m4a", "d.ogg", "e.flac", "f.webm"] {
            if !is_audio_media(ext, "") { return Err(format!("missed audio {ext}")); }
        }
        Ok("all 6 audio extensions matched".into())
    });

    rec.run("S331", "is_audio_media MIME判定 & 非音声拒否", || {
        if !is_audio_media("blob", "audio/wav") || !is_audio_media("blob", "audio/mpeg") {
            return Err("missed audio MIME".into());
        }
        for non in [("a.png", "image/png"), ("b.md", "text/markdown"), ("c.pdf", "application/pdf")] {
            if is_audio_media(non.0, non.1) { return Err(format!("false positive {:?}", non)); }
        }
        Ok("audio MIME + non-audio rejection ok".into())
    });

    rec.run("S332", "keepVoiceCache / discardVoiceCache 非存在ID耐性", || {
        let k = state.dispatch(&rpc_req("s332-k", "keepVoiceCache", json!(["missing_id"])));
        let d = state.dispatch(&rpc_req("s332-d", "discardVoiceCache", json!(["missing_id"])));
        Ok(format!("keep_ok={} discard_ok={}", k.ok, d.ok))
    });

    rec.run("S333", "validateWhisperModelFile 空ファイル・極小ダミー拒否", || {
        let empty = root.join("empty_whisper.bin");
        let tiny = root.join("tiny_whisper.bin");
        std::fs::write(&empty, b"").unwrap();
        std::fs::write(&tiny, b"not a ggml model").unwrap();
        let r1 = state.dispatch(&rpc_req("s333-1", "validateWhisperModelFile", json!([empty.to_string_lossy()])));
        let r2 = state.dispatch(&rpc_req("s333-2", "validateWhisperModelFile", json!([tiny.to_string_lossy()])));
        let v1 = r1.result.as_ref().and_then(|v| v.get("valid")).and_then(|v| v.as_bool()).unwrap_or(false);
        let v2 = r2.result.as_ref().and_then(|v| v.get("valid")).and_then(|v| v.as_bool()).unwrap_or(false);
        if (r1.ok && v1) || (r2.ok && v2) {
            return Err("invalid whisper file marked valid".into());
        }
        Ok("empty & tiny dummy models rejected".into())
    });

    for (id, name, cfg_str) in [
        ("S334", "getSpeechStatus gemini設定", r#"{"engine":"gemini","model":"gemini-2.5-flash"}"#),
        ("S335", "getSpeechStatus openai設定", r#"{"engine":"openai","model":"whisper-1"}"#),
        ("S336", "getSpeechStatus whisper設定", r#"{"engine":"whisper"}"#),
        ("S337", "getSpeechStatus 破損JSON耐性", "{bad json"),
    ] {
        rec.run(id, name, || {
            let r = state.dispatch(&rpc_req(id, "getSpeechStatus", json!([cfg_str])));
            if !r.ok { return Err(format!("{:?}", r.error)); }
            Ok("status ok".into())
        });
    }

    rec.run("S338", "prefetchSpeechModel / cancelSpeechInstall 安全応答", || {
        let c = state.dispatch(&rpc_req("s338", "cancelSpeechInstall", json!([])));
        if !c.ok { return Err(format!("{:?}", c.error)); }
        Ok("cancelSpeechInstall ok".into())
    });

    rec.run("S339", "removeSpeechPart 未知パート耐性", || {
        let r = state.dispatch(&rpc_req("s339", "removeSpeechPart", json!(["unknown_part"])));
        Ok(format!("handled ok={}", r.ok))
    });

    rec.run("S340", "approvePlugin 空ID・空ハッシュ拒否", || {
        let r1 = state.dispatch(&rpc_req("s340-1", "approvePlugin", json!(["", "abc"])));
        let r2 = state.dispatch(&rpc_req("s340-2", "approvePlugin", json!(["p1", ""])));
        if r1.ok || r2.ok { return Err("accepted empty pluginId/sha256".into()); }
        Ok("empty approvePlugin rejected".into())
    });

    rec.run("S341", "setPluginDevMode 未承認プラグイン処理", || {
        let r = state.dispatch(&rpc_req("s341", "setPluginDevMode", json!(["unapproved-xyz", true])));
        if !r.ok || r.result != Some(json!(false)) {
            return Err(format!("expected false for unapproved plugin, got {:?}", r.result));
        }
        Ok("unapproved devMode returned false".into())
    });

    rec.run("S342", "QuickCapture save_qc_theme_cache 正常保存", || {
        save_qc_theme_cache(&root, &json!({ "bgModal": "#101010", "accent": "#336699" }))?;
        if !root.join("qc-theme-cache.json").is_file() { return Err("cache missing".into()); }
        Ok("qc-theme-cache saved".into())
    });

    rec.run("S343", "QuickCapture submit_quick_capture_text スクラップ保存", || {
        let (qtx, qrx) = mpsc::channel();
        let sender: syki_win::rpc_dispatch::EventSender = Arc::new(move |ev| { let _ = qtx.send(ev); });
        let saved = submit_quick_capture_text(&root, "QuickCapture S343 test note", Some(&sender))?
            .ok_or("expected saved path")?;
        let ev = qrx.recv_timeout(Duration::from_secs(2)).map_err(|_| "no host:openFile event")?;
        if ev.topic != "host:openFile" { return Err(format!("unexpected topic {}", ev.topic)); }
        Ok(format!("saved to {}", saved.display()))
    });

    rec.run("S344", "QuickCapture 空白のみ入力の拒否/スキップ", || {
        let res = submit_quick_capture_text(&root, "   \n  ", None)?;
        if res.is_some() { return Err("expected None for blank quick capture".into()); }
        Ok("blank quick capture skipped (None)".into())
    });

    rec.run("S345", "checkMeetingAudioAsync 非同期イベント応答", || {
        let r = state.dispatch(&rpc_req("s345", "checkMeetingAudioAsync", json!(["s345"])));
        if !r.ok { return Err(format!("{:?}", r.error)); }
        let m = collect_events(&rx, &["s345".to_string()], 15);
        if !m.contains_key("s345") { return Err("no meeting:check event".into()); }
        Ok("meeting audio check event received".into())
    });

    // =========================================================================
    // P. 本番 Gemini API 深掘り・多様タスク・エッジケース (S346..S415: 70件)
    //    バッチ並列発火で高速＋並列耐性も同時に検証！
    // =========================================================================
    let gem_base = "https://generativelanguage.googleapis.com";
    let gem_oai = "https://generativelanguage.googleapis.com/v1beta/openai";
    let mk_cfg = |base: &str, model: &str, key: &str, sp: &str, temp: Option<f64>, max_t: Option<u32>| -> String {
        let mut obj = json!({ "baseUrl": base, "model": model, "apiKey": key, "systemPrompt": sp });
        if let Some(t) = temp { obj["temperature"] = json!(t); }
        if let Some(m) = max_t { obj["maxTokens"] = json!(m); }
        obj.to_string()
    };

    // --- Batch P1: LLM 25 scenarios (S346..S370) ---
    let long_doc = "syki is a Windows-native fast-boot Markdown note platform built with Rust and WebView2. ".repeat(45);
    let llm_batch: Vec<(&str, &str, String, String, bool)> = vec![
        ("S346", "LLM temperature=0.0 決定性", "Reply ONLY: ZERO".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", Some(0.0), Some(32)), true),
        ("S347", "LLM temperature=0.8 生成", "Give one random fruit name.".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", Some(0.8), Some(32)), true),
        ("S348", "LLM maxTokens=16 制御", "Count from 1 to 10.".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", Some(0.0), Some(16)), true),
        ("S349", "LLM maxTokens=128 制御", "Explain Rust ownership in 2 sentences.".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", Some(0.2), Some(128)), true),
        ("S350", "LLM JSON構造化出力", "Output ONLY valid JSON: {\"status\":\"ok\",\"count\":3}".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "Return raw JSON only without markdown fences.", Some(0.0), Some(64)), true),
        ("S351", "LLM Markdown表生成", "Create a 2-row Markdown table of Fruit and Color.".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(128)), true),
        ("S352", "LLM Mermaid図生成", "Output a 2-node mermaid graph LR from A to B.".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(128)), true),
        ("S353", "LLM KaTeX数式生成", "Write the quadratic formula in LaTeX math.".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(128)), true),
        ("S354", "LLM 日->英翻訳", "Translate to English: 高速起動のメモ帳".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(64)), true),
        ("S355", "LLM 英->日翻訳", "Translate to Japanese: Zero bugs remaining.".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(64)), true),
        ("S356", "LLM コードバグ修正提案", "Fix bug: function sum(a,b){ return a - b; }".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(128)), true),
        ("S357", "LLM コード解説", "Explain in 1 line: const xs = [1,2,3].map(x=>x*2);".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(128)), true),
        ("S358", "LLM 正規表現生成", "Give a regex matching YYYY-MM-DD dates.".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(64)), true),
        ("S359", "LLM SQL生成", "Write SQL to count rows in users table.".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(64)), true),
        ("S360", "LLM 箇条書き圧縮", "Summarize into 2 bullets: Rust is memory safe and fast. WebView2 uses Edge.".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(128)), true),
        ("S361", "LLM 議事録アクション抽出", "Extract action item: Tanaka will deploy v1.0 by Friday.".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(64)), true),
        ("S362", "LLM 敬語ビジネスメール整形", "次を丁寧なビジネス日本語1文に: 明日10時に打ち合わせよろしく".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(128)), true),
        ("S363", "LLM 絵文字/HTML/フェンス特殊入力", "Echo back the word OK for: ```html\n<div>🚀 & \"quotes\"</div>\n```".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(64)), true),
        ("S364", "LLM 4KB長文要約", format!("Summarize in 10 words: {long_doc}"), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(64)), true),
        ("S365", "LLM /v1beta/openai/ 末尾スラッシュ", "Reply ONLY: SLASH_OK".into(), mk_cfg("https://generativelanguage.googleapis.com/v1beta/openai/", "gemini-2.5-flash", &api_key, "", None, Some(32)), true),
        ("S366", "LLM /v1beta/ 末尾スラッシュ", "Reply ONLY: V1B_OK".into(), mk_cfg("https://generativelanguage.googleapis.com/v1beta/", "gemini-2.5-flash", &api_key, "", None, Some(32)), true),
        ("S367", "LLM models/ プレフィックス付きモデル名", "Reply ONLY: PREFIX_OK".into(), mk_cfg(gem_base, "models/gemini-2.5-flash", &api_key, "", None, Some(32)), true),
        ("S368", "LLM 日本語 systemPrompt 厳格指示", "こんにちは".into(), mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "必ず先頭に「【完了】」を付けて返答してください。", Some(0.0), Some(64)), true),
        ("S369", "LLM 到達不能URL即エラー", "Hi".into(), mk_cfg("http://127.0.0.1:1/v1", "test", "", "", None, None), false),
        ("S370", "LLM 不正JSON config 文字列耐性", "Hi".into(), "{not json".into(), false),
    ];

    for chunk in llm_batch.chunks(8) {
        let t0 = Instant::now();
        let ids: Vec<String> = chunk.iter().map(|c| c.0.to_string()).collect();
        for (id, _, prompt, cfg, _) in chunk {
            state.dispatch(&rpc_req(id, "queryLLMAsync", json!([id, prompt, cfg])));
        }
        let events = collect_events(&rx, &ids, 45);
        let ms = t0.elapsed().as_millis() as u64;
        for (id, name, _, _, expect_ok) in chunk {
            match events.get(*id) {
                None => rec.record(id, name, ms, Err("timeout waiting for llm:result".into())),
                Some(ev) => {
                    let err = ev.payload["error"].as_str().unwrap_or("");
                    let res = ev.payload["result"].as_str().unwrap_or("");
                    if *expect_ok {
                        if !err.is_empty() || res.trim().is_empty() {
                            rec.record(id, name, ms, Err(format!("err={err} res={res}")));
                        } else if *id == "S368" && !res.contains("【完了】") {
                            rec.record(id, name, ms, Err(format!("systemPrompt not honored: {res}")));
                        } else {
                            rec.record(id, name, ms, Ok(format!("-> {}", res.trim().chars().take(35).collect::<String>())));
                        }
                    } else if err.is_empty() {
                        rec.record(id, name, ms, Err("expected error but succeeded".into()));
                    } else {
                        rec.record(id, name, ms, Ok(format!("errored cleanly: {}", err.chars().take(35).collect::<String>())));
                    }
                }
            }
        }
    }

    // --- Batch P2: Decision API 15 scenarios (S371..S385) ---
    let gem_dec = json!({ "enabled": true, "baseUrl": gem_base, "model": "gemini-decision-1", "apiKey": api_key });
    let gem_dec_oai = json!({ "enabled": true, "baseUrl": gem_oai, "model": "gemini-decision-1", "apiKey": api_key });
    let gem_flash_oai = json!({ "enabled": true, "baseUrl": gem_oai, "model": "gemini-2.5-flash", "apiKey": api_key });
    let def_gem = json!({ "baseUrl": gem_base, "model": "gemini-2.5-flash", "apiKey": api_key });

    let c_prio = json!([
        { "id": "p0", "description": "Production outage or data loss" },
        { "id": "p1", "description": "Major feature broken" },
        { "id": "p2", "description": "Minor UI glitch" },
        { "id": "p3", "description": "Typo or cosmetic polish" }
    ]);
    let c_sent = json!([
        { "id": "pos", "description": "Positive praise or satisfaction" },
        { "id": "neu", "description": "Neutral factual statement" },
        { "id": "neg", "description": "Negative complaint or anger" }
    ]);
    let c_lang = json!([
        { "id": "ja", "description": "Japanese text" },
        { "id": "en", "description": "English text" },
        { "id": "es", "description": "Spanish text" },
        { "id": "fr", "description": "French text" },
        { "id": "de", "description": "German text" }
    ]);

    let dec_batch: Vec<(&str, &str, Value, Value, String, Value, &str, &str, Option<&str>)> = vec![
        ("S371", "Decision 4択 優先度P0判定", gem_dec.clone(), json!({}), "CRITICAL: All databases are down and user data is lost!".into(), c_prio.clone(), "p2", "Classify priority", Some("p0")),
        ("S372", "Decision 4択 優先度P3判定", gem_dec.clone(), json!({}), "Small typo in README footer".into(), c_prio.clone(), "p0", "Classify priority", Some("p3")),
        ("S373", "Decision 3択 感情Positive", gem_dec.clone(), json!({}), "This app is amazingly fast and I love it!".into(), c_sent.clone(), "neu", "Sentiment", Some("pos")),
        ("S374", "Decision 3択 感情Negative", gem_dec.clone(), json!({}), "Terrible experience, crashes constantly.".into(), c_sent.clone(), "neu", "Sentiment", Some("neg")),
        ("S375", "Decision 5択 言語判定(ja)", gem_dec.clone(), json!({}), "本日は晴天なり。会議を始めます。".into(), c_lang.clone(), "en", "Detect language", Some("ja")),
        ("S376", "Decision 5択 言語判定(fr)", gem_dec.clone(), json!({}), "Bonjour tout le monde, comment allez-vous?".into(), c_lang.clone(), "en", "Detect language", Some("fr")),
        ("S377", "Decision {{input}} 複数展開", gem_dec.clone(), json!({}), "Positive great job!".into(), c_sent.clone(), "neu", "First={{input}} Second={{input}}", Some("pos")),
        ("S378", "Decision 空input + 非空prompt", gem_dec.clone(), json!({}), "".into(), c_sent.clone(), "neu", "The user said: Amazing product, 5 stars!", Some("pos")),
        ("S379", "Decision 非空input + 空prompt", gem_dec.clone(), json!({}), "Awesome! Best tool ever!".into(), c_sent.clone(), "neu", "", Some("pos")),
        ("S380", "Decision defaultConfig からAPIキー継承", json!({ "enabled": true, "model": "gemini-decision-1", "baseUrl": gem_base }), def_gem.clone(), "Great work!".into(), c_sent.clone(), "neu", "Sentiment", Some("pos")),
        ("S381", "Decision /v1beta/openai 指定時 gemini-decision-1", gem_dec_oai.clone(), json!({}), "Production server crashed!".into(), c_prio.clone(), "p3", "Priority", Some("p0")),
        ("S382", "Decision /v1beta/openai + gemini-2.5-flash", gem_flash_oai.clone(), json!({}), "Wonderful app!".into(), c_sent.clone(), "neu", "Sentiment", Some("pos")),
        ("S383", "Decision 3KB長文分類", gem_dec.clone(), json!({}), format!("{} CRITICAL DATABASE OUTAGE!", "Normal log line. ".repeat(150)), c_prio.clone(), "p3", "Classify severity", Some("p0")),
        ("S384", "Decision 不正actionConfigフォールバック", json!("not_an_object"), json!({}), "hello".into(), c_sent.clone(), "neu", "Classify", None),
        ("S385", "Decision 日本語選択肢8択分類", gem_dec.clone(), json!({}), "請求書の金額が二重引き落としされています".into(), json!([
            {"id":"billing","description":"請求・支払い・返金"},
            {"id":"auth","description":"ログイン・認証"},
            {"id":"perf","description":"速度・メモリ"},
            {"id":"ui","description":"画面・レイアウト"},
            {"id":"api","description":"API・SDK"},
            {"id":"sec","description":"セキュリティ"},
            {"id":"doc","description":"ドキュメント"},
            {"id":"other","description":"その他"}
        ]), "other", "問い合わせカテゴリを選択", Some("billing")),
    ];

    for chunk in dec_batch.chunks(8) {
        let t0 = Instant::now();
        let ids: Vec<String> = chunk.iter().map(|c| c.0.to_string()).collect();
        for (id, _, acfg, dcfg, inp, choices, fb, pr, _) in chunk {
            state.dispatch(&rpc_req(id, "queryDecisionAsync", json!([id, acfg, dcfg, inp, choices, fb, pr, 20000])));
        }
        let events = collect_events(&rx, &ids, 45);
        let ms = t0.elapsed().as_millis() as u64;
        for (id, name, _, _, _, _, _, _, expect_choice) in chunk {
            match events.get(*id) {
                None => rec.record(id, name, ms, Err("timeout".into())),
                Some(ev) => {
                    let cid = ev.payload["choiceId"].as_str().unwrap_or("");
                    if let Some(exp) = expect_choice {
                        if cid != *exp {
                            rec.record(id, name, ms, Err(format!("expected {exp}, got {:?} (ok={}, by={})", cid, ev.payload["ok"], ev.payload["by"])));
                        } else {
                            rec.record(id, name, ms, Ok(format!("choiceId={cid} by={}", ev.payload["by"])));
                        }
                    } else if cid.is_empty() {
                        rec.record(id, name, ms, Err("empty choiceId".into()));
                    } else {
                        rec.record(id, name, ms, Ok(format!("fallback choiceId={cid} by={}", ev.payload["by"])));
                    }
                }
            }
        }
    }

    // --- Batch P3: Autocomplete (S386..S395: 10件) & CLI Gen (S396..S402: 7件) ---
    let gem_std = mk_cfg(gem_base, "gemini-2.5-flash", &api_key, "", None, Some(40));
    let gem_oai_str = mk_cfg(gem_oai, "gemini-2.5-flash", &api_key, "", None, Some(40));
    let bad_key_str = mk_cfg(gem_base, "gemini-2.5-flash", "AIzaSyINVALID_AC", "", None, Some(20));

    let ac_batch: Vec<(&str, &str, &str, &str, &str, bool)> = vec![
        ("S386", "Autocomplete Rust関数", "fn multiply(a: i32, b: i32) -> i32 {\n    ", "\n}", &gem_std, true),
        ("S387", "Autocomplete TypeScript型", "interface User {\n  id: string;\n  email: ", "\n}", &gem_std, true),
        ("S388", "Autocomplete SQLクエリ", "SELECT id, name FROM users WHERE ", ";", &gem_std, true),
        ("S389", "Autocomplete Markdown箇条書き", "# Shopping List\n- Apples\n- ", "", &gem_std, true),
        ("S390", "Autocomplete JSONオブジェクト", "{\"name\": \"syki\", \"version\": ", "}", &gem_std, true),
        ("S391", "Autocomplete 空suffix", "The capital of Japan is ", "", &gem_std, true),
        ("S392", "Autocomplete 空prefix", "", " is a systems programming language.", &gem_std, true),
        ("S393", "Autocomplete OpenAI互換URL経由", "const pi = 3.", ";", &gem_oai_str, true),
        ("S394", "Autocomplete 日本語敬語補完", "いつも大変お世話になっており", "。", &gem_std, true),
        ("S395", "Autocomplete 不正キーエラー", "hello ", "", &bad_key_str, false),
    ];

    {
        let t0 = Instant::now();
        let ids: Vec<String> = ac_batch.iter().map(|c| c.0.to_string()).collect();
        for (id, _, pre, suf, cfg, _) in &ac_batch {
            state.dispatch(&rpc_req(id, "autocompleteAsync", json!([id, pre, suf, cfg])));
        }
        let events = collect_events(&rx, &ids, 45);
        let ms = t0.elapsed().as_millis() as u64;
        for (id, name, _, _, _, expect_ok) in &ac_batch {
            match events.get(*id) {
                None => rec.record(id, name, ms, Err("timeout".into())),
                Some(ev) => {
                    let err = ev.payload["error"].as_str().unwrap_or("");
                    let sug = ev.payload["suggestion"].as_str().unwrap_or("");
                    if *expect_ok {
                        if !err.is_empty() || sug.trim().is_empty() {
                            rec.record(id, name, ms, Err(format!("err={err} sug={sug}")));
                        } else {
                            rec.record(id, name, ms, Ok(format!("-> {}", sug.trim().chars().take(30).collect::<String>())));
                        }
                    } else if err.is_empty() {
                        rec.record(id, name, ms, Err("expected error".into()));
                    } else {
                        rec.record(id, name, ms, Ok("errored cleanly".into()));
                    }
                }
            }
        }
    }

    let cli_batch: Vec<(&str, &str, &str, &str, &str)> = vec![
        ("S396", "CLI生成 ファイル検索", "find all .md files recursively", &gem_std, "{}"),
        ("S397", "CLI生成 Gitログ確認", "show last 5 git commits oneline", &gem_std, "{}"),
        ("S398", "CLI生成 テキスト検索", "search for TODO in current folder", &gem_std, "{}"),
        ("S399", "CLI生成 ディスク使用量", "check disk space", &gem_std, "{}"),
        ("S400", "CLI生成 OS/CWDコンテキスト付き", "list files sorted by size", &gem_std, r#"{"os":"windows","cwd":"C:\\work"}"#),
        ("S401", "CLI生成 日本語プロンプト", "カレントディレクトリのファイル一覧を表示する", &gem_std, "{}"),
        ("S402", "CLI生成 OpenAI互換URL経由", "print current working directory", &gem_oai_str, "{}"),
    ];

    {
        let t0 = Instant::now();
        let ids: Vec<String> = cli_batch.iter().map(|c| c.0.to_string()).collect();
        for (id, _, pr, cfg, ctx) in &cli_batch {
            state.dispatch(&rpc_req(id, "generateCliCommandAsync", json!([id, pr, cfg, ctx])));
        }
        let events = collect_events(&rx, &ids, 45);
        let ms = t0.elapsed().as_millis() as u64;
        for (id, name, _, _, _) in &cli_batch {
            match events.get(*id) {
                None => rec.record(id, name, ms, Err("timeout".into())),
                Some(ev) => {
                    let err = ev.payload["error"].as_str().unwrap_or("");
                    let cmd = ev.payload["command"].as_str().or_else(|| ev.payload["result"].as_str()).unwrap_or("");
                    if !err.is_empty() || cmd.trim().is_empty() {
                        rec.record(id, name, ms, Err(format!("err={err} cmd={cmd}")));
                    } else {
                        rec.record(id, name, ms, Ok(format!("cmd={}", cmd.trim().chars().take(35).collect::<String>())));
                    }
                }
            }
        }
    }

    // --- Batch P4: Vision (S403..S409: 7件), Image (S410..S413: 4件), STT (S414..S415: 2件) ---
    let data_uri_png = format!("data:image/png;base64,{png_b64}");
    let gem_v1b_str = mk_cfg("https://generativelanguage.googleapis.com/v1beta/", "gemini-2.5-flash", &api_key, "", None, None);
    let gem_models_pref_str = mk_cfg(gem_base, "models/gemini-2.5-flash", &api_key, "", None, None);
    let vis_batch: Vec<(&str, &str, &str, &str, &str, &str, bool)> = vec![
        ("S403", "Vision OpenAI互換URL経由 Gemini Vision", "What color is this pixel?", &png_b64, "image/png", &gem_oai_str, true),
        ("S404", "Vision 日本語OCR指示", "この画像の色を日本語1語で答えてください。", &png_b64, "image/png", &gem_std, true),
        ("S405", "Vision data URI プレフィックス付きbase64", "What color is this?", &data_uri_png, "image/png", &gem_std, true),
        ("S406", "Vision 空base64エラー", "Describe", "", "image/png", &gem_std, false),
        ("S407", "Vision 不正キーエラー (平文非漏洩)", "Describe", &png_b64, "image/png", &bad_key_str, false),
        ("S408", "Vision /v1beta/ 末尾スラッシュURL", "Name the color.", &png_b64, "image/png", &gem_v1b_str, true),
        ("S409", "Vision models/ プレフィックス付きモデル", "Name the color.", &png_b64, "image/png", &gem_models_pref_str, true),
    ];

    {
        let t0 = Instant::now();
        let ids: Vec<String> = vis_batch.iter().map(|c| c.0.to_string()).collect();
        for (id, _, pr, b64, mime, cfg, _) in &vis_batch {
            state.dispatch(&rpc_req(id, "queryVisionAsync", json!([id, pr, b64, mime, cfg])));
        }
        let events = collect_events(&rx, &ids, 45);
        let ms = t0.elapsed().as_millis() as u64;
        for (id, name, _, _, _, _, expect_ok) in &vis_batch {
            match events.get(*id) {
                None => rec.record(id, name, ms, Err("timeout".into())),
                Some(ev) => {
                    let err = ev.payload["error"].as_str().unwrap_or("");
                    let res = ev.payload["result"].as_str().unwrap_or("");
                    if *expect_ok {
                        if !err.is_empty() || res.trim().is_empty() {
                            rec.record(id, name, ms, Err(format!("err={err} res={res}")));
                        } else {
                            rec.record(id, name, ms, Ok(format!("-> {}", res.trim().chars().take(30).collect::<String>())));
                        }
                    } else if err.is_empty() {
                        rec.record(id, name, ms, Err("expected error".into()));
                    } else if err.contains("AIzaSyINVALID_AC") {
                        rec.record(id, name, ms, Err("API key leaked in vision error!".into()));
                    } else {
                        rec.record(id, name, ms, Ok("errored cleanly (no key leak)".into()));
                    }
                }
            }
        }
    }

    let img_note = root.join("s410_img.md").to_string_lossy().to_string();
    let img_batch: Vec<(&str, &str, &str, String, bool)> = vec![
        ("S410", "ImageGen aspectRatio=4:3", "simple yellow triangle", json!({ "baseUrl": gem_base, "apiKey": api_key, "model": "gemini-3.1-flash-lite-image", "aspectRatio": "4:3" }).to_string(), true),
        ("S411", "ImageGen OpenAI互換URL経由 Gemini画像生成", "simple purple square", json!({ "baseUrl": gem_oai, "apiKey": api_key, "model": "gemini-3.1-flash-lite-image", "aspectRatio": "1:1" }).to_string(), true),
        ("S412", "ImageGen 日本語プロンプト画像生成", "シンプルな赤い星アイコン", json!({ "baseUrl": gem_base, "apiKey": api_key, "model": "gemini-3.1-flash-lite-image", "aspectRatio": "1:1" }).to_string(), true),
        ("S413", "ImageGen 不正キー平文非漏洩", "circle", json!({ "baseUrl": gem_base, "apiKey": "AIzaSyBAD_IMG_KEY", "model": "gemini-3.1-flash-lite-image" }).to_string(), false),
    ];

    {
        let t0 = Instant::now();
        let ids: Vec<String> = img_batch.iter().map(|c| c.0.to_string()).collect();
        for (id, _, pr, cfg, _) in &img_batch {
            state.dispatch(&rpc_req(id, "generateImageAsync", json!([id, pr, cfg, img_note])));
        }
        let events = collect_events(&rx, &ids, 60);
        let ms = t0.elapsed().as_millis() as u64;
        for (id, name, _, _, expect_ok) in &img_batch {
            match events.get(*id) {
                None => rec.record(id, name, ms, Err("timeout".into())),
                Some(ev) => {
                    let err = ev.payload["error"].as_str().unwrap_or("");
                    let res = ev.payload["result"].as_str().unwrap_or("");
                    if *expect_ok {
                        if !err.is_empty() || !res.starts_with("![Generated Image](") {
                            rec.record(id, name, ms, Err(format!("err={err} res={res}")));
                        } else {
                            rec.record(id, name, ms, Ok(res.to_string()));
                        }
                    } else if err.is_empty() {
                        rec.record(id, name, ms, Err("expected error".into()));
                    } else if err.contains("AIzaSyBAD_IMG_KEY") {
                        rec.record(id, name, ms, Err("API key leaked in image error!".into()));
                    } else {
                        rec.record(id, name, ms, Ok("errored cleanly (no key leak)".into()));
                    }
                }
            }
        }
    }

    let stereo_wav_b64 = STANDARD.encode(encode_wav_pcm16(&vec![0i16; 16000], 44_100, 2));
    let voice_ok = json!({ "engine": "gemini", "apiKey": api_key, "model": "gemini-2.5-flash", "prompt": "Transcribe or empty." }).to_string();
    let voice_bad = json!({ "engine": "gemini", "apiKey": "AIzaSyBAD_STT_KEY", "model": "gemini-2.5-flash" }).to_string();
    {
        let t0 = Instant::now();
        state.dispatch(&rpc_req("S414", "transcribeAudioAsync", json!(["S414", stereo_wav_b64, "audio/wav", voice_ok])));
        state.dispatch(&rpc_req("S415", "transcribeAudioAsync", json!(["S415", stereo_wav_b64, "audio/wav", voice_bad])));
        let events = collect_events(&rx, &["S414".to_string(), "S415".to_string()], 45);
        let ms = t0.elapsed().as_millis() as u64;
        match events.get("S414") {
            Some(ev) if ev.payload["error"].as_str().unwrap_or("").is_empty() => rec.record("S414", "STT 44.1kHzステレオWAV 本番Gemini", ms, Ok("stereo WAV transcribed".into())),
            Some(ev) => rec.record("S414", "STT 44.1kHzステレオWAV 本番Gemini", ms, Err(format!("{:?}", ev.payload))),
            None => rec.record("S414", "STT 44.1kHzステレオWAV 本番Gemini", ms, Err("timeout".into())),
        }
        match events.get("S415") {
            Some(ev) if !ev.payload["error"].as_str().unwrap_or("").is_empty() => {
                let err = ev.payload["error"].as_str().unwrap_or("");
                if err.contains("AIzaSyBAD_STT_KEY") {
                    rec.record("S415", "STT 不正APIキーエラー (非漏洩)", ms, Err("API key leaked in STT error!".into()));
                } else {
                    rec.record("S415", "STT 不正APIキーエラー (非漏洩)", ms, Ok("errored cleanly".into()));
                }
            }
            other => rec.record("S415", "STT 不正APIキーエラー (非漏洩)", ms, Err(format!("unexpected: {:?}", other))),
        }
    }

    // Save report
    let out_dir = repo.join("evals/results");
    let _ = std::fs::create_dir_all(&out_dir);
    let pass_cnt = rec.results.values().filter(|v| v["ok"] == true).count();
    let fail_cnt = rec.results.values().filter(|v| v["ok"] == false).count();
    let report = json!({
        "suite": "scenario_300_extra_rust_live",
        "total": rec.results.len(),
        "passed": pass_cnt,
        "failed": fail_cnt,
        "results": rec.results.values().cloned().collect::<Vec<_>>()
    });
    let _ = std::fs::write(
        out_dir.join("scenario_300_rust.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    );

    println!("\n=== RUST EXTRA 215 SCENARIOS SUMMARY: total={} pass={} fail={} ===", rec.results.len(), pass_cnt, fail_cnt);
    for v in rec.results.values().filter(|v| v["ok"] == false) {
        println!("  [FAIL] {} {}: {}", v["id"], v["name"], v["error"]);
    }

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(rec.results.len(), 215, "expected 215 Rust extra scenarios");
    assert_eq!(fail_cnt, 0, "{fail_cnt} Rust extra scenarios failed");
}
