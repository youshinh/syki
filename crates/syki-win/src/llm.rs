//! AI Inference (`Ctrl+L` / `Ctrl+K` / `Alt+C` / Tab), Image Generation,
//! and Vision AI OCR (`F23`, AI-only — zero WinRT) (#12).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use syki_core::rpc::RpcEvent;

use crate::rpc_dispatch::EventSender;
use crate::shell_pipe::validate_cli_command;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmConfig {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

impl LlmConfig {
    pub fn from_json_str(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "null" {
            return Self::default();
        }
        serde_json::from_str(trimmed).unwrap_or_default()
    }

    pub fn resolved_provider(&self) -> String {
        let p = self.provider.trim().to_ascii_lowercase();
        if !p.is_empty() {
            return p;
        }
        let u = self.base_url.to_ascii_lowercase();
        if u.contains("11434") || u.contains("ollama") {
            "ollama".to_string()
        } else if u.contains("anthropic") {
            "anthropic".to_string()
        } else if u.contains("generativelanguage.googleapis.com") {
            "gemini".to_string()
        } else {
            "openai".to_string()
        }
    }

    pub fn resolved_base_url(&self) -> String {
        let u = self.base_url.trim().trim_end_matches('/');
        if !u.is_empty() {
            return u.to_string();
        }
        match self.resolved_provider().as_str() {
            "ollama" => "http://127.0.0.1:11434".to_string(),
            "anthropic" => "https://api.anthropic.com/v1".to_string(),
            "gemini" => "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
            _ => "https://api.openai.com/v1".to_string(),
        }
    }

    pub fn resolved_model(&self) -> String {
        let m = self.model.trim();
        if !m.is_empty() {
            return m.to_string();
        }
        match self.resolved_provider().as_str() {
            "ollama" => "qwen2.5:7b".to_string(),
            "anthropic" => "claude-3-5-sonnet-latest".to_string(),
            "gemini" => "gemini-2.5-flash".to_string(),
            _ => "gpt-4o-mini".to_string(),
        }
    }

    pub fn resolved_api_key(&self) -> String {
        let k = self.api_key.trim();
        if !k.is_empty() {
            return k.to_string();
        }
        match self.resolved_provider().as_str() {
            "gemini" => std::env::var("GEMINI_API_KEY").unwrap_or_default(),
            "anthropic" => std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
            "openai" => std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            _ => String::new(),
        }
    }
}

/// Probes local Ollama (`127.0.0.1:11434/api/tags`) or configured API key presence.
pub fn detect_llm_provider() -> Value {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(400))
        .build();
    if let Ok(resp) = agent.get("http://127.0.0.1:11434/api/tags").call() {
        if let Ok(raw) = resp.into_string() {
            if let Ok(val) = serde_json::from_str::<Value>(&raw) {
                let first_model = val
                    .get("models")
                    .and_then(|m| m.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|m| m.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("llama3.2");
                return json!({
                    "provider": "ollama",
                    "baseUrl": "http://127.0.0.1:11434",
                    "model": first_model,
                    "available": true
                });
            }
        }
    }
    if std::env::var("GEMINI_API_KEY").map(|s| !s.is_empty()).unwrap_or(false) {
        return json!({
            "provider": "gemini",
            "baseUrl": "https://generativelanguage.googleapis.com/v1beta/openai",
            "model": "gemini-2.5-flash",
            "available": true
        });
    }
    if std::env::var("OPENAI_API_KEY").map(|s| !s.is_empty()).unwrap_or(false) {
        return json!({
            "provider": "openai",
            "baseUrl": "https://api.openai.com/v1",
            "model": "gpt-4o-mini",
            "available": true
        });
    }
    json!({
        "provider": "ollama",
        "baseUrl": "http://127.0.0.1:11434",
        "model": "",
        "available": false
    })
}

fn http_agent_with_timeout(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(timeout).build()
}

fn http_agent() -> ureq::Agent {
    http_agent_with_timeout(Duration::from_secs(60))
}

fn is_timeout_error(err: &dyn std::error::Error) -> bool {
    let mut cur = err.source();
    while let Some(e) = cur {
        if let Some(io_err) = e.downcast_ref::<std::io::Error>() {
            if matches!(
                io_err.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) {
                return true;
            }
        }
        cur = e.source();
    }
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("timed out") || msg.contains("timeout") || msg.contains("deadline")
}

fn send_json_post(req: ureq::Request, body: &Value) -> Result<Value, String> {
    let resp = req
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| {
            if is_timeout_error(&e) {
                format!("HTTP request timed out: {e}")
            } else {
                format!("HTTP request failed: {e}")
            }
        })?;
    let raw = resp
        .into_string()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::TimedOut {
                format!("HTTP read timed out: {e}")
            } else {
                format!("HTTP read failed: {e}")
            }
        })?;
    serde_json::from_str::<Value>(&raw).map_err(|e| format!("invalid JSON response: {e}"))
}

fn is_local_openai_compat(base: &str) -> bool {
    let lower = base.trim().to_ascii_lowercase();
    lower.contains("://localhost")
        || lower.contains("://127.0.0.1")
        || lower.contains("://[::1]")
}

fn resolve_openai_chat_url(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.ends_with("/chat/completions") {
        b.to_string()
    } else if b.ends_with("/v1") || b.ends_with("/v1beta") || b.ends_with("/openai") {
        format!("{b}/chat/completions")
    } else {
        format!("{b}/v1/chat/completions")
    }
}

fn strip_think_tags(s: &str) -> String {
    let mut out = s.to_string();
    while let Some(start) = out.find("<think>") {
        if let Some(rel_end) = out[start..].find("</think>") {
            let end = start + rel_end + "</think>".len();
            out.replace_range(start..end, "");
        } else {
            out.truncate(start);
            break;
        }
    }
    out.trim().to_string()
}

/// Sends a text prompt to the configured LLM endpoint with a custom timeout.
pub fn query_llm_sync_with_timeout(
    prompt: &str,
    cfg: &LlmConfig,
    timeout: Duration,
) -> Result<String, String> {
    let provider = cfg.resolved_provider();
    let base = cfg.resolved_base_url();
    let model = cfg.resolved_model();
    let api_key = cfg.resolved_api_key();
    let agent = http_agent_with_timeout(timeout);

    if provider == "ollama" {
        let url = format!("{base}/api/chat");
        let body = json!({
            "model": model,
            "stream": false,
            "messages": [{ "role": "user", "content": prompt }]
        });
        let val = send_json_post(agent.post(&url), &body)?;
        return val
            .pointer("/message/content")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| "Ollama response missing message.content".to_string());
    }

    if provider == "anthropic" {
        let url = format!("{base}/messages");
        let body = json!({
            "model": model,
            "max_tokens": cfg.max_tokens.unwrap_or(2048),
            "messages": [{ "role": "user", "content": prompt }]
        });
        let mut req = agent
            .post(&url)
            .set("anthropic-version", "2023-06-01");
        if !api_key.is_empty() {
            req = req.set("x-api-key", &api_key);
        }
        let val = send_json_post(req, &body)?;
        return val
            .pointer("/content/0/text")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| "Anthropic response missing content[0].text".to_string());
    }

    if (base.contains("googleapis.com") && !base.contains("/openai")) || provider == "gemini" {
        let root = base
            .trim_end_matches('/')
            .strip_suffix("/v1beta")
            .unwrap_or(base.trim_end_matches('/'));
        let clean_model = model.trim_start_matches("models/");
        let url = format!("{root}/v1beta/models/{clean_model}:generateContent?key={api_key}");
        let mut body = json!({
            "contents": [{
                "role": "user",
                "parts": [{ "text": prompt }]
            }]
        });
        let mut gen_cfg = json!({});
        if let Some(t) = cfg.temperature {
            gen_cfg["temperature"] = json!(t);
        }
        if let Some(m) = cfg.max_tokens {
            gen_cfg["maxOutputTokens"] = json!(m);
        }
        if !gen_cfg.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            body["generationConfig"] = gen_cfg;
        }
        let val = send_json_post(agent.post(&url), &body)?;
        return val
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(|v| v.as_str())
            .map(strip_think_tags)
            .ok_or_else(|| "Gemini response missing candidates[0].content.parts[0].text".to_string());
    }

    // OpenAI-compatible (`/chat/completions`, works for OpenAI, Gemini OpenAI-compat, LM Studio, Strata)
    let url = resolve_openai_chat_url(&base);
    let mut body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }]
    });
    if is_local_openai_compat(&base) {
        body["chat_template_kwargs"] = json!({ "enable_thinking": false });
    }
    if let Some(t) = cfg.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(m) = cfg.max_tokens {
        body["max_tokens"] = json!(m);
    }
    let mut req = agent.post(&url);
    if !api_key.is_empty() {
        req = req.set("Authorization", &format!("Bearer {api_key}"));
    }
    let val = send_json_post(req, &body)?;
    val.pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .map(strip_think_tags)
        .ok_or_else(|| "LLM response missing choices[0].message.content".to_string())
}

