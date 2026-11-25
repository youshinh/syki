//! Local STT (`speech.cpp` / `Haas-ja`), Pure-Rust PCM16 WAV encoder, and Voice Cache (#14).

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

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
static LOCAL_STT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn local_stt_lock() -> &'static Mutex<()> {
    LOCAL_STT_LOCK.get_or_init(|| Mutex::new(()))
}

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

/// Hints the OS to page-cache the largest local ASR model file asynchronously while the user speaks.
pub fn prefetch_model_async(data_dir: PathBuf) {
    std::thread::spawn(move || {
        let base = speech_dir(&data_dir);
        let mut best: Option<(u64, PathBuf)> = None;
        for sub in [base.clone(), base.join("models")] {
            if let Ok(rd) = std::fs::read_dir(&sub) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if !p.is_file() {
                        continue;
                    }
                    let ext = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if matches!(ext.as_str(), "bin" | "gguf" | "onnx") {
                        if let Ok(m) = entry.metadata() {
                            if best.as_ref().map(|(sz, _)| m.len() > *sz).unwrap_or(true) {
                                best = Some((m.len(), p));
                            }
                        }
                    }
                }
            }
        }
        if let Some((_, path)) = best {
            if let Ok(mut f) = std::fs::File::open(path) {
                use std::io::Read;
                let mut buf = vec![0u8; 256 * 1024];
                while let Ok(n) = f.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                }
            }
        }
    });
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

#[derive(Debug, Clone)]
struct CatalogPart {
    id: &'static str,
    kind: &'static str,
    role: &'static str,
    title: &'static str,
    note: &'static str,
    url: String,
    sha256: String,
    size: u64,
    entry: String,
    is_zip: bool,
    extract_patterns: &'static [&'static str],
}

fn catalog_runtime_speech() -> CatalogPart {
    CatalogPart {
        id: "speech-runtime-v0.10.1-win-x64",
        kind: "runtime",
        role: "runtime",
        title: "speech.cpp 実行ファイル (v0.10.1 / Windows x64 AVX2/Vulkan)",
        note: "軽量・高速なローカル音声認識エンジン(約28MB)",
        url: "https://github.com/nyosegawa/speech.cpp/releases/download/v0.10.1/speech-0.10.1-windows-x64-vulkan.zip".into(),
        sha256: "a8fe0e4b4f463c460e7ace9d38c363bec08b254cac5a7f764b7967af35d4fa1a".into(),
        size: 29_548_632,
        entry: "speech.exe".into(),
        is_zip: true,
        extract_patterns: &["speech.exe", "speech.dll"],
    }
}

fn catalog_runtime_whisper() -> CatalogPart {
    CatalogPart {
        id: "whisper-runtime-b5130-win-x64",
        kind: "runtime",
        role: "runtime",
        title: "Whisper 実行ファイル (whisper.cpp b5130 / Windows x64 CPU)",
        note: "音声認識を実行する小さなプログラム(約9MB)",
        url: "https://github.com/ggml-org/whisper.cpp/releases/download/b5130/whisper-bin-x64.zip".into(),
        sha256: "f9ec6c52a2e949b62ab51fa21d0d497958f9e41c3010c157c4e42932d5316f3c".into(),
        size: 8_573_270,
        entry: "whisper-cli.exe".into(),
        is_zip: true,
        extract_patterns: &["whisper-cli.exe", "whisper.dll", "ggml*.dll"],
    }
}

