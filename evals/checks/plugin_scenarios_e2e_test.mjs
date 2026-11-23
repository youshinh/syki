/**
 * End-to-End Integration Verification of 15 Real-World Plugin Scenarios
 * Combines the actual implemented modules:
 *   - frontend/js/plugin_runtime.js
 *   - frontend/js/syki_api_core.js
 *   - frontend/js/syki_api_flow.js
 *   - frontend/js/syki_api_ext.js
 *   - frontend/js/syki_api_media.js
 *   - frontend/js/ext_css.js
 *   - frontend/js/ext_theme_sync.js
 *   - frontend/js/backend_proxy.js
 */
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const require = createRequire(import.meta.url);
const PluginRuntime = require('../../frontend/js/plugin_runtime.js');
const ApiCore = require('../../frontend/js/syki_api_core.js');
const ApiFlow = require('../../frontend/js/syki_api_flow.js');
const ApiExt = require('../../frontend/js/syki_api_ext.js');
const ApiMedia = require('../../frontend/js/syki_api_media.js');
const ExtCss = require('../../frontend/js/ext_css.js');
const ExtThemeSync = require('../../frontend/js/ext_theme_sync.js');
const BackendProxy = require('../../frontend/js/backend_proxy.js');

// --- Shared lightweight test harness wiring all real modules together ---
function createIntegratedHost(overrides = {}) {
  const state = {
    tabs: [
      { id: 't1', title: 'Spec.md', path: 'Spec.md', text: 'Initial draft with API_KEY=sk-1234567890abcdef123456' },
      { id: 't2', title: 'ArticleA.md', path: 'ArticleA.md', text: 'Approach A: fast startup, low memory' },
      { id: 't3', title: 'ArticleB.md', path: 'ArticleB.md', text: 'Approach B: heavy runtime, rich plugins' },
    ],
    activeTabId: 't1',
    selection: { from: 0, to: 13 },
    notes: { 'todo.md': '# TODO\n', 'ideas.md': '# Ideas\n' },
    storage: new Map(),
    toasts: [],
    chrome: { toolbar: true, tabs: true, statusbar: true, zen: false },
    immersiveSynced: false,
    detachGuardCount: 0,
    mermaidAppliedTone: 'dark',
    llmCalls: [],
    decisionCalls: [],
    shellCalls: [],
    savedAssets: [],
    clipboardText: '',
    registeredActions: new Map(),
    mountedPanels: [],
  };

  const activeTab = () => state.tabs.find((t) => t.id === state.activeTabId);

  const stripSecrets = (text) =>
    String(text).replace(/sk-[a-zA-Z0-9]{10,}/g, '[REDACTED_SECRET]');

  const core = ApiCore.createSykiCoreApi({
    getLocale: () => overrides.locale || 'ja',
    stripSecrets,
    editorAdapter: {
      getText: () => activeTab().text,
      setText: (t) => { activeTab().text = t; },
      getSelection: () => activeTab().text.slice(state.selection.from, state.selection.to),
      getSelectionRange: () => ({ ...state.selection }),
      getSelectionRect: () => ({ x: 120, y: 240, width: 80, height: 18 }),
      insert: (t) => { activeTab().text += t; },
      replaceSelection: (t) => {
        const cur = activeTab().text;
        activeTab().text = cur.slice(0, state.selection.from) + t + cur.slice(state.selection.to);
      },
      replaceRange: (from, to, t) => {
        const cur = activeTab().text;
        activeTab().text = cur.slice(0, from) + t + cur.slice(to);
      },
      openTab: ({ title, content }) => {
        const id = `t${state.tabs.length + 1}`;
        state.tabs.push({ id, title: title || 'Untitled', text: content });
        state.activeTabId = id;
      },
      getActiveTab: () => {
        const t = activeTab();
        return { id: t.id, title: t.title, path: t.path };
      },
      listTabs: () => state.tabs.map(({ id, title, path: p }) => ({ id, title, path: p })),
      getTabText: (id) => state.tabs.find((t) => t.id === id)?.text ?? null,
    },
    registerAction: (action) => {
      state.registeredActions.set(action.id, { ...action });
      return {
        id: action.id,
        update(patch) { Object.assign(state.registeredActions.get(action.id), patch); },
        dispose() { state.registeredActions.delete(action.id); },
      };
    },
    ghostDiff: async (before, after) => {
      state.usedGhostDiff = true;
      return before !== after;
    },
    modalDiff: async () => {
      state.usedModalDiff = true;
      return true;
    },
    decideModal: async (opts) => {
      const rec = opts.options.find((o) => o.recommended) || opts.options[0];
      return { choiceId: rec.id };
    },
    reviewModal: async (text) => `${text} [reviewed]`,
    toast: (msg, level) => state.toasts.push({ msg, level }),
    getTheme: () => 'ink',
    setChrome: (c) => Object.assign(state.chrome, c),
    syncImmersiveExit: (zen) => { state.immersiveSynced = Boolean(zen); },
    clipboard: {
      read: async () => ({ text: state.clipboardText }),
      writeText: async (t) => { state.clipboardText = t; },
      writeImage: async () => {},
    },
    storage: {
      get: async (k, fb) => (state.storage.has(k) ? state.storage.get(k) : fb),
      set: async (k, v) => { state.storage.set(k, v); },
    },
  });

  const flow = ApiFlow.createSykiFlowApi({
    stripSecrets,
    acquireDetachGuard: () => { state.detachGuardCount++; },
    releaseDetachGuard: () => { state.detachGuardCount--; },
    searchSemantic: async (q) => [{ path: 'past-memo.md', snippet: `Related to ${q}`, score: 0.92 }],
    searchFulltext: async (q) => [{ path: 'Spec.md', snippet: q, score: 1.0 }],
    updateSemanticIndex: async () => ({ files: 3, chunks: 12 }),
    notes: {
      read: async (p) => state.notes[p] ?? '',
      list: async () => Object.keys(state.notes).map((p) => ({ path: p })),
      create: async ({ title, content }) => { state.notes[title] = content; return title; },
      append: async (p, text) => { state.notes[p] = (state.notes[p] || '') + text; },
      update: async (p, text) => { state.notes[p] = text; },
    },
    assets: {
      save: async (asset, ext) => {
        const saved = `assets/img_${state.savedAssets.length + 1}.${ext || 'png'}`;
        state.savedAssets.push({ saved, asset });
        return saved;
      },
    },
    queryLlm: async (reqObj, opts = {}) => {
      const prompt = typeof reqObj === 'string' ? reqObj : (reqObj?.prompt || '');
      const mergedOpts = typeof reqObj === 'string' ? opts : reqObj;
      state.llmCalls.push({ prompt, opts: mergedOpts });
      if (overrides.queryLlm) return overrides.queryLlm(prompt, mergedOpts);
      return `LLM_OUT(${prompt.slice(0, 20)})`;
    },
    queryDecision: async (req) => {
      state.decisionCalls.push(req);
      if (overrides.queryDecision) return overrides.queryDecision(req);
      return { reqId: req.reqId, choiceId: req.choices[0].id, reason: 'decision-model', by: 'decision' };
    },
    runProgram: async (program, args, opts) => {
      state.shellCalls.push({ program, args, opts });
      if (overrides.runProgram) return overrides.runProgram(program, args, opts);
      return { allowed: true, exitCode: 0, stdout: `ok:${program}`, stderr: '' };
    },
    openPty: async () => 'pty-1',
  });

  const ext = ApiExt.createSykiExtApi({
    acquireDetachGuard: () => { state.detachGuardCount++; },
    releaseDetachGuard: () => { state.detachGuardCount--; },
  });

  const media = ApiMedia.createSykiMediaApi({
    acquireDetachGuard: () => { state.detachGuardCount++; },
    releaseDetachGuard: () => { state.detachGuardCount--; },
    recordVoice: async () => 'ユーザーがログインして設定を保存する流れ',
    recordMeeting: async () => '決定事項: Phase 4を完了する',
    captureScreen: async () => ({ id: 'cap-1', url: '/__ext/tmp/cap-1.png', mime: 'image/png', bytes: 4096 }),
    queryVision: async (_asset, prompt) => `VISION(${prompt})`,
    generateImage: async (prompt) => ({ id: `img-${Math.random().toString(36).slice(2, 6)}`, url: '/__ext/tmp/gen.png', mime: 'image/png', bytes: 8192, prompt }),
    renderMermaidOffscreen: async (code, opts) => {
      // Must NOT touch state.mermaidAppliedTone
      return { id: 'mmd-1', url: '/__ext/tmp/mmd-1.png', mime: 'image/png', bytes: code.length * 10, theme: opts.theme };
    },
    renderHtmlToPng: async () => ({ id: 'h-1', url: '/__ext/tmp/h-1.png', mime: 'image/png', bytes: 1024 }),
    renderHtmlToPdf: async () => 'out.pdf',
  });

  return { state, core, flow, ext, media };
}