/// Sends a text prompt to the configured LLM endpoint and returns assistant text.
pub fn query_llm_sync(prompt: &str, cfg: &LlmConfig) -> Result<String, String> {
    query_llm_sync_with_timeout(prompt, cfg, Duration::from_secs(60))
}

/// Sends a Vision AI OCR / multimodal image prompt (AI-only; no WinRT OCR).
pub fn query_vision_sync(
    prompt: &str,
    base64_data: &str,
    mime_type: &str,
    cfg: &LlmConfig,
) -> Result<String, String> {
    let provider = cfg.resolved_provider();
    let base = cfg.resolved_base_url();
    let model = cfg.resolved_model();
    let api_key = cfg.resolved_api_key();
    let mime = if mime_type.trim().is_empty() {
        "image/png"
    } else {
        mime_type.trim()
    };
    let clean_b64 = base64_data
        .split(";base64,")
        .last()
        .unwrap_or(base64_data)
        .trim();

    if provider == "ollama" {
        let url = format!("{base}/api/chat");
        let body = json!({
            "model": model,
            "stream": false,
            "messages": [{
                "role": "user",
                "content": prompt,
                "images": [clean_b64]
            }]
        });
        let val = send_json_post(http_agent().post(&url), &body)?;
        return val
            .pointer("/message/content")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| "Ollama vision response missing message.content".to_string());
    }

    let is_gemini = base.contains("googleapis.com")
        || base.trim_end_matches('/').ends_with("/v1beta")
        || (provider == "gemini");
    if is_gemini {
        let root = base
            .trim_end_matches('/')
            .strip_suffix("/v1beta")
            .unwrap_or(base.trim_end_matches('/'));
        let clean_model = model.trim_start_matches("models/");
        let url = format!("{root}/v1beta/models/{clean_model}:generateContent?key={api_key}");
        let body = json!({
            "contents": [{
                "parts": [
                    { "text": prompt },
                    { "inline_data": { "mime_type": mime, "data": clean_b64 } }
                ]
            }]
        });
        let val = send_json_post(http_agent().post(&url), &body)?;
        return val
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Gemini vision response missing candidates[0].content.parts[0].text".to_string());
    }

    let data_url = format!("data:{mime};base64,{clean_b64}");
    let url = resolve_openai_chat_url(&base);
    let mut body = json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": { "url": data_url } }
            ]
        }]
    });
    if is_local_openai_compat(&base) {
        body["chat_template_kwargs"] = json!({ "enable_thinking": false });
    }
    let mut req = http_agent().post(&url);
    if !api_key.is_empty() {
        req = req.set("Authorization", &format!("Bearer {api_key}"));
    }
    let val = send_json_post(req, &body)?;
    val.pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .map(strip_think_tags)
        .ok_or_else(|| "Vision LLM response missing choices[0].message.content".to_string())
}

pub fn query_llm_async(req_id: String, prompt: String, config_json: String, sender: Option<EventSender>) {
    std::thread::spawn(move || {
        let cfg = LlmConfig::from_json_str(&config_json);
        let (result, error) = match query_llm_sync(&prompt, &cfg) {
            Ok(text) => (text, String::new()),
            Err(err) => (String::new(), err),
        };
        if let Some(tx) = sender {
            tx(RpcEvent {
                topic: "llm:result".to_string(),
                payload: json!({ "reqId": req_id, "result": result, "error": error }),
            });
        }
    });
}

pub fn autocomplete_async(
    req_id: String,
    prefix: String,
    suffix: String,
    config_json: String,
    sender: Option<EventSender>,
) {
    std::thread::spawn(move || {
        let cfg = LlmConfig::from_json_str(&config_json);
        let tail: String = prefix.chars().rev().take(1200).collect::<Vec<_>>().into_iter().rev().collect();
        let head: String = suffix.chars().take(400).collect();
        let prompt = format!(
            "Continue the markdown text naturally at <CURSOR>. Reply ONLY with the continuation text (no markdown fences, no explanation).\n\n{tail}<CURSOR>{head}"
        );
        let (suggestion, error) = match query_llm_sync(&prompt, &cfg) {
            Ok(text) => (text, String::new()),
            Err(err) => (String::new(), err),
        };
        if let Some(tx) = sender {
            tx(RpcEvent {
                topic: "autocomplete:result".to_string(),
                payload: json!({ "reqId": req_id, "suggestion": suggestion, "error": error }),
            });
        }
    });
}

pub fn query_vision_async(
    req_id: String,
    prompt: String,
    base64_data: String,
    mime_type: String,
    config_json: String,
    sender: Option<EventSender>,
) {
    std::thread::spawn(move || {
        let cfg = LlmConfig::from_json_str(&config_json);
        let (result, error) = match query_vision_sync(&prompt, &base64_data, &mime_type, &cfg) {
            Ok(text) => (text, String::new()),
            Err(err) => (String::new(), err),
        };
        if let Some(tx) = sender {
            tx(RpcEvent {
                topic: "llm:result".to_string(),
                payload: json!({ "reqId": req_id, "result": result, "error": error }),
            });
        }
    });
}

fn ext_for_image_mime(mime: &str) -> &'static str {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "png",
    }
}

