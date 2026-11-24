//! Live E2E verification of all syki-win API inquiry RPCs against real Gemini & Local LLM endpoints.
//! Run with: `cargo test -p syki-win --test live_api_e2e -- --ignored --nocapture`

use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::{json, Value};
use syki_core::rpc::{RpcEvent, RpcRequest, RuntimeEnv};
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

fn make_test_state() -> (PathBuf, CoreState, mpsc::Receiver<RpcEvent>) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("syki-live-e2e-{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    let env = RuntimeEnv {
        data_dir: root.clone(),
        instance_id: "live-e2e".to_string(),
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

fn wait_event(rx: &mpsc::Receiver<RpcEvent>, req_id: &str, timeout_secs: u64) -> RpcEvent {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let rem = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!rem.is_zero(), "timeout waiting for event reqId={req_id}");
        let ev = rx.recv_timeout(rem).expect("recv event");
        if ev.payload.get("reqId").and_then(|v| v.as_str()) == Some(req_id) {
            return ev;
        }
    }
}

fn tiny_png_b64() -> String {
    // 1x1 red PNG
    let png_bytes: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    STANDARD.encode(png_bytes)
}

#[test]
#[ignore = "live API E2E test"]
fn live_all_api_inquiries_gemini_and_local_llm() {
    let api_key = load_gemini_key();
    assert!(!api_key.is_empty(), "Gemini API key not found");
    let (root, state, rx) = make_test_state();

    // 1. detectLLMProvider
    let det = state.dispatch(&rpc_req("1", "detectLLMProvider", json!([])));
    assert!(det.ok, "detectLLMProvider failed: {:?}", det.error);
    println!("PASS: detectLLMProvider");

    // 2. checkAppUpdate
    let upd = state.dispatch(&rpc_req("2", "checkAppUpdate", json!([])));
    assert!(upd.ok, "checkAppUpdate failed: {:?}", upd.error);
    assert!(upd.result.unwrap().get("tag_name").is_some());
    println!("PASS: checkAppUpdate");

    let gemini_cfg = json!({
        "baseUrl": "https://generativelanguage.googleapis.com",
        "model": "gemini-2.5-flash",
        "apiKey": api_key,
        "systemPrompt": "Reply concisely."
    })
    .to_string();

    let ollama_cfg = json!({
        "baseUrl": "http://127.0.0.1:11434",
        "model": "qwen2.5-coder:1.5b",
        "apiKey": "",
        "systemPrompt": "Reply concisely."
    })
    .to_string();

    let strata_cfg = json!({
        "baseUrl": "http://127.0.0.1:8080/v1",
        "model": "qwen3.8-flash-next-iq2_xs",
        "apiKey": "",
        "systemPrompt": "Reply concisely."
    })
    .to_string();

    // 3. queryLLMAsync (Gemini, Ollama, Local OpenAI-compat :8080)
    for (label, id, cfg) in [
        ("Gemini", "llm-gem", &gemini_cfg),
        ("Ollama", "llm-oll", &ollama_cfg),
        ("Local8080", "llm-8080", &strata_cfg),
    ] {
        let r = state.dispatch(&rpc_req(
            id,
            "queryLLMAsync",
            json!([id, "Reply with the word OK.", cfg]),
        ));
        assert!(r.ok);
        let ev = wait_event(&rx, id, 60);
        let err = ev.payload["error"].as_str().unwrap_or("");
        let txt = ev.payload["result"].as_str().unwrap_or("");
        assert!(err.is_empty(), "queryLLMAsync ({label}) error: {err}");
        assert!(!txt.trim().is_empty(), "queryLLMAsync ({label}) empty");
        println!("PASS: queryLLMAsync ({label}) -> {}", txt.trim().chars().take(40).collect::<String>());
    }

    // 4. queryDecisionAsync (Gemini Decision API, Gemini Flash, & Ollama)
    let choices = json!([
        { "id": "yes", "label": "Yes", "description": "Affirmative" },
        { "id": "no", "label": "No", "description": "Negative" }
    ]);
    let gem_decision_cfg = json!({
        "enabled": true,
        "baseUrl": "https://generativelanguage.googleapis.com",
        "model": "gemini-decision-1",
        "apiKey": api_key
    });
    for (label, id, action_cfg, default_cfg) in [
        ("Gemini-Decision-1", "dec-gem1", gem_decision_cfg, json!({})),
        ("Gemini-Flash", "dec-gemf", json!({ "enabled": true, "baseUrl": "https://generativelanguage.googleapis.com", "model": "gemini-2.5-flash", "apiKey": api_key }), json!({})),
        ("Ollama", "dec-oll", json!({ "enabled": true, "baseUrl": "http://127.0.0.1:11434", "model": "qwen2.5-coder:1.5b" }), json!({})),
    ] {
        let r = state.dispatch(&rpc_req(
            id,
            "queryDecisionAsync",
            json!([id, action_cfg, default_cfg, "The user said: Yes, proceed.", choices, "no", "Pick matching choice", 20000]),
        ));
        assert!(r.ok);
        let ev = wait_event(&rx, id, 60);
        assert_eq!(ev.payload["ok"], true, "queryDecisionAsync ({label}) failed: {:?}", ev.payload);
        println!(
            "PASS: queryDecisionAsync ({label}) -> choiceId={} by={}",
            ev.payload["choiceId"], ev.payload["by"]
        );
    }

    // 5. autocompleteAsync (Gemini & Ollama)
    for (label, id, cfg) in [("Gemini", "ac-gem", &gemini_cfg), ("Ollama", "ac-oll", &ollama_cfg)] {
        let r = state.dispatch(&rpc_req(
            id,
            "autocompleteAsync",
            json!([id, "function add(a, b) {\n  return ", "\n}", cfg]),
        ));
        assert!(r.ok);
        let ev = wait_event(&rx, id, 60);
        let err = ev.payload["error"].as_str().unwrap_or("");
        let sug = ev.payload["suggestion"].as_str().unwrap_or("");
        assert!(err.is_empty(), "autocompleteAsync ({label}) error: {err}");
        assert!(!sug.trim().is_empty(), "autocompleteAsync ({label}) empty");
        println!("PASS: autocompleteAsync ({label})");
    }

    // 6. generateCliCommandAsync (Gemini & Ollama)
    for (label, id, cfg) in [("Gemini", "cli-gem", &gemini_cfg), ("Ollama", "cli-oll", &ollama_cfg)] {
        let r = state.dispatch(&rpc_req(
            id,
            "generateCliCommandAsync",
            json!([id, "list files in current directory", cfg, "{}"]),
        ));
        assert!(r.ok);
        let ev = wait_event(&rx, id, 60);
        let err = ev.payload["error"].as_str().unwrap_or("");
        let res = ev.payload["result"].as_str().or_else(|| ev.payload["command"].as_str()).unwrap_or("");
        assert!(err.is_empty(), "generateCliCommandAsync ({label}) error: {err}");
        assert!(!res.trim().is_empty(), "generateCliCommandAsync ({label}) empty: {:?}", ev.payload);
        println!("PASS: generateCliCommandAsync ({label}) -> {}", res.trim().chars().take(40).collect::<String>());
    }

    // 7. queryVisionAsync (Gemini)
    let png_b64 = tiny_png_b64();
    let vis_r = state.dispatch(&rpc_req(
        "vis-gem",
        "queryVisionAsync",
        json!(["vis-gem", "What color is this pixel?", png_b64, "image/png", gemini_cfg]),
    ));
    assert!(vis_r.ok);
    let vis_ev = wait_event(&rx, "vis-gem", 60);
    let vis_err = vis_ev.payload["error"].as_str().unwrap_or("");
    let vis_txt = vis_ev.payload["result"].as_str().unwrap_or("");
    assert!(vis_err.is_empty(), "queryVisionAsync error: {vis_err}");
    assert!(!vis_txt.trim().is_empty(), "queryVisionAsync empty");
    println!("PASS: queryVisionAsync (Gemini)");

    // 8. processDroppedMedia - Image PNG (Gemini)
    let drop_img_r = state.dispatch(&rpc_req(
        "drop-img",
        "processDroppedMedia",
        json!(["drop-img", "tiny.png", "image/png", png_b64, gemini_cfg, "{}"]),
    ));
    assert!(drop_img_r.ok);
    let drop_img_ev = wait_event(&rx, "drop-img", 60);
    let drop_img_err = drop_img_ev.payload["error"].as_str().unwrap_or("");
    assert!(drop_img_err.is_empty(), "processDroppedMedia (PNG) error: {drop_img_err}");
    println!("PASS: processDroppedMedia PNG (Gemini)");

    // 9. processDroppedMedia - Audio WAV (Gemini)
    let wav_b64 = STANDARD.encode(encode_wav_pcm16(&vec![0i16; 8000], 16_000, 1));
    let voice_cfg = json!({
        "engine": "gemini",
        "apiKey": api_key,
        "model": "gemini-2.5-flash",
        "prompt": "Transcribe audio or reply empty if silent."
    })
    .to_string();
    let drop_wav_r = state.dispatch(&rpc_req(
        "drop-wav",
        "processDroppedMedia",
        json!(["drop-wav", "silence.wav", "audio/wav", wav_b64, gemini_cfg, voice_cfg]),
    ));
    assert!(drop_wav_r.ok);
    let drop_wav_ev = wait_event(&rx, "drop-wav", 60);
    assert_eq!(drop_wav_ev.topic, "media:dropped");
    let drop_wav_err = drop_wav_ev.payload["error"].as_str().unwrap_or("");
    assert!(drop_wav_err.is_empty(), "processDroppedMedia (WAV) error: {drop_wav_err}");
    println!("PASS: processDroppedMedia WAV (Gemini)");

    // 10. transcribeAudioAsync (Gemini)
    let tr_r = state.dispatch(&rpc_req(
        "tr-gem",
        "transcribeAudioAsync",
        json!(["tr-gem", wav_b64, "audio/wav", voice_cfg]),
    ));
    assert!(tr_r.ok);
    let tr_ev = wait_event(&rx, "tr-gem", 60);
    let tr_err = tr_ev.payload["error"].as_str().unwrap_or("");
    assert!(tr_err.is_empty(), "transcribeAudioAsync error: {tr_err}");
    println!("PASS: transcribeAudioAsync (Gemini)");

    // 11. generateImageAsync (Gemini)
    let img_cfg = json!({
        "baseUrl": "https://generativelanguage.googleapis.com",
        "apiKey": api_key,
        "model": "gemini-3.1-flash-lite-image",
        "aspectRatio": "1:1"
    })
    .to_string();
    let img_r = state.dispatch(&rpc_req(
        "img-gem",
        "generateImageAsync",
        json!(["img-gem", "a simple minimalist blue circle", img_cfg]),
    ));
    assert!(img_r.ok);
    let img_ev = wait_event(&rx, "img-gem", 60);
    let img_err = img_ev.payload["error"].as_str().unwrap_or("");
    let img_res = img_ev.payload["result"].as_str().unwrap_or("");
    assert!(img_err.is_empty(), "generateImageAsync failed: {img_err}");
    assert!(img_res.starts_with("data:image/"), "expected data:image/... URL");
    println!("PASS: generateImageAsync (Gemini) -> {} bytes data URL", img_res.len());

    let _ = std::fs::remove_dir_all(root);
}
