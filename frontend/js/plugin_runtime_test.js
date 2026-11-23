// Unit tests for frontend/js/plugin_runtime.js
// Covers: circuit breaker isolation, scoped proxy auto-cleanup & post-dispose AbortError,
// declarative actions (lock / cancel-prev / allow / update / error guard),
// Shadow DOM CSS isolation, crash isolation across plugins, and zero-leak hot reload.
'use strict';

const assert = require('assert');

global.window = global;
const Runtime = require('./plugin_runtime.js');

function makeFakeStorage() {
  const map = new Map();
  return {
    getItem(k) { return map.has(k) ? map.get(k) : null; },
    setItem(k, v) { map.set(k, String(v)); },
    removeItem(k) { map.delete(k); },
    size() { return map.size; },
  };
}

function makeFakeHost() {
  const actions = new Map();
  const panels = new Map();
  const settings = new Map();
  const listeners = new Map();
  const commands = new Map();
  const tokens = new Map();
  const chrome = new Map();
  const toasts = [];
  const headStyles = [];
  let rpcCalls = 0;
  let seq = 0;

  function makeFakeElement(tag) {
    const el = {
      tagName: tag.toUpperCase(),
      attrs: {},
      children: [],
      textContent: '',
      shadowRoot: null,
      removed: false,
      ownerDocument: {
        head: {
          children: headStyles,
          appendChild(node) { headStyles.push(node); return node; },
        },
        createElement(t) { return makeFakeElement(t); },
      },
      setAttribute(k, v) { this.attrs[k] = v; },
      getAttribute(k) { return this.attrs[k]; },
      appendChild(child) { this.children.push(child); return child; },
      remove() { this.removed = true; },
      attachShadow(opts) {
        if (this.shadowRoot) throw new Error('Shadow root already attached');
        const sr = {
          mode: opts && opts.mode,
          host: this,
          children: [],
          appendChild(child) { this.children.push(child); return child; },
          replaceChildren() { this.children.length = 0; },
        };
        this.shadowRoot = sr;
        return sr;
      },
    };
    return el;
  }

  const hostApi = {
    registerAction(action) {
      actions.set(action.id, Object.assign({}, action));
      return {
        update(snapshot) {
          if (actions.has(action.id)) {
            actions.set(action.id, Object.assign({}, actions.get(action.id), snapshot));
          }
        },
        dispose() {
          actions.delete(action.id);
        },
      };
    },
    mountPanel(desc) {
      const hostEl = makeFakeElement('div');
      panels.set(desc.id, { desc, hostEl });
      return {
        host: hostEl,
        dispose() {
          hostEl.remove();
          panels.delete(desc.id);
        },
      };
    },
    registerSettings(spec) {
      const key = spec.id || ('settings:' + (++seq));
      settings.set(key, spec);
      return function () { settings.delete(key); };
    },
    on(eventName, fn) {
      if (!listeners.has(eventName)) listeners.set(eventName, new Set());
      listeners.get(eventName).add(fn);
      return function () {
        const set = listeners.get(eventName);
        if (set) {
          set.delete(fn);
          if (set.size === 0) listeners.delete(eventName);
        }
      };
    },
    registerCommand(cmd) {
      commands.set(cmd.id, cmd);
      return { dispose() { commands.delete(cmd.id); } };
    },
    setToken(name, value) {
      tokens.set(name, value);
      return function () { tokens.delete(name); };
    },
    setChrome(key, value) {
      chrome.set(key, value);
      return function () { chrome.delete(key); };
    },
    toast(message, level) {
      toasts.push({ message, level });
    },
    async rpc(method, params) {
      rpcCalls += 1;
      return { ok: true, method, params };
    },
    editor: {
      async getText() {
        rpcCalls += 1;
        return 'hello world';
      },
    },
  };

  function emit(eventName, payload) {
    const set = listeners.get(eventName);
    if (!set) return;
    for (const fn of Array.from(set)) {
      fn(payload);
    }
  }

  function counts() {
    let listenerCount = 0;
    for (const set of listeners.values()) listenerCount += set.size;
    return {
      actions: actions.size,
      panels: panels.size,
      settings: settings.size,
      listeners: listenerCount,
      commands: commands.size,
      tokens: tokens.size,
      chrome: chrome.size,
      headStyles: headStyles.length,
      rpcCalls,
    };
  }

  return { hostApi, actions, panels, settings, listeners, commands, tokens, chrome, toasts, headStyles, emit, counts };
}