fn save_generated_image_markdown(
    data_dir: &Path,
    note_path: &str,
    mime: &str,
    b64_data: &str,
) -> Result<String, String> {
    let trimmed_note = note_path.trim();
    let base_dir = if trimmed_note.is_empty() {
        String::new()
    } else {
        Path::new(trimmed_note)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    let asset = crate::assets_store::save_asset(
        data_dir,
        &base_dir,
        ext_for_image_mime(mime),
        b64_data,
    )?;
    let target = if !asset.rel_path.is_empty() {
        asset.rel_path
    } else {
        asset.file_url
    };
    let safe = target
        .replace('\\', "/")
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29");
    Ok(format!("![Generated Image]({safe})"))
}

pub fn generate_image_async(
    req_id: String,
    prompt: String,
    config_json: String,
    note_path: String,
    data_dir: PathBuf,
    sender: Option<EventSender>,
) {
    std::thread::spawn(move || {
        let cfg = LlmConfig::from_json_str(&config_json);
        let raw_cfg: Value = serde_json::from_str(&config_json).unwrap_or_else(|_| json!({}));
        let base = cfg.resolved_base_url();
        let api_key = cfg.resolved_api_key();
        let model = if cfg.model.trim().is_empty() {
            "gemini-3.1-flash-lite-image"
        } else {
            cfg.model.trim()
        };
        let is_gemini = model.starts_with("gemini")
            || base.contains("googleapis.com")
            || cfg.resolved_provider() == "gemini";

        let (result, error) = if is_gemini {
            let root = if base.contains("googleapis.com") {
                base.trim_end_matches('/')
                    .strip_suffix("/v1beta")
                    .unwrap_or(base.trim_end_matches('/'))
            } else {
                "https://generativelanguage.googleapis.com"
            };
            let clean_model = model.trim_start_matches("models/");
            let url = format!("{root}/v1beta/models/{clean_model}:generateContent?key={api_key}");
            let mut gen_cfg = json!({
                "responseModalities": ["IMAGE", "TEXT"]
            });
            if let Some(ar) = raw_cfg
                .get("aspectRatio")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                gen_cfg["imageConfig"] = json!({ "aspectRatio": ar });
            }
            let body = json!({
                "contents": [{ "role": "user", "parts": [{ "text": prompt }] }],
                "generationConfig": gen_cfg
            });
            match send_json_post(http_agent().post(&url), &body) {
                Ok(val) => {
                    let found = val
                        .pointer("/candidates/0/content/parts")
                        .and_then(|p| p.as_array())
                        .and_then(|parts| {
                            parts.iter().find_map(|part| {
                                let inline =
                                    part.get("inlineData").or_else(|| part.get("inline_data"))?;
                                let data = inline.get("data")?.as_str()?;
                                let mime = inline
                                    .get("mimeType")
                                    .or_else(|| inline.get("mime_type"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("image/png");
                                Some((mime.to_string(), data.to_string()))
                            })
                        });
                    match found {
                        Some((mime, data)) => {
                            match save_generated_image_markdown(&data_dir, &note_path, &mime, &data) {
                                Ok(md) => (md, String::new()),
                                Err(e) => (String::new(), format!("failed to save generated image: {e}")),
                            }
                        }
                        None => (String::new(), "Gemini image response missing inlineData".to_string()),
                    }
                }
                Err(e) => (String::new(), format!("image generation failed: {e}")),
            }
        } else {
            let url = format!("{base}/images/generations");
            let body = json!({
                "model": model,
                "prompt": prompt,
                "n": 1,
                "response_format": "b64_json"
            });
            let mut req = http_agent().post(&url);
            if !api_key.is_empty() {
                req = req.set("Authorization", &format!("Bearer {api_key}"));
            }
            match send_json_post(req, &body) {
                Ok(val) => {
                    if let Some(b64) = val.pointer("/data/0/b64_json").and_then(|v| v.as_str()) {
                        match save_generated_image_markdown(&data_dir, &note_path, "image/png", b64) {
                            Ok(md) => (md, String::new()),
                            Err(e) => (String::new(), format!("failed to save generated image: {e}")),
                        }
                    } else if let Some(u) = val.pointer("/data/0/url").and_then(|v| v.as_str()) {
                        (format!("![Generated Image]({u})"), String::new())
                    } else {
                        (String::new(), "image response missing b64_json/url".to_string())
                    }
                }
                Err(e) => (String::new(), format!("image generation failed: {e}")),
            }
        };
        if let Some(tx) = sender {
            tx(RpcEvent {
                topic: "llm:result".to_string(),
                payload: json!({ "reqId": req_id, "result": result, "error": error }),
            });
        }
    });
}

pub fn process_dropped_media_async(
    req_id: String,
    _file_name: String,
    mime_type: String,
    base64_data: String,
    vision_config_json: String,
    sender: Option<EventSender>,
) {
    std::thread::spawn(move || {
        let cfg = LlmConfig::from_json_str(&vision_config_json);
        let (text, error) = if mime_type.starts_with("image/") || mime_type.is_empty() {
            match query_vision_sync(
                "Extract all readable text and key structure from this image in Markdown.",
                &base64_data,
                &mime_type,
                &cfg,
            ) {
                Ok(t) => (t, String::new()),
                Err(e) => (String::new(), e),
            }
        } else {
            (String::new(), format!("unsupported dropped media mimeType: {mime_type}"))
        };
        if let Some(tx) = sender {
            tx(RpcEvent {
                topic: "media:dropped".to_string(),
                payload: json!({ "reqId": req_id, "text": text, "error": error }),
            });
        }
    });
}

pub fn generate_cli_command_async(
    req_id: String,
    prompt_text: String,
    config_json: String,
    context_meta_json: String,
    sender: Option<EventSender>,
) {
    std::thread::spawn(move || {
        let cfg = LlmConfig::from_json_str(&config_json);
        let prompt = format!(
            "Return ONLY a single-line Windows cmd.exe command for the following request (no markdown, no explanation).\nContext: {context_meta_json}\nRequest: {prompt_text}"
        );
        let (command, error, validation) = match query_llm_sync(&prompt, &cfg) {
            Ok(raw) => {
                let cmd = raw
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && !l.starts_with("```"))
                    .unwrap_or("")
                    .to_string();
                match validate_cli_command(&cmd) {
                    Ok(()) => (cmd, String::new(), json!({ "isBlocked": false })),
                    Err(reason) => (
                        cmd,
                        String::new(),
                        json!({ "isBlocked": true, "reason": reason }),
                    ),
                }
            }
            Err(e) => (String::new(), e, json!({ "isBlocked": false })),
        };
        if let Some(tx) = sender {
            tx(RpcEvent {
                topic: "cli:command".to_string(),
                payload: json!({
                    "reqId": req_id,
                    "command": command,
                    "error": error,
                    "validation": validation
                }),
            });
        }
    });
}

/// Default timeout (8 seconds) for fast Decision model calls.
pub const DEFAULT_DECISION_TIMEOUT_MS: u64 = 8_000;

/// Configuration for the dedicated Decision model slot (`config.json` `action` compatible).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionConfig {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl DecisionConfig {
    pub fn from_json_str(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "null" {
            return Self::default();
        }
        serde_json::from_str(trimmed).unwrap_or_default()
    }

    pub fn to_llm_config(&self) -> LlmConfig {
        LlmConfig {
            provider: self.provider.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            api_key: self.api_key.clone(),
            temperature: self.temperature,
            max_tokens: self.max_tokens,
        }
    }

    pub fn resolved_timeout(&self) -> Duration {
        let ms = self
            .timeout_ms
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_DECISION_TIMEOUT_MS);
        Duration::from_millis(ms)
    }
}

/// Candidate choice passed to the Decision model.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecisionChoice {
    pub id: String,
    #[serde(default)]
    pub description: String,
}

/// Structured result emitted on `llm:decisionResult`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecisionResult {
    pub req_id: String,
    pub choice_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    pub by: String,
    pub ok: bool,
}

