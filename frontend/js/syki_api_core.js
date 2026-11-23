// syki::sok Core v1 API (editor / ui / sys) — Deep Module.
// UMD / CommonJS compatible: exposes window.SykiApiCore / globalThis.SykiApiCore and module.exports.
(function (root, factory) {
  'use strict';
  const api = factory();
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = api;
  }
  if (root) {
    root.SykiApiCore = api;
  }
})(typeof globalThis !== 'undefined' ? globalThis : (typeof window !== 'undefined' ? window : this), function () {
  'use strict';

  const ALLOWED_PANEL_SLOTS = Object.freeze(['side-panel', 'floating', 'modal']);
  const ALLOWED_EVENTS = Object.freeze([
    'editor:change',
    'selection:change',
    'tab:change',
    'theme:change',
    'file:before-save',
    'file:save',
    'file:drop',
    'settings:change',
    'quick-capture:saved',
    'host:before-detach'
  ]);
  const ALLOWED_TOAST_LEVELS = Object.freeze(['info', 'ok', 'success', 'warn', 'error']);
  const DEFAULT_DETACH_TIMEOUT_MS = 500;
  const STORAGE_PREFIX = 'syki:plugin:';

  function makeCodedError(code, message) {
    const err = new Error(message || code);
    err.code = code;
    return err;
  }

  function toText(v) {
    return v === null || v === undefined ? '' : String(v);
  }

  function clampInt(n, min, max) {
    const num = Number(n);
    if (!Number.isFinite(num)) return min;
    const floored = Math.trunc(num);
    if (floored < min) return min;
    if (max !== undefined && floored > max) return max;
    return floored;
  }

  function normalizeRange(from, to, maxLen) {
    const limit = typeof maxLen === 'number' && maxLen >= 0 ? maxLen : undefined;
    let a = clampInt(from, 0, limit);
    let b = clampInt(to, 0, limit);
    if (a > b) {
      const tmp = a;
      a = b;
      b = tmp;
    }
    return { from: a, to: b };
  }

  function normalizeShortcut(combo) {
    if (typeof combo !== 'string') return '';
    return combo
      .split('+')
      .map((part) => part.trim().toLowerCase())
      .filter(Boolean)
      .join('+');
  }

  function isImeComposing(keyEvent) {
    if (!keyEvent || typeof keyEvent !== 'object') return false;
    return keyEvent.isComposing === true || keyEvent.keyCode === 229 || keyEvent.which === 229;
  }

  // Fallback secret masker when deps.stripSecrets (secret_strip.js) is not injected.
  function fallbackStripSecrets(input) {
    let text = toText(input);
    // 1. Strip user:pass@ from http(s) URLs.
    text = text.replace(/\b(https?:\/\/)([^/\s@?#]+@)/gi, '$1');
    // 2. Redact secret query parameters in URLs.
    text = text.replace(
      /([?&](?:api[_-]?key|apikey|token|secret|password|passwd|key|sig|signature|auth|authorization)=)([^&#\s]+)/gi,
      '$1[REDACTED]'
    );
    // 3. Redact Bearer tokens.
    text = text.replace(/\b(Bearer\s+)[A-Za-z0-9._~+/=-]{8,}/gi, '$1[REDACTED]');
    // 4. Redact common key=value / key: value assignments.
    text = text.replace(
      /\b((?:api[_-]?key|apikey|token|secret|password|passwd)\s*[:=]\s*["']?)([^\s"',;}{&#]+)/gi,
      '$1[REDACTED]'
    );
    // 5. Redact well-known provider token prefixes.
    text = text.replace(/\b(?:sk-[A-Za-z0-9_-]{12,}|ghp_[A-Za-z0-9]{16,}|AIza[0-9A-Za-z_-]{20,}|xox[baprs]-[A-Za-z0-9-]{10,})\b/g, '[REDACTED]');
    return text;
  }

  function extractSchemaDefaults(schema) {
    const defaults = {};
    if (!schema) return defaults;
    if (Array.isArray(schema)) {
      for (let i = 0; i < schema.length; i++) {
        const item = schema[i];
        if (item && typeof item.key === 'string') {
          if ('default' in item) {
            defaults[item.key] = item.default;
          } else if ('defaultValue' in item) {
            defaults[item.key] = item.defaultValue;
          }
        }
      }
    } else if (typeof schema === 'object') {
      const entries = Object.keys(schema);
      for (let i = 0; i < entries.length; i++) {
        const k = entries[i];
        const def = schema[k];
        if (def && typeof def === 'object' && 'default' in def) {
          defaults[k] = def.default;
        }
      }
    }
    return defaults;
  }

  function createSykiCoreApi(deps) {
    const d = deps || {};
    const defaultPluginId = typeof d.pluginId === 'string' && d.pluginId.trim() ? d.pluginId.trim() : 'core';

    // ---- Internal state ----
    const listeners = new Map();
    for (let i = 0; i < ALLOWED_EVENTS.length; i++) {
      listeners.set(ALLOWED_EVENTS[i], new Set());
    }

    const memoryStorage = new Map();
    const commands = new Map(); // id -> { id, title, shortcut, normalizedShortcut, exposeRpc, run }
    const shortcutIndex = new Map(); // normalizedShortcut -> commandId
    const registeredSettings = new Map(); // pluginId -> { schema, values }
    const registeredActions = new Map();

    // ---- Storage helper (pluginId namespaced) ----
    function resolveStorageKey(key, opts) {
      const k = toText(key).trim();
      if (!k) throw makeCodedError('INVALID_STORAGE_KEY', 'Storage key must be a non-empty string');
      const ns = (opts && typeof opts.pluginId === 'string' && opts.pluginId.trim())
        ? opts.pluginId.trim()
        : defaultPluginId;
      return STORAGE_PREFIX + ns + ':' + k;
    }

    const storage = {
      get(key, opts) {
        const fullKey = resolveStorageKey(key, opts);
        if (d.storage && typeof d.storage.get === 'function') {
          return d.storage.get(fullKey);
        }
        return memoryStorage.has(fullKey) ? memoryStorage.get(fullKey) : undefined;
      },
      set(key, value, opts) {
        const fullKey = resolveStorageKey(key, opts);
        if (d.storage && typeof d.storage.set === 'function') {
          return d.storage.set(fullKey, value);
        }
        memoryStorage.set(fullKey, value);
        return true;
      }
    };

    // ---- Event bus helpers ----
    function assertValidEvent(event) {
      if (!listeners.has(event)) {
        throw makeCodedError('UNKNOWN_EVENT', 'Unsupported event: ' + String(event));
      }
    }

    function emitSyncOrCollect(event, payload) {
      assertValidEvent(event);
      const set = listeners.get(event);
      const snapshot = Array.from(set);
      const results = [];
      for (let i = 0; i < snapshot.length; i++) {
        results.push(snapshot[i](payload));
      }
      return results;
    }

    // ---- 1. editor ----
    const adapter = d.editorAdapter || {};
    function currentTextLength() {
      if (typeof adapter.getText === 'function') {
        return toText(adapter.getText()).length;
      }
      return undefined;
    }

    const editor = {
      getText() {
        return typeof adapter.getText === 'function' ? toText(adapter.getText()) : '';
      },
      setText(text) {
        if (typeof adapter.setText === 'function') {
          return adapter.setText(toText(text));
        }
        return undefined;
      },
      getSelection() {
        return typeof adapter.getSelection === 'function' ? toText(adapter.getSelection()) : '';
      },
      getSelectionRange() {
        const raw = typeof adapter.getSelectionRange === 'function'
          ? adapter.getSelectionRange()
          : { from: 0, to: 0 };
        const maxLen = currentTextLength();
        if (!raw || typeof raw !== 'object') return { from: 0, to: 0 };
        return normalizeRange(raw.from, raw.to, maxLen);
      },
      getSelectionRect() {
        if (typeof adapter.getSelectionRect === 'function') {
          return adapter.getSelectionRect();
        }
        return null;
      },
      insert(text) {
        if (typeof adapter.insert === 'function') {
          return adapter.insert(toText(text));
        }
        return undefined;
      },
      replaceSelection(text) {
        if (typeof adapter.replaceSelection === 'function') {
          return adapter.replaceSelection(toText(text));
        }
        return undefined;
      },
      replaceRange(from, to, text) {
        const maxLen = currentTextLength();
        const range = normalizeRange(from, to, maxLen);
        if (typeof adapter.replaceRange === 'function') {
          return adapter.replaceRange(range.from, range.to, toText(text));
        }
        return undefined;
      },
      openTab(opts) {
        const payload = opts && typeof opts === 'object'
          ? { title: toText(opts.title), content: toText(opts.content) }
          : { title: '', content: '' };
        if (typeof adapter.openTab === 'function') {
          return adapter.openTab(payload);
        }
        return null;
      },
      getActiveTab() {
        return typeof adapter.getActiveTab === 'function' ? adapter.getActiveTab() : null;
      },
      listTabs() {
        return typeof adapter.listTabs === 'function' ? adapter.listTabs() : [];
      },
      getTabText(tabId) {
        if (typeof adapter.getTabText === 'function') {
          return toText(adapter.getTabText(tabId));
        }
        return '';
      }
    };

    // ---- 2. ui ----
    const ui = {
      registerAction(action) {
        if (!action || typeof action !== 'object') {
          throw makeCodedError('INVALID_ACTION', 'Action definition must be an object');
        }
        if (typeof d.registerAction === 'function') {
          return d.registerAction(action);
        }
        const id = action.id || ('action_' + (registeredActions.size + 1));
        const record = Object.assign({ id }, action);
        registeredActions.set(id, record);
        return {
          id,
          update(patch) {
            if (patch && typeof patch === 'object') Object.assign(record, patch);
          },
          dispose() {
            registeredActions.delete(id);
          }
        };
      },

      mountPanel(slot, opts) {
        if (ALLOWED_PANEL_SLOTS.indexOf(slot) === -1) {
          throw makeCodedError(
            'INVALID_PANEL_SLOT',
            'Invalid panel slot "' + String(slot) + '". Allowed: ' + ALLOWED_PANEL_SLOTS.join(', ')
          );
        }
        const options = opts && typeof opts === 'object' ? opts : {};
        if (typeof d.mountPanel === 'function') {
          return d.mountPanel(slot, options);
        }
        return {
          slot,
          dispose() {},
          unmount() {},
          update() {}
        };
      },

      registerSettings(pluginId, schema) {
        if (typeof pluginId !== 'string' || !pluginId.trim()) {
          throw makeCodedError('INVALID_PLUGIN_ID', 'pluginId must be a non-empty string');
        }
        if (!schema || typeof schema !== 'object') {
          throw makeCodedError('INVALID_SETTINGS_SCHEMA', 'schema must be an object or array');
        }
        const pid = pluginId.trim();
        const defaults = extractSchemaDefaults(schema);
        const persisted = storage.get('settings', { pluginId: pid });
        const initial = Object.assign(
          {},
          defaults,
          persisted && typeof persisted === 'object' ? persisted : {}
        );
        const entry = { pluginId: pid, schema, values: initial };
        registeredSettings.set(pid, entry);

        function applyChange(key, value) {
          const prevValue = entry.values[key];
          if (Object.is(prevValue, value)) {
            return { changed: false, values: Object.assign({}, entry.values) };
          }
          entry.values[key] = value;
          const snapshot = Object.assign({}, entry.values);
          storage.set('settings', snapshot, { pluginId: pid });
          emitSyncOrCollect('settings:change', {
            pluginId: pid,
            key,
            value,
            prevValue,
            values: snapshot
          });
          return { changed: true, values: snapshot };
        }

        return {
          pluginId: pid,
          schema,
          get(key) {
            return entry.values[key];
          },
          getAll() {
            return Object.assign({}, entry.values);
          },
          set(key, value) {
            return applyChange(key, value);
          },
          update(patch) {
            if (!patch || typeof patch !== 'object') return Object.assign({}, entry.values);
            const keys = Object.keys(patch);
            for (let i = 0; i < keys.length; i++) {
              applyChange(keys[i], patch[keys[i]]);
            }
            return Object.assign({}, entry.values);
          },
          dispose() {
            registeredSettings.delete(pid);
          }
        };
      },

      form(schema) {
        if (typeof d.form === 'function') {
          return d.form(schema);
        }
        return Promise.resolve(null);
      },

      review(text, opts) {
        const fn = typeof d.review === 'function' ? d.review : d.reviewModal;
        if (typeof fn === 'function') {
          return Promise.resolve(fn(toText(text), opts || {})).then((res) => {
            if (res && typeof res === 'object' && 'accepted' in res) {
              return res.accepted ? toText(res.text) : null;
            }
            return res === null || res === undefined ? null : toText(res);
          });
        }
        return Promise.resolve(null);
      },

      diffConfirm(before, after, opts) {
        const options = opts && typeof opts === 'object' ? opts : {};
        const beforeStr = toText(before);
        const afterStr = toText(after);

        let isActiveEditor = true;
        if (options.forceModal === true || options.isActiveEditor === false) {
          isActiveEditor = false;
        } else if (options.target && options.target !== 'active-editor' && options.target !== 'editor') {
          isActiveEditor = false;
        } else if (options.tabId !== undefined) {
          const active = editor.getActiveTab();
          if (!active || active.id !== options.tabId) {
            isActiveEditor = false;
          }
        }

        if (isActiveEditor && typeof d.ghostDiff === 'function') {
          return d.ghostDiff(beforeStr, afterStr, options);
        }
        if (typeof d.modalDiff === 'function') {
          return d.modalDiff(beforeStr, afterStr, options);
        }
        return Promise.resolve({ accepted: false, mode: 'none' });
      },

      async decide(params) {
        const p = params && typeof params === 'object' ? params : {};
        if (!Array.isArray(p.options) || p.options.length === 0) {
          return null;
        }
        if (typeof d.decideModal === 'function') {
          return await d.decideModal({
            title: toText(p.title),
            question: toText(p.question),
            context: p.context !== undefined ? p.context : null,
            options: p.options.slice(),
            allowCustom: Boolean(p.allowCustom)
          });
        }
        return null;
      },

      toast(message, level) {
        const lvl = ALLOWED_TOAST_LEVELS.indexOf(level) !== -1 ? level : 'info';
        const msg = toText(message);
        if (typeof d.toast === 'function') {
          return d.toast(msg, lvl);
        }
        return undefined;
      },

      getTheme() {
        if (typeof d.getTheme === 'function') {
          return d.getTheme();
        }
        return { name: 'default', mode: 'light', tokens: {} };
      },

      setToken(name, value) {
        if (typeof name !== 'string' || !name.startsWith('--') || name.length <= 2) {
          throw makeCodedError('INVALID_TOKEN_NAME', 'CSS token name must start with "--": ' + String(name));
        }
        const val = toText(value);
        if (typeof d.setToken === 'function') {
          return d.setToken(name, val);
        }
        return undefined;
      },

      setChrome(opts) {
        const o = opts && typeof opts === 'object' ? opts : {};
        const normalized = {};
        if ('toolbar' in o) normalized.toolbar = Boolean(o.toolbar);
        if ('tabs' in o) normalized.tabs = Boolean(o.tabs);
        if ('statusbar' in o) normalized.statusbar = Boolean(o.statusbar);
        if ('zen' in o) normalized.zen = Boolean(o.zen);

        let result;
        if (typeof d.setChrome === 'function') {
          result = d.setChrome(normalized);
        }
        if (typeof d.syncImmersiveExit === 'function') {
          d.syncImmersiveExit({
            zen: Boolean(normalized.zen),
            chrome: normalized
          });
        }
        return result;
      }
    };

    // ---- 3. sys ----
    const sys = {
      get locale() {
        const raw = typeof d.getLocale === 'function' ? d.getLocale() : 'en';
        const s = toText(raw).trim().toLowerCase();
        return s === 'ja' || s.startsWith('ja-') || s.startsWith('ja_') ? 'ja' : 'en';
      },

      on(event, handler) {
        assertValidEvent(event);
        if (typeof handler !== 'function') {
          throw new TypeError('Event handler must be a function');
        }
        const set = listeners.get(event);
        set.add(handler);
        return function unsubscribe() {
          set.delete(handler);
        };
      },

      emit(event, payload) {
        return emitSyncOrCollect(event, payload);
      },

      async emitBeforeSave(payload) {
        const snapshot = Array.from(listeners.get('file:before-save'));
        for (let i = 0; i < snapshot.length; i++) {
          const res = await snapshot[i](payload);
          if (res && res.cancel === true) {
            return {
              cancel: true,
              cancelled: true,
              reason: toText(res.reason)
            };
          }
        }
        return { cancel: false, cancelled: false, reason: '' };
      },

      async emitFileDrop(payload) {
        const snapshot = Array.from(listeners.get('file:drop'));
        for (let i = 0; i < snapshot.length; i++) {
          const res = await snapshot[i](payload);
          if (res && res.handled === true) {
            return { handled: true, preventDefault: true };
          }
        }
        return { handled: false, preventDefault: false };
      },

      async emitBeforeDetach(timeoutMs) {
        const limit = (typeof timeoutMs === 'number' && Number.isFinite(timeoutMs) && timeoutMs > 0)
          ? timeoutMs
          : DEFAULT_DETACH_TIMEOUT_MS;
        const snapshot = Array.from(listeners.get('host:before-detach'));
        const total = snapshot.length;
        if (total === 0) {
          return { timedOut: false, completed: 0, total: 0, errors: [] };
        }

        let completed = 0;
        const errors = [];
        const tasks = snapshot.map((fn) =>
          Promise.resolve()
            .then(() => fn({ timeoutMs: limit }))
            .then(() => { completed += 1; })
            .catch((err) => {
              completed += 1;
              errors.push(err);
            })
        );

        let timerId = null;
        const timeoutPromise = new Promise((resolve) => {
          timerId = setTimeout(() => resolve('timeout'), limit);
        });

        const allDone = Promise.all(tasks).then(() => 'done');
        const winner = await Promise.race([allDone, timeoutPromise]);
        if (timerId !== null) {
          clearTimeout(timerId);
        }

        return {
          timedOut: winner === 'timeout',
          completed,
          total,
          errors
        };
      },

      registerCommand(def) {
        if (!def || typeof def !== 'object') {
          throw makeCodedError('INVALID_COMMAND', 'Command definition must be an object');
        }
        const id = toText(def.id).trim();
        if (!id) {
          throw makeCodedError('INVALID_COMMAND_ID', 'Command id must be a non-empty string');
        }
        if (typeof def.run !== 'function') {
          throw makeCodedError('INVALID_COMMAND_RUN', 'Command run must be a function');
        }

        const normShortcut = normalizeShortcut(def.shortcut);
        let shortcutRegistered = false;

        if (normShortcut) {
          const existingId = shortcutIndex.get(normShortcut);
          if (existingId && existingId !== id) {
            ui.toast(
              'Shortcut collision for "' + def.shortcut + '": already bound to "' + existingId + '" (keeping existing)',
              'warn'
            );
          } else {
            shortcutIndex.set(normShortcut, id);
            shortcutRegistered = true;
          }
        }

        // If re-registering same id with a different shortcut, release old shortcut.
        const prev = commands.get(id);
        if (prev && prev.normalizedShortcut && prev.normalizedShortcut !== normShortcut) {
          if (shortcutIndex.get(prev.normalizedShortcut) === id) {
            shortcutIndex.delete(prev.normalizedShortcut);
          }
        }

        const record = {
          id,
          title: toText(def.title),
          shortcut: def.shortcut ? String(def.shortcut) : '',
          normalizedShortcut: shortcutRegistered ? normShortcut : '',
          exposeRpc: Boolean(def.exposeRpc),
          run: def.run
        };
        commands.set(id, record);

        return {
          id,
          shortcut: record.shortcut,
          shortcutRegistered,
          dispose() {
            const cur = commands.get(id);
            if (cur === record) {
              commands.delete(id);
              if (cur.normalizedShortcut && shortcutIndex.get(cur.normalizedShortcut) === id) {
                shortcutIndex.delete(cur.normalizedShortcut);
              }
            }
          }
        };
      },

      dispatchShortcut(combo, keyEvent) {
        // Always suppress while IME composition is active (Japanese IME safety).
        if (isImeComposing(keyEvent)) {
          return false;
        }
        const norm = normalizeShortcut(combo);
        if (!norm) return false;
        const cmdId = shortcutIndex.get(norm);
        if (!cmdId) return false;
        const cmd = commands.get(cmdId);
        if (!cmd) return false;
        cmd.run(keyEvent);
        return true;
      },

      runCommand(id, args, opts) {
        const cmd = commands.get(toText(id).trim());
        if (!cmd) {
          throw makeCodedError('COMMAND_NOT_FOUND', 'Command not found: ' + String(id));
        }
        if (opts && opts.viaRpc === true && cmd.exposeRpc !== true) {
          throw makeCodedError('RPC_FORBIDDEN', 'RPC_FORBIDDEN: command "' + cmd.id + '" is not exposed over RPC');
        }
        return cmd.run(args, opts || {});
      },

      stripSecrets(text) {
        if (typeof d.stripSecrets === 'function') {
          return d.stripSecrets(text);
        }
        return fallbackStripSecrets(text);
      },

      clipboard: {
        read() {
          if (d.clipboard && typeof d.clipboard.read === 'function') {
            return d.clipboard.read();
          }
          return Promise.resolve('');
        },
        writeText(text) {
          if (d.clipboard && typeof d.clipboard.writeText === 'function') {
            return d.clipboard.writeText(toText(text));
          }
          return Promise.resolve(false);
        },
        writeImage(imageData) {
          if (d.clipboard && typeof d.clipboard.writeImage === 'function') {
            return d.clipboard.writeImage(imageData);
          }
          return Promise.resolve(false);
        }
      },

      pickFile(opts) {
        if (typeof d.pickFile === 'function') {
          return d.pickFile(opts || {});
        }
        return Promise.resolve(null);
      },

      storage
    };

    return {
      editor,
      ui,
      sys
    };
  }

  return {
    createSykiCoreApi,
    ALLOWED_EVENTS,
    ALLOWED_PANEL_SLOTS,
    DEFAULT_DETACH_TIMEOUT_MS
  };
});
