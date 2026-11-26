//! Core RPC dispatcher for `#04` (config, session, note I/O, folder scan,
//! conflict-checked saves, active file watcher push events, and shell/dialogs).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use syki_core::policy::{PolicyDecisionEngine, PolicyVerdict, TrustRegistry};
use syki_core::rpc::{RpcEvent, RpcRequest, RpcResponse, RuntimeEnv};
use syki_core::search::{scrap_filter_options, search_scraps_parallel, ScrapFilter};
use syki_core::semindex::{
    build_local_semantic_index, embed_local_hash, semantic_status, MmapVectorIndex,
};
use syki_core::storage::note::cyrb53_utf16_sig;
use syki_core::storage::{
    ActiveFileWatcher, ConfigStore, JsonConfigStore, LoadedNote, NoteFileStore, ScrapStore,
    SessionStore,
};

use crate::webview_host::HostHandle;
use crate::{assets_store, clipboard, dialogs, host_lifecycle, ime, llm, meeting_audio, printing, pty, shell_pipe, speech};

pub type EventSender = Arc<dyn Fn(RpcEvent) + Send + Sync>;

/// Shared backend state for the single-window Win32 + WebView2 host.
pub struct CoreState {
    env: RuntimeEnv,
    config_store: JsonConfigStore,
    session_store: SessionStore,
    watcher: Mutex<Option<ActiveFileWatcher>>,
    startup_file: Mutex<Option<PathBuf>>,
    event_sender: Mutex<Option<EventSender>>,
    host_handle: Mutex<Option<HostHandle>>,
    shell_pipe: shell_pipe::ShellPipeManager,
    print_preview: printing::PrintPreviewStore,
    pty_mgr: pty::PtyManager,
    meeting: meeting_audio::MeetingRecorder,
    policy: Mutex<PolicyDecisionEngine>,
    llm_cancel: Arc<llm::LlmCancelRegistry>,
}

impl CoreState {
    pub fn new(env: RuntimeEnv, startup_file: Option<PathBuf>) -> Self {
        let config_store = JsonConfigStore::new(env.data_dir.join("config.json"));
        let session_store = SessionStore::new(env.data_dir.join("session.json"));
        let trust_path = env.data_dir.join("trust-registry.json");
        let trust_registry: TrustRegistry = std::fs::read_to_string(&trust_path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Self {
            env,
            config_store,
            session_store,
            watcher: Mutex::new(None),
            startup_file: Mutex::new(startup_file),
            event_sender: Mutex::new(None),
            host_handle: Mutex::new(None),
            shell_pipe: shell_pipe::ShellPipeManager::new(),
            print_preview: printing::PrintPreviewStore::new(),
            pty_mgr: pty::PtyManager::new(),
            meeting: meeting_audio::MeetingRecorder::new(),
            policy: Mutex::new(PolicyDecisionEngine::new(trust_registry)),
            llm_cancel: Arc::new(llm::LlmCancelRegistry::new()),
        }
    }

    fn save_trust_registry(&self, registry: &TrustRegistry) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(registry)
            .map_err(|e| format!("failed to serialize trust registry: {e}"))?;
        let path = self.env.data_dir.join("trust-registry.json");
        syki_core::storage::atomic::write_atomic_bytes(&path, &bytes)
            .map_err(|e| format!("failed to save trust registry: {e}"))
    }

    pub fn print_preview_store(&self) -> printing::PrintPreviewStore {
        self.print_preview.clone()
    }

    pub fn set_event_sender(&self, sender: EventSender) {
        *self.event_sender.lock().expect("event_sender mutex") = Some(sender);
    }

    pub fn set_host_handle(&self, handle: HostHandle) {
        self.set_event_sender(handle.event_sender.clone());
        *self.host_handle.lock().expect("host_handle mutex") = Some(handle);
    }

