//! Local STT (`speech.cpp` / `Haas-ja`), Pure-Rust PCM16 WAV encoder, and Voice Cache (#14).

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use syki_core::rpc::RpcEvent;
use syki_core::storage::atomic::write_atomic_bytes;

use crate::rpc_dispatch::EventSender;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static INSTALL_CANCELLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceCacheEntry {
    pub path: String,
    pub filename: String,
    pub timestamp: String,
    pub size_str: String,
    pub size_bytes: u64,
    pub kind: String,
    pub kept: bool,
}

/// Encodes mono 16kHz `i16` PCM samples into a 44-byte-header RIFF/WAVE file in pure Rust.
pub fn encode_wav_pcm16(samples: &[i16], sample_rate: u32, channels: u16) -> Vec<u8> {
    let data_bytes = (samples.len() * 2) as u32;
    let byte_rate = sample_rate * (channels as u32) * 2;
    let block_align = channels * 2;
    let mut out = Vec::with_capacity(44 + data_bytes as usize);

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
    out.extend_from_slice(&1u16.to_le_bytes());  // PCM format = 1
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for &s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Linearly resamples interleaved `f32` stereo/mono audio down to mono 16kHz `i16` samples.
pub fn resample_f32_to_mono_16k_i16(samples: &[f32], src_rate: u32, src_channels: u16) -> Vec<i16> {
    if samples.is_empty() || src_rate == 0 || src_channels == 0 {
        return Vec::new();
    }
    let ch = src_channels as usize;
    let frames = samples.len() / ch;
    if frames == 0 {
        return Vec::new();
    }
    let dst_frames = ((frames as u64) * 16_000 / (src_rate as u64)).max(1) as usize;
    let mut out = Vec::with_capacity(dst_frames);
    for i in 0..dst_frames {
        let src_pos = (i as f64) * (frames as f64) / (dst_frames as f64);
        let idx = (src_pos as usize).min(frames - 1);
        let mut sum = 0.0f32;
        for c in 0..ch {
            sum += samples[idx * ch + c];
        }
        let mono = (sum / ch as f32).clamp(-1.0, 1.0);
        out.push((mono * 32767.0) as i16);
    }
    out
}

pub fn speech_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("speech")
}

pub fn voice_cache_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("voice-cache")
}

pub fn validate_whisper_model_file(path: &Path) -> Result<Value, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("model file unreadable: {e}"))?;
    if !meta.is_file() {
        return Err("path is not a file".to_string());
    }
    if meta.len() < 1024 {
        return Err("model file is too small to be a valid Whisper GGML/GGUF model".to_string());
    }
    Ok(json!({
        "valid": true,
        "path": path.to_string_lossy(),
        "bytes": meta.len()
    }))
}

pub fn get_speech_status(data_dir: &Path) -> Value {
    let dir = speech_dir(data_dir);
    let engine = dir.join("whisper-cli.exe");
    let model = dir.join("ggml-model.bin");
    let haas = dir.join("haas-ja");
    let engine_ready = engine.is_file();
    let model_ready = model.is_file();
    let haas_ready = haas.is_dir();
    json!({
        "supported": true,
        "available": engine_ready && model_ready,
        "engineReady": engine_ready,
        "modelReady": model_ready,
        "haasJaReady": haas_ready,
        "storageDir": dir.to_string_lossy(),
        "speechDir": dir.to_string_lossy(),
        "enginePath": engine.to_string_lossy(),
        "modelPath": model.to_string_lossy(),
        "runtime": {
            "installed": engine_ready,
            "part": { "id": "whisper-cli", "label": "whisper-cli" }
        },
        "models": [
            {
                "installed": model_ready,
                "part": { "id": "ggml-model", "label": "ggml-model.bin" }
            }
        ],
        "local": {
            "ready": engine_ready && model_ready
        }
    })
}

pub fn install_speech_part_async(
    data_dir: PathBuf,
    req_id: String,
    part: String,
    sender: Option<EventSender>,
) {
    INSTALL_CANCELLED.store(false, Ordering::Release);
    std::thread::spawn(move || {
        let dir = speech_dir(&data_dir);
        let _ = std::fs::create_dir_all(&dir);
        if part == "haas-ja" {
            let _ = std::fs::create_dir_all(dir.join("haas-ja"));
        }
        let (state, msg) = if INSTALL_CANCELLED.load(Ordering::Acquire) {
            ("cancelled", "speech installation cancelled".to_string())
        } else {
            (
                "ready",
                format!("speech directory prepared for '{part}' at {}", dir.display()),
            )
        };
        if let Some(tx) = sender {
            tx(RpcEvent {
                topic: "speech:install".to_string(),
                payload: json!({
                    "reqId": req_id,
                    "state": state,
                    "done": 1,
                    "total": 1,
                    "message": msg
                }),
            });
        }
    });
}

