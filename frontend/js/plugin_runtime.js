// Syki-lite Frontend Plugin Runtime (UMD / CommonJS compatible Deep Module).
// Provides startup circuit breaker, scoped plugin proxy with automatic resource cleanup,
// declarative concurrency-guarded actions, Shadow DOM panel isolation, and lazy/hot-reload manager.
(function (global) {
  'use strict';

  const BREAKER_PREFIX = 'syki:plugin:boot:';
  const SHADOW_SLOTS = new Set(['side-panel', 'floating', 'modal']);
  const UI_METHODS = new Set([
    'registerAction',
    'mountPanel',
    'registerSettings',
    'on',
    'registerCommand',
    'setToken',
    'setChrome',
  ]);

  function createAbortError(message) {
    const err = new Error(message || 'The operation was aborted.');
    err.name = 'AbortError';
    err.code = 'ABORT_ERR';
    return err;
  }

  function isAbortError(err) {
    return Boolean(err && (err.name === 'AbortError' || err.code === 'ABORT_ERR'));
  }

  function formatError(err) {
    if (!err) return 'Unknown error';
    if (typeof err === 'string') return err;
    if (err instanceof Error) return err.message || err.name || 'Error';
    if (typeof err.message === 'string') return err.message;
    try {
      return String(err);
    } catch (_) {
      return 'Unknown error';
    }
  }

  function pluginModuleUrl(pluginId, rev) {
    const token = (rev !== undefined && rev !== null && String(rev).trim() !== '')
      ? encodeURIComponent(String(rev))
      : '1';
    return '/__ext/r/' + token + '/plugins/' + encodeURIComponent(String(pluginId)) + '/plugin.js';
  }

  function toCleanupFn(disposable) {
    if (typeof disposable === 'function') {
      return disposable;
    }
    if (disposable && typeof disposable.dispose === 'function') {
      return function () { disposable.dispose(); };
    }
    if (disposable && typeof disposable.unmount === 'function') {
      return function () { disposable.unmount(); };
    }
    return null;
  }

  function createMemoryStorage() {
    const map = new Map();
    return {
      getItem(key) { return map.has(key) ? map.get(key) : null; },
      setItem(key, val) { map.set(key, String(val)); },
      removeItem(key) { map.delete(key); },
    };
  }

  // 1. Startup Circuit Breaker
  function createCircuitBreaker(storage, options) {
    const store = (storage && typeof storage.getItem === 'function' && typeof storage.setItem === 'function')
      ? storage
      : createMemoryStorage();
    const prefix = (options && options.prefix) ? String(options.prefix) : BREAKER_PREFIX;

    function keyFor(pluginId) {
      return prefix + String(pluginId);
    }

    function safeGet(key) {
      try {
        return store.getItem(key);
      } catch (_) {
        return null;
      }
    }

    function safeSet(key, val) {
      try {
        store.setItem(key, val);
      } catch (_) {
        // Ignore storage quota / privacy mode errors so host never crashes.
      }
    }

    function safeRemove(key) {
      try {
        if (typeof store.removeItem === 'function') {
          store.removeItem(key);
        } else {
          store.setItem(key, '');
        }
      } catch (_) {
        // Ignore storage errors.
      }
    }

    return {
      markStarting(pluginId) {
        safeSet(keyFor(pluginId), 'starting:' + Date.now());
      },
      markReady(pluginId) {
        safeRemove(keyFor(pluginId));
      },
      isTripped(pluginId) {
        const val = safeGet(keyFor(pluginId));
        return typeof val === 'string' && val.startsWith('starting:');
      },
      reset(pluginId) {
        safeRemove(keyFor(pluginId));
      },
    };
  }

  function linkAbortSignal(parentSignal, childController) {
    if (!parentSignal) return function () {};
    if (parentSignal.aborted) {
      childController.abort(parentSignal.reason || createAbortError('Parent scope disposed'));
      return function () {};
    }
    const onAbort = function () {
      childController.abort(parentSignal.reason || createAbortError('Parent scope disposed'));
    };
    parentSignal.addEventListener('abort', onAbort, { once: true });
    return function unlink() {
      parentSignal.removeEventListener('abort', onAbort);
    };
  }

  function createStyleElement(host, hostApi) {
    if (host && host.ownerDocument && typeof host.ownerDocument.createElement === 'function') {
      return host.ownerDocument.createElement('style');
    }
    if (hostApi && typeof hostApi.createElement === 'function') {
      return hostApi.createElement('style');
    }
    if (typeof document !== 'undefined' && document && typeof document.createElement === 'function') {
      return document.createElement('style');
    }
    return null;
  }

  // 2, 3, 4. Scoped Plugin Context with Action Concurrency Guard & Shadow DOM Isolation
  function createPluginScope(pluginId, hostApi, options) {
    const host = hostApi || {};
    const onErrorCallback = options && typeof options.onError === 'function' ? options.onError : null;
    const controller = new AbortController();
    const cleanups = new Set();
    const errors = [];
    let disposed = false;
    let autoSeq = 0;

    function nextId(prefix) {
      autoSeq += 1;
      return pluginId + ':' + prefix + ':' + autoSeq;
    }

    function track(disposable) {
      const fn = toCleanupFn(disposable);
      if (!fn) return function () {};
      if (disposed) {
        try { fn(); } catch (_) {}
        return function () {};
      }
      cleanups.add(fn);
      return function untrackAndRun() {
        if (cleanups.delete(fn)) {
          try { fn(); } catch (_) {}
        }
      };
    }

    function recordError(source, err) {
      const entry = {
        pluginId,
        source,
        message: formatError(err),
        error: err,
        timestamp: Date.now(),
      };
      errors.push(entry);
      if (onErrorCallback) {
        try { onErrorCallback(err, entry); } catch (_) {}
      }
      if (!disposed && typeof host.toast === 'function') {
        try {
          host.toast('[' + pluginId + '] ' + entry.message, 'error');
        } catch (_) {}
      }
      return entry;
    }

    function dispose() {
      if (disposed) return;
      disposed = true;
      try {
        controller.abort(createAbortError('Plugin "' + pluginId + '" disposed'));
      } catch (_) {}
      const list = Array.from(cleanups).reverse();
      cleanups.clear();
      for (let i = 0; i < list.length; i += 1) {
        try {
          list[i]();
        } catch (_) {
          // Never let one failing cleanup block releasing remaining resources.
        }
      }
    }

    // 3. Declarative Action Management
    function registerAction(spec) {
      const noopHandle = { dispose() {}, update() {} };
      if (disposed) return noopHandle;

      const cfg = spec || {};
      const actionId = cfg.id || nextId('action');
      const concurrency = cfg.concurrency || 'lock';
      const handler = typeof cfg.run === 'function'
        ? cfg.run
        : (typeof cfg.handler === 'function' ? cfg.handler : cfg.onClick);

      const state = {
        id: actionId,
        pluginId,
        label: cfg.label,
        icon: cfg.icon,
        badge: cfg.badge,
        disabled: Boolean(cfg.disabled),
        slot: cfg.slot,
        group: cfg.group,
        shortcut: cfg.shortcut,
        concurrency,
      };

      let actionDisposed = false;
      let locked = false;
      let prevController = null;
      const activeControllers = new Set();

      async function invokeAction(extraCtx) {
        if (disposed || actionDisposed || state.disabled) {
          return undefined;
        }
        if (typeof handler !== 'function') {
          return undefined;
        }
        if (concurrency === 'lock' && locked) {
          return undefined;
        }
        if (concurrency === 'cancel-prev' && prevController) {
          prevController.abort(createAbortError('Cancelled by newer invocation'));
          prevController = null;
        }

        const runController = new AbortController();
        const unlink = linkAbortSignal(controller.signal, runController);
        activeControllers.add(runController);
        if (concurrency === 'lock') {
          locked = true;
        } else if (concurrency === 'cancel-prev') {
          prevController = runController;
        }

        const ctx = Object.assign({}, extraCtx && typeof extraCtx === 'object' ? extraCtx : {}, {
          pluginId,
          actionId,
          signal: runController.signal,
        });

        try {
          return await handler(ctx);
        } catch (err) {
          const cancelledByPrev = runController.signal.aborted && isAbortError(err);
          const disposedAbort = disposed && isAbortError(err);
          if (!cancelledByPrev && !disposedAbort) {
            recordError('action:' + actionId, err);
          }
          return undefined;
        } finally {
          unlink();
          activeControllers.delete(runController);
          if (concurrency === 'lock') {
            locked = false;
          }
          if (prevController === runController) {
            prevController = null;
          }
        }
      }

      const registration = typeof host.registerAction === 'function'
        ? host.registerAction(Object.assign({}, state, { run: invokeAction }))
        : null;
      const hostDispose = toCleanupFn(registration);

      function disposeAction() {
        if (actionDisposed) return;
        actionDisposed = true;
        for (const c of activeControllers) {
          try { c.abort(createAbortError('Action disposed')); } catch (_) {}
        }
        activeControllers.clear();
        prevController = null;
        locked = false;
        if (hostDispose) {
          try { hostDispose(); } catch (_) {}
        }
      }

      const release = track(disposeAction);

      return {
        id: actionId,
        dispose: release,
        update(patch) {
          if (disposed || actionDisposed || !patch || typeof patch !== 'object') {
            return;
          }
          if (Object.prototype.hasOwnProperty.call(patch, 'label')) state.label = patch.label;
          if (Object.prototype.hasOwnProperty.call(patch, 'icon')) state.icon = patch.icon;
          if (Object.prototype.hasOwnProperty.call(patch, 'badge')) state.badge = patch.badge;
          if (Object.prototype.hasOwnProperty.call(patch, 'disabled')) state.disabled = Boolean(patch.disabled);

          const snapshot = {
            id: state.id,
            pluginId: state.pluginId,
            label: state.label,
            icon: state.icon,
            badge: state.badge,
            disabled: state.disabled,
          };
          if (registration && typeof registration.update === 'function') {
            registration.update(snapshot);
          } else if (typeof host.updateAction === 'function') {
            host.updateAction(actionId, snapshot, { pluginId });
          }
        },
      };
    }

    // 4. Shadow DOM Auto-Isolation for Panels
    function mountPanel(spec) {
      const noopHandle = { dispose() {}, shadowRoot: null, host: null };
      if (disposed) return noopHandle;

      const cfg = spec || {};
      const panelId = cfg.id || nextId('panel');
      const slot = cfg.slot || 'side-panel';
      const styleCss = typeof cfg.styleCss === 'string'
        ? cfg.styleCss
        : (typeof cfg.css === 'string' ? cfg.css : '');

      const mountResult = typeof host.mountPanel === 'function'
        ? host.mountPanel({ id: panelId, pluginId, slot, title: cfg.title })
        : (cfg.host || cfg.container || null);

      let hostEl = null;
      let hostDispose = null;
      if (mountResult) {
        if (mountResult.host || mountResult.element || mountResult.container) {
          hostEl = mountResult.host || mountResult.element || mountResult.container;
          hostDispose = toCleanupFn(mountResult);
        } else {
          hostEl = mountResult;
          hostDispose = typeof hostEl.remove === 'function' ? function () { hostEl.remove(); } : null;
        }
      }

      let targetRoot = hostEl;
      let shadowRoot = null;
      if (hostEl && SHADOW_SLOTS.has(slot)) {
        if (hostEl.shadowRoot) {
          shadowRoot = hostEl.shadowRoot;
        } else if (typeof hostEl.attachShadow === 'function') {
          shadowRoot = hostEl.attachShadow({ mode: 'open' });
        }
        if (shadowRoot) {
          targetRoot = shadowRoot;
          if (styleCss) {
            const styleEl = createStyleElement(hostEl, host);
            if (styleEl) {
              styleEl.textContent = styleCss;
              if (typeof styleEl.setAttribute === 'function') {
                styleEl.setAttribute('data-syki-plugin', pluginId);
              }
              if (typeof shadowRoot.appendChild === 'function') {
                shadowRoot.appendChild(styleEl);
              }
            }
          }
        }
      }

      let renderCleanup = null;
      if (typeof cfg.render === 'function' && targetRoot) {
        try {
          const maybeCleanup = cfg.render(targetRoot, {
            pluginId,
            panelId,
            slot,
            signal: controller.signal,
          });
          renderCleanup = toCleanupFn(maybeCleanup);
        } catch (err) {
          recordError('panel:' + panelId, err);
        }
      }

      let panelDisposed = false;
      function disposePanel() {
        if (panelDisposed) return;
        panelDisposed = true;
        if (renderCleanup) {
          try { renderCleanup(); } catch (_) {}
        }
        if (shadowRoot) {
          if (typeof shadowRoot.replaceChildren === 'function') {
            try { shadowRoot.replaceChildren(); } catch (_) {}
          } else if (Array.isArray(shadowRoot.children)) {
            shadowRoot.children.length = 0;
          } else if ('innerHTML' in shadowRoot) {
            try { shadowRoot.innerHTML = ''; } catch (_) {}
          }
        }
        if (hostDispose) {
          try { hostDispose(); } catch (_) {}
        }
      }

      const release = track(disposePanel);
      return {
        id: panelId,
        host: hostEl,
        shadowRoot,
        dispose: release,
      };
    }

    function registerSettings(spec) {
      if (disposed) return { dispose() {} };
      const res = typeof host.registerSettings === 'function'
        ? host.registerSettings(Object.assign({}, spec || {}, { pluginId }))
        : null;
      const release = track(res);
      return { dispose: release };
    }

    function on(eventName, handler) {
      if (disposed || typeof handler !== 'function') return { dispose() {} };
      const wrapped = function () {
        if (disposed) return undefined;
        try {
          return handler.apply(this, arguments);
        } catch (err) {
          recordError('event:' + String(eventName), err);
          return undefined;
        }
      };
      const res = typeof host.on === 'function'
        ? host.on(eventName, wrapped, { pluginId, signal: controller.signal })
        : null;
      const cleanup = toCleanupFn(res) || (typeof host.off === 'function'
        ? function () { host.off(eventName, wrapped); }
        : null);
      const release = track(cleanup);
      return { dispose: release };
    }

    function registerCommand(idOrSpec, handler) {
      if (disposed) return { dispose() {} };
      const isObj = idOrSpec && typeof idOrSpec === 'object';
      const cmdId = isObj ? idOrSpec.id : idOrSpec;
      const fn = isObj ? (idOrSpec.run || idOrSpec.handler) : handler;
      const wrapped = typeof fn === 'function'
        ? function () {
            if (disposed) return undefined;
            return fn.apply(this, arguments);
          }
        : fn;
      const payload = isObj
        ? Object.assign({}, idOrSpec, { pluginId, run: wrapped, handler: wrapped })
        : { id: cmdId, pluginId, run: wrapped, handler: wrapped };
      const res = typeof host.registerCommand === 'function'
        ? host.registerCommand(payload)
        : null;
      const release = track(res);
      return { dispose: release };
    }

    function setToken(name, value) {
      if (disposed) return { dispose() {} };
      const res = typeof host.setToken === 'function'
        ? host.setToken(name, value, { pluginId })
        : null;
      const cleanup = toCleanupFn(res) || (typeof host.unsetToken === 'function'
        ? function () { host.unsetToken(name, { pluginId }); }
        : (typeof host.removeToken === 'function'
          ? function () { host.removeToken(name, { pluginId }); }
          : null));
      const release = track(cleanup);
      return { dispose: release };
    }

    function setChrome(keyOrPatch, value) {
      if (disposed) return { dispose() {} };
      const res = typeof host.setChrome === 'function'
        ? host.setChrome(keyOrPatch, value, { pluginId })
        : null;
      const cleanup = toCleanupFn(res) || (typeof host.resetChrome === 'function'
        ? function () { host.resetChrome(keyOrPatch, { pluginId }); }
        : null);
      const release = track(cleanup);
      return { dispose: release };
    }

    function toast(message, level) {
      if (disposed) return;
      if (typeof host.toast === 'function') {
        host.toast(message, level);
      }
    }

    const baseApi = {
      pluginId,
      signal: controller.signal,
      registerAction,
      mountPanel,
      registerSettings,
      on,
      registerCommand,
      setToken,
      setChrome,
      toast,
    };

    function wrapHostNamespace(targetObj) {
      return new Proxy(targetObj, {
        get(target, prop, receiver) {
          if (typeof prop === 'symbol') {
            return Reflect.get(target, prop, receiver);
          }
          const val = target[prop];
          if (typeof val === 'function') {
            return function () {
              if (disposed || controller.signal.aborted) {
                return Promise.reject(createAbortError('Plugin "' + pluginId + '" has been disposed'));
              }
              return val.apply(target, arguments);
            };
          }
          if (val && typeof val === 'object') {
            return wrapHostNamespace(val);
          }
          return val;
        },
      });
    }

    const scopedApi = new Proxy(baseApi, {
      get(target, prop, receiver) {
        if (Object.prototype.hasOwnProperty.call(target, prop)) {
          return Reflect.get(target, prop, receiver);
        }
        if (typeof prop === 'symbol') {
          return Reflect.get(target, prop, receiver);
        }
        if (UI_METHODS.has(prop)) {
          return target[prop];
        }
        const hostVal = host[prop];
        if (typeof hostVal === 'function') {
          return function () {
            if (disposed || controller.signal.aborted) {
              return Promise.reject(createAbortError('Plugin "' + pluginId + '" has been disposed'));
            }
            return hostVal.apply(host, arguments);
          };
        }
        if (hostVal && typeof hostVal === 'object') {
          return wrapHostNamespace(hostVal);
        }
        return undefined;
      },
    });

    return {
      pluginId,
      signal: controller.signal,
      api: scopedApi,
      track,
      dispose,
      isDisposed() { return disposed; },
      getErrors() { return errors.slice(); },
      getResourceCount() { return cleanups.size; },
    };
  }

  // 5. Lazy Loader & Lifecycle Manager
  function createPluginManager(options) {
    const opts = options || {};
    const importer = typeof opts.importer === 'function'
      ? opts.importer
      : function defaultImporter(url) { return import(url); };
    const policyCheck = typeof opts.policyCheck === 'function'
      ? opts.policyCheck
      : function allowAll() { return true; };
    const breaker = opts.breaker || createCircuitBreaker();
    const hostApi = opts.hostApi || {};
    const now = typeof opts.now === 'function'
      ? opts.now
      : function defaultNow() {
          return (typeof performance !== 'undefined' && typeof performance.now === 'function')
            ? performance.now()
            : Date.now();
        };

    const records = new Map();
    const entriesById = new Map();
    const activeScopes = new Map();

    function ensureRecord(entry) {
      const id = String(entry.id);
      const existing = records.get(id);
      const rec = existing || {
        id,
        name: entry.name || id,
        status: 'pending',
        loadMs: 0,
        lastError: null,
        devMode: Boolean(entry.devMode),
      };
      if (entry.name) rec.name = entry.name;
      if (Object.prototype.hasOwnProperty.call(entry, 'devMode')) {
        rec.devMode = Boolean(entry.devMode);
      }
      records.set(id, rec);
      entriesById.set(id, Object.assign({}, entriesById.get(id) || {}, entry, { id }));
      return rec;
    }

    function resolveInitFn(mod) {
      if (!mod) return null;
      if (typeof mod.init === 'function') return mod.init;
      if (mod.default && typeof mod.default.init === 'function') return mod.default.init;
      if (typeof mod === 'function') return mod;
      if (typeof mod.default === 'function') return mod.default;
      return null;
    }

    async function loadOne(entry, overrideUrl) {
      const rec = ensureRecord(entry);
      const id = rec.id;

      if (entry.enabled === false) {
        rec.status = 'disabled';
        rec.lastError = null;
        return Object.assign({}, rec);
      }

      if (breaker && typeof breaker.isTripped === 'function' && breaker.isTripped(id)) {
        rec.status = 'isolated';
        rec.loadMs = 0;
        rec.lastError = 'Isolated by startup circuit breaker after previous crash';
        return Object.assign({}, rec);
      }

      try {
        const decision = await policyCheck(entry);
        const allowed = (typeof decision === 'object' && decision !== null)
          ? Boolean(decision.allowed)
          : Boolean(decision);
        if (!allowed) {
          rec.status = 'blocked';
          rec.loadMs = 0;
          rec.lastError = (decision && decision.reason) ? String(decision.reason) : 'Blocked by policy';
          return Object.assign({}, rec);
        }
      } catch (err) {
        rec.status = 'blocked';
        rec.loadMs = 0;
        rec.lastError = formatError(err);
        return Object.assign({}, rec);
      }

      // Dispose any stale scope before starting fresh.
      if (activeScopes.has(id)) {
        try { activeScopes.get(id).dispose(); } catch (_) {}
        activeScopes.delete(id);
      }

      const url = overrideUrl || entry.url || pluginModuleUrl(id, entry.rev !== undefined ? entry.rev : 1);
      const scope = createPluginScope(id, hostApi, {
        onError(err) {
          rec.lastError = formatError(err);
        },
      });

      const startedAt = now();
      if (breaker && typeof breaker.markStarting === 'function') {
        breaker.markStarting(id);
      }

      try {
        const mod = await importer(url);
        const initFn = resolveInitFn(mod);
        if (!initFn) {
          throw new Error('Plugin module does not export an init(api) function');
        }
        const maybeDisposable = await initFn(scope.api);
        if (maybeDisposable) {
          scope.track(maybeDisposable);
        }
        activeScopes.set(id, scope);
        rec.status = 'active';
        rec.loadMs = Math.max(0, Math.round((now() - startedAt) * 1000) / 1000);
        rec.lastError = null;
      } catch (err) {
        scope.dispose();
        rec.status = 'error';
        rec.loadMs = Math.max(0, Math.round((now() - startedAt) * 1000) / 1000);
        rec.lastError = formatError(err);
        if (typeof hostApi.toast === 'function') {
          try { hostApi.toast('[' + id + '] ' + rec.lastError, 'error'); } catch (_) {}
        }
      } finally {
        if (breaker && typeof breaker.markReady === 'function') {
          breaker.markReady(id);
        }
      }

      return Object.assign({}, rec);
    }

    async function loadAll(entries) {
      const list = Array.isArray(entries) ? entries : [];
      for (let i = 0; i < list.length; i += 1) {
        const entry = list[i];
        if (!entry || !entry.id) continue;
        await loadOne(entry);
      }
      return getStatus();
    }

    async function reload(pluginId, rev) {
      const id = String(pluginId);
      const existingScope = activeScopes.get(id);
      if (existingScope) {
        existingScope.dispose();
        activeScopes.delete(id);
      }
      const prevEntry = entriesById.get(id) || { id, name: id };
      const nextEntry = Object.assign({}, prevEntry, { rev });
      const url = pluginModuleUrl(id, rev);
      return loadOne(nextEntry, url);
    }

    function unload(pluginId) {
      const id = String(pluginId);
      const scope = activeScopes.get(id);
      if (scope) {
        scope.dispose();
        activeScopes.delete(id);
      }
      const rec = records.get(id);
      if (rec) {
        rec.status = 'unloaded';
      }
    }

    function disposeAll() {
      for (const scope of activeScopes.values()) {
        try { scope.dispose(); } catch (_) {}
      }
      activeScopes.clear();
      for (const rec of records.values()) {
        if (rec.status === 'active') {
          rec.status = 'unloaded';
        }
      }
    }

    function getStatus() {
      const out = [];
      for (const rec of records.values()) {
        out.push({
          id: rec.id,
          name: rec.name,
          status: rec.status,
          loadMs: rec.loadMs,
          lastError: rec.lastError,
          devMode: rec.devMode,
        });
      }
      return out;
    }

    function getScope(pluginId) {
      return activeScopes.get(String(pluginId)) || null;
    }

    return {
      loadAll,
      reload,
      unload,
      disposeAll,
      getStatus,
      getScope,
    };
  }

  const SykiPluginRuntime = {
    createCircuitBreaker,
    createPluginScope,
    createPluginManager,
    pluginModuleUrl,
    createAbortError,
  };

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = SykiPluginRuntime;
  }
  if (global) {
    global.SykiPluginRuntime = SykiPluginRuntime;
  }
})(typeof window !== 'undefined' ? window : (typeof globalThis !== 'undefined' ? globalThis : this));