const results = [];
async function scenario(id, name, fn) {
  await fn();
  results.push({ id, name, ok: true });
  console.log(`PASS [${id}] ${name}`);
}

// ============================================================================
// 15 Real-World Scenarios E2E Verification
// ============================================================================

await scenario('01 (UC#2)', '音声入力 → review → Decisionモデル図種別選定 → Mermaid → オフスクリーンPNG → エディタ挿入', async () => {
  const { state, core, flow, media } = createIntegratedHost({
    queryDecision: async (req) => ({ reqId: req.reqId, choiceId: 'flowchart', by: 'decision' }),
    queryLlm: async () => 'flowchart LR\n  A[Login] --> B[Save]',
  });
  const voice = await media.input.voice();
  const reviewed = await core.ui.review(voice);
  const picked = await flow.flow.decide({
    input: reviewed,
    choices: [{ id: 'flowchart', description: 'flow' }, { id: 'sequence', description: 'seq' }],
    fallback: 'flowchart',
  });
  assert.equal(picked.choiceId, 'flowchart');
  assert.equal(picked.by, 'decision');
  const code = await flow.exec.llm(`Generate ${picked.choiceId}`, { input: reviewed });
  const img = await media.exec.render.mermaidToPng(code, { theme: 'light' });
  assert.equal(state.mermaidAppliedTone, 'dark', 'global mermaidAppliedTone must remain untouched');
  const savedPath = await flow.knowledge.assets.save(img, 'png');
  core.editor.insert(`\n![diagram](${savedPath})\n\`\`\`mermaid\n${code}\n\`\`\``);
  assert.ok(core.editor.getText().includes('![diagram](assets/img_1.png)'));
  assert.equal(state.detachGuardCount, 0);
});