    pub fn dispatch(&self, req: &RpcRequest) -> RpcResponse {
        match req.method.as_str() {
            "getAppVersion" => RpcResponse::ok(&req.id, json!(syki_core::APP_VERSION)),
            "getAppInfo" => RpcResponse::ok(
                &req.id,
                json!({
                    "version": syki_core::APP_VERSION,
                    "os": "windows",
                    "arch": std::env::consts::ARCH,
                    "dataDir": self.env.data_dir,
                    "instanceId": self.env.instance_id,
                    "runtime": "rust-webview2"
                }),
            ),
            "getPlatformCapabilities" => RpcResponse::ok(
                &req.id,
                json!({
                    "os": "win32",
                    "nativeImeSwitch": true,
                    "tray": true,
                    "globalHotkey": true,
                    "aiOcr": true,
                    "winrtOcr": false
                }),
            ),
            "uiReady" => RpcResponse::ok(&req.id, json!(true)),

            "getConfig" => match self.config_store.load_resolved_json() {
                Ok(raw) => RpcResponse::ok(&req.id, Value::String(raw)),
                Err(e) => RpcResponse::err(&req.id, format!("getConfig failed: {e}")),
            },
            "saveConfig" => {
                let Some(cfg) = param_value(&req.params, 0, "config") else {
                    return RpcResponse::err(&req.id, "saveConfig: missing config payload");
                };
                let raw = match cfg {
                    Value::String(s) => s,
                    other => serde_json::to_string(&other).unwrap_or_else(|_| "{}".to_string()),
                };
                match self.config_store.save_raw(&raw) {
                    Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                    Err(e) => RpcResponse::err(&req.id, format!("saveConfig failed: {e}")),
                }
            }

            "getSession" => match self.session_store.load_raw() {
                Ok(raw) => RpcResponse::ok(&req.id, Value::String(raw)),
                Err(e) => RpcResponse::err(&req.id, format!("getSession failed: {e}")),
            },
            "saveSession" | "saveSessionSync" => {
                let Some(sess) = param_value(&req.params, 0, "session") else {
                    return RpcResponse::err(&req.id, "saveSession: missing session payload");
                };
                let raw = match sess {
                    Value::String(s) => s,
                    other => serde_json::to_string(&other).unwrap_or_else(|_| "{}".to_string()),
                };
                match self.session_store.save_raw(&raw) {
                    Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                    Err(e) => RpcResponse::err(&req.id, format!("saveSession failed: {e}")),
                }
            }

            "getStartupFile" => {
                let path_opt = self
                    .startup_file
                    .lock()
                    .expect("startup_file mutex")
                    .take();
                match path_opt {
                    Some(p) if p.is_file() => match NoteFileStore::read_note(&p) {
                        Ok(loaded) => RpcResponse::ok(&req.id, loaded_note_to_json(&loaded)),
                        Err(e) => RpcResponse::err(&req.id, format!("getStartupFile failed: {e}")),
                    },
                    _ => RpcResponse::ok(&req.id, Value::Null),
                }
            }

            "openFile" => match dialogs::pick_open_file(None, None) {
                Ok(Some(path)) => match NoteFileStore::read_note(&path) {
                    Ok(loaded) => RpcResponse::ok(&req.id, loaded_note_to_json(&loaded)),
                    Err(e) => RpcResponse::err(&req.id, format!("openFile read failed: {e}")),
                },
                Ok(None) => RpcResponse::ok(&req.id, Value::Null),
                Err(e) => RpcResponse::err(&req.id, e),
            },

            "openFolder" => match dialogs::pick_folder(None, None) {
                Ok(Some(path)) => RpcResponse::ok(&req.id, json!(path.to_string_lossy())),
                Ok(None) => RpcResponse::ok(&req.id, Value::Null),
                Err(e) => RpcResponse::err(&req.id, e),
            },

            "scanFolderFiles" => {
                let dir = param_str(&req.params, 0, "dir").unwrap_or_default();
                if dir.trim().is_empty() {
                    return RpcResponse::err(&req.id, "scanFolderFiles: missing dir");
                }
                let entries = scan_folder_entries(Path::new(dir.trim()));
                RpcResponse::ok(&req.id, json!(entries))
            }

            "readFileByPath" => {
                let path = param_str(&req.params, 0, "path").unwrap_or_default();
                if path.trim().is_empty() {
                    return RpcResponse::err(&req.id, "readFileByPath: missing path");
                }
                match NoteFileStore::read_note(Path::new(path.trim())) {
                    Ok(loaded) => RpcResponse::ok(&req.id, loaded_note_to_json(&loaded)),
                    Err(e) => RpcResponse::err(&req.id, format!("readFileByPath failed: {e}")),
                }
            }

            "saveFile" => {
                let path = param_str(&req.params, 0, "path").unwrap_or_default();
                let content = param_str(&req.params, 1, "content").unwrap_or_default();
                let encoding = param_str(&req.params, 2, "encoding").unwrap_or_else(|| "UTF-8".into());
                if path.trim().is_empty() {
                    return RpcResponse::err(&req.id, "saveFile: missing path");
                }
                let p = Path::new(path.trim());
                match NoteFileStore::write_atomic(p, &content, &encoding) {
                    Ok(_) => RpcResponse::ok(&req.id, save_result_json(p, &content, false)),
                    Err(e) => RpcResponse::err(&req.id, format!("saveFile failed: {e}")),
                }
            }

            "saveFileChecked" | "saveFileCheckedAsync" => {
                let path = param_str(&req.params, 0, "path").unwrap_or_default();
                let content = param_str(&req.params, 1, "content").unwrap_or_default();
                let encoding = param_str(&req.params, 2, "encoding").unwrap_or_else(|| "UTF-8".into());
                let expect_sig = param_str(&req.params, 3, "expectSig").unwrap_or_default();
                if path.trim().is_empty() {
                    return RpcResponse::err(&req.id, "saveFileChecked: missing path");
                }
                let p = Path::new(path.trim());
                let expected = expect_sig.trim();
                if !expected.is_empty() && p.exists() {
                    match NoteFileStore::read_note(p) {
                        Ok(existing) => {
                            let existing_cyrb = cyrb53_utf16_sig(&existing.content);
                            let matches_cyrb = existing_cyrb.eq_ignore_ascii_case(expected);
                            let matches_sha = existing.sig.sha256_hex.eq_ignore_ascii_case(expected);
                            if !matches_cyrb && !matches_sha {
                                return RpcResponse::ok(
                                    &req.id,
                                    json!({
                                        "path": p.to_string_lossy(),
                                        "title": file_title(p),
                                        "success": false,
                                        "conflict": true,
                                        "sig": existing_cyrb
                                    }),
                                );
                            }
                        }
                        Err(e) => {
                            return RpcResponse::err(&req.id, format!("saveFileChecked read failed: {e}"));
                        }
                    }
                }
                match NoteFileStore::write_atomic(p, &content, &encoding) {
                    Ok(_) => RpcResponse::ok(&req.id, save_result_json(p, &content, false)),
                    Err(e) => RpcResponse::err(&req.id, format!("saveFileChecked write failed: {e}")),
                }
            }

            "saveFileAs" => {
                let content = param_str(&req.params, 0, "content").unwrap_or_default();
                let encoding = param_str(&req.params, 1, "encoding").unwrap_or_else(|| "UTF-8".into());
                let suggested = param_str(&req.params, 2, "suggestedPath");
                match dialogs::pick_save_file(None, suggested.as_deref(), false) {
                    Ok(Some(path)) => match NoteFileStore::write_atomic(&path, &content, &encoding) {
                        Ok(_) => RpcResponse::ok(&req.id, save_result_json(&path, &content, false)),
                        Err(e) => RpcResponse::err(&req.id, format!("saveFileAs write failed: {e}")),
                    },
                    Ok(None) => RpcResponse::ok(&req.id, Value::Null),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "createNoteInFolder" => {
                let content = param_str(&req.params, 0, "content").unwrap_or_default();
                let encoding = param_str(&req.params, 1, "encoding").unwrap_or_else(|| "UTF-8".into());
                let suggested_name = param_str(&req.params, 2, "suggestedName").unwrap_or_default();
                let folder = param_str(&req.params, 3, "folder").unwrap_or_default();
                let target_dir = resolve_note_folder(&self.env, &folder);
                if let Err(e) = std::fs::create_dir_all(&target_dir) {
                    return RpcResponse::err(&req.id, format!("createNoteInFolder mkdir failed: {e}"));
                }
                let target_path = unique_note_path(&target_dir, &suggested_name);
                match NoteFileStore::write_atomic(&target_path, &content, &encoding) {
                    Ok(_) => RpcResponse::ok(&req.id, save_result_json(&target_path, &content, false)),
                    Err(e) => RpcResponse::err(&req.id, format!("createNoteInFolder write failed: {e}")),
                }
            }

            "exportPlainTextAs" => {
                let content = param_str(&req.params, 0, "content").unwrap_or_default();
                let suggested = param_str(&req.params, 1, "suggestedName");
                match dialogs::pick_save_file(None, suggested.as_deref(), true) {
                    Ok(Some(path)) => match NoteFileStore::write_atomic(&path, &content, "UTF-8") {
                        Ok(_) => RpcResponse::ok(&req.id, save_result_json(&path, &content, false)),
                        Err(e) => RpcResponse::err(&req.id, format!("exportPlainTextAs write failed: {e}")),
                    },
                    Ok(None) => RpcResponse::ok(&req.id, Value::Null),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "resolveDroppedFile" => {
                let name = param_str(&req.params, 0, "name").unwrap_or_default();
                let size = param_u64(&req.params, 1, "size").unwrap_or(0);
                let _last_modified = param_u64(&req.params, 2, "lastModified");
                let dirs_raw = param_str(&req.params, 3, "dirs").unwrap_or_default();
                let resolved = resolve_dropped_file_in_dirs(&name, size, &dirs_raw);
                RpcResponse::ok(&req.id, json!(resolved))
            }

            "watchActiveFile" => {
                let path = param_str(&req.params, 0, "path").unwrap_or_default();
                if path.trim().is_empty() {
                    return RpcResponse::err(&req.id, "watchActiveFile: missing path");
                }
                if let Err(e) = self.ensure_watcher_and_watch(Path::new(path.trim())) {
                    return RpcResponse::err(&req.id, format!("watchActiveFile failed: {e}"));
                }
                RpcResponse::ok(&req.id, json!(true))
            }

            "unwatchActiveFile" => {
                let guard = self.watcher.lock().expect("watcher mutex");
                if let Some(w) = guard.as_ref() {
                    let _ = w.unwatch();
                }
                RpcResponse::ok(&req.id, json!(true))
            }

            "openExternal" => {
                let url = param_str(&req.params, 0, "url").unwrap_or_default();
                match dialogs::open_external_url(&url) {
                    Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "openPath" => {
                let path = param_str(&req.params, 0, "path").unwrap_or_default();
                match dialogs::open_path_in_shell(Path::new(path.trim())) {
                    Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "revealPath" | "showInFileExplorer" => {
                let path = param_str(&req.params, 0, "path").unwrap_or_default();
                match dialogs::reveal_in_explorer(Path::new(path.trim())) {
                    Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "saveAsset" => {
                let base_dir = param_str(&req.params, 0, "baseDir").unwrap_or_default();
                let ext = param_str(&req.params, 1, "ext").unwrap_or_default();
                let data_b64 = param_str(&req.params, 2, "dataBase64").unwrap_or_default();
                match assets_store::save_asset(&self.env.data_dir, &base_dir, &ext, &data_b64) {
                    Ok(res) => RpcResponse::ok(&req.id, serde_json::to_value(res).unwrap_or(Value::Null)),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "importAssetFile" => {
                let base_dir = param_str(&req.params, 0, "baseDir").unwrap_or_default();
                let file_name = param_str(&req.params, 1, "fileName").unwrap_or_default();
                let data_b64 = param_str(&req.params, 2, "dataBase64").unwrap_or_default();
                match assets_store::import_asset_file(&self.env.data_dir, &base_dir, &file_name, &data_b64) {
                    Ok(res) => RpcResponse::ok(&req.id, serde_json::to_value(res).unwrap_or(Value::Null)),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "copyPNGToClipboard" => {
                let data_url = param_str(&req.params, 0, "dataUrl").unwrap_or_default();
                match clipboard::copy_png_to_clipboard(&data_url) {
                    Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "setIMEMode" => {
                let enable = param_bool(&req.params, 0, "enableJapanese").unwrap_or(false);
                ime::set_ime_mode(enable);
                RpcResponse::ok(&req.id, json!(true))
            }

            "retypeWithImeAsync" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let romaji = param_str(&req.params, 1, "romaji").unwrap_or_default();
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                ime::retype_with_ime_async(req_id, romaji, sender);
                RpcResponse::ok(&req.id, json!(true))
            }

            "runCommandFilter" => {
                let cmd_str = param_str(&req.params, 0, "cmdStr").unwrap_or_default();
                let input = param_str(&req.params, 1, "input").unwrap_or_default();
                let res = shell_pipe::run_command_filter(&cmd_str, &input, None);
                RpcResponse::ok(&req.id, serde_json::to_value(res).unwrap_or(Value::Null))
            }

            "runCommandFilterAsync" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let cmd_str = param_str(&req.params, 1, "cmdStr").unwrap_or_default();
                let input = param_str(&req.params, 2, "input").unwrap_or_default();
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                self.shell_pipe.run_async(req_id, cmd_str, input, sender);
                RpcResponse::ok(&req.id, json!(true))
            }

            "cancelCommandFilter" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let cancelled = self.shell_pipe.cancel(&req_id);
                RpcResponse::ok(&req.id, json!(cancelled))
            }

            "getScrapDir" => {
                let dir = ScrapStore::resolve_scrap_dir(&self.config_store.scrap_dir());
                RpcResponse::ok(&req.id, json!(dir.to_string_lossy()))
            }

            "searchScraps" => {
                let query = param_str(&req.params, 0, "query").unwrap_or_default();
                let limit = param_u64(&req.params, 1, "maxResults").unwrap_or(100) as usize;
                let filter_raw = param_str(&req.params, 2, "filter")
                    .or_else(|| {
                        param_value(&req.params, 2, "filter")
                            .map(|v| v.to_string())
                    })
                    .unwrap_or_default();
                let filter = match ScrapFilter::parse(&filter_raw) {
                    Ok(f) => f,
                    Err(e) => return RpcResponse::err(&req.id, e),
                };
                let dir = self.config_store.scrap_dir();
                let results = search_scraps_parallel(&dir, &query, limit, &filter);
                RpcResponse::ok(&req.id, serde_json::to_value(results).unwrap_or(json!([])))
            }

            "scrapFilterOptions" => {
                let dir = self.config_store.scrap_dir();
                let opts = scrap_filter_options(&dir);
                RpcResponse::ok(&req.id, serde_json::to_value(opts).unwrap_or(json!({})))
            }

            "semanticStatus" => {
                let index_dir = self.env.data_dir.join("semindex");
                let sec = param_value(&req.params, 0, "section").unwrap_or_else(|| {
                    self.config_store
                        .load_raw()
                        .ok()
                        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                        .and_then(|v| v.get("semantic").cloned())
                        .unwrap_or_else(|| json!({}))
                });
                let enabled = sec.get("enabled").and_then(|b| b.as_bool()).unwrap_or(false);
                let base_url = sec
                    .pointer("/model/baseUrl")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let model = sec
                    .pointer("/model/model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let destination = extract_host_port(base_url);
                let local = is_local_host(&destination);
                let consent_given = local
                    || sec
                        .pointer("/privacy/cloudConsent")
                        .and_then(|c| c.get(&destination))
                        .or_else(|| sec.get("consent").and_then(|c| c.get(&destination)))
                        .map(|v| {
                            v.as_bool() == Some(true)
                                || v.as_str().map(|s| !s.is_empty()).unwrap_or(false)
                        })
                        .unwrap_or(false);
                let scrap_dir = ScrapStore::resolve_scrap_dir(&self.config_store.scrap_dir());
                let folder_missing = !scrap_dir.is_dir();
                let report = semantic_status(&index_dir, enabled);
                let exists = report.state == "ready";
                let updated = std::fs::metadata(index_dir.join("vectors.bin"))
                    .and_then(|m| m.modified())
                    .ok()
                    .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
                    .unwrap_or_default();
                RpcResponse::ok(
                    &req.id,
                    json!({
                        "enabled": enabled,
                        "model": model,
                        "destination": destination,
                        "local": local,
                        "consent_given": consent_given,
                        "folder_missing": folder_missing,
                        "scrap_dir": scrap_dir.to_string_lossy(),
                        "exists": exists,
                        "corrupt": false,
                        "files": report.files_indexed,
                        "chunks": report.chunks_indexed,
                        "size_bytes": report.index_bytes,
                        "updated": updated,
                        "rebuild_needed": false,
                        "new_files": 0,
                        "changed_files": 0,
                        "removed_files": 0,
                        "state": report.state,
                        "filesIndexed": report.files_indexed,
                        "chunksIndexed": report.chunks_indexed,
                        "indexBytes": report.index_bytes,
                        "simdBackend": report.simd_backend
                    }),
                )
            }

            "semanticUpdate" => {
                let sec = param_value(&req.params, 0, "section").unwrap_or_else(|| json!({}));
                if sec.get("enabled").and_then(|b| b.as_bool()) == Some(false) {
                    return RpcResponse::err(&req.id, "not_enabled");
                }
                let rebuild = param_bool(&req.params, 1, "rebuild").unwrap_or(false);
                let scrap_dir = ScrapStore::resolve_scrap_dir(&self.config_store.scrap_dir());
                let index_dir = self.env.data_dir.join("semindex");
                let t0 = std::time::Instant::now();
                match build_local_semantic_index(&scrap_dir, &index_dir) {
                    Ok((files, chunks)) => {
                        let secs = (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0;
                        if let Some(tx) = self.event_sender.lock().expect("event_sender mutex").as_ref() {
                            tx(RpcEvent {
                                topic: "semantic:progress".to_string(),
                                payload: json!({
                                    "filesDone": files,
                                    "filesTotal": files,
                                    "chunksDone": chunks,
                                    "chunksTotal": chunks
                                }),
                            });
                        }
                        RpcResponse::ok(
                            &req.id,
                            json!({
                                "files": files,
                                "files_changed": files,
                                "files_removed": 0,
                                "chunks": chunks,
                                "embedded": chunks,
                                "seconds": secs,
                                "rebuilt": rebuild,
                                "state": "ready"
                            }),
                        )
                    }
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "cancelSemanticUpdate" => RpcResponse::ok(&req.id, json!(true)),

            "searchScrapsSemantic" => {
                let query = param_str(&req.params, 0, "query").unwrap_or_default();
                let limit = param_u64(&req.params, 1, "limit").unwrap_or(20) as usize;
                let index_dir = self.env.data_dir.join("semindex");
                if let Ok(idx) = MmapVectorIndex::open_mmap(&index_dir) {
                    let q_vec = embed_local_hash(&query, idx.dim());
                    let hits = idx.search(&q_vec, limit);
                    return RpcResponse::ok(
                        &req.id,
                        json!({
                            "results": hits,
                            "notes": [],
                            "truncated": false,
                            "semantic": true,
                            "pending": 0,
                            "leftOut": 0
                        }),
                    );
                }
                let dir = self.config_store.scrap_dir();
                let fallback = search_scraps_parallel(&dir, &query, limit, &ScrapFilter::default());
                RpcResponse::ok(
                    &req.id,
                    json!({
                        "results": fallback,
                        "notes": [],
                        "truncated": false,
                        "semantic": false,
                        "pending": 0,
                        "leftOut": 0
                    }),
                )
            }

            "trimMemory" => {
                host_lifecycle::trim_working_set_async();
                RpcResponse::ok(&req.id, json!(true))
            }

            "minimizeWindow" => {
                if let Some(h) = self.host_handle.lock().expect("host_handle mutex").as_ref() {
                    h.minimize();
                }
                RpcResponse::ok(&req.id, json!(true))
            }

            "toggleMaximize" | "toggleFullscreen" => {
                if let Some(h) = self.host_handle.lock().expect("host_handle mutex").as_ref() {
                    h.toggle_maximize();
                }
                RpcResponse::ok(&req.id, json!(true))
            }

            "closeWindow" => {
                if let Some(h) = self.host_handle.lock().expect("host_handle mutex").as_ref() {
                    h.hide_to_tray();
                }
                RpcResponse::ok(&req.id, json!(true))
            }

            "forceQuit" => {
                if let Some(h) = self.host_handle.lock().expect("host_handle mutex").as_ref() {
                    h.force_quit();
                }
                RpcResponse::ok(&req.id, json!(true))
            }

            "openQuickCapture" => {
                if let Some(h) = self.host_handle.lock().expect("host_handle mutex").as_ref() {
                    h.open_quick_capture();
                }
                RpcResponse::ok(&req.id, json!(true))
            }

            "updateGlobalShortcut" => {
                let spec = param_str(&req.params, 0, "shortcut").unwrap_or_default();
                let guard = self.host_handle.lock().expect("host_handle mutex");
                if let Some(h) = guard.as_ref() {
                    match h.update_main_hotkey(&spec) {
                        Ok(ok) => RpcResponse::ok(&req.id, json!(ok)),
                        Err(e) => RpcResponse::err(&req.id, e),
                    }
                } else {
                    match host_lifecycle::parse_shortcut(&spec) {
                        Some(_) => RpcResponse::ok(&req.id, json!(true)),
                        None => RpcResponse::err(&req.id, format!("invalid shortcut: {spec}")),
                    }
                }
            }

            "updateQuickCaptureShortcut" => {
                let spec = param_str(&req.params, 0, "shortcut").unwrap_or_default();
                let guard = self.host_handle.lock().expect("host_handle mutex");
                if let Some(h) = guard.as_ref() {
                    match h.update_qc_hotkey(&spec) {
                        Ok(ok) => RpcResponse::ok(&req.id, json!(ok)),
                        Err(e) => RpcResponse::err(&req.id, e),
                    }
                } else if spec.trim().is_empty() || host_lifecycle::parse_shortcut(&spec).is_some() {
                    RpcResponse::ok(&req.id, json!(true))
                } else {
                    RpcResponse::err(&req.id, format!("invalid shortcut: {spec}"))
                }
            }

            "isSendToShortcutInstalled" => {
                let installed = sendto_cmd_path().map(|p| p.is_file()).unwrap_or(false);
                RpcResponse::ok(&req.id, json!(installed))
            }
            "installSendToShortcut" => match sendto_cmd_path() {
                Some(p) => {
                    let exe = std::env::current_exe()
                        .map(|e| e.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| "syki.exe".to_string());
                    let body = format!("@echo off\r\nstart \"\" \"{exe}\" \"%~1\"\r\n");
                    if let Some(parent) = p.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    match std::fs::write(&p, body) {
                        Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                        Err(e) => RpcResponse::err(&req.id, format!("installSendToShortcut failed: {e}")),
                    }
                }
                None => RpcResponse::err(&req.id, "APPDATA not set"),
            },
            "uninstallSendToShortcut" => match sendto_cmd_path() {
                Some(p) => {
                    if p.exists() {
                        if let Err(e) = std::fs::remove_file(&p) {
                            return RpcResponse::err(&req.id, format!("uninstallSendToShortcut failed: {e}"));
                        }
                    }
                    RpcResponse::ok(&req.id, json!(true))
                }
                None => RpcResponse::err(&req.id, "APPDATA not set"),
            },
            "fileTypesStatus" => RpcResponse::ok(&req.id, json!({ "registered": false })),

            "printPreview" => {
                let opts_val = param_value(&req.params, 0, "options").unwrap_or_else(|| json!({}));
                let settings: printing::PrintSettings = match opts_val {
                    Value::String(s) => serde_json::from_str(&s).unwrap_or_default(),
                    other => serde_json::from_value(other).unwrap_or_default(),
                };
                let cdp_params = match settings.cdp_params() {
                    Ok(p) => p,
                    Err(e) => return RpcResponse::err(&req.id, e),
                };
                let handle = self.host_handle.lock().expect("host_handle mutex").clone();
                let Some(h) = handle else {
                    return RpcResponse::err(&req.id, "printPreview requires active WebView2 host");
                };
                match h
                    .call_devtools_protocol("Page.printToPDF", &cdp_params.to_string())
                    .and_then(|raw| printing::decode_pdf_result(&raw))
                {
                    Ok(pdf) => {
                        let (url, pages) = self.print_preview.set_pdf(pdf);
                        RpcResponse::ok(&req.id, json!({ "url": url, "pages": pages }))
                    }
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "printPickPdfPath" => {
                let suggested = param_str(&req.params, 0, "suggestedName");
                match printing::pick_pdf_save_path(suggested.as_deref()) {
                    Ok(Some(p)) => RpcResponse::ok(&req.id, json!(p.to_string_lossy())),
                    Ok(None) => RpcResponse::ok(&req.id, Value::Null),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "printSavePdf" => {
                let opts_val = param_value(&req.params, 0, "options").unwrap_or_else(|| json!({}));
                let out_path = param_str(&req.params, 1, "outPath").unwrap_or_default();
                if out_path.trim().is_empty() {
                    return RpcResponse::err(&req.id, "printSavePdf: missing output path");
                }
                let settings: printing::PrintSettings = match opts_val {
                    Value::String(s) => serde_json::from_str(&s).unwrap_or_default(),
                    other => serde_json::from_value(other).unwrap_or_default(),
                };
                let cdp_params = match settings.cdp_params() {
                    Ok(p) => p,
                    Err(e) => return RpcResponse::err(&req.id, e),
                };
                let handle = self.host_handle.lock().expect("host_handle mutex").clone();
                let Some(h) = handle else {
                    return RpcResponse::err(&req.id, "printSavePdf requires active WebView2 host");
                };
                match h
                    .call_devtools_protocol("Page.printToPDF", &cdp_params.to_string())
                    .and_then(|raw| printing::decode_pdf_result(&raw))
                {
                    Ok(pdf) => {
                        match syki_core::storage::atomic::write_atomic_bytes(
                            Path::new(out_path.trim()),
                            &pdf,
                        ) {
                            Ok(()) => RpcResponse::ok(&req.id, json!(out_path.trim())),
                            Err(e) => RpcResponse::err(&req.id, format!("write PDF failed: {e}")),
                        }
                    }
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "printPreviewClose" => {
                self.print_preview.clear();
                RpcResponse::ok(&req.id, json!(true))
            }

            "printSystem" => {
                let handle = self.host_handle.lock().expect("host_handle mutex").clone();
                if let Some(h) = handle {
                    match h.eval_js_rpc("window.print(); true") {
                        Ok(_) => RpcResponse::ok(&req.id, json!(true)),
                        Err(e) => RpcResponse::err(&req.id, e),
                    }
                } else {
                    RpcResponse::err(&req.id, "printSystem requires active WebView2 host")
                }
            }

            "detectLLMProvider" => RpcResponse::ok(&req.id, llm::detect_llm_provider()),

            "fetchModels" => {
                let base_url = param_str(&req.params, 0, "baseUrl").unwrap_or_default();
                let api_key = param_str(&req.params, 1, "apiKey").unwrap_or_default();
                match llm::fetch_models_sync(&base_url, &api_key) {
                    Ok(val) => RpcResponse::ok(&req.id, val),
                    Err(err) => RpcResponse::err(&req.id, err),
                }
            }

            "queryLLMAsync" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let prompt = param_str(&req.params, 1, "prompt").unwrap_or_default();
                let config_json = param_str(&req.params, 2, "config").unwrap_or_default();
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                llm::query_llm_async_cancellable(
                    req_id,
                    prompt,
                    config_json,
                    Some(Arc::clone(&self.llm_cancel)),
                    sender,
                );
                RpcResponse::ok(&req.id, json!(true))
            }

            "queryDecisionAsync" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let mut action_cfg_json =
                    param_json_str(&req.params, 1, "actionConfig").unwrap_or_default();
                let default_cfg_json =
                    param_json_str(&req.params, 2, "defaultConfig").unwrap_or_default();
                let input = param_str(&req.params, 3, "input").unwrap_or_default();
                let choices = param_decision_choices(&req.params, 4, "choices");
                let fallback = param_str(&req.params, 5, "fallback").unwrap_or_default();
                let prompt_raw = param_str(&req.params, 6, "prompt").unwrap_or_default();
                let timeout_ms = param_u64(&req.params, 7, "timeoutMs");

                if let Some(ms) = timeout_ms.filter(|&v| v > 0) {
                    let mut cfg = llm::DecisionConfig::from_json_str(&action_cfg_json);
                    if cfg.timeout_ms.is_none() {
                        cfg.timeout_ms = Some(ms);
                        if let Ok(serialized) = serde_json::to_string(&cfg) {
                            action_cfg_json = serialized;
                        }
                    }
                }

                let combined_prompt = if prompt_raw.is_empty() {
                    input
                } else if input.is_empty() {
                    prompt_raw
                } else if prompt_raw.contains("{{input}}") {
                    prompt_raw.replace("{{input}}", &input)
                } else {
                    format!("{prompt_raw}\n\n{input}")
                };

                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                llm::query_decision_async(
                    req_id,
                    combined_prompt,
                    choices,
                    fallback,
                    action_cfg_json,
                    default_cfg_json,
                    Some(Arc::clone(&self.llm_cancel)),
                    sender,
                );
                RpcResponse::ok(&req.id, json!(true))
            }

            "cancelLLMRequest" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                RpcResponse::ok(&req.id, json!(self.llm_cancel.cancel(&req_id)))
            }

            "runProgramAsync" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let program = param_str(&req.params, 1, "program").unwrap_or_default();
                let args = param_str_vec(&req.params, 2, "args");
                let stdin_data = param_str(&req.params, 3, "stdin").unwrap_or_default();

                let decision = self
                    .policy
                    .lock()
                    .expect("policy mutex")
                    .evaluate_program_exec(&program, &args);
                if decision.verdict != PolicyVerdict::Allow {
                    return RpcResponse::ok(
                        &req.id,
                        json!({ "allowed": false, "decision": decision }),
                    );
                }

                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                run_program_direct_async(req_id, program, args, stdin_data, sender);
                RpcResponse::ok(&req.id, json!({ "allowed": true, "decision": decision }))
            }

            "getPolicyAuditLog" => {
                let log = self.policy.lock().expect("policy mutex").audit_log();
                RpcResponse::ok(&req.id, json!(log))
            }

            "approvePlugin" => {
                let plugin_id = param_str(&req.params, 0, "pluginId").unwrap_or_default();
                let sha256_hex = param_str(&req.params, 1, "sha256")
                    .or_else(|| param_str(&req.params, 1, "sha256Hex"))
                    .unwrap_or_default();
                if plugin_id.trim().is_empty() || sha256_hex.trim().is_empty() {
                    return RpcResponse::err(&req.id, "pluginId and sha256 are required");
                }
                let snapshot = {
                    let mut engine = self.policy.lock().expect("policy mutex");
                    engine.approve_plugin(&plugin_id, &sha256_hex);
                    engine.registry().clone()
                };
                match self.save_trust_registry(&snapshot) {
                    Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "setPluginDevMode" => {
                let plugin_id = param_str(&req.params, 0, "pluginId").unwrap_or_default();
                let enabled = param_bool(&req.params, 1, "enabled").unwrap_or(false);
                let (changed, snapshot) = {
                    let mut engine = self.policy.lock().expect("policy mutex");
                    let ok = engine.set_dev_mode(&plugin_id, enabled);
                    (ok, engine.registry().clone())
                };
                if changed {
                    if let Err(e) = self.save_trust_registry(&snapshot) {
                        return RpcResponse::err(&req.id, e);
                    }
                }
                RpcResponse::ok(&req.id, json!(changed))
            }

            "approveProgram" => {
                let program = param_str(&req.params, 0, "program").unwrap_or_default();
                if program.trim().is_empty() {
                    return RpcResponse::err(&req.id, "program is required");
                }
                let snapshot = {
                    let mut engine = self.policy.lock().expect("policy mutex");
                    engine.approve_program(&program);
                    engine.registry().clone()
                };
                match self.save_trust_registry(&snapshot) {
                    Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "saveQcThemeCache" => {
                let snapshot_val = match &req.params {
                    Value::Array(arr) => arr.first().cloned(),
                    Value::Object(map) => map
                        .get("snapshot")
                        .cloned()
                        .or_else(|| Some(req.params.clone())),
                    _ => None,
                };
                let Some(val) = snapshot_val else {
                    return RpcResponse::err(&req.id, "snapshot object is required");
                };
                let bytes = match val {
                    Value::String(s) => s.into_bytes(),
                    other => match serde_json::to_vec_pretty(&other) {
                        Ok(b) => b,
                        Err(e) => {
                            return RpcResponse::err(
                                &req.id,
                                format!("failed to serialize qc theme cache: {e}"),
                            );
                        }
                    },
                };
                let path = self.env.data_dir.join("qc-theme-cache.json");
                match syki_core::storage::atomic::write_atomic_bytes(&path, &bytes) {
                    Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                    Err(e) => RpcResponse::err(&req.id, format!("failed to save qc theme cache: {e}")),
                }
            }

            "autocompleteAsync" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let prefix = param_str(&req.params, 1, "prefix").unwrap_or_default();
                let suffix = param_str(&req.params, 2, "suffix").unwrap_or_default();
                let config_json = param_str(&req.params, 3, "config").unwrap_or_default();
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                llm::autocomplete_async(req_id, prefix, suffix, config_json, sender);
                RpcResponse::ok(&req.id, json!(true))
            }

            "queryVisionAsync" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let prompt = param_str(&req.params, 1, "prompt").unwrap_or_default();
                let b64 = param_str(&req.params, 2, "base64").unwrap_or_default();
                let mime = param_str(&req.params, 3, "mimeType").unwrap_or_else(|| "image/png".into());
                let config_json = param_str(&req.params, 4, "config").unwrap_or_default();
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                llm::query_vision_async(req_id, prompt, b64, mime, config_json, sender);
                RpcResponse::ok(&req.id, json!(true))
            }

            "generateImageAsync" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let prompt = param_str(&req.params, 1, "prompt").unwrap_or_default();
                let config_json = param_str(&req.params, 2, "config").unwrap_or_default();
                let note_path = param_str(&req.params, 3, "notePath").unwrap_or_default();
                let data_dir = self.env.data_dir.clone();
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                llm::generate_image_async(req_id, prompt, config_json, note_path, data_dir, sender);
                RpcResponse::ok(&req.id, json!(true))
            }

            "processDroppedMedia" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let file_name = param_str(&req.params, 1, "fileName").unwrap_or_default();
                let mime = param_str(&req.params, 2, "mimeType").unwrap_or_default();
                let b64 = param_str(&req.params, 3, "data").unwrap_or_default();
                let vision_cfg = param_str(&req.params, 4, "visionConfig").unwrap_or_default();
                let voice_cfg = param_str(&req.params, 5, "voiceConfig").unwrap_or_default();
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                if speech::is_audio_media(&file_name, &mime) {
                    speech::transcribe_dropped_audio_async(
                        self.env.data_dir.clone(),
                        req_id,
                        file_name,
                        b64,
                        mime,
                        voice_cfg,
                        sender,
                    );
                } else {
                    llm::process_dropped_media_async(
                        req_id,
                        file_name,
                        mime,
                        b64,
                        vision_cfg,
                        sender,
                    );
                }
                RpcResponse::ok(&req.id, json!(true))
            }

            "checkAppUpdate" => {
                let agent = ureq::AgentBuilder::new()
                    .timeout(std::time::Duration::from_secs(12))
                    .user_agent("syki-app")
                    .build();
                match agent
                    .get("https://api.github.com/repos/youshinh/syki/releases/latest")
                    .set("Accept", "application/vnd.github.v3+json")
                    .call()
                {
                    Ok(resp) => match resp.into_string() {
                        Ok(body) => match serde_json::from_str::<serde_json::Value>(&body) {
                            Ok(val) => RpcResponse::ok(&req.id, val),
                            Err(e) => RpcResponse::err(&req.id, format!("invalid JSON: {e}")),
                        },
                        Err(e) => RpcResponse::err(&req.id, format!("read response error: {e}")),
                    },
                    Err(ureq::Error::Status(404, _)) => RpcResponse::ok(
                        &req.id,
                        json!({
                            "tag_name": syki_core::APP_VERSION,
                            "html_url": "https://github.com/youshinh/syki/releases",
                            "no_release": true
                        }),
                    ),
                    Err(ureq::Error::Status(code, _)) => {
                        RpcResponse::err(&req.id, format!("GitHub HTTP {code}"))
                    }
                    Err(e) => RpcResponse::err(&req.id, format!("GitHub check failed: {e}")),
                }
            }

            "generateCliCommandAsync" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let prompt_text = param_str(&req.params, 1, "promptText").unwrap_or_default();
                let config_json = param_str(&req.params, 2, "config").unwrap_or_default();
                let ctx_json = param_str(&req.params, 3, "contextMeta").unwrap_or_default();
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                llm::generate_cli_command_async(req_id, prompt_text, config_json, ctx_json, sender);
                RpcResponse::ok(&req.id, json!(true))
            }

            "getAvailableSkillsJSON" => RpcResponse::ok(&req.id, json!("[]")),
            "getSkillInstruction" => RpcResponse::ok(&req.id, json!("")),

            "ptyAvailablePresets" => {
                RpcResponse::ok(&req.id, json!(pty::available_presets().to_string()))
            }
            "ptyStart" => {
                let preset = param_str(&req.params, 0, "preset").unwrap_or_else(|| "cmd".into());
                let dir = param_str(&req.params, 1, "dir").unwrap_or_default();
                let cols = param_u64(&req.params, 2, "cols").unwrap_or(100) as u16;
                let rows = param_u64(&req.params, 3, "rows").unwrap_or(30) as u16;
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                match self
                    .pty_mgr
                    .start_session_with_size("", &preset, &dir, "", cols, rows, sender)
                {
                    Ok(info) => RpcResponse::ok(&req.id, json!(info.to_string())),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }
            "ptyStartWithOptions" => {
                let explicit_id = param_str(&req.params, 0, "explicitId").unwrap_or_default();
                let preset = param_str(&req.params, 1, "preset").unwrap_or_else(|| "cmd".into());
                let dir = param_str(&req.params, 2, "dir").unwrap_or_default();
                let label = param_str(&req.params, 3, "label").unwrap_or_default();
                let cols = param_u64(&req.params, 6, "cols").unwrap_or(100) as u16;
                let rows = param_u64(&req.params, 7, "rows").unwrap_or(30) as u16;
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                match self.pty_mgr.start_session_with_size(
                    &explicit_id,
                    &preset,
                    &dir,
                    &label,
                    cols,
                    rows,
                    sender,
                ) {
                    Ok(info) => RpcResponse::ok(&req.id, json!(info.to_string())),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }
            "ptyWrite" => {
                use base64::engine::general_purpose::STANDARD;
                use base64::Engine as _;
                let id = param_str(&req.params, 0, "id").unwrap_or_default();
                let data_b64 = param_str(&req.params, 1, "data").unwrap_or_default();
                let bytes = match STANDARD.decode(data_b64.as_bytes()) {
                    Ok(b) => b,
                    Err(_) => data_b64.into_bytes(),
                };
                match self.pty_mgr.write_bytes(&id, &bytes) {
                    Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }
            "ptyResize" => {
                let id = param_str(&req.params, 0, "id").unwrap_or_default();
                let cols = param_u64(&req.params, 1, "cols").unwrap_or(80) as u16;
                let rows = param_u64(&req.params, 2, "rows").unwrap_or(24) as u16;
                match self.pty_mgr.resize(&id, cols, rows) {
                    Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }
            "ptyRename" => {
                let id = param_str(&req.params, 0, "id").unwrap_or_default();
                let label = param_str(&req.params, 1, "label").unwrap_or_default();
                match self.pty_mgr.rename(&id, &label) {
                    Ok(lbl) => RpcResponse::ok(&req.id, json!(lbl)),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }
            "ptyClose" => {
                let id = param_str(&req.params, 0, "id").unwrap_or_default();
                RpcResponse::ok(&req.id, json!(self.pty_mgr.close(&id)))
            }
            "ptyHistory" => {
                let id = param_str(&req.params, 0, "id").unwrap_or_default();
                RpcResponse::ok(&req.id, json!(self.pty_mgr.history(&id)))
            }
            "ptyDetectGate" => {
                let dir = param_str(&req.params, 0, "dir").unwrap_or_default();
                RpcResponse::ok(&req.id, json!(pty::detect_project_gate(Path::new(dir.trim()))))
            }
            "ptyPickFolder" => match dialogs::pick_folder(None, None) {
                Ok(Some(p)) => RpcResponse::ok(&req.id, json!(p.to_string_lossy())),
                Ok(None) => RpcResponse::ok(&req.id, Value::Null),
                Err(e) => RpcResponse::err(&req.id, e),
            },
            "ptyPrepareWorktree" => {
                let dir = param_str(&req.params, 0, "dir").unwrap_or_default();
                let id = param_str(&req.params, 1, "id").unwrap_or_default();
                RpcResponse::ok(&req.id, json!(pty::prepare_worktree(&dir, &id).to_string()))
            }
            "ptySuggestDirs" => RpcResponse::ok(&req.id, json!(pty::suggest_dirs())),
            "ptyLaunchElevated" => {
                let preset = param_str(&req.params, 0, "preset").unwrap_or_else(|| "shell".into());
                let dir = param_str(&req.params, 1, "dir").unwrap_or_default();
                match pty::launch_elevated(&preset, &dir) {
                    Ok(info) => RpcResponse::ok(&req.id, json!(info.to_string())),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }

            "getSpeechStatus" => {
                let voice_cfg = param_str(&req.params, 0, "voiceConfigJson").unwrap_or_else(|| "{}".into());
                RpcResponse::ok(&req.id, speech::get_speech_status(&self.env.data_dir, &voice_cfg))
            }
            "prefetchSpeechModel" => {
                speech::prefetch_model_async(self.env.data_dir.clone());
                RpcResponse::ok(&req.id, json!(true))
            }
            "installSpeechPartAsync" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let part = param_str(&req.params, 1, "part").unwrap_or_else(|| "model".into());
                let voice_cfg = param_str(&req.params, 2, "voiceConfigJson").unwrap_or_else(|| "{}".into());
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                speech::install_speech_part_async(self.env.data_dir.clone(), req_id, part, voice_cfg, sender);
                RpcResponse::ok(&req.id, json!(true))
            }
            "cancelSpeechInstall" => {
                speech::cancel_speech_install();
                RpcResponse::ok(&req.id, json!(true))
            }
            "removeSpeechPart" => {
                let part = param_str(&req.params, 0, "part").unwrap_or_default();
                let voice_cfg = param_str(&req.params, 1, "voiceConfigJson").unwrap_or_else(|| "{}".into());
                match speech::remove_speech_part(&self.env.data_dir, &part, &voice_cfg) {
                    Ok(()) => RpcResponse::ok(&req.id, json!("")),
                    Err(e) => RpcResponse::ok(&req.id, json!(e)),
                }
            }
            "pickFilePath" => match dialogs::pick_open_file(None, None) {
                Ok(Some(p)) => RpcResponse::ok(&req.id, json!(p.to_string_lossy())),
                Ok(None) => RpcResponse::ok(&req.id, Value::Null),
                Err(e) => RpcResponse::err(&req.id, e),
            },
            "validateWhisperModelFile" => {
                let path = param_str(&req.params, 0, "path").unwrap_or_default();
                match speech::validate_whisper_model_file(Path::new(path.trim())) {
                    Ok(v) => RpcResponse::ok(&req.id, v),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }
            "transcribeAudioAsync" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let audio_b64 = param_str(&req.params, 1, "audioBase64").unwrap_or_default();
                let mime = param_str(&req.params, 2, "mimeType").unwrap_or_else(|| "audio/webm".into());
                let voice_cfg = param_str(&req.params, 3, "voiceConfigJson").unwrap_or_else(|| "{}".into());
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                speech::transcribe_audio_async(
                    self.env.data_dir.clone(),
                    req_id,
                    audio_b64,
                    mime,
                    voice_cfg,
                    sender,
                );
                RpcResponse::ok(&req.id, json!(true))
            }
            "transcribeVoiceCache" => {
                let cache_path = param_str(&req.params, 0, "cachePath").unwrap_or_default();
                let voice_cfg = param_str(&req.params, 1, "voiceConfigJson").unwrap_or_else(|| "{}".into());
                match speech::transcribe_cached_file_with_config(
                    &self.env.data_dir,
                    Path::new(cache_path.trim()),
                    &voice_cfg,
                    true,
                ) {
                    Ok(t) => RpcResponse::ok(&req.id, json!(t)),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }
            "retryVoiceCacheAsync" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let cache_path = param_str(&req.params, 1, "cachePath").unwrap_or_default();
                let voice_cfg = param_str(&req.params, 2, "voiceConfigJson").unwrap_or_else(|| "{}".into());
                let data_dir = self.env.data_dir.clone();
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                std::thread::spawn(move || {
                    let (text, error) = match speech::transcribe_cached_file_with_config(
                        &data_dir,
                        Path::new(cache_path.trim()),
                        &voice_cfg,
                        true,
                    ) {
                        Ok(t) => (t, String::new()),
                        Err(e) => (String::new(), e),
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
                RpcResponse::ok(&req.id, json!(true))
            }
            "listVoiceCaches" => {
                let list = speech::list_voice_caches(&self.env.data_dir);
                RpcResponse::ok(&req.id, serde_json::to_value(list).unwrap_or(json!([])))
            }
            "keepVoiceCache" => {
                let cache_path = param_str(&req.params, 0, "cachePath").unwrap_or_default();
                match speech::keep_voice_cache(Path::new(cache_path.trim())) {
                    Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }
            "discardVoiceCache" => {
                let cache_path = param_str(&req.params, 0, "cachePath").unwrap_or_default();
                match speech::discard_voice_cache(Path::new(cache_path.trim())) {
                    Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                    Err(e) => RpcResponse::err(&req.id, e),
                }
            }
            "discardAllVoiceCaches" => match speech::discard_all_voice_caches(&self.env.data_dir) {
                Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                Err(e) => RpcResponse::err(&req.id, e),
            },

            "meetingRecordingSupported" => RpcResponse::ok(&req.id, json!(true)),
            "checkMeetingAudioAsync" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                std::thread::spawn(move || {
                    let probe = meeting_audio::probe_wasapi_endpoints();
                    if let Some(tx) = sender {
                        tx(RpcEvent {
                            topic: "meeting:check".to_string(),
                            payload: json!({
                                "reqId": req_id,
                                "json": probe.to_string()
                            }),
                        });
                    }
                });
                RpcResponse::ok(&req.id, json!(true))
            }
            "startMeetingRecording" => match self.meeting.start() {
                Ok(()) => RpcResponse::ok(&req.id, json!(true)),
                Err(e) => RpcResponse::err(&req.id, e),
            },
            "stopMeetingRecordingAsync" => {
                let req_id = param_str(&req.params, 0, "reqId").unwrap_or_default();
                let voice_cfg = param_str(&req.params, 1, "voiceConfigJson").unwrap_or_default();
                let sender = self.event_sender.lock().expect("event_sender mutex").clone();
                self.meeting.stop_async(self.env.data_dir.clone(), req_id, voice_cfg, sender);
                RpcResponse::ok(&req.id, json!(true))
            }
            "abortMeetingRecording" => {
                RpcResponse::ok(&req.id, json!(self.meeting.abort()))
            }

            other => RpcResponse::err(&req.id, format!("not_implemented_yet: {other}")),
        }
    }

    fn ensure_watcher_and_watch(&self, path: &Path) -> Result<(), String> {
        let mut guard = self.watcher.lock().expect("watcher mutex");
        if guard.is_none() {
            let sender_slot = self.event_sender.lock().expect("event_sender mutex").clone();
            let watcher = ActiveFileWatcher::start(Arc::new(move |changed_path| {
                if let Ok(loaded) = NoteFileStore::read_note(&changed_path) {
                    if let Some(sender) = sender_slot.as_ref() {
                        sender(RpcEvent {
                            topic: "file:changed".to_string(),
                            payload: loaded_note_to_json(&loaded),
                        });
                    }
                }
            }))
            .map_err(|e| e.to_string())?;
            *guard = Some(watcher);
        }
        guard
            .as_ref()
            .expect("watcher initialized")
            .watch(path)
            .map_err(|e| e.to_string())
    }
}

fn param_value(params: &Value, idx: usize, key: &str) -> Option<Value> {
    match params {
        Value::Array(arr) => arr.get(idx).cloned(),
        Value::Object(map) => map.get(key).cloned().or_else(|| {
            if idx == 0 && !map.is_empty() {
                Some(Value::Object(map.clone()))
            } else {
                None
            }
        }),
        _ => None,
    }
}

fn param_str(params: &Value, idx: usize, key: &str) -> Option<String> {
    match params {
        Value::Array(arr) => arr.get(idx).and_then(|v| v.as_str()).map(str::to_string),
        Value::Object(map) => map.get(key).and_then(|v| v.as_str()).map(str::to_string),
        _ => None,
    }
}

fn param_u64(params: &Value, idx: usize, key: &str) -> Option<u64> {
    match params {
        Value::Array(arr) => arr.get(idx).and_then(|v| v.as_u64()),
        Value::Object(map) => map.get(key).and_then(|v| v.as_u64()),
        _ => None,
    }
}

fn param_bool(params: &Value, idx: usize, key: &str) -> Option<bool> {
    match params {
        Value::Array(arr) => arr.get(idx).and_then(|v| v.as_bool()),
        Value::Object(map) => map.get(key).and_then(|v| v.as_bool()),
        _ => None,
    }
}

fn param_json_str(params: &Value, idx: usize, key: &str) -> Option<String> {
    let val = param_value(params, idx, key)?;
    match val {
        Value::String(s) => Some(s),
        Value::Null => None,
        other => serde_json::to_string(&other).ok(),
    }
}

fn param_decision_choices(params: &Value, idx: usize, key: &str) -> Vec<llm::DecisionChoice> {
    let Some(val) = param_value(params, idx, key) else {
        return Vec::new();
    };
    match val {
        Value::String(s) => serde_json::from_str(&s).unwrap_or_default(),
        Value::Array(_) => serde_json::from_value(val).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn param_str_vec(params: &Value, idx: usize, key: &str) -> Vec<String> {
    let Some(val) = param_value(params, idx, key) else {
        return Vec::new();
    };
    match val {
        Value::Array(arr) => arr
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        Value::String(s) => serde_json::from_str(&s).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn run_program_direct_async(
    req_id: String,
    program: String,
    args: Vec<String>,
    stdin_data: String,
    sender: Option<EventSender>,
) {
    std::thread::spawn(move || {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let res = match cmd.spawn() {
            Ok(mut child) => {
                if let Some(mut child_stdin) = child.stdin.take() {
                    if !stdin_data.is_empty() {
                        let _ = child_stdin.write_all(stdin_data.as_bytes());
                    }
                }
                match child.wait_with_output() {
                    Ok(out) => {
                        let stdout_clean =
                            shell_pipe::strip_ansi(&String::from_utf8_lossy(&out.stdout));
                        let stderr_clean =
                            shell_pipe::strip_ansi(&String::from_utf8_lossy(&out.stderr));
                        let exit_code = out.status.code().unwrap_or(-1);
                        shell_pipe::CommandResult {
                            output: stdout_clean,
                            error: stderr_clean,
                            exit_code,
                        }
                    }
                    Err(err) => shell_pipe::CommandResult {
                        output: String::new(),
                        error: format!("program wait error: {err}"),
                        exit_code: -1,
                    },
                }
            }
            Err(err) => shell_pipe::CommandResult {
                output: String::new(),
                error: format!("failed to spawn program: {err}"),
                exit_code: -1,
            },
        };

        if let Some(tx) = sender {
            let err_str = if res.exit_code != 0 && !res.error.is_empty() {
                res.error.clone()
            } else {
                String::new()
            };
            tx(RpcEvent {
                topic: "cli:result".to_string(),
                payload: json!({
                    "reqId": req_id,
                    "result": res,
                    "error": err_str
                }),
            });
        }
    });
}

fn extract_host_port(base_url: &str) -> String {
    let s = base_url.trim();
    if s.is_empty() {
        return String::new();
    }
    let after_scheme = s.split("://").nth(1).unwrap_or(s);
    after_scheme.split('/').next().unwrap_or("").to_string()
}

fn is_local_host(host_port: &str) -> bool {
    if host_port.is_empty() {
        return true;
    }
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "::1"
    )
}

fn sendto_cmd_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("SendTo")
            .join("syki.cmd"),
    )
}

fn file_title(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("untitled.md")
        .to_string()
}

fn loaded_note_to_json(loaded: &LoadedNote) -> Value {
    json!({
        "path": loaded.path.to_string_lossy(),
        "title": file_title(&loaded.path),
        "content": loaded.content,
        "encoding": loaded.encoding_name,
        "sig": cyrb53_utf16_sig(&loaded.content)
    })
}

fn save_result_json(path: &Path, content: &str, conflict: bool) -> Value {
    json!({
        "path": path.to_string_lossy(),
        "title": file_title(path),
        "success": !conflict,
        "conflict": conflict,
        "sig": cyrb53_utf16_sig(content)
    })
}

fn resolve_note_folder(env: &RuntimeEnv, folder: &str) -> PathBuf {
    let trimmed = folder.trim();
    if trimmed.is_empty() || trimmed == "~" {
        return env.data_dir.join("notes");
    }
    if let Some(rest) = trimmed.strip_prefix("~/").or_else(|| trimmed.strip_prefix("~\\")) {
        if let Ok(home) = std::env::var("USERPROFILE") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(trimmed)
}

fn unique_note_path(dir: &Path, suggested_name: &str) -> PathBuf {
    let clean = dialogs::sanitize_filename(suggested_name);
    let candidate = dir.join(&clean);
    if !candidate.exists() {
        return candidate;
    }
    let stem = Path::new(&clean)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    let ext = Path::new(&clean)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("md");
    for idx in 1..1000 {
        let next = dir.join(format!("{stem}-{idx}.{ext}"));
        if !next.exists() {
            return next;
        }
    }
    dir.join(format!("{stem}-copy.{ext}"))
}

fn resolve_dropped_file_in_dirs(name: &str, size: u64, dirs_raw: &str) -> String {
    let target = name.trim();
    if target.is_empty() {
        return String::new();
    }
    for dir in dirs_raw.lines().map(str::trim).filter(|d| !d.is_empty()) {
        let candidate = Path::new(dir).join(target);
        if let Ok(meta) = std::fs::metadata(&candidate) {
            if meta.is_file() && (size == 0 || meta.len() == size) {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    String::new()
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FolderEntry {
    path: String,
    rel_path: String,
    title: String,
    snippet: String,
}

fn scan_folder_entries(root: &Path) -> Vec<FolderEntry> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    scan_dir_recursive(root, root, 0, &mut out);
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    out
}

fn scan_dir_recursive(root: &Path, current: &Path, depth: usize, out: &mut Vec<FolderEntry>) {
    if depth > 6 || out.len() >= 500 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(current) else {
        return;
    };
    for entry in rd.flatten() {
        if out.len() >= 500 {
            break;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.')
            || matches!(
                name_str.as_ref(),
                "node_modules" | "target" | "dist" | "build"
            )
        {
            continue;
        }
        if path.is_dir() {
            scan_dir_recursive(root, &path, depth + 1, out);
        } else if path.is_file() && is_note_extension(&path) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let snippet = NoteFileStore::read_note(&path)
                .map(|n| {
                    n.content
                        .lines()
                        .map(str::trim)
                        .find(|l| !l.is_empty())
                        .unwrap_or("")
                        .chars()
                        .take(120)
                        .collect::<String>()
                })
                .unwrap_or_default();
            out.push(FolderEntry {
                path: path.to_string_lossy().into_owned(),
                rel_path: rel,
                title: file_title(&path),
                snippet,
            });
        }
    }
}

fn is_note_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "txt"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn temp_env(label: &str) -> (PathBuf, CoreState) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("syki-rpc-{label}-{nanos}"));
        std::fs::create_dir_all(&root).expect("mkdir temp");
        let env = RuntimeEnv {
            data_dir: root.clone(),
            instance_id: "test".to_string(),
        };
        let state = CoreState::new(env, None);
        (root, state)
    }

    fn req(method: &str, params: Value) -> RpcRequest {
        RpcRequest {
            id: "1".to_string(),
            method: method.to_string(),
            params,
        }
    }

    #[test]
    fn app_meta_config_and_session_roundtrip() {
        let (root, state) = temp_env("meta");
        assert!(state.dispatch(&req("getAppVersion", json!({}))).ok);
        assert!(state.dispatch(&req("uiReady", json!({}))).ok);
        let info = state.dispatch(&req("getAppInfo", json!({})));
        assert!(info.ok);
        assert_eq!(info.result.unwrap()["os"], "windows");

        let save_cfg = state.dispatch(&req("saveConfig", json!([{ "general": { "language": "ja" } }])));
        assert!(save_cfg.ok);
        let got_cfg = state.dispatch(&req("getConfig", json!({})));
        let cfg_str = got_cfg.result.unwrap();
        let cfg_parsed: Value = serde_json::from_str(cfg_str.as_str().expect("getConfig returns JSON string")).unwrap();
        assert_eq!(cfg_parsed["general"]["language"], "ja");

        let save_sess = state.dispatch(&req("saveSessionSync", json!([{ "activeTab": 2 }])));
        assert!(save_sess.ok);
        let got_sess = state.dispatch(&req("getSession", json!({})));
        let sess_str = got_sess.result.unwrap();
        let sess_parsed: Value = serde_json::from_str(sess_str.as_str().expect("getSession returns JSON string")).unwrap();
        assert_eq!(sess_parsed["activeTab"], 2);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn startup_file_is_consumed_once() {
        let (root, _) = temp_env("startup");
        let note = root.join("boot.md");
        std::fs::write(&note, "# Boot\n").expect("write");
        let env = RuntimeEnv {
            data_dir: root.clone(),
            instance_id: "test".to_string(),
        };
        let state = CoreState::new(env, Some(note));
        let first = state.dispatch(&req("getStartupFile", json!({})));
        assert!(first.ok);
        assert_eq!(first.result.unwrap()["content"], "# Boot\n");
        let second = state.dispatch(&req("getStartupFile", json!({})));
        assert!(second.ok);
        assert_eq!(second.result.unwrap(), Value::Null);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn save_file_checked_detects_conflict_and_preserves_disk() {
        let (root, state) = temp_env("conflict");
        let note = root.join("note.md");
        let save1 = state.dispatch(&req(
            "saveFile",
            json!([note.to_string_lossy(), "v1\n", "UTF-8"]),
        ));
        assert!(save1.ok);
        let sig1 = save1.result.unwrap()["sig"].as_str().unwrap().to_string();

        std::fs::write(&note, "external v2\n").expect("external edit");
        let conflict = state.dispatch(&req(
            "saveFileCheckedAsync",
            json!([note.to_string_lossy(), "stale\n", "UTF-8", sig1]),
        ));
        assert!(conflict.ok);
        let res = conflict.result.unwrap();
        assert_eq!(res["conflict"], true);
        assert_eq!(std::fs::read_to_string(&note).unwrap(), "external v2\n");

        let sig2 = res["sig"].as_str().unwrap().to_string();
        let ok_save = state.dispatch(&req(
            "saveFileChecked",
            json!([note.to_string_lossy(), "merged v3\n", "UTF-8", sig2]),
        ));
        assert!(ok_save.ok);
        assert_eq!(ok_save.result.unwrap()["conflict"], false);
        assert_eq!(std::fs::read_to_string(&note).unwrap(), "merged v3\n");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn scan_create_and_resolve_dropped_file() {
        let (root, state) = temp_env("scan");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/ignored.md"), "ignore").unwrap();

        let created1 = state.dispatch(&req(
            "createNoteInFolder",
            json!(["# Hello\nBody", "UTF-8", "daily.md", root.to_string_lossy()]),
        ));
        assert!(created1.ok);
        let created2 = state.dispatch(&req(
            "createNoteInFolder",
            json!(["# Hello 2", "UTF-8", "daily.md", root.to_string_lossy()]),
        ));
        assert!(created2.ok);
        assert_ne!(
            created1.result.as_ref().unwrap()["path"],
            created2.result.as_ref().unwrap()["path"]
        );

        let scanned = state.dispatch(&req("scanFolderFiles", json!([root.to_string_lossy()])));
        assert!(scanned.ok);
        let arr = scanned.result.unwrap().as_array().unwrap().clone();
        assert_eq!(arr.len(), 2);

        let size = std::fs::metadata(root.join("daily.md")).unwrap().len();
        let resolved = state.dispatch(&req(
            "resolveDroppedFile",
            json!(["daily.md", size, 0, root.to_string_lossy()]),
        ));
        assert!(resolved.ok);
        assert!(resolved.result.unwrap().as_str().unwrap().ends_with("daily.md"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn watch_active_file_emits_file_changed_event() {
        let (root, state) = temp_env("watch");
        let note = root.join("watched.md");
        std::fs::write(&note, "initial\n").unwrap();

        let (tx, rx) = std::sync::mpsc::channel::<RpcEvent>();
        state.set_event_sender(Arc::new(move |ev| {
            let _ = tx.send(ev);
        }));

        let w = state.dispatch(&req("watchActiveFile", json!([note.to_string_lossy()])));
        assert!(w.ok);

        std::thread::sleep(Duration::from_millis(120));
        std::fs::write(&note, "updated\n").unwrap();

        let ev = rx.recv_timeout(Duration::from_secs(3)).expect("file:changed event");
        assert_eq!(ev.topic, "file:changed");
        assert_eq!(ev.payload["content"], "updated\n");

        let uw = state.dispatch(&req("unwatchActiveFile", json!({})));
        assert!(uw.ok);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn open_external_rejects_unsafe_schemes_and_unknown_methods_report_status() {
        let (root, state) = temp_env("ext");
        let bad = state.dispatch(&req("openExternal", json!(["file:///C:/Windows/win.ini"])));
        assert!(!bad.ok);
        let unimpl = state.dispatch(&req("someUnknownMethodForTest", json!([])));
        assert!(!unimpl.ok);
        assert!(unimpl.error.unwrap().starts_with("not_implemented_yet:"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn search_scraps_and_semantic_index_dispatch() {
        let (root, state) = temp_env("search-sem");
        let scrap_dir = root.join("scraps");
        std::fs::create_dir_all(&scrap_dir).unwrap();
        std::fs::write(
            scrap_dir.join("2026-11-21.md"),
            "## [09:00] #rust\nSIMD mmap vector search\n",
        )
        .unwrap();
        state
            .dispatch(&req(
                "saveConfig",
                json!([{ "general": { "scrapDir": scrap_dir.to_string_lossy() } }]),
            ));

        let found = state.dispatch(&req("searchScraps", json!(["mmap", 10, ""])));
        assert!(found.ok);
        assert_eq!(found.result.unwrap().as_array().unwrap().len(), 1);

        let opts = state.dispatch(&req("scrapFilterOptions", json!({})));
        assert!(opts.ok);
        assert_eq!(opts.result.unwrap()["files"], 1);

        let upd = state.dispatch(&req("semanticUpdate", json!([{ "enabled": true }, false, true])));
        assert!(upd.ok);
        let upd_val = upd.result.unwrap();
        assert_eq!(upd_val["state"], "ready");
        assert!(upd_val["embedded"].as_u64().unwrap() >= 1);

        let st = state.dispatch(&req(
            "semanticStatus",
            json!([{ "enabled": true, "model": { "baseUrl": "http://localhost:11434", "model": "bge-m3" } }]),
        ));
        assert!(st.ok);
        let st_val = st.result.unwrap();
        assert_eq!(st_val["enabled"], true);
        assert_eq!(st_val["model"], "bge-m3");
        assert_eq!(st_val["local"], true);
        assert_eq!(st_val["exists"], true);

        let sem_hits = state.dispatch(&req("searchScrapsSemantic", json!(["vector search", 5])));
        assert!(sem_hits.ok);
        let hits = sem_hits.result.unwrap();
        assert_eq!(hits["semantic"], true);

        let cloud_consent_status = state.dispatch(&req(
            "semanticStatus",
            json!([{
                "enabled": true,
                "model": { "baseUrl": "https://api.openai.com/v1", "model": "text-embedding-3-small" },
                "privacy": { "cloudConsent": { "api.openai.com": "2026-11-24" } }
            }]),
        ));
        assert!(cloud_consent_status.ok);
        assert_eq!(cloud_consent_status.result.unwrap()["consent_given"], true);
        let arr = hits["results"].as_array().unwrap();
        assert!(!arr.is_empty());
        assert!(arr[0]["matches"][0]["text"]
            .as_str()
            .unwrap()
            .contains("vector search"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pty_write_decodes_base64_and_streams_conpty_output() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine as _;

        let (root, state) = temp_env("pty-b64");
        let (tx, rx) = std::sync::mpsc::channel::<RpcEvent>();
        state.set_event_sender(Arc::new(move |ev| {
            let _ = tx.send(ev);
        }));

        let started = state.dispatch(&req(
            "ptyStartWithOptions",
            json!(["rpc-pty-1", "cmd", ".", "Test", "", false, 80, 24]),
        ));
        assert!(started.ok);

        std::thread::sleep(Duration::from_millis(200));
        let cmd_b64 = STANDARD.encode(b"echo SYKI_B64_OK\r\n");
        let wrote = state.dispatch(&req("ptyWrite", json!(["rpc-pty-1", cmd_b64])));
        assert!(wrote.ok);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut combined = String::new();
        while std::time::Instant::now() < deadline {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(150)) {
                if ev.topic == "pty:data" {
                    if let Some(b64) = ev.payload["b64"].as_str() {
                        if let Ok(bytes) = STANDARD.decode(b64.as_bytes()) {
                            combined.push_str(&String::from_utf8_lossy(&bytes));
                            if combined.contains("SYKI_B64_OK") {
                                break;
                            }
                        }
                    }
                }
            }
        }
        assert!(combined.contains("SYKI_B64_OK"), "got: {combined}");
        assert!(state.dispatch(&req("ptyClose", json!(["rpc-pty-1"]))).ok);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn save_asset_import_asset_and_retype_ime_dispatch() {
        let (root, state) = temp_env("assets-ime");
        let saved = state.dispatch(&req("saveAsset", json!([root.to_string_lossy(), "png", "aGVsbG8="])));
        assert!(saved.ok);
        let rel = saved.result.unwrap()["relPath"].as_str().unwrap().to_string();
        assert!(rel.starts_with("./assets/"));

        let imported = state.dispatch(&req("importAssetFile", json!([root.to_string_lossy(), "pic.png", "aGVsbG8="])));
        assert!(imported.ok);
        assert_eq!(imported.result.unwrap()["relPath"], "./assets/pic.png");

        let (tx, rx) = std::sync::mpsc::channel::<RpcEvent>();
        state.set_event_sender(Arc::new(move |ev| {
            let _ = tx.send(ev);
        }));
        let retype = state.dispatch(&req("retypeWithImeAsync", json!(["req-1", "INVALID!"])));
        assert!(retype.ok);
        let ev = rx.recv_timeout(Duration::from_secs(2)).expect("ime:retype event");
        assert_eq!(ev.topic, "ime:retype");
        assert_eq!(ev.payload["reqId"], "req-1");
        assert!(!ev.payload["error"].as_str().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn run_program_policy_denies_shell_host_asks_unknown_and_allows_approved() {
        let (root, state) = temp_env("policy-program");

        // 1. Unapproved shell host interpreter must be Deny and not executed
        let denied_prog = concat!("power", "shell.exe");
        let denied = state.dispatch(&req(
            "runProgramAsync",
            json!(["req-ps", denied_prog, ["-Command", "echo hi"], ""]),
        ));
        assert!(denied.ok);
        let denied_val = denied.result.unwrap();
        assert_eq!(denied_val["allowed"], false);
        assert_eq!(denied_val["decision"]["verdict"], "deny");

        // 2. Unapproved non-denied program must be Ask and not executed
        let asked = state.dispatch(&req(
            "runProgramAsync",
            json!(["req-git", "git", ["--version"], ""]),
        ));
        assert!(asked.ok);
        let asked_val = asked.result.unwrap();
        assert_eq!(asked_val["allowed"], false);
        assert_eq!(asked_val["decision"]["verdict"], "ask");

        // 3. Approve program and verify execution is allowed and persists to trust-registry.json
        let approved = state.dispatch(&req("approveProgram", json!(["where.exe"])));
        assert!(approved.ok);
        assert!(root.join("trust-registry.json").exists());

        let (tx, rx) = std::sync::mpsc::channel::<RpcEvent>();
        state.set_event_sender(Arc::new(move |ev| {
            let _ = tx.send(ev);
        }));
        let allowed = state.dispatch(&req(
            "runProgramAsync",
            json!(["req-where", "where.exe", ["cmd.exe"], ""]),
        ));
        assert!(allowed.ok);
        let allowed_val = allowed.result.unwrap();
        assert_eq!(allowed_val["allowed"], true);
        assert_eq!(allowed_val["decision"]["verdict"], "allow");

        let ev = rx.recv_timeout(Duration::from_secs(5)).expect("cli:result event");
        assert_eq!(ev.topic, "cli:result");
        assert_eq!(ev.payload["reqId"], "req-where");
        assert_eq!(ev.payload["result"]["exitCode"], 0);

        // 4. Policy audit log must record all 3 evaluations
        let audit = state.dispatch(&req("getPolicyAuditLog", json!([])));
        assert!(audit.ok);
        let entries = audit.result.unwrap();
        let arr = entries.as_array().expect("audit log array");
        assert!(arr.len() >= 3);
        assert_eq!(arr[0]["verdict"], "deny");
        assert_eq!(arr[1]["verdict"], "ask");
        assert_eq!(arr[2]["verdict"], "allow");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cancel_llm_request_and_plugin_trust_and_theme_cache_rpc() {
        let (root, state) = temp_env("cancel-trust-theme");

        // cancelLLMRequest returns false for unknown id, true for active registered id
        let miss = state.dispatch(&req("cancelLLMRequest", json!(["nonexistent-req"])));
        assert!(miss.ok);
        assert_eq!(miss.result.unwrap(), false);

        let flag = state.llm_cancel.register("active-req-1");
        let hit = state.dispatch(&req("cancelLLMRequest", json!(["active-req-1"])));
        assert!(hit.ok);
        assert_eq!(hit.result.unwrap(), true);
        assert!(flag.load(std::sync::atomic::Ordering::SeqCst));
        state.llm_cancel.finish("active-req-1");

        // approvePlugin + setPluginDevMode
        let dev_before = state.dispatch(&req("setPluginDevMode", json!(["demo-plugin", true])));
        assert!(dev_before.ok);
        assert_eq!(dev_before.result.unwrap(), false);

        let approve = state.dispatch(&req("approvePlugin", json!(["demo-plugin", "deadbeef"])));
        assert!(approve.ok);
        let dev_after = state.dispatch(&req("setPluginDevMode", json!(["demo-plugin", true])));
        assert!(dev_after.ok);
        assert_eq!(dev_after.result.unwrap(), true);

        // saveQcThemeCache writes atomic JSON
        let saved = state.dispatch(&req(
            "saveQcThemeCache",
            json!([{ "theme": "dark", "bg": "#111111" }]),
        ));
        assert!(saved.ok);
        let raw = std::fs::read_to_string(root.join("qc-theme-cache.json")).unwrap();
        assert!(raw.contains("\"dark\""));

        let _ = std::fs::remove_dir_all(root);
    }
}