function deferred() {
  let resolve, reject;
  const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

async function testUMDExport() {
  assert.strictEqual(global.window.SykiPluginRuntime, Runtime);
  assert.strictEqual(typeof Runtime.createCircuitBreaker, 'function');
  assert.strictEqual(typeof Runtime.createPluginScope, 'function');
  assert.strictEqual(typeof Runtime.createPluginManager, 'function');
}

async function testCircuitBreakerAutoIsolation() {
  const storage = makeFakeStorage();
  const breaker1 = Runtime.createCircuitBreaker(storage);

  // Simulate previous run where 'crasher' started and process died before markReady
  assert.strictEqual(breaker1.isTripped('crasher'), false);
  breaker1.markStarting('crasher');
  assert.strictEqual(breaker1.isTripped('crasher'), true);

  // Healthy plugin finished normally
  breaker1.markStarting('healthy');
  breaker1.markReady('healthy');
  assert.strictEqual(breaker1.isTripped('healthy'), false);

  // New boot with same persisted storage
  const breaker2 = Runtime.createCircuitBreaker(storage);
  assert.strictEqual(breaker2.isTripped('crasher'), true);

  const importedUrls = [];
  const host = makeFakeHost();
  const manager = Runtime.createPluginManager({
    breaker: breaker2,
    hostApi: host.hostApi,
    importer: async (url) => {
      importedUrls.push(url);
      return { init() {} };
    },
  });

  const statuses = await manager.loadAll([
    { id: 'crasher', name: 'Crasher Plugin' },
    { id: 'healthy', name: 'Healthy Plugin' },
  ]);

  const crashStatus = statuses.find((s) => s.id === 'crasher');
  const healthyStatus = statuses.find((s) => s.id === 'healthy');
  assert.strictEqual(crashStatus.status, 'isolated');
  assert.ok(crashStatus.lastError && crashStatus.lastError.includes('circuit breaker'));
  assert.strictEqual(healthyStatus.status, 'active');
  assert.strictEqual(importedUrls.length, 1, 'tripped plugin must not be imported');
  assert.ok(importedUrls[0].includes('/plugins/healthy/plugin.js'));

  // Reset allows reload
  breaker2.reset('crasher');
  assert.strictEqual(breaker2.isTripped('crasher'), false);
  const reloaded = await manager.reload('crasher', 2);
  assert.strictEqual(reloaded.status, 'active');
}

async function testScopedProxyAutoCleanupAndPostDisposeAbort() {
  const host = makeFakeHost();
  const scope = Runtime.createPluginScope('demo', host.hostApi);
  const api = scope.api;

  // Plugin registers all 7 resource types and intentionally does NOT manually dispose any of them.
  let eventCount = 0;
  api.registerAction({ id: 'act-1', label: 'Run', run: async () => {} });
  api.mountPanel({ id: 'panel-1', slot: 'side-panel', render() {} });
  api.registerSettings({ id: 'set-1', title: 'Demo Settings' });
  api.on('note:saved', () => { eventCount += 1; });
  api.registerCommand('demo.ping', () => 'pong');
  api.setToken('--demo-accent', '#ff00aa');
  api.setChrome('sidebar.demo', true);

  // Active async API works before dispose
  const rpcRes = await api.rpc('note.list', { limit: 5 });
  assert.strictEqual(rpcRes.ok, true);
  const text = await api.editor.getText();
  assert.strictEqual(text, 'hello world');

  host.emit('note:saved', {});
  assert.strictEqual(eventCount, 1);

  const before = host.counts();
  assert.strictEqual(before.actions, 1);
  assert.strictEqual(before.panels, 1);
  assert.strictEqual(before.settings, 1);
  assert.strictEqual(before.listeners, 1);
  assert.strictEqual(before.commands, 1);
  assert.strictEqual(before.tokens, 1);
  assert.strictEqual(before.chrome, 1);

  // Dispose releases EVERYTHING automatically
  scope.dispose();
  assert.strictEqual(scope.isDisposed(), true);
  assert.strictEqual(scope.signal.aborted, true);
  assert.strictEqual(scope.getResourceCount(), 0);

  const after = host.counts();
  assert.strictEqual(after.actions, 0);
  assert.strictEqual(after.panels, 0);
  assert.strictEqual(after.settings, 0);
  assert.strictEqual(after.listeners, 0);
  assert.strictEqual(after.commands, 0);
  assert.strictEqual(after.tokens, 0);
  assert.strictEqual(after.chrome, 0);

  // Events no longer fire
  host.emit('note:saved', {});
  assert.strictEqual(eventCount, 1);

  // Post-dispose async API calls immediately reject with AbortError without hitting host
  const rpcBefore = host.counts().rpcCalls;
  await assert.rejects(
    () => api.rpc('note.list'),
    (err) => err && err.name === 'AbortError'
  );
  await assert.rejects(
    () => api.editor.getText(),
    (err) => err && err.name === 'AbortError'
  );
  assert.strictEqual(host.counts().rpcCalls, rpcBefore, 'disposed async calls must not reach host');

  // Post-dispose UI registrations are no-ops
  api.registerAction({ id: 'act-2', label: 'Noop', run: async () => {} });
  api.mountPanel({ id: 'panel-2', slot: 'modal', render() {} });
  api.registerSettings({ id: 'set-2', title: 'Noop' });
  api.on('note:saved', () => {});
  api.registerCommand('demo.noop', () => {});
  api.setToken('--noop', '1');
  api.setChrome('noop', true);
  const afterNoop = host.counts();
  assert.strictEqual(afterNoop.actions, 0);
  assert.strictEqual(afterNoop.panels, 0);
  assert.strictEqual(afterNoop.settings, 0);
  assert.strictEqual(afterNoop.listeners, 0);
  assert.strictEqual(afterNoop.commands, 0);
  assert.strictEqual(afterNoop.tokens, 0);
  assert.strictEqual(afterNoop.chrome, 0);
}

async function testDeclarativeActionsConcurrencyAndUpdate() {
  const host = makeFakeHost();
  const scope = Runtime.createPluginScope('act-plugin', host.hostApi);
  const api = scope.api;

  // 1. Default concurrency === 'lock' prevents double-click spam
  const gate1 = deferred();
  let lockRuns = 0;
  const lockHandle = api.registerAction({
    id: 'save-btn',
    label: 'Save',
    icon: 'save',
    badge: null,
    disabled: false,
    run: async () => {
      lockRuns += 1;
      await gate1.promise;
      return 'saved';
    },
  });

  const registeredLock = host.actions.get('save-btn');
  const p1 = registeredLock.run();
  const p2 = registeredLock.run(); // Ignored while locked
  const p3 = registeredLock.run(); // Ignored while locked
  assert.strictEqual(lockRuns, 1, 'lock mode must ignore concurrent double-clicks');
  gate1.resolve();
  assert.strictEqual(await p1, 'saved');
  assert.strictEqual(await p2, undefined);
  assert.strictEqual(await p3, undefined);

  // After unlock, next click runs
  const gate2 = deferred();
  const p4 = registeredLock.run();
  assert.strictEqual(lockRuns, 2);
  gate2.resolve();
  await p4;

  // ActionHandle.update() updates label/icon/badge/disabled
  lockHandle.update({ label: 'Saved!', icon: 'check', badge: '1', disabled: true });
  const updated = host.actions.get('save-btn');
  assert.strictEqual(updated.label, 'Saved!');
  assert.strictEqual(updated.icon, 'check');
  assert.strictEqual(updated.badge, '1');
  assert.strictEqual(updated.disabled, true);

  // Disabled action ignores clicks
  await updated.run();
  assert.strictEqual(lockRuns, 2);

  // 2. concurrency === 'cancel-prev' aborts previous run's signal
  const signals = [];
  const gates = [deferred(), deferred()];
  let callIdx = 0;
  api.registerAction({
    id: 'search-btn',
    label: 'Search',
    concurrency: 'cancel-prev',
    run: async ({ signal }) => {
      const idx = callIdx++;
      signals.push(signal);
      await gates[idx].promise;
      if (signal.aborted) {
        throw Runtime.createAbortError('aborted');
      }
      return 'result-' + idx;
    },
  });

  const searchAction = host.actions.get('search-btn');
  const s1 = searchAction.run();
  assert.strictEqual(signals[0].aborted, false);
  const s2 = searchAction.run();
  assert.strictEqual(signals[0].aborted, true, 'second click must abort first signal');
  assert.strictEqual(signals[1].aborted, false);
  gates[0].resolve();
  gates[1].resolve();
  await s1;
  assert.strictEqual(await s2, 'result-1');
  assert.strictEqual(host.toasts.length, 0, 'cancel-prev abort must not spam error toast');

  // 3. Top-level exception is caught, toasted as 'error', and recorded in error history
  api.registerAction({
    id: 'boom-btn',
    label: 'Boom',
    run: async () => {
      throw new Error('Disk exploded');
    },
  });
  await host.actions.get('boom-btn').run();
  assert.strictEqual(host.toasts.length, 1);
  assert.strictEqual(host.toasts[0].level, 'error');
  assert.ok(host.toasts[0].message.includes('Disk exploded'));
  assert.strictEqual(scope.getErrors().length, 1);
  assert.ok(scope.getErrors()[0].message.includes('Disk exploded'));

  scope.dispose();
  assert.strictEqual(host.counts().actions, 0);
}

async function testShadowDomCssIsolation() {
  const host = makeFakeHost();
  const scope = Runtime.createPluginScope('ui-plugin', host.hostApi);
  const api = scope.api;

  for (const slot of ['side-panel', 'floating', 'modal']) {
    let renderedRoot = null;
    let cleanupRan = false;
    const cssText = '.panel-box { color: red; } /* ' + slot + ' */';

    const handle = api.mountPanel({
      id: 'p-' + slot,
      slot,
      styleCss: cssText,
      render(root) {
        renderedRoot = root;
        const div = root.host.ownerDocument.createElement('div');
        div.textContent = 'content-' + slot;
        root.appendChild(div);
        return () => { cleanupRan = true; };
      },
    });

    assert.ok(handle.shadowRoot, slot + ' must create a ShadowRoot');
    assert.strictEqual(handle.shadowRoot.mode, 'open');
    assert.strictEqual(renderedRoot, handle.shadowRoot, 'render must receive shadowRoot');
    assert.strictEqual(host.headStyles.length, 0, 'styleCss must NEVER leak to document.head');
    assert.strictEqual(handle.host.children.length, 0, 'styleCss must NEVER leak to light DOM');
    assert.strictEqual(handle.shadowRoot.children.length, 2, 'shadowRoot should contain <style> + rendered node');
    assert.strictEqual(handle.shadowRoot.children[0].tagName, 'STYLE');
    assert.strictEqual(handle.shadowRoot.children[0].textContent, cssText);

    handle.dispose();
    assert.strictEqual(cleanupRan, true, 'panel render cleanup must run on dispose');
    assert.strictEqual(handle.shadowRoot.children.length, 0, 'shadowRoot must be cleared on dispose');
    assert.strictEqual(handle.host.removed, true, 'host element must be removed on dispose');
  }

  scope.dispose();
  assert.strictEqual(host.counts().panels, 0);
}

async function testCrashIsolationAndPolicyCheck() {
  const host = makeFakeHost();
  const storage = makeFakeStorage();
  const breaker = Runtime.createCircuitBreaker(storage);
  breaker.markStarting('prev-crash');

  const manager = Runtime.createPluginManager({
    breaker,
    hostApi: host.hostApi,
    policyCheck: async (entry) => {
      if (entry.id === 'blocked-one') {
        return { allowed: false, reason: 'Untrusted origin' };
      }
      return true;
    },
    importer: async (url) => {
      if (url.includes('/plugins/throwing-one/')) {
        return {
          init(api) {
            // Partial registration before throwing must be rolled back!
            api.registerAction({ id: 'partial-act', label: 'Partial', run: () => {} });
            api.setToken('--leaked-token', 'bad');
            throw new Error('Init failed catastrophically');
          },
        };
      }
      if (url.includes('/plugins/good-one/')) {
        return {
          init(api) {
            api.registerAction({ id: 'good-act', label: 'Good', run: () => {} });
          },
        };
      }
      throw new Error('Unexpected URL: ' + url);
    },
  });

  const statuses = await manager.loadAll([
    { id: 'prev-crash', name: 'Previous Crash' },
    { id: 'blocked-one', name: 'Blocked Plugin' },
    { id: 'throwing-one', name: 'Throwing Plugin', devMode: true },
    { id: 'good-one', name: 'Good Plugin', devMode: false },
  ]);

  assert.strictEqual(statuses.length, 4);
  const byId = Object.fromEntries(statuses.map((s) => [s.id, s]));

  assert.strictEqual(byId['prev-crash'].status, 'isolated');
  assert.strictEqual(byId['blocked-one'].status, 'blocked');
  assert.strictEqual(byId['blocked-one'].lastError, 'Untrusted origin');

  assert.strictEqual(byId['throwing-one'].status, 'error');
  assert.strictEqual(byId['throwing-one'].devMode, true);
  assert.ok(byId['throwing-one'].lastError.includes('Init failed catastrophically'));
  assert.strictEqual(typeof byId['throwing-one'].loadMs, 'number');

  assert.strictEqual(byId['good-one'].status, 'active');
  assert.strictEqual(byId['good-one'].lastError, null);
  assert.strictEqual(byId['good-one'].devMode, false);

  // Partial registrations from throwing-one must have been cleaned up; only good-one remains
  assert.strictEqual(host.counts().actions, 1);
  assert.ok(host.actions.has('good-act'));
  assert.strictEqual(host.counts().tokens, 0);

  manager.disposeAll();
  assert.strictEqual(host.counts().actions, 0);
}

async function testHotReloadZeroLeaks() {
  const host = makeFakeHost();
  const importedUrls = [];
  const capturedApis = [];
  let firedEvents = [];

  const manager = Runtime.createPluginManager({
    hostApi: host.hostApi,
    importer: async (url) => {
      importedUrls.push(url);
      const revMatch = url.match(/\/__ext\/r\/([^/]+)\/plugins\/([^/]+)\/plugin\.js$/);
      assert.ok(revMatch, 'reload URL must match /__ext/r/<rev>/plugins/<id>/plugin.js: ' + url);
      const rev = revMatch[1];
      return {
        init(api) {
          capturedApis.push({ rev, api });
          api.registerAction({ id: 'alpha-act', label: 'Alpha r' + rev, run: async () => rev });
          api.mountPanel({ id: 'alpha-panel', slot: 'side-panel', styleCss: '.a{}', render() {} });
          api.registerSettings({ id: 'alpha-set', title: 'Rev ' + rev });
          api.on('tick', () => { firedEvents.push(rev); });
          api.registerCommand('alpha.cmd', () => rev);
          api.setToken('--alpha-rev', rev);
          api.setChrome('alpha.visible', true);
        },
      };
    },
  });

  await manager.loadAll([{ id: 'alpha', name: 'Alpha', rev: 1, devMode: true }]);
  assert.strictEqual(importedUrls[0], '/__ext/r/1/plugins/alpha/plugin.js');

  // Hot-reload across multiple revisions
  for (const rev of [2, 3, 4]) {
    const rec = await manager.reload('alpha', rev);
    assert.strictEqual(rec.status, 'active');
    assert.strictEqual(importedUrls[importedUrls.length - 1], '/__ext/r/' + rev + '/plugins/alpha/plugin.js');

    // Resource counts must stay strictly at 1 per resource type (0 leaks!)
    const c = host.counts();
    assert.strictEqual(c.actions, 1);
    assert.strictEqual(c.panels, 1);
    assert.strictEqual(c.settings, 1);
    assert.strictEqual(c.listeners, 1);
    assert.strictEqual(c.commands, 1);
    assert.strictEqual(c.tokens, 1);
    assert.strictEqual(c.chrome, 1);
  }

  // Only latest rev (4) responds to events
  firedEvents = [];
  host.emit('tick', {});
  assert.deepStrictEqual(firedEvents, ['4']);
  assert.strictEqual(host.actions.get('alpha-act').label, 'Alpha r4');

  // Previous revision APIs are disposed and reject async calls with AbortError
  for (let i = 0; i < 3; i += 1) {
    assert.strictEqual(capturedApis[i].api.signal.aborted, true);
    await assert.rejects(
      () => capturedApis[i].api.rpc('ping'),
      (err) => err && err.name === 'AbortError'
    );
  }
  assert.strictEqual(capturedApis[3].api.signal.aborted, false);

  manager.disposeAll();
  const finalCounts = host.counts();
  assert.strictEqual(finalCounts.actions, 0);
  assert.strictEqual(finalCounts.panels, 0);
  assert.strictEqual(finalCounts.settings, 0);
  assert.strictEqual(finalCounts.listeners, 0);
  assert.strictEqual(finalCounts.commands, 0);
  assert.strictEqual(finalCounts.tokens, 0);
  assert.strictEqual(finalCounts.chrome, 0);
}

(async function runAll() {
  const tests = [
    testUMDExport,
    testCircuitBreakerAutoIsolation,
    testScopedProxyAutoCleanupAndPostDisposeAbort,
    testDeclarativeActionsConcurrencyAndUpdate,
    testShadowDomCssIsolation,
    testCrashIsolationAndPolicyCheck,
    testHotReloadZeroLeaks,
  ];
  for (const fn of tests) {
    await fn();
  }
  console.log('plugin_runtime_test: all ' + tests.length + ' tests passed');
})().catch((err) => {
  console.error('plugin_runtime_test FAILED:', err);
  process.exit(1);
});
