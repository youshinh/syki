//! Meeting Recording via Windows WASAPI Loopback + Mic capture (#15).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use syki_core::rpc::RpcEvent;
use syki_core::storage::atomic::write_atomic_bytes;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
};

use crate::rpc_dispatch::EventSender;
use crate::speech;

/// Mixes loopback + microphone `f32` streams into a 16kHz mono PCM16 WAV byte vector.
pub fn mix_streams_to_wav_16k(
    loopback: &[f32],
    loopback_rate: u32,
    loopback_channels: u16,
    mic: &[f32],
    mic_rate: u32,
    mic_channels: u16,
) -> Vec<u8> {
    let lb_16k = speech::resample_f32_to_mono_16k_i16(loopback, loopback_rate, loopback_channels);
    let mic_16k = speech::resample_f32_to_mono_16k_i16(mic, mic_rate, mic_channels);
    let len = lb_16k.len().max(mic_16k.len());
    let mut mixed = Vec::with_capacity(len);
    for i in 0..len {
        let a = lb_16k.get(i).copied().unwrap_or(0) as i32;
        let b = mic_16k.get(i).copied().unwrap_or(0) as i32;
        let sum = ((a + b) / if a != 0 && b != 0 { 2 } else { 1 }).clamp(-32768, 32767) as i16;
        mixed.push(sum);
    }
    speech::encode_wav_pcm16(&mixed, 16_000, 1)
}

/// Probes Windows WASAPI default render (loopback) and capture (mic) endpoints.
pub fn probe_wasapi_endpoints() -> Value {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: Result<IMMDeviceEnumerator, _> =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL);
        let Ok(enum_iface) = enumerator else {
            return json!({
                "microphone": { "ok": false, "error": "MMDeviceEnumerator unavailable" },
                "system": { "ok": false, "error": "MMDeviceEnumerator unavailable" },
                "loopbackReady": false,
                "micReady": false,
                "error": "MMDeviceEnumerator unavailable"
            });
        };
        let loopback_res = enum_iface.GetDefaultAudioEndpoint(eRender, eConsole);
        let mic_res = enum_iface.GetDefaultAudioEndpoint(eCapture, eConsole);
        let loopback_ready = loopback_res.is_ok();
        let mic_ready = mic_res.is_ok();
        let mic_err = mic_res.err().map(|e| e.to_string()).unwrap_or_default();
        let sys_err = loopback_res.err().map(|e| e.to_string()).unwrap_or_default();
        json!({
            "microphone": { "ok": mic_ready, "error": mic_err },
            "system": { "ok": loopback_ready, "error": sys_err },
            "loopbackReady": loopback_ready,
            "micReady": mic_ready,
            "error": ""
        })
    }
}

fn capture_wasapi_stream(
    is_loopback: bool,
    stop: Arc<AtomicBool>,
    out_samples: Arc<Mutex<Vec<f32>>>,
    out_rate: Arc<Mutex<u32>>,
    out_channels: Arc<Mutex<u16>>,
) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(enumerator): Result<IMMDeviceEnumerator, _> =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        else {
            return;
        };
        let flow = if is_loopback { eRender } else { eCapture };
        let Ok(device) = enumerator.GetDefaultAudioEndpoint(flow, eConsole) else {
            return;
        };
        let Ok(client): Result<IAudioClient, _> = device.Activate(CLSCTX_ALL, None) else {
            return;
        };
        let Ok(fmt_ptr) = client.GetMixFormat() else {
            return;
        };
        let fmt: WAVEFORMATEX = *fmt_ptr;
        let rate = fmt.nSamplesPerSec;
        let channels = fmt.nChannels;
        let bits = fmt.wBitsPerSample;
        *out_rate.lock().expect("rate mutex") = rate;
        *out_channels.lock().expect("channels mutex") = channels;

        let flags = if is_loopback {
            AUDCLNT_STREAMFLAGS_LOOPBACK
        } else {
            0
        };
        let init_ok = client
            .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 10_000_000, 0, fmt_ptr, None)
            .is_ok();
        CoTaskMemFree(Some(fmt_ptr as *const _));
        if !init_ok {
            return;
        }
        let Ok(capture): Result<IAudioCaptureClient, _> = client.GetService() else {
            return;
        };
        if client.Start().is_err() {
            return;
        }

        while !stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(20));
            while let Ok(pkt_frames) = capture.GetNextPacketSize() {
                if pkt_frames == 0 {
                    break;
                }
                let mut data_ptr: *mut u8 = std::ptr::null_mut();
                let mut num_frames = 0u32;
                let mut buf_flags = 0u32;
                if capture
                    .GetBuffer(&mut data_ptr, &mut num_frames, &mut buf_flags, None, None)
                    .is_err()
                {
                    break;
                }
                let total_samples = (num_frames as usize) * (channels as usize);
                if !data_ptr.is_null() && total_samples > 0 {
                    let mut guard = out_samples.lock().expect("samples mutex");
                    if bits == 32 {
                        let slice = std::slice::from_raw_parts(data_ptr as *const f32, total_samples);
                        guard.extend_from_slice(slice);
                    } else if bits == 16 {
                        let slice = std::slice::from_raw_parts(data_ptr as *const i16, total_samples);
                        for &s in slice {
                            guard.push((s as f32) / 32768.0);
                        }
                    }
                }
                let _ = capture.ReleaseBuffer(num_frames);
            }
        }
        let _ = client.Stop();
    }
}

