// Unit tests for syki_api_core.js (Core v1 API: editor / ui / sys).
'use strict';

const assert = require('assert');
const SykiApiCore = require('./syki_api_core.js');

assert.strictEqual(typeof SykiApiCore.createSykiCoreApi, 'function', 'module.exports exposes createSykiCoreApi');
assert.strictEqual(globalThis.SykiApiCore, SykiApiCore, 'UMD exposes globalThis.SykiApiCore');

const { createSykiCoreApi } = SykiApiCore;

const tests = [];
function test(name, fn) {
  tests.push({ name, fn });
}

// 1. editor delegation & safe range clamping/validation
test('editor: delegates all methods and clamps invalid/inverted/negative ranges', () => {
  let buf = 'hello world';
  const calls = [];
  const tabs = [{ id: 'tab-1', title: 'Note 1', content: 'hello world' }];

  const api = createSykiCoreApi({
    editorAdapter: {
      getText: () => buf,
      setText: (t) => { buf = t; calls.push(['setText', t]); },
      getSelection: () => 'world',
      getSelectionRange: () => ({ from: 100, to: -5 }),
      getSelectionRect: () => ({ x: 10, y: 20, width: 50, height: 16 }),
      insert: (t) => { calls.push(['insert', t]); },
      replaceSelection: (t) => { calls.push(['replaceSelection', t]); },
      replaceRange: (from, to, text) => {
        calls.push(['replaceRange', from, to, text]);
        buf = buf.slice(0, from) + text + buf.slice(to);
      },
      openTab: (opts) => {
        const tab = { id: 'tab-2', title: opts.title, content: opts.content };
        tabs.push(tab);
        return tab;
      },
      getActiveTab: () => tabs[0],
      listTabs: () => tabs.slice(),
      getTabText: (id) => (tabs.find((t) => t.id === id) || { content: '' }).content
    }
  });

  assert.strictEqual(api.editor.getText(), 'hello world');
  assert.strictEqual(api.editor.getSelection(), 'world');
  // getSelectionRange clamps negative and > length, and orders from <= to
  assert.deepStrictEqual(api.editor.getSelectionRange(), { from: 0, to: 11 });
  assert.deepStrictEqual(api.editor.getSelectionRect(), { x: 10, y: 20, width: 50, height: 16 });

  api.editor.insert('!');
  api.editor.replaceSelection('syki');
  // Negative & inverted range: from=5, to=-3 -> clamped to [0, 5]
  api.editor.replaceRange(5, -3, 'HI');
  assert.strictEqual(buf, 'HI world');
  // Overflowing & inverted range: from=999, to=3 -> clamped to [3, 8]
  api.editor.replaceRange(999, 3, 'ALL');
  assert.strictEqual(buf, 'HI ALL');

  const newTab = api.editor.openTab({ title: 'New', content: 'abc' });
  assert.strictEqual(newTab.id, 'tab-2');
  assert.strictEqual(api.editor.getActiveTab().id, 'tab-1');
  assert.strictEqual(api.editor.listTabs().length, 2);
  assert.strictEqual(api.editor.getTabText('tab-2'), 'abc');

  api.editor.setText('reset');
  assert.strictEqual(api.editor.getText(), 'reset');
});

// 2. IME composition shortcut auto-suppression & shortcut collision protection
test('sys.dispatchShortcut: suppresses during Japanese IME composition (isComposing / keyCode 229)', () => {
  const ran = [];
  const toasts = [];
  const api = createSykiCoreApi({
    toast: (msg, level) => toasts.push({ msg, level })
  });

  api.sys.registerCommand({
    id: 'cmd.bold',
    title: 'Bold',
    shortcut: 'Ctrl+B',
    run: () => ran.push('bold')
  });

  // IME composing via isComposing === true -> MUST suppress
  assert.strictEqual(api.sys.dispatchShortcut('Ctrl+B', { isComposing: true, keyCode: 66 }), false);
  // IME composing via keyCode === 229 -> MUST suppress
  assert.strictEqual(api.sys.dispatchShortcut('Ctrl+B', { isComposing: false, keyCode: 229 }), false);
  assert.strictEqual(ran.length, 0);

  // Normal dispatch works (case-insensitive normalization)
  assert.strictEqual(api.sys.dispatchShortcut('ctrl+b', { isComposing: false, keyCode: 66 }), true);
  assert.deepStrictEqual(ran, ['bold']);

  // Shortcut collision warns via toast and does NOT overwrite existing binding
  const h2 = api.sys.registerCommand({
    id: 'cmd.other',
    title: 'Other',
    shortcut: 'ctrl+b',
    run: () => ran.push('other')
  });
  assert.strictEqual(h2.shortcutRegistered, false);
  assert.strictEqual(toasts.length, 1);
  assert.strictEqual(toasts[0].level, 'warn');

  assert.strictEqual(api.sys.dispatchShortcut('Ctrl+B', { isComposing: false, keyCode: 66 }), true);
  assert.deepStrictEqual(ran, ['bold', 'bold']);
});