await scenario('02 (UC#16)', '記事タイトル → 3案サムネイル並列生成 (flow.parallel + AssetRef + detachGuard)', async () => {
  const { state, core, flow, media } = createIntegratedHost();
  const title = core.editor.getSelection();
  const imgs = await flow.flow.parallel([
    media.exec.generateImage(`${title} minimal`),
    media.exec.generateImage(`${title} bold`),
    media.exec.generateImage(`${title} editorial`),
  ]);
  assert.equal(imgs.length, 3);
  assert.ok(imgs.every((i) => i.mime === 'image/png' && i.url.startsWith('/__ext/')));
  assert.equal(state.detachGuardCount, 0);
});

await scenario('03 (UC#21/24)', 'ローカルRAG サイドパネル (Shadow DOM CSS隔離 + 意味検索 + secret_stripデフォルトON)', async () => {
  const { state, flow } = createIntegratedHost();
  const shadowChildren = [];
  const fakeHost = {
    attachShadow: () => ({
      appendChild: (el) => shadowChildren.push(el),
      children: shadowChildren,
    }),
  };
  const scope = PluginRuntime.createPluginScope('local-rag', {
    createElement: (tag) => ({ tag, textContent: '' }),
    mountPanel: () => ({ container: fakeHost, dispose: () => {} }),
    toast: () => {},
  });
  scope.api.mountPanel({
    slot: 'side-panel',
    title: 'Local RAG',
    styleCss: '.rag-box { color: var(--text-main); }',
    render: (root) => root.appendChild({ tag: 'div', textContent: 'RAG UI' }),
  });
  assert.equal(shadowChildren.length, 2, 'style + panel content isolated in ShadowRoot');
  const hits = await flow.knowledge.searchSemantic('design');
  assert.equal(hits[0].path, 'past-memo.md');
  await flow.exec.llm('Answer with context', { input: 'API_KEY=sk-1234567890abcdef123456' });
  assert.ok(!state.llmCalls[0].opts.input.includes('sk-1234567890'), 'secret must be stripped by default');
  scope.dispose();
});