fn catalog_asr_models() -> Vec<CatalogPart> {
    let mk = |id: &'static str, title: &'static str, note: &'static str, url: String, sha: &'static str, size: u64, file: &'static str| CatalogPart {
        id, kind: "model", role: "asr", title, note, url, sha256: sha.into(), size, entry: file.into(), is_zip: false, extract_patterns: &[]
    };
    vec![
        mk("haas-ja-v1.0-q8_0", "Haas-ja v1.0 量子化版(q8_0・推奨)", "日本語特化の超高速・高精度ストリーミング対応ASRモデル。約626MB",
           "https://huggingface.co/sakasegawa/Haas-ja-v1.0-GGUF/resolve/main/Haas-616M-ja-v1.0-Q8_0.gguf".into(),
           "cd5f60b2f1c411404b83fca4a65e3ce30a26d9e951b6a346529e65f22758afeb", 656_381_472, "Haas-616M-ja-v1.0-Q8_0.gguf"),
        mk("haas-ja-v1.0-q4_k", "Haas-ja v1.0 超軽量版(q4_k)", "メモリ消費約350MBの極小・高速ASRモデル",
           "https://huggingface.co/sakasegawa/Haas-ja-v1.0-GGUF/resolve/main/Haas-616M-ja-v1.0-Q4_K.gguf".into(),
           "d24d53f29b75214a933ff482b0de1217e0a800f4118cca39ad6cf03609a89d6f", 351_901_216, "Haas-616M-ja-v1.0-Q4_K.gguf"),
        mk("kotoba-v2.0-q5_0", "kotoba-whisper v2.0 量子化版(日本語特化)", "日本語に特化した高精度モデル。約513MB",
           "https://huggingface.co/kotoba-tech/kotoba-whisper-v2.0-ggml/resolve/main/ggml-kotoba-whisper-v2.0-q5_0.bin".into(),
           "4a3b92192b5d3578ff854a5876213e2e27af0c2d357492c2d14271e82c303658", 537_819_875, "ggml-kotoba-whisper-v2.0-q5_0.bin"),
        mk("kotoba-v2.0", "kotoba-whisper v2.0 フル精度版(日本語特化)", "日本語に特化した最高精度モデル。約1.4GB",
           "https://huggingface.co/kotoba-tech/kotoba-whisper-v2.0-ggml/resolve/main/ggml-kotoba-whisper-v2.0.bin".into(),
           "eff70a8a236e731abba774ba71e1f6d0fce53302137208c32207e694e0bf4546", 1_519_521_155, "ggml-kotoba-whisper-v2.0.bin"),
        mk("large-v3-turbo-q5_0", "Whisper large-v3-turbo 量子化版(多言語)", "多言語モデル。約547MB",
           "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin".into(),
           "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2", 574_041_195, "ggml-large-v3-turbo-q5_0.bin"),
        mk("small-q5_1", "Whisper small 量子化版(軽量)", "約181MB の軽量な多言語モデル",
           "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin".into(),
           "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb", 190_085_487, "ggml-small-q5_1.bin"),
        mk("base", "Whisper base(最軽量・低精度)", "約141MB の最小クラスモデル",
           "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin".into(),
           "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe", 147_951_465, "ggml-base.bin"),
    ]
}

fn catalog_addons() -> Vec<CatalogPart> {
    let mk = |id: &'static str, role: &'static str, title: &'static str, note: &'static str, url: &'static str, sha: &'static str, size: u64, file: &'static str| CatalogPart {
        id, kind: "model", role, title, note, url: url.into(), sha256: sha.into(), size, entry: file.into(), is_zip: false, extract_patterns: &[]
    };
    vec![
        mk("silero-vad", "vad", "Silero VAD v6.2 (音声区間検出)", "リアルタイム無音検知モデル。約1.2MB",
           "https://huggingface.co/sakasegawa/silero-vad-GGUF/resolve/main/silero-vad-309K-v6.2-F32.gguf",
           "2fc312514e5fc4fa4ffd02b25c76a4853fa4130e8fb9777d01502183b01238cb", 1_238_624, "silero-vad-309K-v6.2-F32.gguf"),
        mk("nemotron-3-diarization", "diarize", "Nemotron-3 Diarization (話者識別)", "会議録音で発言者を分離・識別するモデル。約106MB",
           "https://huggingface.co/sakasegawa/Nemotron-3-Diarization-GGUF/resolve/main/Nemotron-3-Diarization-99M-Q8_0.gguf",
           "336df6350dce2ea5ff06aa6e2b249d331a859ced13305fbb45c44bdb3c0abe20", 106_179_488, "Nemotron-3-Diarization-99M-Q8_0.gguf"),
        mk("re-use", "enhance", "RE-USE (ノイズ除去・音声強調)", "環境ノイズを除去するモデル。約38MB",
           "https://huggingface.co/sakasegawa/NVIDIA-NemotronLabs-RE-USE-GGUF/resolve/main/NVIDIA-NemotronLabs-RE-USE-10M-F32.gguf",
           "827faf209c6988b9cfd07b08ee7f212a7b2eeb7870a25380df671d55338b3879", 38_519_136, "NVIDIA-NemotronLabs-RE-USE-10M-F32.gguf"),
    ]
}