// 3. host:before-detach 500ms hard timeout forced cutoff
test('sys.emitBeforeDetach: enforces 500ms hard timeout against hanging handlers', async () => {
  const api = createSykiCoreApi();
  let fastDone = false;

  api.sys.on('host:before-detach', async () => {
    await new Promise((r) => setTimeout(r, 15));
    fastDone = true;
  });
  // Hanging handler that never resolves
  api.sys.on('host:before-detach', () => new Promise(() => {}));

  const t0 = Date.now();
  const res = await api.sys.emitBeforeDetach(); // default 500ms
  const elapsed = Date.now() - t0;

  assert.strictEqual(res.timedOut, true);
  assert.strictEqual(fastDone, true);
  assert.strictEqual(res.completed, 1);
  assert.strictEqual(res.total, 2);
  assert.ok(elapsed >= 450 && elapsed < 900, 'expected ~500ms cutoff, got ' + elapsed + 'ms');
});

test('sys.emitBeforeDetach: resolves immediately when all handlers finish quickly', async () => {
  const api = createSykiCoreApi();
  api.sys.on('host:before-detach', async () => {
    await new Promise((r) => setTimeout(r, 10));
  });
  api.sys.on('host:before-detach', () => 'sync-ok');

  const t0 = Date.now();
  const res = await api.sys.emitBeforeDetach(500);
  const elapsed = Date.now() - t0;

  assert.strictEqual(res.timedOut, false);
  assert.strictEqual(res.completed, 2);
  assert.strictEqual(res.total, 2);
  assert.ok(elapsed < 200, 'should not wait for 500ms timeout, got ' + elapsed + 'ms');
});

// 4. file:before-save cancellation
test('sys.emitBeforeSave: cancels save when any handler returns { cancel: true, reason }', async () => {
  const api = createSykiCoreApi();

  api.sys.on('file:before-save', () => ({ cancel: false }));
  let res = await api.sys.emitBeforeSave({ path: 'a.md' });
  assert.strictEqual(res.cancel, false);
  assert.strictEqual(res.cancelled, false);

  api.sys.on('file:before-save', async () => ({ cancel: true, reason: 'secret detected' }));
  res = await api.sys.emitBeforeSave({ path: 'a.md' });
  assert.strictEqual(res.cancel, true);
  assert.strictEqual(res.cancelled, true);
  assert.strictEqual(res.reason, 'secret detected');
});

// 5. file:drop default suppression
test('sys.emitFileDrop: suppresses default behavior when any handler returns { handled: true }', async () => {
  const api = createSykiCoreApi();

  const unsub1 = api.sys.on('file:drop', () => ({ handled: false }));
  let res = await api.sys.emitFileDrop({ files: ['a.png'] });
  assert.strictEqual(res.handled, false);
  assert.strictEqual(res.preventDefault, false);

  api.sys.on('file:drop', async () => ({ handled: true }));
  res = await api.sys.emitFileDrop({ files: ['a.png'] });
  assert.strictEqual(res.handled, true);
  assert.strictEqual(res.preventDefault, true);
  unsub1();
});

// 6. registerSettings -> emits settings:change & auto-persists to sys.storage.set
test('ui.registerSettings: emits settings:change and persists via namespaced sys.storage.set', () => {
  const persisted = new Map();
  const events = [];

  const api = createSykiCoreApi({
    pluginId: 'host-default',
    storage: {
      get: (k) => persisted.get(k),
      set: (k, v) => persisted.set(k, v)
    }
  });

  api.sys.on('settings:change', (ev) => events.push(ev));

  const settings = api.ui.registerSettings('my-plugin', {
    fontSize: { type: 'number', default: 14 },
    autoWrap: { type: 'boolean', default: true }
  });

  assert.strictEqual(settings.get('fontSize'), 14);
  assert.strictEqual(events.length, 0);

  settings.set('fontSize', 18);

  assert.strictEqual(settings.get('fontSize'), 18);
  assert.strictEqual(events.length, 1);
  assert.strictEqual(events[0].pluginId, 'my-plugin');
  assert.strictEqual(events[0].key, 'fontSize');
  assert.strictEqual(events[0].prevValue, 14);
  assert.strictEqual(events[0].value, 18);
  assert.deepStrictEqual(persisted.get('syki:plugin:my-plugin:settings'), {
    fontSize: 18,
    autoWrap: true
  });

  // No-op when setting identical value
  settings.set('fontSize', 18);
  assert.strictEqual(events.length, 1);
});

