//! AI Inference (`Ctrl+L` / `Ctrl+K` / `Alt+C` / Tab), Image Generation,
//! and Vision AI OCR (`F23`, AI-only — zero WinRT) (#12).

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

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .build()
}

fn send_json_post(req: ureq::Request, body: &Value) -> Result<Value, String> {
    let resp = req
        .set("Content-Type", "application/json")
        .send_string(&body.to_string())
        .map_err(|e| format!("HTTP request failed: {e}"))?;
    let raw = resp
        .into_string()
        .map_err(|e| format!("HTTP read failed: {e}"))?;
    serde_json::from_str::<Value>(&raw).map_err(|e| format!("invalid JSON response: {e}"))
}

/// Sends a text prompt to the configured LLM endpoint and returns assistant text.
pub fn query_llm_sync(prompt: &str, cfg: &LlmConfig) -> Result<String, String> {
    let provider = cfg.resolved_provider();
    let base = cfg.resolved_base_url();
    let model = cfg.resolved_model();
    let api_key = cfg.resolved_api_key();

    if provider == "ollama" {
        let url = format!("{base}/api/chat");
        let body = json!({
            "model": model,
            "stream": false,
            "messages": [{ "role": "user", "content": prompt }]
        });
        let val = send_json_post(http_agent().post(&url), &body)?;
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
        let mut req = http_agent()
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

    // OpenAI-compatible (`/chat/completions`, works for OpenAI, Gemini OpenAI-compat, LM Studio, Strata)
    let url = if base.ends_with("/chat/completions") {
        base
    } else {
        format!("{base}/chat/completions")
    };
    let mut body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }]
    });
    if let Some(t) = cfg.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(m) = cfg.max_tokens {
        body["max_tokens"] = json!(m);
    }
    let mut req = http_agent().post(&url);
    if !api_key.is_empty() {
        req = req.set("Authorization", &format!("Bearer {api_key}"));
    }
    let val = send_json_post(req, &body)?;
    val.pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| "LLM response missing choices[0].message.content".to_string())
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
    let url = format!("{base}/chat/completions");
    let body = json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": { "url": data_url } }
            ]
        }]
    });
    let mut req = http_agent().post(&url);
    if !api_key.is_empty() {
        req = req.set("Authorization", &format!("Bearer {api_key}"));
    }
    let val = send_json_post(req, &body)?;
    val.pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .map(str::to_string)
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

pub fn generate_image_async(
    req_id: String,
    prompt: String,
    config_json: String,
    sender: Option<EventSender>,
) {
    std::thread::spawn(move || {
        let cfg = LlmConfig::from_json_str(&config_json);
        let base = cfg.resolved_base_url();
        let api_key = cfg.resolved_api_key();
        let url = format!("{base}/images/generations");
        let body = json!({
            "model": if cfg.model.trim().is_empty() { "gpt-image-1" } else { cfg.model.trim() },
            "prompt": prompt,
            "n": 1,
            "response_format": "b64_json"
        });
        let mut req = http_agent().post(&url);
        if !api_key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {api_key}"));
        }
        let (result, error) = match send_json_post(req, &body) {
            Ok(val) => {
                if let Some(b64) = val.pointer("/data/0/b64_json").and_then(|v| v.as_str()) {
                    (format!("data:image/png;base64,{b64}"), String::new())
                } else if let Some(u) = val.pointer("/data/0/url").and_then(|v| v.as_str()) {
                    (u.to_string(), String::new())
                } else {
                    (String::new(), "image response missing b64_json/url".to_string())
                }
            }
            Err(e) => (String::new(), format!("image generation failed: {e}")),
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
}
