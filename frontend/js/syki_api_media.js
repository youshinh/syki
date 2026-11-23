// syki::sok Phase 3 API: Multimodal Input (exclusive audio lock + screen capture),
// Offscreen Render (Vision / Image Gen / Mermaid / HTML), and pipe:in Hook — Deep Module.
// UMD / CommonJS compatible: exposes window.SykiApiMedia / globalThis.SykiApiMedia and module.exports.
(function (global) {
  'use strict';

  function makeCodedError(code, message, extra) {
    const err = new Error(message || code);
    err.code = code;
    if (extra && typeof extra === 'object') {
      Object.keys(extra).forEach((k) => {
        err[k] = extra[k];
      });
    }
    return err;
  }

  function createAbortError(message) {
    const err = new Error(message || 'The operation was aborted.');
    err.name = 'AbortError';
    err.code = 'ABORT_ERR';
    return err;
  }

  function isAbortError(err) {
    return Boolean(err && (err.name === 'AbortError' || err.code === 'ABORT_ERR'));
  }

  function createAudioBusyError(activeMode) {
    const detail = activeMode ? ` (${activeMode} recording in progress)` : '';
    const err = makeCodedError(
      'AUDIO_BUSY',
      `Audio recording is already in progress${detail}`,
      { activeMode: activeMode || null }
    );
    err.name = 'AudioBusyError';
    return err;
  }

  function acquireGuard(deps) {
    if (!deps) return null;
    if (typeof deps.acquireDetachGuard === 'function') {
      deps.acquireDetachGuard();
      return () => {
        if (typeof deps.releaseDetachGuard === 'function') {
          deps.releaseDetachGuard();
        }
      };
    }
    if (deps.detachGuard && typeof deps.detachGuard.acquire === 'function') {
      const token = deps.detachGuard.acquire();
      return () => {
        if (typeof token === 'function') {
          token();
        } else if (typeof deps.detachGuard.release === 'function') {
          deps.detachGuard.release(token);
        }
      };
    }
    return null;
  }

  function withDetachGuard(deps, fn) {
    const release = acquireGuard(deps);
    let result;
    try {
      result = fn();
    } catch (err) {
      if (release) release();
      return Promise.reject(err);
    }
    return Promise.resolve(result).finally(() => {
      if (release) release();
    });
  }

  function raceWithSignal(promise, signal) {
    if (!signal) return Promise.resolve(promise);
    if (signal.aborted) {
      return Promise.reject(createAbortError());
    }
    return new Promise((resolve, reject) => {
      const onAbort = () => reject(createAbortError());
      signal.addEventListener('abort', onAbort, { once: true });
      Promise.resolve(promise).then(
        (val) => {
          signal.removeEventListener('abort', onAbort);
          resolve(val);
        },
        (err) => {
          signal.removeEventListener('abort', onAbort);
          reject(err);
        }
      );
    });
  }

  function createSykiMediaApi(deps) {
    const d = deps || {};

    // Exclusive audio recording lock shared across voice() and meeting().
    let activeAudioMode = null;

    // Registered pipe:in handlers in insertion order.
    const pipeInHandlers = new Set();

    async function runExclusiveAudio(mode, fnName, opts) {
      const options = opts && typeof opts === 'object' ? opts : {};
      const signal = options.signal;

      if (activeAudioMode !== null) {
        throw createAudioBusyError(activeAudioMode);
      }
      if (signal && signal.aborted) {
        throw createAbortError();
      }
      const recorder = d[fnName];
      if (typeof recorder !== 'function') {
        throw makeCodedError('NOT_SUPPORTED', `${fnName} dependency is not configured`);
      }

      activeAudioMode = mode;
      try {
        const result = await withDetachGuard(d, () =>
          raceWithSignal(recorder({ signal }), signal)
        );
        if (result === null || result === undefined) {
          return null;
        }
        return String(result);
      } finally {
        activeAudioMode = null;
      }
    }

    // ---- 1. input (exclusive recording lock + screen capture) ----
    const input = {
      voice(opts) {
        return runExclusiveAudio('voice', 'recordVoice', opts);
      },

      meeting(opts) {
        return runExclusiveAudio('meeting', 'recordMeeting', opts);
      },

      async captureScreen(opts) {
        if (typeof d.captureScreen !== 'function') {
          throw makeCodedError('NOT_SUPPORTED', 'captureScreen dependency is not configured');
        }
        const result = await withDetachGuard(d, () => d.captureScreen(opts));
        return result === null || result === undefined ? null : result;
      },
    };

    // ---- 2. exec (vision / generateImage / offscreen render) ----
    const render = {
      async mermaidToPng(mermaidCode, opts) {
        if (typeof mermaidCode !== 'string' || !mermaidCode.trim()) {
          throw makeCodedError('INVALID_MERMAID_CODE', 'mermaidCode must be a non-empty string');
        }
        if (typeof d.renderMermaidOffscreen !== 'function') {
          throw makeCodedError('NOT_SUPPORTED', 'renderMermaidOffscreen dependency is not configured');
        }
        const options = opts && typeof opts === 'object' ? opts : {};
        const signal = options.signal;
        if (signal && signal.aborted) {
          throw createAbortError(signal.reason);
        }
        const rawTheme = typeof options.theme === 'string'
          ? options.theme
          : (options.theme && typeof options.theme === 'object' && typeof options.theme.mode === 'string'
            ? options.theme.mode
            : '');
        const theme = rawTheme.trim() ? rawTheme.trim() : 'dark';
        const scale = typeof options.scale === 'number' && Number.isFinite(options.scale) && options.scale > 0
          ? options.scale
          : 2;

        const renderOpts = signal !== undefined ? { theme, scale, signal } : { theme, scale };

        // Delegates strictly to an isolated offscreen renderer; never mutates host globals
        // such as mermaidAppliedTone.
        return withDetachGuard(d, () =>
          raceWithSignal(
            d.renderMermaidOffscreen(mermaidCode, renderOpts),
            signal
          )
        );
      },

      async htmlToPng(html, opts) {
        if (typeof html !== 'string' || !html.trim()) {
          throw makeCodedError('INVALID_HTML', 'html must be a non-empty string');
        }
        if (typeof d.renderHtmlToPng !== 'function') {
          throw makeCodedError('NOT_SUPPORTED', 'renderHtmlToPng dependency is not configured');
        }
        return withDetachGuard(d, () => d.renderHtmlToPng(html, opts && typeof opts === 'object' ? opts : {}));
      },

      async htmlToPdf(html, opts) {
        if (typeof html !== 'string' || !html.trim()) {
          throw makeCodedError('INVALID_HTML', 'html must be a non-empty string');
        }
        if (typeof d.renderHtmlToPdf !== 'function') {
          throw makeCodedError('NOT_SUPPORTED', 'renderHtmlToPdf dependency is not configured');
        }
        return withDetachGuard(d, () => d.renderHtmlToPdf(html, opts && typeof opts === 'object' ? opts : {}));
      },
    };

    const exec = {
      async vision(assetOrBlob, prompt, opts) {
        if (assetOrBlob === null || assetOrBlob === undefined) {
          throw makeCodedError('INVALID_ARGUMENT', 'assetOrBlob is required');
        }
        if (typeof prompt !== 'string' || !prompt.trim()) {
          throw makeCodedError('INVALID_ARGUMENT', 'prompt must be a non-empty string');
        }
        const options = opts && typeof opts === 'object' ? opts : {};
        const signal = options.signal;
        if (signal && signal.aborted) {
          throw createAbortError();
        }
        if (typeof d.queryVision !== 'function') {
          throw makeCodedError('NOT_SUPPORTED', 'queryVision dependency is not configured');
        }
        return withDetachGuard(d, () =>
          raceWithSignal(d.queryVision(assetOrBlob, prompt, { signal }), signal)
        );
      },

      async generateImage(prompt, opts) {
        if (typeof prompt !== 'string' || !prompt.trim()) {
          throw makeCodedError('INVALID_ARGUMENT', 'prompt must be a non-empty string');
        }
        const options = opts && typeof opts === 'object' ? opts : {};
        const size = options.size;
        const signal = options.signal;
        if (signal && signal.aborted) {
          throw createAbortError();
        }
        if (typeof d.generateImage !== 'function') {
          throw makeCodedError('NOT_SUPPORTED', 'generateImage dependency is not configured');
        }
        return withDetachGuard(d, () =>
          raceWithSignal(d.generateImage(prompt, { size, signal }), signal)
        );
      },

      render,
    };

    // ---- 3. pipe:in hook ----
    function onPipeIn(handler) {
      if (typeof handler !== 'function') {
        throw makeCodedError('INVALID_HANDLER', 'pipe:in handler must be a function');
      }
      pipeInHandlers.add(handler);
      let disposed = false;
      return {
        dispose() {
          if (disposed) return;
          disposed = true;
          pipeInHandlers.delete(handler);
        },
      };
    }

    async function dispatchPipeIn(payload) {
      const p = payload && typeof payload === 'object' ? payload : {};
      const event = {
        text: p.text === null || p.text === undefined ? '' : String(p.text),
        source: p.source === null || p.source === undefined ? '' : String(p.source),
      };

      const snapshot = Array.from(pipeInHandlers);
      for (let i = 0; i < snapshot.length; i += 1) {
        try {
          const res = await snapshot[i](event);
          if (res && res.handled === true) {
            const out = { handled: true };
            if (res.transformedText !== undefined) {
              out.transformedText = String(res.transformedText);
            }
            return out;
          }
        } catch (err) {
          if (typeof d.onPipeInError === 'function') {
            try {
              d.onPipeInError(err, event);
            } catch (_) {
              // Isolate error reporter failures as well.
            }
          }
        }
      }

      return { handled: false };
    }

    return {
      input,
      exec,
      onPipeIn,
      dispatchPipeIn,
    };
  }

  const exported = {
    createSykiMediaApi,
    createAudioBusyError,
    createAbortError,
    isAbortError,
    makeCodedError,
  };

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = exported;
  }
  if (global) {
    global.SykiApiMedia = exported;
  }
})(typeof window !== 'undefined' ? window : globalThis);