// 7. exposeRpc guard on sys.runCommand
test('sys.runCommand: enforces exposeRpc guard with RPC_FORBIDDEN when viaRpc is true', () => {
  const api = createSykiCoreApi();

  api.sys.registerCommand({
    id: 'internal.only',
    title: 'Internal',
    exposeRpc: false,
    run: (args) => 'internal:' + args
  });
  api.sys.registerCommand({
    id: 'public.rpc',
    title: 'Public',
    exposeRpc: true,
    run: (args) => 'public:' + args
  });

  assert.strictEqual(api.sys.runCommand('internal.only', 'ok'), 'internal:ok');
  assert.strictEqual(api.sys.runCommand('public.rpc', 'ok', { viaRpc: true }), 'public:ok');

  assert.throws(
    () => api.sys.runCommand('internal.only', 'nope', { viaRpc: true }),
    (err) => err && err.code === 'RPC_FORBIDDEN' && /RPC_FORBIDDEN/.test(err.message)
  );
});

// 8. ui.decide (empty options -> immediate null; non-empty -> decideModal)
test('ui.decide: returns null immediately on empty options and delegates non-empty to decideModal', async () => {
  let modalCalls = 0;
  const api = createSykiCoreApi({
    decideModal: async (req) => {
      modalCalls += 1;
      return { selected: req.options[0], customText: null };
    }
  });

  const emptyRes = await api.ui.decide({
    title: 'Pick one',
    question: 'Which?',
    options: []
  });
  assert.strictEqual(emptyRes, null);
  assert.strictEqual(modalCalls, 0);

  const validRes = await api.ui.decide({
    title: 'Pick one',
    question: 'Which?',
    context: 'ctx',
    options: ['A', 'B'],
    allowCustom: true
  });
  assert.deepStrictEqual(validRes, { selected: 'A', customText: null });
  assert.strictEqual(modalCalls, 1);
});

// 9. ui.diffConfirm (active editor -> ghostDiff priority; otherwise -> modalDiff fallback)
test('ui.diffConfirm: prioritizes ghostDiff for active editor and falls back to modalDiff otherwise', async () => {
  const log = [];
  const api = createSykiCoreApi({
    editorAdapter: {
      getActiveTab: () => ({ id: 'tab-active' })
    },
    ghostDiff: async (b, a) => { log.push('ghost'); return { mode: 'ghost', before: b, after: a }; },
    modalDiff: async (b, a) => { log.push('modal'); return { mode: 'modal', before: b, after: a }; }
  });

  // Default (active editor) -> ghostDiff
  const r1 = await api.ui.diffConfirm('old', 'new');
  assert.strictEqual(r1.mode, 'ghost');

  // Matching active tabId -> ghostDiff
  const r2 = await api.ui.diffConfirm('old', 'new', { tabId: 'tab-active' });
  assert.strictEqual(r2.mode, 'ghost');

  // Different tabId -> modalDiff fallback
  const r3 = await api.ui.diffConfirm('old', 'new', { tabId: 'tab-other' });
  assert.strictEqual(r3.mode, 'modal');

  // Non-editor target -> modalDiff fallback
  const r4 = await api.ui.diffConfirm('old', 'new', { target: 'file' });
  assert.strictEqual(r4.mode, 'modal');

  // Without ghostDiff injected -> modalDiff fallback even for active editor
  const apiNoGhost = createSykiCoreApi({
    modalDiff: async () => ({ mode: 'modal-only' })
  });
  const r5 = await apiNoGhost.ui.diffConfirm('a', 'b');
  assert.strictEqual(r5.mode, 'modal-only');
  assert.deepStrictEqual(log, ['ghost', 'ghost', 'modal', 'modal']);
});