pub fn remove_speech_part(data_dir: &Path, part: &str) -> Result<(), String> {
    let dir = speech_dir(data_dir);
    match part.trim() {
        "engine" => {
            let p = dir.join("whisper-cli.exe");
            if p.exists() {
                std::fs::remove_file(p).map_err(|e| e.to_string())?;
            }
        }
        "model" => {
            let p = dir.join("ggml-model.bin");
            if p.exists() {
                std::fs::remove_file(p).map_err(|e| e.to_string())?;
            }
        }
        "haas-ja" => {
            let p = dir.join("haas-ja");
            if p.exists() {
                std::fs::remove_dir_all(p).map_err(|e| e.to_string())?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn format_byte_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", (bytes as f64) / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

fn format_cache_timestamp(name: &str, meta: Option<&std::fs::Metadata>) -> String {
    // Parse "voice-YYYYMMDD-HHMMSS..." or "YYYY-MM-DD-HHMMSS..."
    if let Some(rest) = name.strip_prefix("voice-") {
        if rest.len() >= 15 && rest.as_bytes()[8] == b'-' {
            let d = &rest[0..8];
            let t = &rest[9..15];
            if d.bytes().all(|b| b.is_ascii_digit()) && t.bytes().all(|b| b.is_ascii_digit()) {
                return format!(
                    "{}/{}/{} {}:{}:{}",
                    &d[0..4], &d[4..6], &d[6..8], &t[0..2], &t[2..4], &t[4..6]
                );
            }
        }
    }
    if name.len() >= 17 && name.as_bytes()[4] == b'-' && name.as_bytes()[7] == b'-' && name.as_bytes()[10] == b'-' {
        let y = &name[0..4];
        let m = &name[5..7];
        let d = &name[8..10];
        let t = &name[11..17];
        if [y, m, d, t].iter().all(|s| s.bytes().all(|b| b.is_ascii_digit())) {
            return format!("{y}/{m}/{d} {}:{}:{}", &t[0..2], &t[2..4], &t[4..6]);
        }
    }
    if let Some(m) = meta {
        if let Ok(modified) = m.modified() {
            let dt: chrono::DateTime<Local> = modified.into();
            return dt.format("%Y/%m/%d %H:%M:%S").to_string();
        }
    }
    Local::now().format("%Y/%m/%d %H:%M:%S").to_string()
}

pub fn list_voice_caches(data_dir: &Path) -> Vec<VoiceCacheEntry> {
    let dir = voice_cache_dir(data_dir);
    let mut out = Vec::new();
    let scan = |target_dir: &Path, kind: &str, out: &mut Vec<VoiceCacheEntry>| {
        let Ok(rd) = std::fs::read_dir(target_dir) else {
            return;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(
                ext.to_ascii_lowercase().as_str(),
                "wav" | "webm" | "ogg" | "m4a" | "mp4"
            ) {
                continue;
            }
            let meta = entry.metadata().ok();
            let size_bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            if size_bytes == 0 {
                continue;
            }
            let filename = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let timestamp = format_cache_timestamp(&filename, meta.as_ref());
            let size_str = format_byte_size(size_bytes);
            let kept = p.with_extension("kept").exists();
            out.push(VoiceCacheEntry {
                path: p.to_string_lossy().into_owned(),
                filename,
                timestamp,
                size_str,
                size_bytes,
                kind: kind.to_string(),
                kept,
            });
        }
    };
    scan(&dir, "voice", &mut out);
    let meeting_dir = dir.join("meetings");
    if meeting_dir.is_dir() {
        scan(&meeting_dir, "meeting", &mut out);
    }
    out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    out
}

pub fn keep_voice_cache(cache_path: &Path) -> Result<(), String> {
    if !cache_path.is_file() {
        return Err("cache file not found".to_string());
    }
    write_atomic_bytes(&cache_path.with_extension("kept"), b"1").map_err(|e| e.to_string())
}

pub fn discard_voice_cache(cache_path: &Path) -> Result<(), String> {
    if cache_path.exists() {
        std::fs::remove_file(cache_path).map_err(|e| e.to_string())?;
    }
    let kept = cache_path.with_extension("kept");
    if kept.exists() {
        let _ = std::fs::remove_file(kept);
    }
    Ok(())
}

pub fn discard_all_voice_caches(data_dir: &Path) -> Result<(), String> {
    let dir = voice_cache_dir(data_dir);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn save_audio_to_cache(data_dir: &Path, audio_b64: &str, mime_type: &str) -> Result<PathBuf, String> {
    let clean = audio_b64.split(";base64,").last().unwrap_or(audio_b64).trim();
    let bytes = STANDARD
        .decode(clean.as_bytes())
        .map_err(|e| format!("base64 decode audio failed: {e}"))?;
    let ext = if mime_type.contains("wav") {
        "wav"
    } else if mime_type.contains("ogg") {
        "ogg"
    } else if mime_type.contains("m4a") {
        "m4a"
    } else {
        "webm"
    };
    let dir = voice_cache_dir(data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = Local::now().format("%Y%m%d-%H%M%S-%3f").to_string();
    let path = dir.join(format!("voice-{stamp}.{ext}"));
    write_atomic_bytes(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn transcribe_wav_file(data_dir: &Path, audio_path: &Path, language: &str) -> Result<String, String> {
    let dir = speech_dir(data_dir);
    let engine = dir.join("whisper-cli.exe");
    let model = dir.join("ggml-model.bin");
    if !engine.is_file() || !model.is_file() {
        return Err("local speech engine/model not installed".to_string());
    }
    let lang = if language.trim().is_empty() { "ja" } else { language.trim() };
    let out = Command::new(&engine)
        .args([
            "-m",
            &model.to_string_lossy(),
            "-f",
            &audio_path.to_string_lossy(),
            "-l",
            lang,
            "--no-timestamps",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("whisper-cli spawn failed: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("whisper-cli failed: {stderr}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn sniff_audio_mime(bytes: &[u8], path: Option<&Path>) -> &'static str {
    if bytes.len() >= 8 && &bytes[4..8] == b"ftyp" {
        return "audio/mp4";
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"OggS" {
        return "audio/ogg";
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        return "audio/wav";
    }
    if let Some(ext) = path.and_then(|p| p.extension()).and_then(|e| e.to_str()) {
        match ext.to_ascii_lowercase().as_str() {
            "wav" => return "audio/wav",
            "ogg" => return "audio/ogg",
            "m4a" | "mp4" => return "audio/mp4",
            _ => {}
        }
    }
    "audio/webm"
}

fn strip_v1beta_suffix(base: &str) -> String {
    let s = base.trim().trim_end_matches('/');
    s.strip_suffix("/v1beta").unwrap_or(s).to_string()
}

fn read_fallback_gemini_key(data_dir: &Path) -> String {
    let Ok(raw) = std::fs::read_to_string(data_dir.join("config.json")) else {
        return String::new();
    };
    let Ok(val) = serde_json::from_str::<Value>(&raw) else {
        return String::new();
    };
    for ptr in ["/voice/apiKey", "/vision/apiKey", "/text/apiKey"] {
        if let Some(k) = val.pointer(ptr).and_then(|v| v.as_str()) {
            let trimmed = k.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

fn post_gemini_json(
    agent: &ureq::Agent,
    url: &str,
    api_key_header: Option<&str>,
    body: &Value,
) -> Result<Value, String> {
    let mut req = agent
        .post(url)
        .set("Content-Type", "application/json");
    if let Some(k) = api_key_header {
        req = req.set("x-goog-api-key", k);
    }
    let payload = serde_json::to_string(body).map_err(|e| format!("JSON encode error: {e}"))?;
    let resp = req.send_string(&payload).map_err(|e| match e {
        ureq::Error::Status(code, r) => {
            let msg = r.into_string().unwrap_or_default();
            format!("Gemini APIエラー ({code}): {msg}")
        }
        ureq::Error::Transport(t) => format!("Gemini接続エラー: {t}"),
    })?;
    let text = resp
        .into_string()
        .map_err(|e| format!("Gemini応答読込エラー: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("Gemini応答解析エラー: {e}"))
}

fn transcribe_cloud(
    data_dir: &Path,
    audio_b64: &str,
    mime_type: &str,
    cfg: &Value,
) -> Result<String, String> {
    let clean_b64 = audio_b64
        .split(";base64,")
        .last()
        .unwrap_or(audio_b64)
        .trim();
    if clean_b64.is_empty() {
        return Err("empty audio data".to_string());
    }
    let raw_base = cfg
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let base = if raw_base.is_empty() {
        "https://generativelanguage.googleapis.com".to_string()
    } else {
        strip_v1beta_suffix(raw_base)
    };
    let model = cfg
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("gemini-3.5-transcribe");
    let mut api_key = cfg
        .get("apiKey")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if api_key.is_empty() {
        api_key = read_fallback_gemini_key(data_dir);
    }
    if api_key.is_empty() && base.contains("googleapis.com") {
        return Err("Gemini APIキーが設定されていません".to_string());
    }

    let api_style = cfg
        .get("apiStyle")
        .and_then(|v| v.as_str())
        .unwrap_or("auto")
        .trim();
    let use_interactions = api_style.eq_ignore_ascii_case("interactions")
        || (api_style.eq_ignore_ascii_case("auto") && model.contains("transcribe"));

    let timeout_secs = cfg
        .get("timeout")
        .and_then(|v| v.as_u64())
        .filter(|&n| n > 0)
        .unwrap_or(60);
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build();

    if use_interactions {
        let mode_str = cfg.get("mode").and_then(|v| v.as_str()).unwrap_or("smart");
        let mode_val = if mode_str.eq_ignore_ascii_case("verbatim") {
            json!({ "type": "verbatim" })
        } else {
            json!("smart")
        };
        let mut tc = serde_json::Map::new();
        tc.insert("mode".to_string(), mode_val);
        if let Some(codes) = cfg.get("languageCodes").and_then(|v| v.as_array()) {
            let list: Vec<String> = codes
                .iter()
                .filter_map(|v| v.as_str().map(str::trim))
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if !list.is_empty() {
                tc.insert("language_codes".to_string(), json!(list));
            }
        }
        let mime_clean = mime_type.split(';').next().unwrap_or("audio/webm").trim();
        let body = json!({
            "model": model.trim_start_matches("models/"),
            "store": false,
            "input": [{
                "type": "audio",
                "data": clean_b64,
                "mime_type": mime_clean
            }],
            "generation_config": {
                "transcription_config": Value::Object(tc)
            }
        });
        let url = format!("{base}/v1beta/interactions");
        let val = post_gemini_json(&agent, &url, Some(&api_key), &body)?;
        if let Some(status) = val.get("status").and_then(|v| v.as_str()) {
            if !status.is_empty() && status != "completed" {
                return Err(format!("Gemini APIエラー (status: {status})"));
            }
        }
        if let Some(out) = val.get("output_text").and_then(|v| v.as_str()) {
            let trimmed = out.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }
        let mut combined = String::new();
        if let Some(steps) = val.get("steps").and_then(|v| v.as_array()) {
            for step in steps {
                if step.get("type").and_then(|v| v.as_str()) != Some("model_output") {
                    continue;
                }
                if let Some(contents) = step.get("content").and_then(|v| v.as_array()) {
                    for c in contents {
                        if c.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                                combined.push_str(t);
                            }
                        }
                    }
                }
            }
        }
        let trimmed = combined.trim();
        if trimmed.is_empty() {
            return Err("話し声が検出されませんでした".to_string());
        }
        return Ok(trimmed.to_string());
    }

    // generateContent fallback (gemini-2.x / general models)
    let prompt = cfg
        .get("prompt")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("この音声を正確に文字起こししてください。前置きや解説は不要です。句読点を含む自然な日本語テキストのみを出力してください。");
    let mime_clean = mime_type.split(';').next().unwrap_or("audio/webm").trim();
    let url = format!(
        "{base}/v1beta/models/{}:generateContent?key={api_key}",
        model.trim_start_matches("models/")
    );
    let body = json!({
        "contents": [{
            "parts": [
                { "text": prompt },
                { "inline_data": { "mime_type": mime_clean, "data": clean_b64 } }
            ]
        }]
    });
    let val = post_gemini_json(&agent, &url, None, &body)?;
    let text = val
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if text.is_empty() {
        return Err("文字起こし結果が空でした".to_string());
    }
    Ok(text.to_string())
}

pub fn transcribe_cached_file_with_config(
    data_dir: &Path,
    cache_path: &Path,
    voice_config_json: &str,
    delete_on_success: bool,
) -> Result<String, String> {
    if !cache_path.is_file() {
        return Err("キャッシュファイルが見つかりません".to_string());
    }
    let cfg: Value = serde_json::from_str(voice_config_json).unwrap_or_else(|_| json!({}));
    let engine = cfg
        .get("engine")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    let res = if engine == "whisper-local" {
        let lang = cfg
            .pointer("/whisper/language")
            .and_then(|v| v.as_str())
            .unwrap_or("ja");
        let local_res = transcribe_wav_file(data_dir, cache_path, lang);
        if local_res.is_err()
            && cfg
                .pointer("/whisper/cloudFallback")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        {
            let bytes = std::fs::read(cache_path).map_err(|e| e.to_string())?;
            let mime = sniff_audio_mime(&bytes, Some(cache_path));
            let b64 = STANDARD.encode(&bytes);
            transcribe_cloud(data_dir, &b64, mime, &cfg)
        } else {
            local_res
        }
    } else {
        let bytes = std::fs::read(cache_path).map_err(|e| e.to_string())?;
        let mime = sniff_audio_mime(&bytes, Some(cache_path));
        let b64 = STANDARD.encode(&bytes);
        transcribe_cloud(data_dir, &b64, mime, &cfg)
    };

    if res.is_ok() && delete_on_success {
        let _ = std::fs::remove_file(cache_path);
        let kept = cache_path.with_extension("kept");
        if kept.exists() {
            let _ = std::fs::remove_file(kept);
        }
    }
    res
}

pub fn transcribe_audio_async(
    data_dir: PathBuf,
    req_id: String,
    audio_b64: String,
    mime_type: String,
    voice_config_json: String,
    sender: Option<EventSender>,
) {
    std::thread::spawn(move || {
        let cfg: Value = serde_json::from_str(&voice_config_json).unwrap_or_else(|_| json!({}));
        let engine = cfg
            .get("engine")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let no_cache = cfg
            .get("noCache")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let res = if engine == "whisper-local" {
            let lang = cfg
                .pointer("/whisper/language")
                .and_then(|v| v.as_str())
                .unwrap_or("ja");
            match save_audio_to_cache(&data_dir, &audio_b64, &mime_type) {
                Ok(p) => {
                    let r = transcribe_wav_file(&data_dir, &p, lang);
                    if r.is_ok() {
                        let _ = std::fs::remove_file(&p);
                    }
                    r.map_err(|e| (e, p.to_string_lossy().into_owned()))
                }
                Err(e) => Err((e, String::new())),
            }
        } else {
            transcribe_cloud(&data_dir, &audio_b64, &mime_type, &cfg).map_err(|e| {
                let cp = if no_cache {
                    String::new()
                } else {
                    save_audio_to_cache(&data_dir, &audio_b64, &mime_type)
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default()
                };
                (e, cp)
            })
        };

        let (text, error, cache_path) = match res {
            Ok(t) => (t, String::new(), String::new()),
            Err((e, cp)) => (String::new(), e, cp),
        };
        if let Some(tx) = sender {
            tx(RpcEvent {
                topic: "voice:result".to_string(),
                payload: json!({
                    "reqId": req_id,
                    "text": text,
                    "error": error,
                    "cachePath": cache_path,
                    "refineErr": ""
                }),
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn pure_rust_wav_encoder_writes_valid_riff_header_and_pcm16_samples() {
        let wav = encode_wav_pcm16(&[0, 16384, -16384, 32767], 16_000, 1);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 44 + 8);
    }

    #[test]
    fn resample_f32_to_mono_16k_downsamples_48k_stereo() {
        let stereo_48k = vec![0.5f32; 480 * 2]; // 10ms at 48kHz stereo
        let mono_16k = resample_f32_to_mono_16k_i16(&stereo_48k, 48_000, 2);
        assert_eq!(mono_16k.len(), 160);
        assert!(mono_16k[0] > 16000);
    }

    #[test]
    fn voice_cache_lifecycle_save_list_keep_and_discard() {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("syki-speech-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();

        let b64 = STANDARD.encode(encode_wav_pcm16(&[1, 2, 3, 4], 16_000, 1));
        let p = save_audio_to_cache(&dir, &b64, "audio/wav").unwrap();
        assert!(p.is_file());

        keep_voice_cache(&p).unwrap();
        let list = list_voice_caches(&dir);
        assert_eq!(list.len(), 1);
        assert!(list[0].kept);

        discard_voice_cache(&p).unwrap();
        assert!(list_voice_caches(&dir).is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }
}
