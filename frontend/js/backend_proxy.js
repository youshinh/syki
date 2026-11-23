// Single WebView2 PostWebMessageAsJson bridge shim for syki-lite (#04).
// Exposes Promise-based window.backend.<method>(...) over window.chrome.webview.postMessage
// and routes incoming RpcEvent topics to registered listeners and global host callbacks.
(function (global) {
  'use strict';

  // Kept syki-lite backend methods across F01/F02/F04/F05/F06/F07/F08/F10a/F10b/F11/F12/F13/F18/F21/F22/F23(AI-OCR).
  // Deliberately omits all 13 pruned features (MobileDrop, CFBridge, Inbox, GitSync, ConfigPack, TagEdit,
  // DeepSearch, JEV/AutoSelector, SlotAgent, Lessons, OllamaOps, WinRT OCR, NewWindow).
  const KEPT_METHODS = Object.freeze([
    // Lifecycle, config, session & Win32 window/shell integration
    'uiReady',
    'getAppVersion',
    'getAppInfo',
    'getPlatformCapabilities',
    'getConfig',
    'saveConfig',
    'getSession',
    'saveSession',
    'saveSessionSync',
    'getStartupFile',
    'trimMemory',
    'closeWindow',
    'minimizeWindow',
    'toggleMaximize',
    'toggleFullscreen',
    'forceQuit',
    'openQuickCapture',
    'updateGlobalShortcut',
    'updateQuickCaptureShortcut',
    'isSendToShortcutInstalled',
    'installSendToShortcut',
    'uninstallSendToShortcut',
    'fileTypesStatus',
    'registerFileTypes',
    'unregisterFileTypes',

    // F01 Editor files, watch & dialogs
    'openFile',
    'openFolder',
    'scanFolderFiles',
    'readFileByPath',
    'saveFile',
    'saveFileAs',
    'saveFileChecked',
    'saveFileCheckedAsync',
    'createNoteInFolder',
    'exportPlainTextAs',
    'resolveDroppedFile',
    'watchActiveFile',
    'unwatchActiveFile',
    'openExternal',
    'openPath',
    'revealPath',
    'showInFileExplorer',

    // F02 & F13 Daily scraps, full-text search & semantic index
    'getScrapDir',
    'searchScraps',
    'searchScrapsSemantic',
    'scrapFilterOptions',
    'semanticStatus',
    'semanticUpdate',
    'cancelSemanticUpdate',

    // F04/F05/F11/F12/F23(AI-OCR) Inference, assets & CLI shell pipe
    'detectLLMProvider',
    'queryLLMAsync',
    'queryDecisionAsync',
    'cancelLLMRequest',
    'autocompleteAsync',
    'queryVisionAsync',
    'generateImageAsync',
    'processDroppedMedia',
    'saveAsset',
    'importAssetFile',
    'copyPNGToClipboard',
    'runCommandFilter',
    'runCommandFilterAsync',
    'cancelCommandFilter',
    'generateCliCommandAsync',
    'getAvailableSkillsJSON',
    'getSkillInstruction',

    // F07 IME & F08 Printing
    'setIMEMode',
    'retypeWithImeAsync',
    'printPreview',
    'printPickPdfPath',
    'printSavePdf',
    'printPreviewClose',
    'printSystem',

    // F18 Embedded PTY
    'ptyAvailablePresets',
    'ptyClose',
    'ptyDetectGate',
    'ptyHistory',
    'ptyPickFolder',
    'ptyPrepareWorktree',
    'ptyRename',
    'ptyResize',
    'ptyStart',
    'ptyStartWithOptions',
    'ptySuggestDirs',
    'ptyWrite',

    // F21 Local STT (Haas-ja / speech.cpp) & F22 WASAPI Meeting audio
    'getSpeechStatus',
    'prefetchSpeechModel',
    'installSpeechPartAsync',
    'cancelSpeechInstall',
    'removeSpeechPart',
    'pickFilePath',
    'validateWhisperModelFile',
    'transcribeAudioAsync',
    'transcribeVoiceCache',
    'retryVoiceCacheAsync',
    'listVoiceCaches',
    'keepVoiceCache',
    'discardVoiceCache',
    'discardAllVoiceCaches',
    'meetingRecordingSupported',
    'checkMeetingAudioAsync',
    'startMeetingRecording',
    'stopMeetingRecordingAsync',
    'abortMeetingRecording',

    // Extension platform: PolicyDecisionEngine, Program Allowlist, QcTheme cache
    'runProgramAsync',
    'getPolicyAuditLog',
    'approvePlugin',
    'setPluginDevMode',
    'approveProgram',
    'saveQcThemeCache'
  ]);

  function parseIncomingMessage(raw) {
    if (typeof raw === 'string') {
      try {
        raw = JSON.parse(raw);
      } catch (_) {
        return null;
      }
    }
    return raw && typeof raw === 'object' ? raw : null;
  }

  function dispatchBuiltinTopic(win, topic, payload) {
    if (!win) return;
    const p = payload && typeof payload === 'object' ? payload : {};
    switch (topic) {
      case 'file:changed':
        if (typeof win.__onDiskTextSeen === 'function') win.__onDiskTextSeen(p.path, p);
        break;
      case 'file:written':
        if (typeof win.__onDiskTextWritten === 'function') win.__onDiskTextWritten(p.path, p.text);
        break;
      case 'host:openFile':
        if (typeof win.__onHostOpenFile === 'function') win.__onHostOpenFile(p);
        else if (typeof win.onScrapAppended === 'function') {
          win.onScrapAppended({
            filePath: p.filePath || p.path || '',
            fileName: p.fileName || p.title || '',
            ...p,
          });
        }
        break;
      case 'host:flushSession':
      case 'host:flush':
        if (typeof win.__sykiBeforeQuit === 'function') win.__sykiBeforeQuit();
        break;
      case 'llm:result':
        if (typeof win.__onLLMResult === 'function') win.__onLLMResult(p.reqId, p.result, p.error);
        break;
      case 'llm:decisionResult':
        if (typeof win.__onLLMDecisionResult === 'function') {
          win.__onLLMDecisionResult(p.reqId, p.choiceId, p.reason, p.by, p.ok);
        }
        break;
      case 'autocomplete:result':
        if (typeof win.__onAutocompleteResult === 'function') win.__onAutocompleteResult(p.reqId, p.suggestion, p.error);
        break;
      case 'cli:result':
        if (typeof win.__onCliFilterResult === 'function') win.__onCliFilterResult(p.reqId, p.result, p.error);
        break;
      case 'cli:command':
        if (typeof win.__onCliCommandGenerated === 'function') win.__onCliCommandGenerated(p.reqId, p.command, p.error, p.validation);
        break;
      case 'semantic:progress':
        if (typeof win.__semanticProgress === 'function') win.__semanticProgress(p.filesDone, p.filesTotal, p.chunksDone, p.chunksTotal);
        break;
      case 'ime:retype':
        if (typeof win.__onImeRetypeResult === 'function') win.__onImeRetypeResult(p.reqId, p.error);
        break;
      case 'meeting:check':
        if (typeof win.__onMeetingAudioCheck === 'function') win.__onMeetingAudioCheck(p.reqId, p.json);
        break;
      case 'media:dropped':
        if (typeof win.__onDroppedMedia === 'function') win.__onDroppedMedia(p.reqId, p.text, p.error);
        break;
      case 'voice:result':
        if (typeof win.__onVoiceResult === 'function') win.__onVoiceResult(p.reqId, p.text, p.error, p.cachePath, p.refineErr);
        break;
      case 'speech:install':
        if (typeof win.__onSpeechInstall === 'function') win.__onSpeechInstall(p.reqId, p.state, p.done, p.total, p.message);
        break;
      case 'scrap:appended':
        if (typeof win.onScrapAppended === 'function') win.onScrapAppended(p);
        break;
      case 'window:restored':
        if (typeof win.__onWindowRestored === 'function') win.__onWindowRestored();
        break;
      case 'window:newTab':
        if (typeof win.__onNewTabRequested === 'function') win.__onNewTabRequested();
        break;
      case 'window:requestClose':
        if (typeof win.__sykiRequestCloseWindow === 'function') win.__sykiRequestCloseWindow();
        break;
      case 'pty:data':
        if (typeof win.__onPtyData === 'function') win.__onPtyData(p.id, p.b64);
        break;
      case 'pty:exit':
        if (typeof win.__onPtyExit === 'function') win.__onPtyExit(p.id, p.code, p.error);
        break;
      default:
        break;
    }
  }

  function makeAbortError(message) {
    const err = new Error(message || 'Request aborted');
    err.name = 'AbortError';
    err.code = 'ABORT_ERR';
    return err;
  }

  function createReqIdDemux(options) {
    const opts = options || {};
    const defaultOnCancel = typeof opts.onCancel === 'function' ? opts.onCancel : null;
    const pending = new Map();

    function safeOnCancel(fn, reqId, topic) {
      if (typeof fn !== 'function') return;
      try {
        fn(reqId, topic);
      } catch (_) {}
    }

    function wait(reqId, waitOpts) {
      const key = String(reqId);
      const o = waitOpts || {};
      const topic = typeof o.topic === 'string' && o.topic ? o.topic : null;
      const signal = o.signal || null;
      const timeoutMs = Number(o.timeoutMs) > 0 ? Number(o.timeoutMs) : 0;
      const onCancel = typeof o.onCancel === 'function' ? o.onCancel : defaultOnCancel;
      const rejectOnError = o.rejectOnError === true;

      if (signal && signal.aborted) {
        safeOnCancel(onCancel, key, topic);
        const reason = signal.reason ? String(signal.reason) : 'Request aborted';
        return Promise.reject(makeAbortError(reason));
      }

      if (pending.has(key)) {
        const prev = pending.get(key);
        prev.abort('Superseded by new request with same reqId');
      }

      return new Promise((resolve, reject) => {
        const entry = {
          topic,
          rejectOnError,
          resolve,
          reject,
          timer: null,
          onAbort: null,
          signal,
          settled: false,
          cleanup() {
            if (entry.settled) return;
            entry.settled = true;
            pending.delete(key);
            if (entry.timer) {
              clearTimeout(entry.timer);
              entry.timer = null;
            }
            if (entry.signal && entry.onAbort && typeof entry.signal.removeEventListener === 'function') {
              entry.signal.removeEventListener('abort', entry.onAbort);
              entry.onAbort = null;
            }
          },
          abort(reason) {
            if (entry.settled) return;
            entry.cleanup();
            safeOnCancel(onCancel, key, topic);
            reject(makeAbortError(reason || 'Request aborted'));
          }
        };

        if (signal && typeof signal.addEventListener === 'function') {
          entry.onAbort = function () {
            const reason = signal.reason ? String(signal.reason) : 'Request aborted';
            entry.abort(reason);
          };
          signal.addEventListener('abort', entry.onAbort, { once: true });
        }

        if (timeoutMs > 0) {
          entry.timer = setTimeout(() => {
            if (entry.settled) return;
            entry.cleanup();
            safeOnCancel(onCancel, key, topic);
            const err = new Error(`Event timeout after ${timeoutMs}ms: ${topic || key}`);
            err.name = 'TimeoutError';
            err.code = 'TIMEOUT_ERR';
            reject(err);
          }, timeoutMs);
        }

        pending.set(key, entry);
      });
    }

    function dispatch(topicOrPayload, maybePayload) {
      let topic = null;
      let payload = topicOrPayload;
      if (arguments.length >= 2) {
        topic = typeof topicOrPayload === 'string' ? topicOrPayload : null;
        payload = maybePayload;
      }
      if (!payload || typeof payload !== 'object') return false;
      if (payload.reqId === undefined || payload.reqId === null) return false;
      const key = String(payload.reqId);
      const entry = pending.get(key);
      if (!entry || entry.settled) return false;
      if (entry.topic && topic && entry.topic !== topic) return false;
      entry.cleanup();
      if (entry.rejectOnError && payload.error) {
        entry.reject(new Error(String(payload.error)));
      } else {
        entry.resolve(payload);
      }
      return true;
    }

    function cancel(reqId, reason) {
      const key = String(reqId);
      const entry = pending.get(key);
      if (!entry || entry.settled) return false;
      entry.abort(reason || 'Request cancelled');
      return true;
    }

    function has(reqId) {
      return pending.has(String(reqId));
    }

    function pendingCount() {
      return pending.size;
    }

    function dispose(reason) {
      const msg = reason || 'Demux disposed';
      for (const entry of Array.from(pending.values())) {
        entry.cleanup();
        entry.reject(makeAbortError(msg));
      }
      pending.clear();
    }

    return { wait, dispatch, cancel, has, pendingCount, dispose };
  }

  function createPostMessageBackendProxy(options) {
    const opts = options || {};
    const targetWindow = opts.targetWindow !== undefined
      ? opts.targetWindow
      : (typeof global !== 'undefined' && global.window ? global.window : global);
    const webview = opts.webview !== undefined
      ? opts.webview
      : (targetWindow && targetWindow.chrome ? targetWindow.chrome.webview : null);
    const timeoutMs = Number(opts.timeoutMs) > 0 ? Number(opts.timeoutMs) : 0;

    let nextSeq = 1;
    let disposed = false;
    const pending = new Map();
    const listeners = new Map();
    const reqDemux = createReqIdDemux();

    function settlePending(id, msg) {
      const entry = pending.get(id);
      if (!entry) return;
      pending.delete(id);
      if (entry.timer) clearTimeout(entry.timer);
      if (msg.ok === false) {
        const err = new Error(msg.error || 'RPC error');
        if (msg.result && typeof msg.result === 'object') {
          err.detail = msg.result;
          if (msg.result.code !== undefined) err.code = msg.result.code;
        }
        entry.reject(err);
      } else {
        entry.resolve(msg.result);
      }
    }

    function handleEvent(topic, payload) {
      dispatchBuiltinTopic(targetWindow, topic, payload);
      reqDemux.dispatch(topic, payload);
      const set = listeners.get(topic);
      if (!set || set.size === 0) return;
      for (const fn of Array.from(set)) {
        try {
          fn(payload);
        } catch (err) {
          if (typeof console !== 'undefined' && console.error) {
            console.error('[backend_proxy] event listener failed for ' + topic + ':', err);
          }
        }
      }
    }

    function onMessage(ev) {
      if (disposed) return;
      const msg = parseIncomingMessage(ev && ev.data);
      if (!msg) return;
      if (msg.id !== undefined && msg.id !== null) {
        settlePending(String(msg.id), msg);
        return;
      }
      if (typeof msg.topic === 'string') {
        handleEvent(msg.topic, msg.payload);
      }
    }

    if (webview && typeof webview.addEventListener === 'function') {
      webview.addEventListener('message', onMessage);
    }

    function callMethod(method, args) {
      if (disposed) return Promise.reject(new Error('backend proxy disposed'));
      const preload = targetWindow && targetWindow.__sykiPreload;
      if (preload && args.length === 0) {
        if (method === 'getConfig' && typeof preload.config === 'string') {
          const v = preload.config;
          preload.config = null;
          return Promise.resolve(v);
        }
        if (method === 'getSession' && typeof preload.session === 'string') {
          const v = preload.session;
          preload.session = null;
          return Promise.resolve(v);
        }
        if (method === 'getPlatformCapabilities' && preload.caps && typeof preload.caps === 'object') {
          const v = preload.caps;
          preload.caps = null;
          return Promise.resolve(v);
        }
      }
      if (!webview || typeof webview.postMessage !== 'function') {
        return Promise.reject(new Error('WebView2 postMessage bridge unavailable'));
      }
      const id = String(nextSeq++);
      const params = args.length === 0 ? {} : Array.prototype.slice.call(args);
      return new Promise((resolve, reject) => {
        const entry = { resolve, reject, timer: null };
        if (timeoutMs > 0) {
          entry.timer = setTimeout(() => {
            if (!pending.has(id)) return;
            pending.delete(id);
            reject(new Error(`RPC timeout after ${timeoutMs}ms: ${method}`));
          }, timeoutMs);
        }
        pending.set(id, entry);
        try {
          webview.postMessage({ id, method, params });
        } catch (err) {
          pending.delete(id);
          if (entry.timer) clearTimeout(entry.timer);
          reject(err);
        }
      });
    }

    const proxy = {
      onEvent: function (topic, fn) {
        if (typeof topic !== 'string' || typeof fn !== 'function') return function () {};
        let set = listeners.get(topic);
        if (!set) {
          set = new Set();
          listeners.set(topic, set);
        }
        set.add(fn);
        return function () {
          set.delete(fn);
          if (set.size === 0) listeners.delete(topic);
        };
      },
      waitForEvent: function (topic, reqId, waitOpts) {
        if (disposed) return Promise.reject(new Error('backend proxy disposed'));
        const merged = Object.assign({}, waitOpts || {}, { topic });
        return reqDemux.wait(reqId, merged);
      },
      cancelPendingEvent: function (reqId, reason) {
        return reqDemux.cancel(reqId, reason);
      },
      reqDemux,
      __dispose: function () {
        if (disposed) return;
        disposed = true;
        if (webview && typeof webview.removeEventListener === 'function') {
          webview.removeEventListener('message', onMessage);
        }
        for (const entry of pending.values()) {
          if (entry.timer) clearTimeout(entry.timer);
          entry.reject(new Error('backend proxy disposed'));
        }
        pending.clear();
        listeners.clear();
        reqDemux.dispose('backend proxy disposed');
      }
    };

    for (let i = 0; i < KEPT_METHODS.length; i++) {
      const method = KEPT_METHODS[i];
      proxy[method] = function () {
        return callMethod(method, arguments);
      };
    }

    return proxy;
  }

  const api = { createPostMessageBackendProxy, createReqIdDemux, KEPT_METHODS };
  if (global) global.SykiBackendProxy = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;

  // Auto-attach only inside live WebView2 (never overwrite eval_server's mock_backend.js).
  if (
    global &&
    !global.backend &&
    global.chrome &&
    global.chrome.webview &&
    typeof global.chrome.webview.postMessage === 'function'
  ) {
    global.backend = createPostMessageBackendProxy({
      targetWindow: global,
      webview: global.chrome.webview
    });
  }
})(typeof window !== 'undefined' ? window : globalThis);