await scenario('04 (UC#41)', 'テスト合格まで自己修復ループ (flow.loop + exec.shell + exec.llm → 2ターンで収束)', async () => {
  let runCount = 0;
  const { state, core, flow, ext } = createIntegratedHost({
    runProgram: async () => {
      runCount++;
      return runCount === 1
        ? { allowed: true, exitCode: 1, stdout: '', stderr: 'AssertionError: expected 2' }
        : { allowed: true, exitCode: 0, stdout: '1 passed', stderr: '' };
    },
    queryLlm: async () => 'fixed_code()',
  });
  const res = await ext.flow.loop({
    init: { code: 'broken_code()', exitCode: 1 },
    maxTurns: 3,
    step: async (st, _turn, signal) => {
      const sh = await flow.exec.shell('pytest', ['-q'], { signal });
      if (sh.exitCode === 0) return { code: st.code, exitCode: 0 };
      const patched = await flow.exec.llm('Fix error', { input: sh.stderr, signal });
      return { code: patched, exitCode: 1 };
    },
    until: (st) => st.exitCode === 0,
  });
  assert.equal(res.completed, true);
  assert.equal(res.turns, 2);
  core.editor.replaceSelection(res.state.code);
  assert.ok(core.editor.getText().startsWith('fixed_code()'));
  assert.equal(state.detachGuardCount, 0);
});

await scenario('05 (UC#48)', '保存前シークレット混入スキャナー (file:before-save キャンセル + sys.stripSecrets コピー)', async () => {
  const { state, core } = createIntegratedHost();
  core.sys.on('file:before-save', ({ text }) => {
    if (text.includes('sk-')) return { cancel: true, reason: 'APIキー混入を検出' };
  });
  const check = await core.sys.emitBeforeSave({ path: 'Spec.md', text: core.editor.getText() });
  assert.equal(check.cancel, true);
  const safeCopy = core.sys.stripSecrets(core.editor.getText());
  await core.sys.clipboard.writeText(safeCopy);
  assert.ok(!state.clipboardText.includes('sk-1234567890'));
  assert.ok(state.clipboardText.includes('[REDACTED_SECRET]'));
});

await scenario('06 (UC#51/Combo#25)', '翻訳ミニバー + 設定変更リアクティブ反映 (settings:change → ActionHandle.update → ghost_diff)', async () => {
  const { state, core, flow } = createIntegratedHost({ queryLlm: async () => 'Translated Text' });
  const handle = core.ui.registerAction({ id: 'tr', label: '翻訳(EN)', slot: 'toolbar', run: () => {} });
  const settings = core.ui.registerSettings('translate-bar', [{ key: 'lang', type: 'select', default: 'EN' }]);
  core.sys.on('settings:change', ({ key, value }) => {
    if (key === 'lang') handle.update({ label: `翻訳(${value})` });
  });
  await settings.set('lang', 'ZH');
  assert.equal(state.registeredActions.get('tr').label, '翻訳(ZH)');
  const translated = await flow.exec.llm('Translate', { input: core.editor.getSelection() });
  const ok = await core.ui.diffConfirm(core.editor.getSelection(), translated, { activeEditor: true });
  assert.equal(ok, true);
  assert.equal(state.usedGhostDiff, true);
});