/// Thread-safe cancellation registry keyed by `req_id`.
#[derive(Debug, Default, Clone)]
pub struct LlmCancelRegistry {
    flags: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl LlmCancelRegistry {
    pub fn new() -> Self {
        Self {
            flags: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Registers a new cancellation flag for `req_id` and returns it.
    pub fn register(&self, req_id: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        if let Ok(mut map) = self.flags.lock() {
            map.insert(req_id.to_string(), Arc::clone(&flag));
        }
        flag
    }

    /// Marks `req_id` as cancelled. Returns `true` if the request was active.
    pub fn cancel(&self, req_id: &str) -> bool {
        if let Ok(map) = self.flags.lock() {
            if let Some(flag) = map.get(req_id) {
                flag.store(true, Ordering::SeqCst);
                return true;
            }
        }
        false
    }

    /// Removes `req_id` from the registry once the worker finishes.
    pub fn finish(&self, req_id: &str) {
        if let Ok(mut map) = self.flags.lock() {
            map.remove(req_id);
        }
    }

    /// Returns whether `req_id` is currently marked cancelled.
    pub fn is_cancelled(&self, req_id: &str) -> bool {
        if let Ok(map) = self.flags.lock() {
            if let Some(flag) = map.get(req_id) {
                return flag.load(Ordering::SeqCst);
            }
        }
        false
    }

    /// Number of currently tracked in-flight requests.
    pub fn active_count(&self) -> usize {
        self.flags.lock().map(|m| m.len()).unwrap_or(0)
    }
}

struct CancelGuard {
    registry: Option<Arc<LlmCancelRegistry>>,
    req_id: String,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if let Some(reg) = &self.registry {
            reg.finish(&self.req_id);
        }
    }
}

fn is_llm_config_configured(cfg: &LlmConfig) -> bool {
    !cfg.model.trim().is_empty()
        || !cfg.base_url.trim().is_empty()
        || !cfg.provider.trim().is_empty()
        || !cfg.api_key.trim().is_empty()
}

fn normalize_decision_llm_config(raw: &LlmConfig, fallback_cfg: Option<&LlmConfig>) -> LlmConfig {
    let mut cfg = raw.clone();
    let model_lower = cfg.model.trim().to_ascii_lowercase();
    let base_lower = cfg.base_url.trim().to_ascii_lowercase();

    if cfg.provider.trim().is_empty() {
        if base_lower.contains("11434") || base_lower.contains("ollama") {
            cfg.provider = "ollama".to_string();
        } else if model_lower.starts_with("gemini-")
            || base_lower.contains("generativelanguage.googleapis.com")
        {
            cfg.provider = "gemini".to_string();
        } else if model_lower.starts_with("claude-") || base_lower.contains("anthropic") {
            cfg.provider = "anthropic".to_string();
        } else if model_lower.starts_with("gpt-") || model_lower.starts_with("jev-") {
            cfg.provider = "openai".to_string();
        } else {
            cfg.provider = cfg.resolved_provider();
        }
    }

    if cfg.base_url.trim().is_empty() {
        if model_lower.starts_with("jev-") {
            if let Some(fb) = fallback_cfg {
                if !fb.base_url.trim().is_empty() {
                    cfg.base_url = fb.base_url.trim().trim_end_matches('/').to_string();
                }
            }
        }
        if cfg.base_url.trim().is_empty() {
            cfg.base_url = cfg.resolved_base_url();
        }
    }

    if cfg.model.trim().is_empty() {
        if cfg.provider.eq_ignore_ascii_case("gemini") {
            cfg.model = "gemini-decision-1".to_string();
        } else {
            cfg.model = cfg.resolved_model();
        }
    }

    if cfg.api_key.trim().is_empty() {
        if let Some(fb) = fallback_cfg {
            let same_provider = fb.resolved_provider().eq_ignore_ascii_case(&cfg.provider);
            if !fb.api_key.trim().is_empty()
                && (same_provider || model_lower.starts_with("jev-"))
            {
                cfg.api_key = fb.api_key.trim().to_string();
            }
        }
    }

    if cfg.temperature.is_none() {
        cfg.temperature = Some(0.0);
    }
    if cfg.max_tokens.is_none() {
        cfg.max_tokens = Some(128);
    }

    cfg
}

/// Statically resolves the Decision model configuration with zero network I/O.
///
/// Priority:
/// 1. Explicit `action_cfg` (`gemini-decision-1`, `jev-latest`, `gpt-6-luna`, Ollama, OpenAI-compat).
/// 2. Fallback to `default_cfg` when `action_cfg` is empty.
/// 3. `None` when both are empty (caller safely degrades to rule/fallback).
pub fn resolve_decision_config(
    action_cfg: &LlmConfig,
    default_cfg: &LlmConfig,
) -> Option<LlmConfig> {
    if is_llm_config_configured(action_cfg) {
        return Some(normalize_decision_llm_config(action_cfg, Some(default_cfg)));
    }
    if is_llm_config_configured(default_cfg) {
        return Some(normalize_decision_llm_config(default_cfg, None));
    }
    None
}

fn strip_code_fences(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let after_lang = match rest.find('\n') {
            Some(idx) => &rest[idx + 1..],
            None => rest,
        };
        let body = match after_lang.rfind("```") {
            Some(idx) => &after_lang[..idx],
            None => after_lang,
        };
        return body.trim().to_string();
    }
    trimmed.to_string()
}

fn extract_json_candidate(s: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        return Some(v);
    }
    if let (Some(start), Some(end)) = (s.find('{'), s.rfind('}')) {
        if start < end {
            if let Ok(v) = serde_json::from_str::<Value>(&s[start..=end]) {
                return Some(v);
            }
        }
    }
    None
}

fn match_valid_id<'a>(candidate: &str, valid_ids: &'a [&'a str]) -> Option<&'a str> {
    let cleaned = candidate
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .trim();
    valid_ids.iter().copied().find(|&id| id == cleaned)
}

/// Strictly extracts a choice ID matching `valid_ids` from JSON, fenced JSON, or plain text.
///
/// Returns `(choice_id, reason, used_fallback)`. Any unrecognized or empty output falls back
/// to `fallback` with `used_fallback = true` to prevent hallucinated IDs.
pub fn parse_decision_choice(
    raw: &str,
    valid_ids: &[&str],
    fallback: &str,
) -> (String, Option<String>, bool) {
    if valid_ids.is_empty() {
        return (
            fallback.to_string(),
            Some("no valid choices provided".to_string()),
            true,
        );
    }

    let cleaned = strip_think_tags(raw);
    let unfenced = strip_code_fences(&cleaned);
    if unfenced.is_empty() {
        return (
            fallback.to_string(),
            Some("empty decision response".to_string()),
            true,
        );
    }

    if let Some(val) = extract_json_candidate(&unfenced) {
        if let Some(obj) = val.as_object() {
            let reason = obj
                .get("reason")
                .or_else(|| obj.get("explanation"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);

            let id_field = obj
                .get("choiceId")
                .or_else(|| obj.get("choice_id"))
                .or_else(|| obj.get("choice"))
                .or_else(|| obj.get("id"))
                .or_else(|| obj.get("decision"))
                .and_then(|v| v.as_str());

            if let Some(raw_id) = id_field {
                if let Some(matched) = match_valid_id(raw_id, valid_ids) {
                    return (matched.to_string(), reason, false);
                }
                return (
                    fallback.to_string(),
                    Some(format!("unrecognized choiceId: {}", raw_id.trim())),
                    true,
                );
            }
            return (
                fallback.to_string(),
                Some("decision JSON missing choiceId field".to_string()),
                true,
            );
        } else if let Some(s) = val.as_str() {
            if let Some(matched) = match_valid_id(s, valid_ids) {
                return (matched.to_string(), None, false);
            }
            return (
                fallback.to_string(),
                Some(format!("unrecognized choice string: {}", s.trim())),
                true,
            );
        }
    }

    // Exact match against whole plain-text response
    if let Some(matched) = match_valid_id(&unfenced, valid_ids) {
        return (matched.to_string(), None, false);
    }

    // First-line exact match or `choiceId: <id>` style
    let mut lines = unfenced.lines().map(str::trim).filter(|l| !l.is_empty());
    if let Some(first_line) = lines.next() {
        let rest_reason: Option<String> = {
            let rest: Vec<&str> = lines.collect();
            if rest.is_empty() {
                None
            } else {
                Some(rest.join(" "))
            }
        };

        if let Some(matched) = match_valid_id(first_line, valid_ids) {
            return (matched.to_string(), rest_reason, false);
        }

        if let Some((k, v)) = first_line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            if matches!(key.as_str(), "choiceid" | "choice_id" | "choice" | "id" | "decision") {
                if let Some(matched) = match_valid_id(v, valid_ids) {
                    return (matched.to_string(), rest_reason, false);
                }
            }
        }
    }

    (
        fallback.to_string(),
        Some("response did not match any valid choice".to_string()),
        true,
    )
}

fn build_decision_prompt(prompt: &str, choices: &[DecisionChoice]) -> String {
    let mut out = String::from(
        "Select exactly ONE choice ID from the allowed list below. Reply ONLY with a JSON object {\"choiceId\":\"<id>\",\"reason\":\"<short reason>\"}.\n\nAllowed choices:\n",
    );
    for c in choices {
        if c.description.trim().is_empty() {
            out.push_str(&format!("- {}\n", c.id.trim()));
        } else {
            out.push_str(&format!("- {}: {}\n", c.id.trim(), c.description.trim()));
        }
    }
    out.push_str("\nInput:\n");
    out.push_str(prompt);
    out
}

fn is_gemini_decisions_model(cfg: &LlmConfig) -> bool {
    cfg.resolved_model()
        .to_ascii_lowercase()
        .starts_with("gemini-decision")
}

fn gemini_decisions_endpoint(cfg: &LlmConfig) -> String {
    let raw = cfg.base_url.trim().trim_end_matches('/');
    if raw.is_empty() || raw.to_ascii_lowercase().contains("generativelanguage.googleapis.com") {
        "https://generativelanguage.googleapis.com/v1beta/decisions".to_string()
    } else if raw.ends_with("/decisions") {
        raw.to_string()
    } else {
        format!("{raw}/v1beta/decisions")
    }
}

fn query_gemini_decisions_sync(
    prompt: &str,
    choices: &[DecisionChoice],
    cfg: &LlmConfig,
    timeout: Duration,
) -> Result<(String, Option<String>), String> {
    let api_key = cfg.resolved_api_key();
    if api_key.is_empty() {
        return Err("missing GEMINI_API_KEY for gemini-decision-1".to_string());
    }
    let endpoint = gemini_decisions_endpoint(cfg);
    let mut criteria = serde_json::Map::new();
    for c in choices {
        let id = c.id.trim();
        if id.is_empty() {
            continue;
        }
        let desc = if c.description.trim().is_empty() {
            id.to_string()
        } else {
            c.description.trim().to_string()
        };
        criteria.insert(id.to_string(), Value::String(desc));
    }
    let body = json!({
        "model": cfg.resolved_model(),
        "input": prompt,
        "questions": {
            "decision": {
                "type": "choice",
                "instructions": "Select the best matching choice for the input.",
                "criteria": Value::Object(criteria),
            }
        }
    });
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let resp = agent
        .post(&endpoint)
        .set("x-goog-api-key", &api_key)
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| format!("gemini decisions error: {e}"))?;
    let raw_resp = resp
        .into_string()
        .map_err(|e| format!("gemini decisions read error: {e}"))?;
    let val: Value = serde_json::from_str(&raw_resp)
        .map_err(|e| format!("gemini decisions parse error: {e}"))?;
    let ans = val
        .get("answers")
        .and_then(|a| a.get("decision"))
        .ok_or_else(|| "missing answers.decision in response".to_string())?;
    let choice = ans
        .get("choice")
        .and_then(|c| c.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "missing answers.decision.choice".to_string())?;
    let reason = ans
        .get("confidence")
        .and_then(|c| c.as_f64())
        .map(|conf| format!("confidence={conf:.2}"));
    Ok((choice, reason))
}