fn selected_whisper_model_id(cfg: &Value) -> String {
    let raw = cfg
        .pointer("/whisper/model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("haas-ja-v1.0-q8_0");
    raw.to_string()
}

fn uses_speech_cpp(model_id: &str, cfg: &Value) -> bool {
    if model_id.starts_with("haas-") {
        return true;
    }
    if model_id == "custom-path" {
        let p = cfg.pointer("/whisper/modelPath").and_then(|v| v.as_str()).unwrap_or("");
        return p.to_ascii_lowercase().ends_with(".gguf");
    }
    if model_id == "custom-url" {
        let u = cfg.pointer("/whisper/customUrl").and_then(|v| v.as_str()).unwrap_or("");
        return u.to_ascii_lowercase().ends_with(".gguf");
    }
    false
}

fn resolve_model_part(cfg: &Value) -> Option<CatalogPart> {
    let id = selected_whisper_model_id(cfg);
    if id == "custom-path" {
        return None;
    }
    if id == "custom-url" {
        let url = cfg.pointer("/whisper/customUrl").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if url.is_empty() {
            return None;
        }
        let sha = cfg.pointer("/whisper/customSha256").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let file = url.split('?').next().unwrap_or(&url).rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("custom-model.bin").to_string();
        return Some(CatalogPart {
            id: "custom-url",
            kind: "model",
            role: "asr",
            title: "カスタムモデル (URL)",
            note: "",
            url,
            sha256: sha,
            size: 0,
            entry: file,
            is_zip: false,
            extract_patterns: &[],
        });
    }
    catalog_asr_models()
        .into_iter()
        .find(|m| m.id == id)
        .or_else(|| catalog_asr_models().into_iter().next())
}

fn resolve_part_for(which: &str, cfg: &Value) -> Option<CatalogPart> {
    if which == "runtime" {
        let mid = selected_whisper_model_id(cfg);
        return Some(if uses_speech_cpp(&mid, cfg) {
            catalog_runtime_speech()
        } else {
            catalog_runtime_whisper()
        });
    }
    if which == "model" {
        return resolve_model_part(cfg);
    }
    if let Some(addon_id) = which.strip_prefix("addon:") {
        return catalog_addons().into_iter().find(|a| a.id == addon_id);
    }
    None
}

fn part_entry_path(dir: &Path, part: &CatalogPart) -> PathBuf {
    dir.join(&part.entry)
}

fn part_status_json(dir: &Path, part: &CatalogPart) -> Value {
    let p = part_entry_path(dir, part);
    let meta = std::fs::metadata(&p).ok();
    let installed = meta.as_ref().map(|m| m.is_file() && m.len() > 0).unwrap_or(false);
    let size = meta.map(|m| m.len()).unwrap_or(0);
    json!({
        "installed": installed,
        "size": size,
        "path": p.to_string_lossy(),
    })
}

fn part_view_json(dir: &Path, part: &CatalogPart) -> Value {
    json!({
        "part": {
            "id": part.id,
            "kind": part.kind,
            "title": part.title,
            "note": part.note,
            "size": part.size,
            "entry": part.entry,
        },
        "role": part.role,
        "status": part_status_json(dir, part),
    })
}

pub fn get_speech_status(data_dir: &Path, voice_config_json: &str) -> Value {
    let dir = speech_dir(data_dir);
    let cfg: Value = serde_json::from_str(voice_config_json).unwrap_or_else(|_| json!({}));
    let mid = selected_whisper_model_id(&cfg);
    let rt = if uses_speech_cpp(&mid, &cfg) {
        catalog_runtime_speech()
    } else {
        catalog_runtime_whisper()
    };
    let rt_installed = part_entry_path(&dir, &rt).is_file();

    let model_installed = if mid == "custom-path" {
        let mp = cfg.pointer("/whisper/modelPath").and_then(|v| v.as_str()).unwrap_or("").trim();
        !mp.is_empty() && Path::new(mp).is_file()
    } else if let Some(mp) = resolve_model_part(&cfg) {
        part_entry_path(&dir, &mp).is_file()
    } else {
        false
    };

    let reason = if rt_installed && model_installed {
        String::new()
    } else if !model_installed {
        "ローカル音声認識モデルが未ダウンロードです。下のボタンからダウンロードしてください。".to_string()
    } else {
        "音声認識を実行するプログラム（実行ファイル）が未ダウンロードです。".to_string()
    };

    let models: Vec<Value> = catalog_asr_models().iter().map(|m| part_view_json(&dir, m)).collect();
    let addons: Vec<Value> = catalog_addons().iter().map(|a| part_view_json(&dir, a)).collect();
    let custom = if mid == "custom-url" {
        resolve_model_part(&cfg).map(|c| part_view_json(&dir, &c))
    } else {
        None
    };

    json!({
        "supported": true,
        "available": rt_installed && model_installed,
        "storageDir": dir.to_string_lossy(),
        "defaultModel": "haas-ja-v1.0-q8_0",
        "runtime": part_view_json(&dir, &rt),
        "models": models,
        "addons": addons,
        "custom": custom,
        "local": {
            "ready": rt_installed && model_installed,
            "runtimeInstalled": rt_installed,
            "modelInstalled": model_installed,
            "reason": reason,
            "modelWarning": "",
        }
    })
}

fn zip_matches_pattern(name: &str, patterns: &[&str]) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    if base.is_empty() {
        return false;
    }
    patterns.iter().any(|pat| {
        if let Some((prefix, suffix)) = pat.split_once('*') {
            base.starts_with(prefix) && base.ends_with(suffix)
        } else {
            base.eq_ignore_ascii_case(pat)
        }
    })
}