await scenario('07 (UC#61)', '外部パイプ cat log | syki 受信フック + Decision モデル異常検知', async () => {
  const { state, core, flow, media } = createIntegratedHost();
  media.onPipeIn(async ({ text }) => {
    const decision = await flow.flow.decide({
      input: text,
      choices: [{ id: 'normal', description: 'ok' }, { id: 'critical_alert', description: 'fatal' }],
      rule: (s) => (s.includes('FATAL') ? 'critical_alert' : null),
      fallback: 'normal',
    });
    if (decision.choiceId === 'critical_alert') {
      core.ui.toast('FATALログを検出', 'error');
      return { handled: true, transformedText: `[ALERT]\n${text}` };
    }
    return { handled: false };
  });
  const out = await media.dispatchPipeIn({ text: '2026-11-23 FATAL OOM killed', source: 'stdin' });
  assert.equal(out.handled, true);
  assert.equal(state.decisionCalls.length, 0, 'rule short-circuit must use 0 network calls');
  assert.equal(state.toasts[0].level, 'error');
});

await scenario('08 (UC#68/Combo#43)', 'PDFドロップ横取り (file:drop handled) → pdftotext → 要約ノート作成', async () => {
  const { state, core, flow } = createIntegratedHost({
    runProgram: async () => ({ allowed: true, exitCode: 0, stdout: 'Chapter 1: Intro', stderr: '' }),
  });
  core.sys.on('file:drop', async ({ paths }) => {
    if (!paths[0].endsWith('.pdf')) return;
    const sh = await flow.exec.shell('pdftotext', [paths[0], '-']);
    const summary = await flow.exec.llm('Summarize', { input: sh.stdout });
    await flow.knowledge.notes.create({ title: 'pdf-summary.md', content: summary });
    return { handled: true };
  });
  const res = await core.sys.emitFileDrop({ paths: ['C:/docs/paper.pdf'], mimes: ['application/pdf'] });
  assert.equal(res.handled, true);
  assert.ok(state.notes['pdf-summary.md'].includes('LLM_OUT'));
});

await scenario('09 (UC#74/Combo#44)', '複数タブ横断比較 → 新規タブに比較マトリクス生成', async () => {
  const { core, flow } = createIntegratedHost({ queryLlm: async () => '| A | B |\n|---|---|' });
  const tabs = core.editor.listTabs();
  const combined = tabs.map((t) => `# ${t.title}\n${core.editor.getTabText(t.id)}`).join('\n\n');
  const table = await flow.exec.llm('Compare approaches', { input: combined });
  core.editor.openTab({ title: 'Comparison.md', content: table });
  assert.equal(core.editor.getActiveTab().title, 'Comparison.md');
  assert.ok(core.editor.getText().includes('| A | B |'));
});

await scenario('10 (UC#81/Combo#21/30)', '0-fetch Preload / user.css ホットリロード / QcTheme rgb→hex 同期 / 没入モード同期', async () => {
  const headChildren = [];
  const doc = {
    head: { appendChild: (el) => headChildren.push(el) },
    getElementById: (id) => headChildren.find((e) => e.id === id) || null,
    createElement: (tag) => ({ tag, id: '', rel: '', href: '' }),
  };
  // No user.css -> 0 DOM ops
  ExtCss.initUserCss(doc, { ext: { hasUserCss: false, safeMode: false } });
  assert.equal(headChildren.length, 0);
  // With user.css -> initial link + hot reload
  ExtCss.initUserCss(doc, { ext: { hasUserCss: true, safeMode: false } });
  assert.equal(headChildren[0].href, '/__ext/r/1/user.css');
  ExtCss.reloadUserCss(doc, 7);
  assert.equal(headChildren[0].href, '/__ext/r/7/user.css');

  // QcTheme rgb -> hex normalization
  const snap = ExtThemeSync.extractQcThemeSnapshot(
    { getPropertyValue: (k) => (k === '--bg-modal' ? 'rgb(30, 34, 39)' : k === '--accent-color' ? '#556b2f' : '') },
    true,
  );
  assert.equal(snap.bg, '#1e2227');
  assert.equal(snap.btnSave, '#556b2f');

  const { state, core } = createIntegratedHost();
  core.ui.setChrome({ zen: true, toolbar: false, statusbar: false });
  assert.equal(state.chrome.zen, true);
  assert.equal(state.immersiveSynced, true);
});