/// Synchronously calls the Decision model with cancellation & timeout support.
pub fn query_decision_sync(
    req_id: &str,
    prompt: &str,
    choices: &[DecisionChoice],
    fallback: &str,
    cfg: &LlmConfig,
    timeout: Duration,
    cancel_flag: Option<&AtomicBool>,
) -> DecisionResult {
    if cancel_flag.map(|f| f.load(Ordering::SeqCst)).unwrap_or(false) {
        return DecisionResult {
            req_id: req_id.to_string(),
            choice_id: fallback.to_string(),
            reason: Some("cancelled before request".to_string()),
            by: "cancelled".to_string(),
            ok: false,
        };
    }

    let valid_ids: Vec<&str> = choices.iter().map(|c| c.id.as_str()).collect();
    let raw_res = if is_gemini_decisions_model(cfg) {
        query_gemini_decisions_sync(prompt, choices, cfg, timeout).map(|(choice, reason)| {
            if let Some(r) = reason {
                json!({ "choiceId": choice, "reason": r }).to_string()
            } else {
                choice
            }
        })
    } else {
        let full_prompt = build_decision_prompt(prompt, choices);
        query_llm_sync_with_timeout(&full_prompt, cfg, timeout)
    };

    if cancel_flag.map(|f| f.load(Ordering::SeqCst)).unwrap_or(false) {
        return DecisionResult {
            req_id: req_id.to_string(),
            choice_id: fallback.to_string(),
            reason: Some("cancelled".to_string()),
            by: "cancelled".to_string(),
            ok: false,
        };
    }

    match raw_res {
        Ok(raw_text) => {
            let (choice_id, reason, used_fallback) =
                parse_decision_choice(&raw_text, &valid_ids, fallback);
            DecisionResult {
                req_id: req_id.to_string(),
                choice_id,
                reason,
                by: if used_fallback {
                    "fallback".to_string()
                } else {
                    "model".to_string()
                },
                ok: true,
            }
        }
        Err(err) => {
            let lower = err.to_ascii_lowercase();
            let by = if lower.contains("timed out") || lower.contains("timeout") {
                "timeout"
            } else {
                "fallback"
            };
            DecisionResult {
                req_id: req_id.to_string(),
                choice_id: fallback.to_string(),
                reason: Some(err),
                by: by.to_string(),
                ok: false,
            }
        }
    }
}

/// Cancellable variant of `query_llm_async` using `LlmCancelRegistry`.
pub fn query_llm_async_cancellable(
    req_id: String,
    prompt: String,
    config_json: String,
    cancel_registry: Option<Arc<LlmCancelRegistry>>,
    sender: Option<EventSender>,
) {
    let cancel_flag = cancel_registry.as_ref().map(|r| r.register(&req_id));
    std::thread::spawn(move || {
        let _guard = CancelGuard {
            registry: cancel_registry,
            req_id: req_id.clone(),
        };
        if cancel_flag
            .as_deref()
            .map(|f| f.load(Ordering::SeqCst))
            .unwrap_or(false)
        {
            drop(_guard);
            if let Some(tx) = sender {
                tx(RpcEvent {
                    topic: "llm:result".to_string(),
                    payload: json!({ "reqId": req_id, "result": "", "error": "cancelled" }),
                });
            }
            return;
        }
        let cfg = LlmConfig::from_json_str(&config_json);
        let (result, error) = match query_llm_sync(&prompt, &cfg) {
            Ok(text) => {
                if cancel_flag
                    .as_deref()
                    .map(|f| f.load(Ordering::SeqCst))
                    .unwrap_or(false)
                {
                    (String::new(), "cancelled".to_string())
                } else {
                    (text, String::new())
                }
            }
            Err(err) => (String::new(), err),
        };
        drop(_guard);
        if let Some(tx) = sender {
            tx(RpcEvent {
                topic: "llm:result".to_string(),
                payload: json!({ "reqId": req_id, "result": result, "error": error }),
            });
        }
    });
}

