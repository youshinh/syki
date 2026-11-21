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
    pub name: String,
    pub bytes: u64,
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

pub fn list_voice_caches(data_dir: &Path) -> Vec<VoiceCacheEntry> {
    let dir = voice_cache_dir(data_dir);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext.to_ascii_lowercase().as_str(), "wav" | "webm" | "ogg" | "m4a") {
            continue;
        }
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let kept = p.with_extension("kept").exists();
        out.push(VoiceCacheEntry {
            path: p.to_string_lossy().into_owned(),
            name,
            bytes,
            kept,
        });
    }
    out.sort_by(|a, b| b.name.cmp(&a.name));
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

pub fn transcribe_audio_async(
    data_dir: PathBuf,
    req_id: String,
    audio_b64: String,
    mime_type: String,
    sender: Option<EventSender>,
) {
    std::thread::spawn(move || {
        let cache_res = save_audio_to_cache(&data_dir, &audio_b64, &mime_type);
        let (text, error, cache_path) = match cache_res {
            Ok(p) => match transcribe_wav_file(&data_dir, &p, "ja") {
                Ok(t) => (t, String::new(), p.to_string_lossy().into_owned()),
                Err(e) => (String::new(), e, p.to_string_lossy().into_owned()),
            },
            Err(e) => (String::new(), e, String::new()),
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