await scenario('11 (UC#92/Combo#50)', '起動サーキットブレーカー自動隔離 + クラッシュ隔離 + リロード後リーク0', async () => {
  const mem = new Map();
  const breaker = PluginRuntime.createCircuitBreaker({
    getItem: (k) => mem.get(k) ?? null,
    setItem: (k, v) => mem.set(k, v),
    removeItem: (k) => mem.delete(k),
  });
  breaker.markStarting('bad-loop'); // Simulate previous crash before markReady
  assert.equal(breaker.isTripped('bad-loop'), true);

  let disposedCount = 0;
  const mgr = PluginRuntime.createPluginManager({
    breaker,
    policyCheck: async () => ({ allowed: true }),
    importer: async (url) => {
      if (url.includes('good')) {
        return {
          default: {
            id: 'good',
            name: 'Good Plugin',
            apiVersion: 1,
            init: (syki) => { syki.on('tab:change', () => {}); },
          },
        };
      }
      throw new Error('unexpected import');
    },
    hostApi: {
      on: () => ({ dispose: () => { disposedCount++; } }),
      toast: () => {},
    },
  });
  await mgr.loadAll([
    { id: 'bad-loop', url: '/__ext/r/1/plugins/bad-loop/plugin.js' },
    { id: 'good', url: '/__ext/r/1/plugins/good/plugin.js' },
  ]);
  const st = mgr.getStatus();
  assert.equal(st.find((p) => p.id === 'bad-loop').status, 'isolated');
  assert.equal(st.find((p) => p.id === 'good').status, 'active');
  await mgr.reload('good', 2);
  assert.equal(disposedCount, 1, 'old listener must be disposed on reload');
});

await scenario('12 (Combo#12)', 'Detached中 Quick Capture 保留キュー順次配信 → Decision自動トリアージ → ノート追記', async () => {
  const { state, core, flow, ext } = createIntegratedHost({
    queryDecision: async (req) => ({
      reqId: req.reqId,
      choiceId: req.input.includes('修正') ? 'task' : 'idea',
      by: 'decision',
    }),
  });
  core.sys.on('quick-capture:saved', async (ev) => {
    const d = await flow.flow.decide({
      input: ev.text,
      choices: [{ id: 'task', description: 'todo' }, { id: 'idea', description: 'idea' }],
      fallback: 'idea',
    });
    const dest = d.choiceId === 'task' ? 'todo.md' : 'ideas.md';
    await flow.knowledge.notes.append(dest, `- ${ev.text}\n`);
  });
  await ext.drainQuickCaptureQueue(
    [
      { text: 'ログインバグを修正する', assetPaths: [], scrapPath: '2026-11-23.md' },
      { text: '新テーマのアイデア', assetPaths: [], scrapPath: '2026-11-23.md' },
    ],
    (evName, payload) => core.sys.emit(evName, payload),
  );
  // Allow async listeners to settle
  await new Promise((r) => setTimeout(r, 20));
  assert.ok(state.notes['todo.md'].includes('ログインバグを修正する'));
  assert.ok(state.notes['ideas.md'].includes('新テーマのアイデア'));
});

await scenario('13 (Combo#17)', 'Detached直前のポモドーロ状態退避 + ハングプラグインの500ms強制打ち切り', async () => {
  const { state, core } = createIntegratedHost();
  core.sys.on('host:before-detach', async () => {
    await core.sys.storage.set('pomodoro.remaining', 720);
  });
  core.sys.on('host:before-detach', () => new Promise(() => {})); // intentionally hangs forever
  const t0 = Date.now();
  await core.sys.emitBeforeDetach(80); // verify hard-timeout mechanism quickly
  const elapsed = Date.now() - t0;
  assert.ok(elapsed < 400, `must force-complete after timeout, got ${elapsed}ms`);
  assert.equal(await core.sys.storage.get('pomodoro.remaining'), 720);
});