/// Asynchronously invokes the Decision model with `LlmCancelRegistry` and timeout (default 8s),
/// emitting `llm:decisionResult` (`reqId`, `choiceId`, `reason`, `by`, `ok`).
#[allow(clippy::too_many_arguments)]
pub fn query_decision_async(
    req_id: String,
    prompt: String,
    choices: Vec<DecisionChoice>,
    fallback: String,
    config_json: String,
    default_config_json: String,
    cancel_registry: Option<Arc<LlmCancelRegistry>>,
    sender: Option<EventSender>,
) {
    let cancel_flag = cancel_registry.as_ref().map(|r| r.register(&req_id));
    std::thread::spawn(move || {
        let _guard = CancelGuard {
            registry: cancel_registry,
            req_id: req_id.clone(),
        };
        let decision_cfg = DecisionConfig::from_json_str(&config_json);
        let timeout = decision_cfg.resolved_timeout();
        let action_llm_cfg = decision_cfg.to_llm_config();
        let default_llm_cfg = LlmConfig::from_json_str(&default_config_json);

        let res = match resolve_decision_config(&action_llm_cfg, &default_llm_cfg) {
            Some(resolved_cfg) => query_decision_sync(
                &req_id,
                &prompt,
                &choices,
                &fallback,
                &resolved_cfg,
                timeout,
                cancel_flag.as_deref(),
            ),
            None => DecisionResult {
                req_id: req_id.clone(),
                choice_id: fallback.clone(),
                reason: Some("decision model not configured".to_string()),
                by: "fallback".to_string(),
                ok: false,
            },
        };

        drop(_guard);
        if let Some(tx) = sender {
            tx(RpcEvent {
                topic: "llm:decisionResult".to_string(),
                payload: json!({
                    "reqId": res.req_id,
                    "choiceId": res.choice_id,
                    "reason": res.reason,
                    "by": res.by,
                    "ok": res.ok,
                }),
            });
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelsProviderKind {
    Google,
    Anthropic,
    Ollama,
    OpenAiCompat,
}

fn ends_with_version_segment(base: &str) -> bool {
    let Some(pos) = base.rfind('/') else {
        return false;
    };
    let tail = &base[pos + 1..];
    tail.starts_with('v') && tail.len() > 1 && tail[1..].bytes().all(|b| b.is_ascii_digit())
}

pub fn resolve_models_list_url(base_url: &str) -> Result<(String, ModelsProviderKind), String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err("baseUrl must start with http:// or https://".to_string());
    }
    if trimmed.contains("generativelanguage.googleapis.com") {
        return Ok((
            "https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000".to_string(),
            ModelsProviderKind::Google,
        ));
    }
    if trimmed.contains("api.anthropic.com") {
        let url = if trimmed.ends_with("/models") {
            format!("{trimmed}?limit=1000")
        } else if ends_with_version_segment(trimmed) {
            format!("{trimmed}/models?limit=1000")
        } else {
            format!("{trimmed}/v1/models?limit=1000")
        };
        return Ok((url, ModelsProviderKind::Anthropic));
    }
    if let Some(port_idx) = trimmed.find(":11434") {
        let after = &trimmed[port_idx + 6..];
        if after.is_empty() || after.starts_with('/') {
            let root = trimmed
                .strip_suffix("/v1")
                .or_else(|| trimmed.strip_suffix("/api"))
                .unwrap_or(trimmed);
            return Ok((format!("{root}/api/tags"), ModelsProviderKind::Ollama));
        }
    }
    let url = if trimmed.ends_with("/models") {
        trimmed.to_string()
    } else if ends_with_version_segment(trimmed) {
        format!("{trimmed}/models")
    } else {
        format!("{trimmed}/v1/models")
    };
    Ok((url, ModelsProviderKind::OpenAiCompat))
}

pub fn fetch_models_sync(base_url: &str, api_key: &str) -> Result<Value, String> {
    let (url, kind) = resolve_models_list_url(base_url)?;
    let key = api_key.trim();
    let agent = http_agent_with_timeout(Duration::from_secs(15));
    let mut req = agent.get(&url);

    match kind {
        ModelsProviderKind::Google => {
            if key.is_empty() {
                return Err("API key is required for Gemini".to_string());
            }
            req = req.set("x-goog-api-key", key);
        }
        ModelsProviderKind::Anthropic => {
            if key.is_empty() {
                return Err("API key is required for Anthropic".to_string());
            }
            req = req
                .set("x-api-key", key)
                .set("anthropic-version", "2023-06-01");
        }
        ModelsProviderKind::OpenAiCompat => {
            if !key.is_empty() {
                req = req.set("Authorization", &format!("Bearer {key}"));
            }
        }
        ModelsProviderKind::Ollama => {}
    }

    match req.call() {
        Ok(resp) => {
            let body = resp
                .into_string()
                .map_err(|e| format!("Failed to read models response: {e}"))?;
            serde_json::from_str::<Value>(&body)
                .map_err(|e| format!("Invalid JSON in models response: {e}"))
        }
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            let snippet = body.trim().chars().take(200).collect::<String>();
            if snippet.is_empty() {
                Err(format!("HTTP {code}"))
            } else {
                Err(format!("HTTP {code}: {snippet}"))
            }
        }
        Err(ureq::Error::Transport(t)) => Err(format!("Network error: {t}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn llm_config_resolves_provider_defaults() {
        let cfg = LlmConfig::from_json_str(r#"{"baseUrl":"http://localhost:11434"}"#);
        assert_eq!(cfg.resolved_provider(), "ollama");
        assert_eq!(cfg.resolved_base_url(), "http://localhost:11434");

        let gem = LlmConfig::from_json_str(r#"{"provider":"gemini"}"#);
        assert_eq!(
            gem.resolved_base_url(),
            "https://generativelanguage.googleapis.com/v1beta/openai"
        );
    }

    #[test]
    fn query_llm_sync_works_against_local_mock_openai_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();

        let srv = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let mut req_buf = Vec::new();
            let mut tmp = [0u8; 1024];
            while let Ok(n) = stream.read(&mut tmp) {
                if n == 0 {
                    break;
                }
                req_buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = req_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let hdr = String::from_utf8_lossy(&req_buf[..pos]);
                    let content_len = hdr
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            if k.trim().eq_ignore_ascii_case("content-length") {
                                v.trim().parse::<usize>().ok()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);
                    if req_buf.len() >= pos + 4 + content_len {
                        break;
                    }
                }
            }
            let body = r#"{"choices":[{"message":{"content":"mock-reply"}}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
            stream.flush().unwrap();
        });

        let cfg = LlmConfig {
            provider: "openai".into(),
            base_url: format!("http://{addr}/v1"),
            model: "test-model".into(),
            api_key: "sk-test".into(),
            ..Default::default()
        };
        let out = query_llm_sync("hello", &cfg).expect("mock reply");
        assert_eq!(out, "mock-reply");
        srv.join().unwrap();
    }

    #[test]
    fn parse_decision_choice_handles_json_fenced_plain_and_hallucination() {
        let valid = &["task", "idea", "meeting", "bug", "memo"];

        // 1. Strict JSON object
        let (id, reason, used_fb) =
            parse_decision_choice(r#"{"choiceId":"task","reason":"actionable todo"}"#, valid, "memo");
        assert_eq!(id, "task");
        assert_eq!(reason.as_deref(), Some("actionable todo"));
        assert!(!used_fb);

        // 2. Fenced ```json block with <think> prefix
        let fenced = "<think>let me classify</think>\n```json\n{\"choice_id\": \"bug\", \"reason\": \"stack trace\"}\n```";
        let (id, reason, used_fb) = parse_decision_choice(fenced, valid, "memo");
        assert_eq!(id, "bug");
        assert_eq!(reason.as_deref(), Some("stack trace"));
        assert!(!used_fb);

        // 3. Plain text exact match & labeled plain text
        let (id, reason, used_fb) = parse_decision_choice("  `idea`  ", valid, "memo");
        assert_eq!(id, "idea");
        assert_eq!(reason, None);
        assert!(!used_fb);

        let (id, reason, used_fb) = parse_decision_choice("choiceId: meeting\nweekly sync", valid, "memo");
        assert_eq!(id, "meeting");
        assert_eq!(reason.as_deref(), Some("weekly sync"));
        assert!(!used_fb);

        // 4. Hallucinated JSON ID falls back safely
        let (id, reason, used_fb) =
            parse_decision_choice(r#"{"choiceId":"urgent_epic","reason":"invented"}"#, valid, "memo");
        assert_eq!(id, "memo");
        assert!(reason.is_some());
        assert!(used_fb);

        // 5. Substring in prose must NOT falsely match "task"
        let (id, reason, used_fb) =
            parse_decision_choice("I do not think this is a task at all.", valid, "memo");
        assert_eq!(id, "memo");
        assert!(reason.is_some());
        assert!(used_fb);

        // 6. Empty / invalid response falls back safely
        let (id, reason, used_fb) = parse_decision_choice("   ", valid, "memo");
        assert_eq!(id, "memo");
        assert!(reason.is_some());
        assert!(used_fb);
    }

    #[test]
    fn resolve_decision_config_supports_all_providers_and_fallbacks_statically() {
        let empty = LlmConfig::default();

        // Both empty -> None (zero network probe)
        assert!(resolve_decision_config(&empty, &empty).is_none());

        // gemini-decision-1
        let gemini_action = LlmConfig {
            model: "gemini-decision-1".into(),
            api_key: "AIza-test".into(),
            ..Default::default()
        };
        let resolved = resolve_decision_config(&gemini_action, &empty).expect("gemini");
        assert_eq!(resolved.provider, "gemini");
        assert_eq!(
            resolved.base_url,
            "https://generativelanguage.googleapis.com/v1beta/openai"
        );
        assert_eq!(resolved.model, "gemini-decision-1");
        assert_eq!(resolved.api_key, "AIza-test");
        assert_eq!(resolved.temperature, Some(0.0));

        // jev-latest inherits custom OpenAI-compat base_url & api_key from default_cfg
        let jev_action = LlmConfig {
            model: "jev-latest".into(),
            ..Default::default()
        };
        let default_compat = LlmConfig {
            provider: "openai".into(),
            base_url: "https://jev.example.com/v1".into(),
            model: "gpt-4o-mini".into(),
            api_key: "sk-jev".into(),
            ..Default::default()
        };
        let resolved = resolve_decision_config(&jev_action, &default_compat).expect("jev");
        assert_eq!(resolved.provider, "openai");
        assert_eq!(resolved.base_url, "https://jev.example.com/v1");
        assert_eq!(resolved.model, "jev-latest");
        assert_eq!(resolved.api_key, "sk-jev");

        // gpt-6-luna
        let luna_action = LlmConfig {
            model: "gpt-6-luna".into(),
            api_key: "sk-openai".into(),
            ..Default::default()
        };
        let resolved = resolve_decision_config(&luna_action, &empty).expect("gpt-6-luna");
        assert_eq!(resolved.provider, "openai");
        assert_eq!(resolved.base_url, "https://api.openai.com/v1");
        assert_eq!(resolved.model, "gpt-6-luna");

        // Local Ollama
        let ollama_action = LlmConfig {
            base_url: "http://127.0.0.1:11434".into(),
            model: "qwen2.5:1.5b".into(),
            ..Default::default()
        };
        let resolved = resolve_decision_config(&ollama_action, &empty).expect("ollama");
        assert_eq!(resolved.provider, "ollama");
        assert_eq!(resolved.base_url, "http://127.0.0.1:11434");
        assert_eq!(resolved.model, "qwen2.5:1.5b");

        // Empty action_cfg falls back to default_cfg
        let resolved = resolve_decision_config(&empty, &default_compat).expect("fallback default");
        assert_eq!(resolved.model, "gpt-4o-mini");
        assert_eq!(resolved.base_url, "https://jev.example.com/v1");

        // Empty action_cfg + gemini default without model -> gemini-decision-1
        let gemini_key_only = LlmConfig {
            provider: "gemini".into(),
            api_key: "AIza-key".into(),
            ..Default::default()
        };
        let resolved = resolve_decision_config(&empty, &gemini_key_only).expect("gemini fallback");
        assert_eq!(resolved.model, "gemini-decision-1");
    }

    #[test]
    fn llm_cancel_registry_lifecycle() {
        let reg = LlmCancelRegistry::new();
        assert_eq!(reg.active_count(), 0);
        assert!(!reg.cancel("missing"));

        let flag = reg.register("req-1");
        assert_eq!(reg.active_count(), 1);
        assert!(!flag.load(Ordering::SeqCst));
        assert!(!reg.is_cancelled("req-1"));

        assert!(reg.cancel("req-1"));
        assert!(flag.load(Ordering::SeqCst));
        assert!(reg.is_cancelled("req-1"));

        reg.finish("req-1");
        assert_eq!(reg.active_count(), 0);
        assert!(!reg.cancel("req-1"));
    }

    fn spawn_single_mock_http(response_body: String, delay: Option<Duration>) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                let mut req_buf = Vec::new();
                let mut tmp = [0u8; 1024];
                while let Ok(n) = stream.read(&mut tmp) {
                    if n == 0 {
                        break;
                    }
                    req_buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = req_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let hdr = String::from_utf8_lossy(&req_buf[..pos]);
                        let content_len = hdr
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                if k.trim().eq_ignore_ascii_case("content-length") {
                                    v.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0);
                        if req_buf.len() >= pos + 4 + content_len {
                            break;
                        }
                    }
                }
                if let Some(d) = delay {
                    std::thread::sleep(d);
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        addr
    }

    #[test]
    fn query_decision_async_emits_model_fallback_cancel_and_timeout_events() {
        let choices = vec![
            DecisionChoice { id: "task".into(), description: "todo".into() },
            DecisionChoice { id: "memo".into(), description: "note".into() },
        ];

        // 1. Valid JSON model response
        let addr = spawn_single_mock_http(
            r#"{"choices":[{"message":{"content":"{\"choiceId\":\"task\",\"reason\":\"has deadline\"}"}}]}"#.into(),
            None,
        );
        let reg = Arc::new(LlmCancelRegistry::new());
        let (tx, rx) = std::sync::mpsc::channel();
        let sender: EventSender = Arc::new(move |ev| {
            let _ = tx.send(ev);
        });
        let cfg_json = json!({
            "provider": "openai",
            "baseUrl": format!("http://{addr}/v1"),
            "model": "gpt-6-luna",
            "apiKey": "sk-test",
            "timeoutMs": 3000
        })
        .to_string();

        query_decision_async(
            "dec-1".into(),
            "finish report by 5pm".into(),
            choices.clone(),
            "memo".into(),
            cfg_json,
            "{}".into(),
            Some(Arc::clone(&reg)),
            Some(sender),
        );
        let ev = rx.recv_timeout(Duration::from_secs(3)).expect("decision event");
        assert_eq!(ev.topic, "llm:decisionResult");
        assert_eq!(ev.payload["reqId"], "dec-1");
        assert_eq!(ev.payload["choiceId"], "task");
        assert_eq!(ev.payload["reason"], "has deadline");
        assert_eq!(ev.payload["by"], "model");
        assert_eq!(ev.payload["ok"], true);
        assert_eq!(reg.active_count(), 0);

        // 2. Unconfigured fallback (zero network)
        let (tx2, rx2) = std::sync::mpsc::channel();
        let sender2: EventSender = Arc::new(move |ev| {
            let _ = tx2.send(ev);
        });
        query_decision_async(
            "dec-2".into(),
            "anything".into(),
            choices.clone(),
            "memo".into(),
            "{}".into(),
            "{}".into(),
            Some(Arc::clone(&reg)),
            Some(sender2),
        );
        let ev2 = rx2.recv_timeout(Duration::from_secs(2)).expect("unconfigured event");
        assert_eq!(ev2.payload["choiceId"], "memo");
        assert_eq!(ev2.payload["by"], "fallback");
        assert_eq!(ev2.payload["ok"], false);
        assert_eq!(reg.active_count(), 0);

        // 3. Timeout fallback
        let slow_addr = spawn_single_mock_http(
            r#"{"choices":[{"message":{"content":"{\"choiceId\":\"task\"}"}}]}"#.into(),
            Some(Duration::from_millis(600)),
        );
        let (tx3, rx3) = std::sync::mpsc::channel();
        let sender3: EventSender = Arc::new(move |ev| {
            let _ = tx3.send(ev);
        });
        let slow_cfg = json!({
            "provider": "openai",
            "baseUrl": format!("http://{slow_addr}/v1"),
            "model": "gpt-6-luna",
            "timeoutMs": 120
        })
        .to_string();
        query_decision_async(
            "dec-3".into(),
            "slow".into(),
            choices.clone(),
            "memo".into(),
            slow_cfg,
            "{}".into(),
            Some(Arc::clone(&reg)),
            Some(sender3),
        );
        let ev3 = rx3.recv_timeout(Duration::from_secs(3)).expect("timeout event");
        assert_eq!(ev3.payload["choiceId"], "memo");
        assert_eq!(ev3.payload["by"], "timeout");
        assert_eq!(ev3.payload["ok"], false);
        assert_eq!(reg.active_count(), 0);

        // 4. Pre-cancelled sync check (no network call)
        let flag = AtomicBool::new(true);
        let cancelled = query_decision_sync(
            "dec-4",
            "cancel me",
            &choices,
            "memo",
            &LlmConfig {
                provider: "openai".into(),
                base_url: "http://127.0.0.1:1/v1".into(),
                model: "gpt-6-luna".into(),
                ..Default::default()
            },
            Duration::from_secs(2),
            Some(&flag),
        );
        assert_eq!(cancelled.choice_id, "memo");
        assert_eq!(cancelled.by, "cancelled");
        assert!(!cancelled.ok);
    }

    #[test]
    fn gemini_decisions_api_sends_v1beta_decisions_payload_and_parses_choice() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut req_bytes = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).expect("read");
                if n == 0 {
                    break;
                }
                req_bytes.extend_from_slice(&buf[..n]);
                if let Some(pos) = req_bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let hdr = String::from_utf8_lossy(&req_bytes[..pos]);
                    let cl: usize = hdr
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            if k.eq_ignore_ascii_case("content-length") {
                                v.trim().parse().ok()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);
                    if req_bytes.len() >= pos + 4 + cl {
                        break;
                    }
                }
            }
            let req_text = String::from_utf8_lossy(&req_bytes).to_string();
            assert!(req_text.starts_with("POST /v1beta/decisions "), "req={req_text}");
            assert!(
                req_text.to_ascii_lowercase().contains("x-goog-api-key: test-goog-key"),
                "missing x-goog-api-key header"
            );
            assert!(req_text.contains("\"type\":\"choice\""), "missing choice type");
            assert!(req_text.contains("\"gemini-decision-1\""), "missing model");

            let body = r#"{"answers":{"decision":{"choice":"task","confidence":0.94}}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).expect("write");
        });

        let cfg = LlmConfig {
            provider: "gemini".into(),
            base_url: format!("http://{addr}"),
            model: "gemini-decision-1".into(),
            api_key: "test-goog-key".into(),
            ..Default::default()
        };
        let choices = vec![
            DecisionChoice { id: "task".into(), description: "actionable task".into() },
            DecisionChoice { id: "memo".into(), description: "general note".into() },
        ];
        let res = query_decision_sync(
            "dec-g1",
            "明日の10時にAPI仕様書レビュー",
            &choices,
            "memo",
            &cfg,
            Duration::from_secs(3),
            None,
        );
        server.join().expect("server");
        assert!(res.ok);
        assert_eq!(res.choice_id, "task");
        assert_eq!(res.by, "model");
        assert_eq!(res.reason.as_deref(), Some("confidence=0.94"));
    }

    #[test]
    #[ignore = "Live Gemini Decisions API smoke test (requires valid GEMINI_API_KEY)"]
    fn live_gemini_decision_1_smoke() {
        let cfg = LlmConfig {
            provider: "gemini".into(),
            model: "gemini-decision-1".into(),
            ..Default::default()
        };
        assert!(!cfg.resolved_api_key().is_empty(), "GEMINI_API_KEY must be set");

        // 1. Quick Capture triage
        let r1 = query_decision_sync(
            "live-1",
            "明日の10時にAPI仕様書をレビューする",
            &[
                DecisionChoice { id: "task".into(), description: "Actionable todo item with schedule".into() },
                DecisionChoice { id: "idea".into(), description: "Brainstorm or concept".into() },
                DecisionChoice { id: "memo".into(), description: "General reference note".into() },
            ],
            "memo",
            &cfg,
            Duration::from_secs(15),
            None,
        );
        assert!(r1.ok, "r1 failed: {:?}", r1);
        assert_eq!(r1.choice_id, "task");

        // 2. Diagram type selection
        let r2 = query_decision_sync(
            "live-2",
            "ユーザーがログインボタンを押すと、Authサーバーへ認証リクエストを送り、JWTを受け取ってDBからプロフィールを取得する",
            &[
                DecisionChoice { id: "sequence".into(), description: "Interactions and messages between actors/services over time".into() },
                DecisionChoice { id: "er".into(), description: "Database entity relationship table schema".into() },
                DecisionChoice { id: "mindmap".into(), description: "Hierarchical brainstorm topics".into() },
            ],
            "sequence",
            &cfg,
            Duration::from_secs(15),
            None,
        );
        assert!(r2.ok, "r2 failed: {:?}", r2);
        assert_eq!(r2.choice_id, "sequence");

        // 3. Log anomaly detection
        let r3 = query_decision_sync(
            "live-3",
            "2026-11-23T09:50:01Z FATAL OutOfMemoryError: worker process killed by OOM killer",
            &[
                DecisionChoice { id: "normal".into(), description: "Normal informational log".into() },
                DecisionChoice { id: "warn".into(), description: "Recoverable warning".into() },
                DecisionChoice { id: "critical_alert".into(), description: "Fatal crash or outage requiring immediate alert".into() },
            ],
            "normal",
            &cfg,
            Duration::from_secs(15),
            None,
        );
        assert!(r3.ok, "r3 failed: {:?}", r3);
        assert_eq!(r3.choice_id, "critical_alert");
        println!("LIVE OK: r1={:?}, r2={:?}, r3={:?}", r1, r2, r3);
    }

    #[test]
    fn resolve_models_list_url_supports_all_providers() {
        assert_eq!(
            resolve_models_list_url("https://generativelanguage.googleapis.com/v1beta/openai").unwrap(),
            (
                "https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000".to_string(),
                ModelsProviderKind::Google
            )
        );
        assert_eq!(
            resolve_models_list_url("https://api.anthropic.com").unwrap(),
            (
                "https://api.anthropic.com/v1/models?limit=1000".to_string(),
                ModelsProviderKind::Anthropic
            )
        );
        assert_eq!(
            resolve_models_list_url("https://api.anthropic.com/v1/").unwrap(),
            (
                "https://api.anthropic.com/v1/models?limit=1000".to_string(),
                ModelsProviderKind::Anthropic
            )
        );
        assert_eq!(
            resolve_models_list_url("https://api.openai.com/v1").unwrap(),
            (
                "https://api.openai.com/v1/models".to_string(),
                ModelsProviderKind::OpenAiCompat
            )
        );
        assert_eq!(
            resolve_models_list_url("https://openrouter.ai/api/v1").unwrap(),
            (
                "https://openrouter.ai/api/v1/models".to_string(),
                ModelsProviderKind::OpenAiCompat
            )
        );
        assert_eq!(
            resolve_models_list_url("http://127.0.0.1:11434/v1").unwrap(),
            (
                "http://127.0.0.1:11434/api/tags".to_string(),
                ModelsProviderKind::Ollama
            )
        );
        assert_eq!(
            resolve_models_list_url("http://127.0.0.1:8080").unwrap(),
            (
                "http://127.0.0.1:8080/v1/models".to_string(),
                ModelsProviderKind::OpenAiCompat
            )
        );
        assert!(resolve_models_list_url("ftp://example.com").is_err());
    }

    #[test]
    fn save_generated_image_markdown_writes_asset_and_returns_link() {
        let tmp = std::env::temp_dir().join(format!("syki_img_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let note_path = tmp.join("note.md");
        let b64_1x1 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
        let md = save_generated_image_markdown(
            &tmp,
            &note_path.to_string_lossy(),
            "image/png",
            b64_1x1,
        )
        .expect("save_generated_image_markdown");
        assert!(md.starts_with("![Generated Image](./assets/"), "got: {md}");
        assert!(md.ends_with(".png)"), "got: {md}");
        let assets_dir = tmp.join("assets");
        let count = std::fs::read_dir(&assets_dir).unwrap().count();
        assert_eq!(count, 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
