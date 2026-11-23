(function (global) {
  'use strict';

  function resolveModules(customModules) {
    const overrides = customModules && typeof customModules === 'object' ? customModules : {};
    const hasRequire = typeof module !== 'undefined' && module.exports && typeof require === 'function';

    const coreMod = overrides.core || (hasRequire ? require('./syki_api_core.js') : global && global.SykiApiCore);
    const flowMod = overrides.flow || (hasRequire ? require('./syki_api_flow.js') : global && global.SykiApiFlow);
    const extMod = overrides.ext || (hasRequire ? require('./syki_api_ext.js') : global && global.SykiApiExt);
    const mediaMod = overrides.media || (hasRequire ? require('./syki_api_media.js') : global && global.SykiApiMedia);
    const runtimeMod = overrides.pluginRuntime || (hasRequire ? require('./plugin_runtime.js') : global && global.SykiPluginRuntime);

    if (!coreMod || typeof coreMod.createSykiCoreApi !== 'function') {
      throw new Error('SykiApiCore module is not available');
    }
    if (!flowMod || typeof flowMod.createSykiFlowApi !== 'function') {
      throw new Error('SykiApiFlow module is not available');
    }
    if (!extMod || typeof extMod.createSykiExtApi !== 'function') {
      throw new Error('SykiApiExt module is not available');
    }
    if (!mediaMod || typeof mediaMod.createSykiMediaApi !== 'function') {
      throw new Error('SykiApiMedia module is not available');
    }
    if (!runtimeMod || typeof runtimeMod.createPluginManager !== 'function') {
      throw new Error('SykiPluginRuntime module is not available');
    }

    return { coreMod, flowMod, extMod, mediaMod, runtimeMod };
  }

  function normalizeTheme(raw) {
    if (raw === 'ink' || raw === 'paper') {
      return raw;
    }
    if (raw && typeof raw === 'object') {
      if (raw.theme === 'ink' || raw.theme === 'paper') return raw.theme;
      if (raw.mode === 'dark') return 'ink';
      if (raw.mode === 'light') return 'paper';
    }
    return 'paper';
  }

  function buildCapabilities(capOverrides, flowDeps, mediaDeps) {
    const ov = capOverrides && typeof capOverrides === 'object' ? capOverrides : {};
    const caps = {
      llm: typeof ov.llm === 'boolean' ? ov.llm : typeof flowDeps.queryLlm === 'function',
      decisionModel: typeof ov.decisionModel === 'boolean'
        ? ov.decisionModel
        : (typeof flowDeps.queryDecision === 'function' || typeof flowDeps.queryDecisionModel === 'function'),
      semanticSearch: typeof ov.semanticSearch === 'boolean'
        ? ov.semanticSearch
        : typeof flowDeps.searchSemantic === 'function',
      voice: typeof ov.voice === 'boolean' ? ov.voice : typeof mediaDeps.recordVoice === 'function',
      meeting: typeof ov.meeting === 'boolean' ? ov.meeting : typeof mediaDeps.recordMeeting === 'function',
    };
    if (typeof ov.vision === 'boolean' || typeof mediaDeps.queryVision === 'function') {
      caps.vision = typeof ov.vision === 'boolean' ? ov.vision : typeof mediaDeps.queryVision === 'function';
    }
    if (typeof ov.imageGen === 'boolean' || typeof mediaDeps.generateImage === 'function') {
      caps.imageGen = typeof ov.imageGen === 'boolean' ? ov.imageGen : typeof mediaDeps.generateImage === 'function';
    }
    if (typeof ov.screenCapture === 'boolean' || typeof mediaDeps.captureScreen === 'function') {
      caps.screenCapture = typeof ov.screenCapture === 'boolean'
        ? ov.screenCapture
        : typeof mediaDeps.captureScreen === 'function';
    }
    return Object.freeze(caps);
  }

  function createDisposableWrapper(fn) {
    if (typeof fn === 'function') {
      const dispose = () => fn();
      dispose.dispose = dispose;
      return dispose;
    }
    if (fn && typeof fn.dispose === 'function') {
      return fn;
    }
    return { dispose() {} };
  }

  function createSykiPlatform(deps) {
    const d = deps && typeof deps === 'object' ? deps : {};
    const { coreMod, flowMod, extMod, mediaMod, runtimeMod } = resolveModules(d.modules);

    const coreDeps = Object.assign({}, d, d.core && typeof d.core === 'object' ? d.core : {});
    const flowDeps = Object.assign({}, d, d.flow && typeof d.flow === 'object' ? d.flow : {});
    if (typeof flowDeps.queryDecision !== 'function' && typeof flowDeps.queryDecisionModel === 'function') {
      flowDeps.queryDecision = flowDeps.queryDecisionModel;
    }
    const extDeps = Object.assign({}, d, d.ext && typeof d.ext === 'object' ? d.ext : {});
    const mediaDeps = Object.assign({}, d, d.media && typeof d.media === 'object' ? d.media : {});

    const defaultPluginId = typeof coreDeps.pluginId === 'string' && coreDeps.pluginId.trim()
      ? coreDeps.pluginId.trim()
      : 'core';

    const core = coreMod.createSykiCoreApi(coreDeps);
    const flowApi = flowMod.createSykiFlowApi(flowDeps);
    const extApi = extMod.createSykiExtApi(extDeps);
    const mediaApi = mediaMod.createSykiMediaApi(mediaDeps);

    const capabilities = buildCapabilities(d.capabilities, flowDeps, mediaDeps);

    // Align UI contract with syki-api.d.ts (single-arg registerSettings, Disposable mountPanel, 'ink'|'paper' theme).
    const ui = Object.assign({}, core.ui, {
      registerSettings(schemaOrPluginId, maybeSchema) {
        if (typeof schemaOrPluginId === 'string' && maybeSchema !== undefined) {
          return core.ui.registerSettings(schemaOrPluginId, maybeSchema);
        }
        return core.ui.registerSettings(defaultPluginId, schemaOrPluginId);
      },

      mountPanel(slot, opts) {
        const handle = core.ui.mountPanel(slot, opts);
        if (handle && typeof handle.dispose === 'function') {
          return handle;
        }
        return Object.assign({}, handle || { slot }, {
          dispose() {
            if (handle && typeof handle.unmount === 'function') {
              handle.unmount();
            }
          },
        });
      },

      getTheme() {
        return normalizeTheme(core.ui.getTheme());
      },

      toast(message, level) {
        const msg = message === null || message === undefined ? '' : String(message);
        const normalizedLevel = (level === 'info' || level === 'ok' || level === 'warn' || level === 'error')
          ? level
          : (level === 'success' ? 'ok' : 'info');
        if (typeof coreDeps.toast === 'function') {
          return coreDeps.toast(msg, normalizedLevel);
        }
        return core.ui.toast(msg, normalizedLevel === 'ok' ? 'success' : normalizedLevel);
      },
    });

    // Align sys contract with syki-api.d.ts (locale, Disposable on(), pipe:in routing, async storage.get with fallback).
    const locale = d.locale === 'en' ? 'en' : 'ja';
    const sys = Object.assign({}, core.sys, {
      locale,

      on(event, handler) {
        if (event === 'pipe:in') {
          return mediaApi.onPipeIn(handler);
        }
        return createDisposableWrapper(core.sys.on(event, handler));
      },

      onPipeIn: mediaApi.onPipeIn,

      storage: {
        async get(key, fallback) {
          const val = await core.sys.storage.get(key);
          return val === undefined ? fallback : val;
        },
        async set(key, value) {
          await core.sys.storage.set(key, value);
        },
      },
    });

    const exec = Object.freeze({
      llm: flowApi.exec.llm,
      shell: flowApi.exec.shell,
      pty: flowApi.exec.pty,
      vision: mediaApi.exec.vision,
      generateImage: mediaApi.exec.generateImage,
      render: mediaApi.exec.render,
    });

    const flow = Object.freeze({
      steps: flowApi.flow.steps,
      parallel: flowApi.flow.parallel,
      decide: flowApi.flow.decide,
      loop: extApi.flow.loop,
    });

    const syki = Object.freeze({
      apiVersion: 1,
      capabilities,
      editor: core.editor,
      knowledge: flowApi.knowledge,
      exec,
      ui,
      flow,
      input: mediaApi.input,
      sys,
    });

    // Bridge object passed to PluginRuntime so scoped plugins can access both namespaced syki.* and runtime hooks.
    const hostApiBridge = Object.assign(
      {
        registerAction: ui.registerAction,
        mountPanel: ui.mountPanel,
        registerSettings: ui.registerSettings,
        on: sys.on,
        registerCommand: sys.registerCommand,
        setToken: ui.setToken,
        setChrome: ui.setChrome,
        toast: ui.toast,
      },
      syki
    );

    const pmOpts = Object.assign(
      {},
      d.pluginManager && typeof d.pluginManager === 'object' ? d.pluginManager : {},
      { hostApi: hostApiBridge }
    );
    const pluginManager = runtimeMod.createPluginManager(pmOpts);

    let disposed = false;

    function drainQuickCaptureQueue(events) {
      return extApi.drainQuickCaptureQueue(events, (eventName, payload) => {
        core.sys.emit(eventName, payload);
      });
    }

    function dispose() {
      if (disposed) return;
      disposed = true;
      pluginManager.disposeAll();
    }

    return {
      syki,
      pluginManager,
      dispatchPipeIn: mediaApi.dispatchPipeIn,
      drainQuickCaptureQueue,
      dispose,
    };
  }

  function installWindowSyki(deps, targetGlobal) {
    const platform = createSykiPlatform(deps);
    const target = targetGlobal || global;
    if (target && typeof target === 'object') {
      Object.defineProperty(target, 'syki', {
        value: platform.syki,
        writable: false,
        enumerable: true,
        configurable: true,
      });
    }
    return platform;
  }

  const exported = {
    createSykiPlatform,
    installWindowSyki,
  };

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = exported;
  }
  if (global) {
    global.SykiBootstrap = exported;
  }
})(typeof window !== 'undefined' ? window : (typeof globalThis !== 'undefined' ? globalThis : this));