fn extract_zip_filtered(zip_bytes: &[u8], dest_dir: &Path, patterns: &[&str]) -> Result<(), String> {
    use std::io::Read;
    // Locate End of Central Directory (0x06054b50)
    let search_start = zip_bytes.len().saturating_sub(65_557);
    let eocd_pos = (search_start..=zip_bytes.len().saturating_sub(22))
        .rev()
        .find(|&i| zip_bytes[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or_else(|| "invalid zip archive (EOCD not found)".to_string())?;
    let total_entries = u16::from_le_bytes([zip_bytes[eocd_pos + 10], zip_bytes[eocd_pos + 11]]) as usize;
    let cd_offset = u32::from_le_bytes([
        zip_bytes[eocd_pos + 16], zip_bytes[eocd_pos + 17], zip_bytes[eocd_pos + 18], zip_bytes[eocd_pos + 19],
    ]) as usize;

    let mut pos = cd_offset;
    for _ in 0..total_entries {
        if pos + 46 > zip_bytes.len() || zip_bytes[pos..pos + 4] != [0x50, 0x4b, 0x01, 0x02] {
            break;
        }
        let method = u16::from_le_bytes([zip_bytes[pos + 10], zip_bytes[pos + 11]]);
        let comp_size = u32::from_le_bytes([zip_bytes[pos + 20], zip_bytes[pos + 21], zip_bytes[pos + 22], zip_bytes[pos + 23]]) as usize;
        let name_len = u16::from_le_bytes([zip_bytes[pos + 28], zip_bytes[pos + 29]]) as usize;
        let extra_len = u16::from_le_bytes([zip_bytes[pos + 30], zip_bytes[pos + 31]]) as usize;
        let comment_len = u16::from_le_bytes([zip_bytes[pos + 32], zip_bytes[pos + 33]]) as usize;
        let local_off = u32::from_le_bytes([zip_bytes[pos + 42], zip_bytes[pos + 43], zip_bytes[pos + 44], zip_bytes[pos + 45]]) as usize;
        let entry_name = String::from_utf8_lossy(&zip_bytes[pos + 46..pos + 46 + name_len]).into_owned();
        pos += 46 + name_len + extra_len + comment_len;

        if entry_name.ends_with('/') || !zip_matches_pattern(&entry_name, patterns) {
            continue;
        }
        if local_off + 30 > zip_bytes.len() || zip_bytes[local_off..local_off + 4] != [0x50, 0x4b, 0x03, 0x04] {
            continue;
        }
        let l_name_len = u16::from_le_bytes([zip_bytes[local_off + 26], zip_bytes[local_off + 27]]) as usize;
        let l_extra_len = u16::from_le_bytes([zip_bytes[local_off + 28], zip_bytes[local_off + 29]]) as usize;
        let data_start = local_off + 30 + l_name_len + l_extra_len;
        let data_end = data_start.saturating_add(comp_size);
        if data_end > zip_bytes.len() {
            return Err("truncated zip entry".to_string());
        }
        let slice = &zip_bytes[data_start..data_end];
        let out_bytes = match method {
            0 => slice.to_vec(),
            8 => {
                let mut dec = flate2::read::DeflateDecoder::new(slice);
                let mut buf = Vec::new();
                dec.read_to_end(&mut buf).map_err(|e| format!("zip deflate error: {e}"))?;
                buf
            }
            other => return Err(format!("unsupported zip compression method: {other}")),
        };
        let base = entry_name.rsplit('/').next().unwrap_or(&entry_name);
        write_atomic_bytes(&dest_dir.join(base), &out_bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn cancel_speech_install() {
    INSTALL_CANCELLED.store(true, Ordering::Release);
}

pub fn install_speech_part_async(
    data_dir: PathBuf,
    req_id: String,
    which: String,
    voice_config_json: String,
    sender: Option<EventSender>,
) {
    INSTALL_CANCELLED.store(false, Ordering::Release);
    std::thread::spawn(move || {
        let emit = |state: &str, done: u64, total: u64, msg: &str| {
            if let Some(ref tx) = sender {
                tx(RpcEvent {
                    topic: "speech:install".to_string(),
                    payload: json!({
                        "reqId": req_id,
                        "state": state,
                        "done": done,
                        "total": total,
                        "message": msg
                    }),
                });
            }
        };

        let cfg: Value = serde_json::from_str(&voice_config_json).unwrap_or_else(|_| json!({}));
        let Some(part) = resolve_part_for(&which, &cfg) else {
            emit("error", 0, 0, "この項目はインストールできません");
            return;
        };
        let dir = speech_dir(&data_dir);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            emit("error", 0, 0, &e.to_string());
            return;
        }

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(20))
            .build();
        let resp = match agent.get(&part.url).call() {
            Ok(r) => r,
            Err(e) => {
                emit("error", 0, 0, &format!("ダウンロード失敗: {e}"));
                return;
            }
        };
        let total = resp
            .header("Content-Length")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(part.size);

        use sha2::Digest;
        use std::io::{Read, Write};
        let tmp_path = dir.join(format!(".dl-{}.tmp", req_id.replace(['/', '\\', ':'], "_")));
        let mut reader = resp.into_reader();
        let mut hasher = sha2::Sha256::new();
        let mut done = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        let mut zip_buf = if part.is_zip { Vec::with_capacity(total as usize) } else { Vec::new() };
        let mut tmp_file = if !part.is_zip {
            match std::fs::File::create(&tmp_path) {
                Ok(f) => Some(f),
                Err(e) => {
                    emit("error", 0, 0, &e.to_string());
                    return;
                }
            }
        } else {
            None
        };

        let mut last_emit = std::time::Instant::now();
        loop {
            if INSTALL_CANCELLED.load(Ordering::Acquire) {
                drop(tmp_file);
                let _ = std::fs::remove_file(&tmp_path);
                emit("canceled", done, total, "");
                return;
            }
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    hasher.update(&buf[..n]);
                    if part.is_zip {
                        zip_buf.extend_from_slice(&buf[..n]);
                    } else if let Some(ref mut f) = tmp_file {
                        if let Err(e) = f.write_all(&buf[..n]) {
                            drop(tmp_file);
                            let _ = std::fs::remove_file(&tmp_path);
                            emit("error", done, total, &e.to_string());
                            return;
                        }
                    }
                    done += n as u64;
                    if last_emit.elapsed() >= std::time::Duration::from_millis(120) {
                        emit("progress", done, total, "");
                        last_emit = std::time::Instant::now();
                    }
                }
                Err(e) => {
                    drop(tmp_file);
                    let _ = std::fs::remove_file(&tmp_path);
                    emit("error", done, total, &format!("受信エラー: {e}"));
                    return;
                }
            }
        }
        emit("progress", done, total, "");

        if !part.sha256.is_empty() {
            let got = format!("{:x}", hasher.finalize());
            if !got.eq_ignore_ascii_case(&part.sha256) {
                drop(tmp_file);
                let _ = std::fs::remove_file(&tmp_path);
                emit("error", done, total, "SHA-256 検証に失敗しました");
                return;
            }
        }

        if part.is_zip {
            if let Err(e) = extract_zip_filtered(&zip_buf, &dir, part.extract_patterns) {
                emit("error", done, total, &e);
                return;
            }
        } else {
            drop(tmp_file);
            let dest = part_entry_path(&dir, &part);
            if let Err(e) = std::fs::rename(&tmp_path, &dest) {
                let _ = std::fs::remove_file(&tmp_path);
                emit("error", done, total, &e.to_string());
                return;
            }
        }

        emit("done", done, total, "");
    });
}

pub fn remove_speech_part(data_dir: &Path, which: &str, voice_config_json: &str) -> Result<(), String> {
    let dir = speech_dir(data_dir);
    let cfg: Value = serde_json::from_str(voice_config_json).unwrap_or_else(|_| json!({}));
    if let Some(part) = resolve_part_for(which, &cfg) {
        let p = part_entry_path(&dir, &part);
        if p.exists() {
            std::fs::remove_file(p).map_err(|e| e.to_string())?;
        }
        return Ok(());
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

fn parse_speech_json_output(raw: &str) -> String {
    if let Ok(val) = serde_json::from_str::<Value>(raw.trim()) {
        if let Some(t) = val.get("text").and_then(|v| v.as_str()) {
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        if let Some(regions) = val.get("regions").and_then(|v| v.as_array()) {
            let joined: Vec<&str> = regions
                .iter()
                .filter_map(|r| r.get("text").and_then(|v| v.as_str()))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if !joined.is_empty() {
                return joined.join("\n");
            }
        }
        // Valid JSON with empty `text` and no `regions` means no speech was recognized;
        // never leak raw JSON like `{"file":...,"text":"","stop":"complete"}` into the note.
        if val.is_object() {
            return String::new();
        }
    }
    raw.trim().to_string()
}

fn ensure_wav_input_if_needed(audio_path: &Path) -> (PathBuf, Option<PathBuf>) {
    let is_wav = std::fs::File::open(audio_path)
        .ok()
        .and_then(|mut f| {
            use std::io::Read;
            let mut hdr = [0u8; 12];
            f.read_exact(&mut hdr).ok()?;
            Some(&hdr[0..4] == b"RIFF" && &hdr[8..12] == b"WAVE")
        })
        .unwrap_or(false);
    if is_wav {
        return (audio_path.to_path_buf(), None);
    }
    let tmp_wav = audio_path.with_extension("tmp16k.wav");
    let ok = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-y",
            "-i",
            &audio_path.to_string_lossy(),
            "-vn",
            "-ar",
            "16000",
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
            &tmp_wav.to_string_lossy(),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok && tmp_wav.is_file() {
        (tmp_wav.clone(), Some(tmp_wav))
    } else {
        let _ = std::fs::remove_file(&tmp_wav);
        (audio_path.to_path_buf(), None)
    }
}

pub fn transcribe_local_with_config(
    data_dir: &Path,
    audio_path: &Path,
    cfg: &Value,
) -> Result<String, String> {
    let dir = speech_dir(data_dir);
    let mid = selected_whisper_model_id(cfg);
    let use_speech = uses_speech_cpp(&mid, cfg);
    let engine = dir.join(if use_speech { "speech.exe" } else { "whisper-cli.exe" });
    let model_path = if mid == "custom-path" {
        PathBuf::from(
            cfg.pointer("/whisper/modelPath")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim(),
        )
    } else if let Some(part) = resolve_model_part(cfg) {
        part_entry_path(&dir, &part)
    } else {
        dir.join("ggml-model.bin")
    };
    if !engine.is_file() || !model_path.is_file() {
        return Err("local speech engine/model not installed".to_string());
    }
    let _stt_guard = local_stt_lock().lock().unwrap_or_else(|e| e.into_inner());

    let (input_path, cleanup_wav) = ensure_wav_input_if_needed(audio_path);
    let lang = cfg
        .pointer("/whisper/language")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "auto")
        .unwrap_or("ja");
    let threads = cfg
        .pointer("/whisper/threads")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let mut cmd = Command::new(&engine);
    cmd.current_dir(&dir);
    if use_speech {
        cmd.arg("asr").arg(&model_path);
        let vad = dir.join("silero-vad-309K-v6.2-F32.gguf");
        let has_vad = vad.is_file();
        if has_vad {
            cmd.arg("--vad").arg(&vad);
        }
        if has_vad && cfg.pointer("/whisper/diarize").and_then(|v| v.as_bool()).unwrap_or(false) {
            let dz = dir.join("Nemotron-3-Diarization-99M-Q8_0.gguf");
            if dz.is_file() {
                cmd.arg("--diarize").arg(&dz);
            }
        }
        if cfg.pointer("/whisper/enhance").and_then(|v| v.as_bool()).unwrap_or(false) {
            let en = dir.join("NVIDIA-NemotronLabs-RE-USE-10M-F32.gguf");
            if en.is_file() {
                cmd.arg("--enhance").arg(&en);
            }
        }
        if cfg.pointer("/whisper/timestamps").and_then(|v| v.as_bool()).unwrap_or(false) {
            cmd.arg("--timestamps");
        }
        cmd.args(["--device", "cpu", "--format", "json"]);
        if threads > 0 {
            cmd.arg("--threads").arg(threads.to_string());
        }
        cmd.arg(&input_path);
    } else {
        cmd.args([
            "-m",
            &model_path.to_string_lossy(),
            "-f",
            &input_path.to_string_lossy(),
            "-l",
            lang,
            "--no-timestamps",
        ]);
        if threads > 0 {
            cmd.arg("-t").arg(threads.to_string());
        }
    }

    let out = cmd
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("local speech spawn failed: {e}"));
    if let Some(tmp) = cleanup_wav {
        let _ = std::fs::remove_file(tmp);
    }
    let out = out?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("local speech failed: {stderr}"));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    if use_speech {
        Ok(parse_speech_json_output(&stdout))
    } else {
        Ok(stdout.trim().to_string())
    }
}

fn normalize_voice_engine(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "whisper-local" | "speech-local" | "local" => "whisper-local",
        _ => "gemini",
    }
}