await scenario('14 (Combo#41/JSON-RPC)', '日本語IME変換中ショートカット抑止 + exposeRpc外部実行ガード + reqId並列デマルチプレクサ', async () => {
  const { core } = createIntegratedHost();
  let ran = 0;
  core.sys.registerCommand({ id: 'internal-only', title: 'Internal', shortcut: 'Ctrl+Shift+T', exposeRpc: false, run: () => ++ran });
  core.sys.registerCommand({ id: 'public-rpc', title: 'Public', exposeRpc: true, run: (args) => ({ echoed: args }) });

  // IME composing must suppress shortcut
  assert.equal(core.sys.dispatchShortcut('Ctrl+Shift+T', { isComposing: true }), false);
  assert.equal(ran, 0);
  assert.equal(core.sys.dispatchShortcut('Ctrl+Shift+T', { isComposing: false }), true);
  assert.equal(ran, 1);

  // exposeRpc guard
  await assert.rejects(async () => core.sys.runCommand('internal-only', {}, { viaRpc: true }), /RPC_FORBIDDEN/);
  const rpcRes = await core.sys.runCommand('public-rpc', { q: 'hello' }, { viaRpc: true });
  assert.deepEqual(rpcRes, { echoed: { q: 'hello' } });

  // reqId parallel demux (out-of-order resolution)
  const demux = BackendProxy.createReqIdDemux();
  const p1 = demux.wait('r1', 'llm:decisionResult');
  const p2 = demux.wait('r2', 'llm:decisionResult');
  demux.dispatch('llm:decisionResult', { reqId: 'r2', choiceId: 'B' });
  demux.dispatch('llm:decisionResult', { reqId: 'r1', choiceId: 'A' });
  const [res1, res2] = await Promise.all([p1, p2]);
  assert.equal(res1.choiceId, 'A');
  assert.equal(res2.choiceId, 'B');
});

await scenario('15 (Combo#1/2)', 'ui.decide 多肢選択ゲート + 実機 syki-cli plugin check (正常サンプルPASS / 危険コードFAIL)', async () => {
  const { core } = createIntegratedHost();
  const pick = await core.ui.decide({
    title: '出力先を選択',
    options: [
      { id: 'insert', label: 'カーソル挿入' },
      { id: 'new-tab', label: '新規タブ', recommended: true },
    ],
  });
  assert.equal(pick.choiceId, 'new-tab');

  // Run real compiled syki-cli.exe plugin check against valid sample and malicious temp plugin
  const cliBin = path.resolve('target/debug/syki-cli.exe');
  const okOut = JSON.parse(execFileSync(cliBin, ['plugin', 'check', 'examples/plugins/voice-to-diagram/plugin.js'], { encoding: 'utf8' }));
  assert.equal(okOut.ok, true);

  const badPath = path.join(os.tmpdir(), `syki-bad-plugin-${Date.now()}.js`);
  fs.writeFileSync(badPath, 'export default { id: "evil", init() { window.chrome.webview.postMessage("x"); fetch("https://evil.example"); } };');
  try {
    let failed = false;
    try {
      execFileSync(cliBin, ['plugin', 'check', badPath], { encoding: 'utf8' });
    } catch (err) {
      failed = true;
      const report = JSON.parse(err.stdout);
      assert.equal(report.ok, false);
      assert.ok(report.errors.length >= 2);
    }
    assert.equal(failed, true, 'malicious plugin must fail syki-cli plugin check');
  } finally {
    fs.rmSync(badPath, { force: true });
  }
});

console.log(`\n=== ALL ${results.length}/15 REAL-WORLD SCENARIOS PASSED ===`);