// 10. sys.stripSecrets (delegation + safe fallback)
test('sys.stripSecrets: delegates to deps.stripSecrets and masks safely when uninjected', () => {
  const apiDelegated = createSykiCoreApi({
    stripSecrets: (t) => 'DELEGATED:' + t
  });
  assert.strictEqual(apiDelegated.sys.stripSecrets('abc'), 'DELEGATED:abc');

  const apiFallback = createSykiCoreApi();
  const raw = 'url=https://user:pass@example.com/v1?api_key=SECRET123&x=1 token=my_secret_val Bearer eyJhbGciOiJIUzI1NiJ9 sk-abcdefghijklmno1234';
  const cleaned = apiFallback.sys.stripSecrets(raw);
  assert.ok(!cleaned.includes('user:pass@'), 'userinfo stripped');
  assert.ok(!cleaned.includes('SECRET123'), 'query api_key redacted');
  assert.ok(!cleaned.includes('my_secret_val'), 'token redacted');
  assert.ok(!cleaned.includes('eyJhbGciOiJIUzI1NiJ9'), 'bearer token redacted');
  assert.ok(!cleaned.includes('sk-abcdefghijklmno1234'), 'sk- token redacted');
  assert.ok(cleaned.includes('x=1'), 'non-secret params preserved');
});

// 11. ui.mountPanel, ui.setToken, ui.setChrome, ui.registerAction, sys.locale, strict event bus, storage/clipboard/pickFile
test('ui & sys contracts: panel slots, CSS token prefix, chrome sync, locale, strict events, storage namespace', async () => {
  let localeVal = 'ja-JP';
  const chromeLog = [];
  const immersiveLog = [];
  const store = new Map();

  const api = createSykiCoreApi({
    pluginId: 'alpha',
    getLocale: () => localeVal,
    setChrome: (c) => chromeLog.push(c),
    syncImmersiveExit: (s) => immersiveLog.push(s),
    storage: {
      get: (k) => store.get(k),
      set: (k, v) => store.set(k, v)
    },
    clipboard: {
      read: async () => 'clip-data',
      writeText: async (t) => 'wrote:' + t,
      writeImage: async () => true
    },
    pickFile: async (opts) => ({ path: '/tmp/a.md', opts })
  });

  // mountPanel whitelist
  assert.strictEqual(api.ui.mountPanel('side-panel', {}).slot, 'side-panel');
  assert.strictEqual(api.ui.mountPanel('floating', {}).slot, 'floating');
  assert.strictEqual(api.ui.mountPanel('modal', {}).slot, 'modal');
  assert.throws(() => api.ui.mountPanel('statusbar', {}), (e) => e.code === 'INVALID_PANEL_SLOT');

  // setToken requires -- prefix
  api.ui.setToken('--syki-accent', '#00f');
  assert.throws(() => api.ui.setToken('color', '#00f'), (e) => e.code === 'INVALID_TOKEN_NAME');
  assert.throws(() => api.ui.setToken('--', '#00f'), (e) => e.code === 'INVALID_TOKEN_NAME');

  // setChrome syncs with immersive_exit
  api.ui.setChrome({ toolbar: false, tabs: false, statusbar: false, zen: true });
  assert.deepStrictEqual(chromeLog[0], { toolbar: false, tabs: false, statusbar: false, zen: true });
  assert.strictEqual(immersiveLog[0].zen, true);

  // registerAction returns ActionHandle
  const actionHandle = api.ui.registerAction({ id: 'act-1', label: 'Run' });
  assert.strictEqual(actionHandle.id, 'act-1');
  actionHandle.dispose();

  // locale normalization
  assert.strictEqual(api.sys.locale, 'ja');
  localeVal = 'en-US';
  assert.strictEqual(api.sys.locale, 'en');

  // strict event bus rejects unknown events
  assert.throws(() => api.sys.on('unknown:event', () => {}), (e) => e.code === 'UNKNOWN_EVENT');

  // namespaced storage, clipboard, pickFile
  api.sys.storage.set('draft', 'hello');
  assert.strictEqual(store.get('syki:plugin:alpha:draft'), 'hello');
  assert.strictEqual(api.sys.storage.get('draft'), 'hello');
  assert.strictEqual(await api.sys.clipboard.read(), 'clip-data');
  assert.strictEqual(await api.sys.clipboard.writeText('hi'), 'wrote:hi');
  assert.strictEqual(await api.sys.clipboard.writeImage(new Uint8Array([1])), true);
  assert.strictEqual((await api.sys.pickFile({ multiple: false })).path, '/tmp/a.md');
});

// ---- Runner ----
(async function run() {
  let passed = 0;
  let failed = 0;
  for (const t of tests) {
    try {
      await t.fn();
      console.log('PASS:', t.name);
      passed += 1;
    } catch (err) {
      console.error('FAIL:', t.name);
      console.error(err && err.stack ? err.stack : err);
      failed += 1;
    }
  }
  console.log(`\nSummary: ${passed} passed, ${failed} failed (total ${tests.length})`);
  if (failed > 0) process.exit(1);
})();