pub fn resolve_voice_profile_config(data_dir: &Path, profile: &str, language: &str) -> Value {
    let root = std::fs::read_to_string(data_dir.join("config.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    let voice = root.get("voice").and_then(|v| v.as_object());
    let vision = root.get("vision").and_then(|v| v.as_object());
    let is_batch = profile == "batch";
    let prof_key = if is_batch { "batch" } else { "realtime" };
    let prof = voice
        .and_then(|v| v.get(prof_key))
        .and_then(|v| v.as_object());

    let str_field = |obj: Option<&serde_json::Map<String, Value>>, key: &str| -> String {
        obj.and_then(|m| m.get(key))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("")
            .to_string()
    };

    let raw_engine = if is_batch {
        let pe = str_field(prof, "engine");
        if !pe.is_empty() { pe } else { str_field(voice, "engine") }
    } else {
        str_field(prof, "engine")
    };
    let engine = normalize_voice_engine(&raw_engine);

    let model = {
        let pm = str_field(prof, "model");
        if !pm.is_empty() { pm } else { str_field(voice, "model") }
    };
    let api_style = {
        let ps = str_field(prof, "apiStyle");
        if !ps.is_empty() {
            ps
        } else {
            let vs = str_field(voice, "apiStyle");
            if !vs.is_empty() { vs } else { "auto".to_string() }
        }
    };
    let mut whisper_val = prof
        .and_then(|p| p.get("whisper"))
        .filter(|v| v.is_object())
        .cloned()
        .or_else(|| voice.and_then(|v| v.get("whisper")).filter(|v| v.is_object()).cloned())
        .unwrap_or_else(|| json!({}));
    if let Some(obj) = whisper_val.as_object_mut() {
        let lang_empty = obj
            .get("language")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().is_empty())
            .unwrap_or(true);
        if lang_empty && !language.trim().is_empty() {
            obj.insert("language".to_string(), json!(language.trim()));
        }
    }

    let base_url = {
        let vb = str_field(voice, "baseUrl");
        if !vb.is_empty() { vb } else { str_field(vision, "baseUrl") }
    };
    let api_key = str_field(voice, "apiKey");
    let prompt = str_field(voice, "prompt");
    let mode = str_field(voice, "mode");

    let mut out = json!({
        "engine": engine,
        "apiStyle": api_style,
        "whisper": whisper_val,
    });
    if let Some(m) = out.as_object_mut() {
        if !model.is_empty() {
            m.insert("model".to_string(), json!(model));
        }
        if !base_url.is_empty() {
            m.insert("baseUrl".to_string(), json!(base_url));
        }
        if !api_key.is_empty() {
            m.insert("apiKey".to_string(), json!(api_key));
        }
        if !prompt.is_empty() {
            m.insert("prompt".to_string(), json!(prompt));
        }
        if !mode.is_empty() {
            m.insert("mode".to_string(), json!(mode));
        }
        if let Some(lc) = voice.and_then(|v| v.get("languageCodes")) {
            m.insert("languageCodes".to_string(), lc.clone());
        }
        if let Some(cv) = voice.and_then(|v| v.get("customVocabulary")) {
            m.insert("customVocabulary".to_string(), cv.clone());
        }
    }
    out
}

pub fn is_local_profile_configured(data_dir: &Path, profile: &str) -> bool {
    resolve_voice_profile_config(data_dir, profile, "")
        .get("engine")
        .and_then(|v| v.as_str())
        == Some("whisper-local")
}

pub fn transcribe_wav_file(data_dir: &Path, audio_path: &Path, language: &str) -> Result<String, String> {
    let cfg = resolve_voice_profile_config(data_dir, "realtime", language);
    transcribe_cached_file_with_config(data_dir, audio_path, &cfg.to_string(), false)
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
    if let Ok(raw) = std::fs::read_to_string(data_dir.join("config.json")) {
        if let Ok(val) = serde_json::from_str::<Value>(&raw) {
            for ptr in ["/voice/apiKey", "/vision/apiKey", "/text/apiKey", "/llm/apiKey"] {
                if let Some(k) = val.pointer(ptr).and_then(|v| v.as_str()) {
                    let trimmed = k.trim();
                    if !trimmed.is_empty() {
                        return trimmed.to_string();
                    }
                }
            }
        }
    }
    std::env::var("GEMINI_API_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
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
        let local_res = transcribe_local_with_config(data_dir, cache_path, &cfg);
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
            match save_audio_to_cache(&data_dir, &audio_b64, &mime_type) {
                Ok(p) => {
                    let r = transcribe_local_with_config(&data_dir, &p, &cfg);
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

pub fn is_audio_media(file_name: &str, mime: &str) -> bool {
    if mime.trim().to_ascii_lowercase().starts_with("audio/") {
        return true;
    }
    let ext = Path::new(file_name)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "wav" | "mp3" | "m4a" | "aac" | "ogg" | "oga" | "opus" | "flac" | "weba"
    )
}

pub fn infer_audio_mime(file_name: &str, mime: &str) -> String {
    let trimmed = mime.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    let ext = Path::new(file_name)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "weba" => "audio/webm",
        _ => "audio/wav",
    }
    .to_string()
}

pub fn transcribe_dropped_audio_async(
    data_dir: PathBuf,
    req_id: String,
    file_name: String,
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
        let effective_mime = infer_audio_mime(&file_name, &mime_type);
        let has_cloud_key = cfg
            .get("apiKey")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
            || !read_fallback_gemini_key(&data_dir).is_empty();
        let use_local = engine == "whisper-local" || (engine.is_empty() && !has_cloud_key);

        let res = if use_local {
            match save_audio_to_cache(&data_dir, &audio_b64, &effective_mime) {
                Ok(p) => {
                    let r = transcribe_local_with_config(&data_dir, &p, &cfg);
                    let _ = std::fs::remove_file(&p);
                    r
                }
                Err(e) => Err(e),
            }
        } else {
            transcribe_cloud(&data_dir, &audio_b64, &effective_mime, &cfg)
        };

        let (text, error) = match res {
            Ok(t) => (t, String::new()),
            Err(e) => (String::new(), e),
        };
        if let Some(tx) = sender {
            tx(RpcEvent {
                topic: "media:dropped".to_string(),
                payload: json!({
                    "reqId": req_id,
                    "text": text,
                    "error": error
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

    #[test]
    fn parse_speech_json_output_returns_empty_instead_of_raw_json_when_text_empty() {
        let raw = r#"{"file":"C:\\tmp\\meeting-20261123.wav","text":"","stop":"complete"}"#;
        assert_eq!(parse_speech_json_output(raw), "");

        let with_text = r#"{"file":"C:\\tmp\\meeting.wav","text":"こんにちは","stop":"complete"}"#;
        assert_eq!(parse_speech_json_output(with_text), "こんにちは");

        let with_regions = r#"{"text":"","regions":[{"text":"一行目"},{"text":"二行目"}]}"#;
        assert_eq!(parse_speech_json_output(with_regions), "一行目\n二行目");
    }

    #[test]
    fn is_audio_media_and_infer_audio_mime_detect_wav_and_ignore_png() {
        assert!(is_audio_media("meeting.wav", ""));
        assert!(is_audio_media("voice.mp3", "application/octet-stream"));
        assert!(is_audio_media("clip.bin", "audio/wav"));
        assert!(!is_audio_media("photo.png", "image/png"));
        assert_eq!(infer_audio_mime("meeting.wav", ""), "audio/wav");
        assert_eq!(infer_audio_mime("meeting.wav", "audio/wav"), "audio/wav");
    }

    #[test]
    fn resolve_voice_profile_config_respects_cloud_and_local_profiles() {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("syki_speech_prof_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = json!({
            "voice": {
                "apiKey": "k-test",
                "realtime": { "engine": "gemini", "model": "gemini-3.5-transcribe" },
                "batch": { "engine": "whisper-local", "whisper": { "model": "haas-ja-medium" } }
            }
        });
        std::fs::write(dir.join("config.json"), cfg.to_string()).unwrap();

        let rt = resolve_voice_profile_config(&dir, "realtime", "ja");
        assert_eq!(rt["engine"], "gemini");
        assert_eq!(rt["model"], "gemini-3.5-transcribe");
        assert!(!is_local_profile_configured(&dir, "realtime"));

        let bt = resolve_voice_profile_config(&dir, "batch", "ja");
        assert_eq!(bt["engine"], "whisper-local");
        assert_eq!(bt["whisper"]["model"], "haas-ja-medium");
        assert!(is_local_profile_configured(&dir, "batch"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcribe_wav_file_uses_cloud_path_when_gemini_selected() {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("syki_speech_wav_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = json!({
            "voice": {
                "baseUrl": "http://127.0.0.1:1",
                "apiKey": "dummy-key",
                "realtime": { "engine": "gemini" }
            }
        });
        std::fs::write(dir.join("config.json"), cfg.to_string()).unwrap();
        let wav_path = dir.join("sample.wav");
        std::fs::write(&wav_path, encode_wav_pcm16(&[0i16; 160], 16_000, 1)).unwrap();

        let err = transcribe_wav_file(&dir, &wav_path, "ja").unwrap_err();
        assert_ne!(err, "local speech engine/model not installed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_stt_lock_serializes_concurrent_callers() {
        let guard = local_stt_lock().lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let _g = local_stt_lock().lock().unwrap();
            tx.send(true).unwrap();
        });
        assert!(rx.recv_timeout(std::time::Duration::from_millis(50)).is_err());
        drop(guard);
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), true);
        handle.join().unwrap();
    }
}