struct ActiveRecording {
    stop: Arc<AtomicBool>,
    lb_samples: Arc<Mutex<Vec<f32>>>,
    lb_rate: Arc<Mutex<u32>>,
    lb_channels: Arc<Mutex<u16>>,
    mic_samples: Arc<Mutex<Vec<f32>>>,
    mic_rate: Arc<Mutex<u32>>,
    mic_channels: Arc<Mutex<u16>>,
    joins: Vec<std::thread::JoinHandle<()>>,
}

/// Manages active WASAPI meeting recording session.
#[derive(Default)]
pub struct MeetingRecorder {
    active: Mutex<Option<ActiveRecording>>,
}

impl MeetingRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_recording(&self) -> bool {
        self.active.lock().expect("recorder mutex").is_some()
    }

    pub fn start(&self) -> Result<(), String> {
        let mut guard = self.active.lock().expect("recorder mutex");
        if guard.is_some() {
            return Err("meeting recording already active".to_string());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let lb_samples = Arc::new(Mutex::new(Vec::new()));
        let lb_rate = Arc::new(Mutex::new(48_000u32));
        let lb_channels = Arc::new(Mutex::new(2u16));
        let mic_samples = Arc::new(Mutex::new(Vec::new()));
        let mic_rate = Arc::new(Mutex::new(48_000u32));
        let mic_channels = Arc::new(Mutex::new(1u16));

        let s1 = Arc::clone(&stop);
        let l1 = Arc::clone(&lb_samples);
        let r1 = Arc::clone(&lb_rate);
        let c1 = Arc::clone(&lb_channels);
        let j1 = std::thread::spawn(move || capture_wasapi_stream(true, s1, l1, r1, c1));

        let s2 = Arc::clone(&stop);
        let m2 = Arc::clone(&mic_samples);
        let r2 = Arc::clone(&mic_rate);
        let c2 = Arc::clone(&mic_channels);
        let j2 = std::thread::spawn(move || capture_wasapi_stream(false, s2, m2, r2, c2));

        *guard = Some(ActiveRecording {
            stop,
            lb_samples,
            lb_rate,
            lb_channels,
            mic_samples,
            mic_rate,
            mic_channels,
            joins: vec![j1, j2],
        });
        Ok(())
    }

    pub fn abort(&self) -> bool {
        let rec = self.active.lock().expect("recorder mutex").take();
        if let Some(r) = rec {
            r.stop.store(true, Ordering::Release);
            for j in r.joins {
                let _ = j.join();
            }
            true
        } else {
            false
        }
    }

    pub fn stop_async(
        &self,
        data_dir: PathBuf,
        req_id: String,
        voice_config_json: String,
        sender: Option<EventSender>,
    ) {
        let rec = self.active.lock().expect("recorder mutex").take();
        std::thread::spawn(move || {
            let Some(r) = rec else {
                if let Some(tx) = sender {
                    tx(RpcEvent {
                        topic: "voice:result".to_string(),
                        payload: json!({
                            "reqId": req_id,
                            "text": "",
                            "error": "no active meeting recording",
                            "cachePath": "",
                            "refineErr": ""
                        }),
                    });
                }
                return;
            };

            r.stop.store(true, Ordering::Release);
            for j in r.joins {
                let _ = j.join();
            }

            let lb = r.lb_samples.lock().expect("lb mutex").clone();
            let lb_rate = *r.lb_rate.lock().expect("lb rate");
            let lb_ch = *r.lb_channels.lock().expect("lb ch");
            let mic = r.mic_samples.lock().expect("mic mutex").clone();
            let mic_rate = *r.mic_rate.lock().expect("mic rate");
            let mic_ch = *r.mic_channels.lock().expect("mic ch");

            let wav = mix_streams_to_wav_16k(&lb, lb_rate, lb_ch, &mic, mic_rate, mic_ch);
            let cache_dir = speech::voice_cache_dir(&data_dir);
            let _ = std::fs::create_dir_all(&cache_dir);
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
            let wav_path = cache_dir.join(format!("meeting-{stamp}.wav"));
            let _ = write_atomic_bytes(&wav_path, &wav);

            let trimmed_cfg = voice_config_json.trim();
            let effective_cfg_json = if trimmed_cfg.is_empty() || trimmed_cfg == "{}" {
                speech::resolve_voice_profile_config(&data_dir, "batch", "ja").to_string()
            } else {
                trimmed_cfg.to_string()
            };
            let (text, error, cache_out) =
                match speech::transcribe_cached_file_with_config(&data_dir, &wav_path, &effective_cfg_json, true) {
                    Ok(t) => (t, String::new(), String::new()),
                    Err(e) => (String::new(), e, wav_path.to_string_lossy().into_owned()),
                };
            if let Some(tx) = sender {
                tx(RpcEvent {
                    topic: "voice:result".to_string(),
                    payload: json!({
                        "reqId": req_id,
                        "text": text,
                        "error": error,
                        "cachePath": cache_out,
                        "refineErr": ""
                    }),
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_streams_produces_valid_16k_mono_wav() {
        let lb = vec![0.25f32; 960]; // 10ms @ 48k stereo
        let mic = vec![0.25f32; 480]; // 10ms @ 48k mono
        let wav = mix_streams_to_wav_16k(&lb, 48_000, 2, &mic, 48_000, 1);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(wav.len(), 44 + 160 * 2);
    }

    #[test]
    fn probe_wasapi_endpoints_returns_structured_json() {
        let val = probe_wasapi_endpoints();
        assert!(val.get("loopbackReady").is_some());
        assert!(val.get("micReady").is_some());
    }
}
