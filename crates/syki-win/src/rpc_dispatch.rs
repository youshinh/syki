//! Core RPC dispatcher for `#04` (config, session, note I/O, folder scan,
//! conflict-checked saves, active file watcher push events, and shell/dialogs).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
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
use crate::{assets_store, clipboard, dialogs, host_lifecycle, ime, printing, shell_pipe};

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
}

impl CoreState {
    pub fn new(env: RuntimeEnv, startup_file: Option<PathBuf>) -> Self {
        let config_store = JsonConfigStore::new(env.data_dir.join("config.json"));
        let session_store = SessionStore::new(env.data_dir.join("session.json"));
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
        }
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

            "getConfig" => match self.config_store.load_raw() {
                Ok(raw) => {
                    let val: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
                    RpcResponse::ok(&req.id, val)
                }
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
                Ok(raw) => {
                    let val: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
                    RpcResponse::ok(&req.id, val)
                }
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
                let enabled = param_value(&req.params, 0, "section")
                    .and_then(|v| v.get("enabled").and_then(|b| b.as_bool()))
                    .unwrap_or(true);
                let report = semantic_status(&index_dir, enabled);
                RpcResponse::ok(&req.id, serde_json::to_value(report).unwrap_or(json!({})))
            }

            "semanticUpdate" => {
                let scrap_dir = ScrapStore::resolve_scrap_dir(&self.config_store.scrap_dir());
                let index_dir = self.env.data_dir.join("semindex");
                match build_local_semantic_index(&scrap_dir, &index_dir) {
                    Ok((files, chunks)) => {
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
                        let report = semantic_status(&index_dir, true);
                        RpcResponse::ok(&req.id, serde_json::to_value(report).unwrap_or(json!({})))
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
                    return RpcResponse::ok(&req.id, serde_json::to_value(hits).unwrap_or(json!([])));
                }
                let dir = self.config_store.scrap_dir();
                let fallback = search_scraps_parallel(&dir, &query, limit, &ScrapFilter::default());
                RpcResponse::ok(&req.id, serde_json::to_value(fallback).unwrap_or(json!([])))
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

            "isSendToShortcutInstalled" => RpcResponse::ok(&req.id, json!(false)),
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

            "printSystem" => RpcResponse::ok(&req.id, json!(true)),

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
        assert_eq!(got_cfg.result.unwrap()["general"]["language"], "ja");

        let save_sess = state.dispatch(&req("saveSessionSync", json!([{ "activeTab": 2 }])));
        assert!(save_sess.ok);
        let got_sess = state.dispatch(&req("getSession", json!({})));
        assert_eq!(got_sess.result.unwrap()["activeTab"], 2);

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
        let unimpl = state.dispatch(&req("ptyStart", json!([])));
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

        let upd = state.dispatch(&req("semanticUpdate", json!([{}, false, true])));
        assert!(upd.ok);
        assert_eq!(upd.result.unwrap()["state"], "ready");

        let sem_hits = state.dispatch(&req("searchScrapsSemantic", json!(["vector search", 5])));
        assert!(sem_hits.ok);
        let hits = sem_hits.result.unwrap();
        let arr = hits.as_array().unwrap();
        assert!(!arr.is_empty());
        assert!(arr[0]["matches"][0]["text"]
            .as_str()
            .unwrap()
            .contains("vector search"));

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
}
