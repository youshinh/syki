'use strict';

const assert = require('assert');
const path = require('path');
const { pathToFileURL } = require('url');

const { createSykiPlatform, installWindowSyki } = require('./syki_bootstrap.js');

const REPO_ROOT = path.resolve(__dirname, '..', '..');

function pluginUrl(relPath) {
  return pathToFileURL(path.join(REPO_ROOT, relPath)).href;
}

function createProgressSpy() {
  const messages = [];
  let doneCalled = false;
  return {
    messages,
    get doneCalled() { return doneCalled; },
    set(msg) { messages.push(msg); },
    done() { doneCalled = true; },
  };
}

function createFakeEditor(initialText = '', initialSelection = '') {
  let text = initialText;
  let selection = initialSelection;
  const inserted = [];
  const openedTabs = [];

  return {
    getText: () => text,
    setText: (next) => { text = next; },
    getSelection: () => selection,
    getSelectionRange: () => ({ from: 0, to: selection.length }),
    getSelectionRect: () => null,
    insert: (chunk) => {
      inserted.push(chunk);
      text += chunk;
    },
    replaceSelection: (replacement) => {
      if (selection && text.includes(selection)) {
        text = text.replace(selection, replacement);
      } else {
        text = replacement;
      }
      selection = replacement;
    },
    replaceRange: (from, to, replacement) => {
      text = text.slice(0, from) + replacement + text.slice(to);
    },
    openTab: (tab) => { openedTabs.push(tab); return tab; },
    getActiveTab: () => ({ id: 'tab-1', title: 'main.md', dirty: false }),
    listTabs: () => [{ id: 'tab-1', title: 'main.md', dirty: false }],
    getTabText: () => text,
    // Inspection helpers
    _setSelection: (next) => { selection = next; },
    _inserted: inserted,
    _openedTabs: openedTabs,
  };
}

function createFakeDom() {
  class FakeElement {
    constructor(tagName) {
      this.tagName = String(tagName).toUpperCase();
      this.className = '';
      this.type = '';
      this.placeholder = '';
      this.textContent = '';
      this.value = '';
      this.hidden = false;
      this.disabled = false;
      this.children = [];
      this._listeners = new Map();
    }
    append(...nodes) {
      for (const n of nodes) this.children.push(n);
    }
    appendChild(node) {
      this.children.push(node);
      return node;
    }
    replaceChildren(...nodes) {
      this.children = [...nodes];
    }
    addEventListener(type, fn) {
      if (!this._listeners.has(type)) this._listeners.set(type, []);
      this._listeners.get(type).push(fn);
    }
    async dispatchEvent(type, eventObj = {}) {
      const listeners = this._listeners.get(type) || [];
      const ev = Object.assign({ preventDefault() {} }, eventObj);
      for (const fn of listeners) {
        await fn(ev);
      }
    }
    findByClass(cls) {
      const stack = [...this.children];
      while (stack.length > 0) {
        const cur = stack.shift();
        if (cur && typeof cur.className === 'string' && cur.className.split(/\s+/).includes(cls)) {
          return cur;
        }
        if (cur && Array.isArray(cur.children)) {
          stack.push(...cur.children);
        }
      }
      return null;
    }
  }

  return {
    createElement: (tag) => new FakeElement(tag),
    createShadowRoot: () => new FakeElement('shadow-root'),
  };
}

async function runTests() {
  const results = [];

  async function test(name, fn) {
    try {
      await fn();
      results.push({ name, ok: true });
      console.log(`PASS: ${name}`);
    } catch (err) {
      results.push({ name, ok: false, err });
      console.error(`FAIL: ${name}\n`, err);
    }
  }

  // 1. Contract & Platform Surface
  await test('createSykiPlatform exposes complete SykiAPI v1 contract and platform controls', async () => {
    const quickCaptureReceived = [];
    const pipeInReceived = [];

    const platform = createSykiPlatform({
      locale: 'ja',
      editorAdapter: createFakeEditor('hello'),
      queryLlm: async (prompt) => `llm:${prompt}`,
      queryDecisionModel: async () => 'flowchart',
      searchSemantic: async () => [],
      recordVoice: async () => 'voice text',
      recordMeeting: async () => 'meeting text',
      queryVision: async () => 'vision desc',
      generateImage: async () => ({ id: 'img-1', mime: 'image/png', size: 12 }),
      renderMermaidOffscreen: async () => ({ id: 'mmd-1', mime: 'image/png', size: 24 }),
      captureScreen: async () => ({ id: 'scr-1', mime: 'image/png', size: 36 }),
    });

    const { syki } = platform;

    assert.strictEqual(syki.apiVersion, 1);
    assert.strictEqual(Object.isFrozen(syki), true);
    assert.strictEqual(Object.isFrozen(syki.capabilities), true);
    assert.deepStrictEqual(
      {
        llm: syki.capabilities.llm,
        decisionModel: syki.capabilities.decisionModel,
        semanticSearch: syki.capabilities.semanticSearch,
        voice: syki.capabilities.voice,
        meeting: syki.capabilities.meeting,
      },
      {
        llm: true,
        decisionModel: true,
        semanticSearch: true,
        voice: true,
        meeting: true,
      }
    );

    // Namespaces & key methods
    assert.strictEqual(typeof syki.editor.getText, 'function');
    assert.strictEqual(typeof syki.knowledge.searchSemantic, 'function');
    assert.strictEqual(typeof syki.exec.llm, 'function');
    assert.strictEqual(typeof syki.exec.shell, 'function');
    assert.strictEqual(typeof syki.exec.pty.open, 'function');
    assert.strictEqual(typeof syki.exec.vision, 'function');
    assert.strictEqual(typeof syki.exec.generateImage, 'function');
    assert.strictEqual(typeof syki.exec.render.mermaidToPng, 'function');
    assert.strictEqual(typeof syki.ui.registerAction, 'function');
    assert.strictEqual(typeof syki.flow.steps, 'function');
    assert.strictEqual(typeof syki.flow.parallel, 'function');
    assert.strictEqual(typeof syki.flow.decide, 'function');
    assert.strictEqual(typeof syki.flow.loop, 'function');
    assert.strictEqual(typeof syki.input.voice, 'function');
    assert.strictEqual(typeof syki.input.meeting, 'function');
    assert.strictEqual(typeof syki.input.captureScreen, 'function');
    assert.strictEqual(syki.sys.locale, 'ja');
    assert.strictEqual(typeof syki.sys.on, 'function');
    assert.strictEqual(typeof syki.sys.onPipeIn, 'function');
    assert.strictEqual(typeof syki.sys.storage.get, 'function');
    assert.strictEqual(typeof syki.sys.storage.set, 'function');

    // Platform extras
    assert.strictEqual(typeof platform.pluginManager.loadAll, 'function');
    assert.strictEqual(typeof platform.dispatchPipeIn, 'function');
    assert.strictEqual(typeof platform.drainQuickCaptureQueue, 'function');
    assert.strictEqual(typeof platform.dispose, 'function');

    // flow.loop integration
    const loopRes = await syki.flow.loop({
      init: 0,
      maxTurns: 5,
      step: async (n) => n + 1,
      until: (n) => n >= 3,
    });
    assert.deepStrictEqual(loopRes, { state: 3, turns: 3, completed: true });

    // sys.onPipeIn + dispatchPipeIn
    const unsubPipe = syki.sys.onPipeIn((ev) => {
      pipeInReceived.push(ev);
      return { handled: true, transformedText: ev.text.toUpperCase() };
    });
    const pipeResult = await platform.dispatchPipeIn({ text: 'piped', source: 'cli' });
    assert.deepStrictEqual(pipeResult, { handled: true, transformedText: 'PIPED' });
    assert.strictEqual(pipeInReceived.length, 1);
    unsubPipe.dispose();

    // drainQuickCaptureQueue -> sys.on('quick-capture:saved')
    const unsubQc = syki.sys.on('quick-capture:saved', (ev) => quickCaptureReceived.push(ev));
    const drainRes = platform.drainQuickCaptureQueue([
      { text: 'memo-1', assetPaths: [], scrapPath: 'scraps/1.md' },
      { text: 'memo-2', assetPaths: [], scrapPath: 'scraps/2.md' },
    ]);
    assert.strictEqual(drainRes.delivered, 2);
    assert.strictEqual(drainRes.failed, 0);
    assert.deepStrictEqual(quickCaptureReceived.map((e) => e.text), ['memo-1', 'memo-2']);
    unsubQc.dispose();

    // installWindowSyki
    const fakeWin = {};
    const installed = installWindowSyki({ locale: 'en' }, fakeWin);
    assert.strictEqual(fakeWin.syki.apiVersion, 1);
    assert.strictEqual(fakeWin.syki.sys.locale, 'en');
    installed.dispose();
    platform.dispose();
  });

  // 2. Real Plugin Integration: translate-bar
  await test('real plugin translate-bar: dynamic import(), init(syki), settings:change, and action run', async () => {
    const editor = createFakeEditor('こんにちは世界\n残りの行', 'こんにちは世界');
    const actions = new Map();
    const toasts = [];
    const llmCalls = [];
    const diffCalls = [];

    const platform = createSykiPlatform({
      editorAdapter: editor,
      registerAction(action) {
        const record = Object.assign({}, action);
        actions.set(action.id, record);
        return {
          id: action.id,
          update(patch) { Object.assign(record, patch); },
          dispose() { actions.delete(action.id); },
        };
      },
      ghostDiff: async (before, after, opts) => {
        diffCalls.push({ before, after, opts });
        return true;
      },
      toast: (msg, level) => toasts.push({ msg, level }),
      queryLlm: async (req) => {
        llmCalls.push(req);
        return 'Hello World';
      },
    });

    const mod = await import(pluginUrl('examples/plugins/translate-bar/plugin.js'));
    const plugin = mod.default;
    assert.strictEqual(plugin.id, 'translate-bar');
    await plugin.init(platform.syki);

    const action = actions.get('translate-selection');
    assert.ok(action, 'translate-selection action must be registered');
    assert.strictEqual(action.badge, 'EN');

    // Emit settings:change -> badge updates to JA
    platform.syki.sys.emit('settings:change', { key: 'targetLang', value: 'ja' });
    assert.strictEqual(action.badge, 'JA');

    // Change back to EN and run translation action
    platform.syki.sys.emit('settings:change', { key: 'targetLang', value: 'en' });
    const progress = createProgressSpy();
    const controller = new AbortController();
    await action.run({ progress, signal: controller.signal });

    assert.strictEqual(llmCalls.length, 1);
    assert.strictEqual(llmCalls[0].input, 'こんにちは世界');
    assert.strictEqual(diffCalls.length, 1);
    assert.strictEqual(diffCalls[0].after, 'Hello World');
    assert.strictEqual(editor.getText(), 'Hello World\n残りの行');
    assert.ok(progress.messages.some((m) => m.includes('EN')));
    assert.ok(toasts.some((t) => t.level === 'ok' && t.msg.includes('EN')));

    platform.dispose();
  });

  // 3. Real Plugin Integration: voice-to-diagram
  await test('real plugin voice-to-diagram: dynamic import(), init(syki), and full voice->mermaid->asset->editor flow', async () => {
    const editor = createFakeEditor('# 議事録\n');
    const actions = new Map();
    const toasts = [];
    const savedAssets = [];
    let renderOptsSeen = null;

    const platform = createSykiPlatform({
      editorAdapter: editor,
      getTheme: () => 'ink',
      registerAction(action) {
        const record = Object.assign({}, action);
        actions.set(action.id, record);
        return {
          id: action.id,
          update(patch) { Object.assign(record, patch); },
          dispose() { actions.delete(action.id); },
        };
      },
      recordVoice: async () => 'クライアントがAPIへリクエストし、DBから結果を返す',
      review: async (text) => `${text}（確認済）`,
      queryDecisionModel: async () => 'sequence',
      queryLlm: async () => '```mermaid\nsequenceDiagram\n  Client->>API: Request\n  API-->>Client: Response\n```',
      renderMermaidOffscreen: async (code, opts) => {
        renderOptsSeen = { code, opts };
        return { id: 'asset-seq-1', mime: 'image/png', size: 128 };
      },
      assets: {
        save: async (asset, ext) => {
          savedAssets.push({ asset, ext });
          return `assets/${asset.id}.${ext}`;
        },
      },
      toast: (msg, level) => toasts.push({ msg, level }),
    });

    const mod = await import(pluginUrl('examples/plugins/voice-to-diagram/plugin.js'));
    const plugin = mod.default;
    assert.strictEqual(plugin.id, 'voice-to-diagram');
    await plugin.init(platform.syki);

    const action = actions.get('run');
    assert.ok(action, 'voice-to-diagram run action must be registered');

    const progress = createProgressSpy();
    await action.run({ progress, signal: new AbortController().signal });

    assert.ok(renderOptsSeen, 'renderMermaidOffscreen must be invoked');
    assert.strictEqual(renderOptsSeen.opts.theme, 'ink');
    assert.strictEqual(savedAssets.length, 1);
    assert.strictEqual(savedAssets[0].ext, 'png');
    assert.ok(editor.getText().includes('![図解 (sequence)](assets/asset-seq-1.png)'));
    assert.ok(editor.getText().includes('sequenceDiagram'));
    assert.ok(toasts.some((t) => t.level === 'ok' && t.msg.includes('sequence')));

    platform.dispose();
  });

  // 4. Real Plugin Integration: local-rag-panel
  await test('real plugin local-rag-panel: dynamic import(), init(syki), panel mount, RAG query, openTab & insert', async () => {
    const fakeDom = createFakeDom();
    const prevDocument = globalThis.document;
    globalThis.document = { createElement: fakeDom.createElement };

    try {
      const editor = createFakeEditor('# 作業メモ');
      const actions = new Map();
      const toasts = [];
      let mountedRoot = null;
      let panelDisposed = false;

      const platform = createSykiPlatform({
        editorAdapter: editor,
        registerAction(action) {
          const record = Object.assign({}, action);
          actions.set(action.id, record);
          return {
            id: action.id,
            update(patch) { Object.assign(record, patch); },
            dispose() { actions.delete(action.id); },
          };
        },
        mountPanel(slot, opts) {
          assert.strictEqual(slot, 'side-panel');
          mountedRoot = fakeDom.createShadowRoot();
          opts.render(mountedRoot);
          return {
            slot,
            dispose() { panelDisposed = true; },
          };
        },
        searchSemantic: async (query) => [
          {
            path: 'notes/arch.md',
            title: 'Architecture Note',
            snippet: `Related to ${query}: syki uses modular APIs.`,
            score: 0.94,
          },
        ],
        notes: {
          read: async (relPath) => `# Full content of ${relPath}`,
        },
        queryLlm: async () => 'syki はモジュラー API 構成を採用しています [1]。',
        toast: (msg, level) => toasts.push({ msg, level }),
      });

      const mod = await import(pluginUrl('examples/plugins/local-rag-panel/plugin.js'));
      const plugin = mod.default;
      assert.strictEqual(plugin.id, 'local-rag-panel');
      await plugin.init(platform.syki);

      const toggleAction = actions.get('toggle-rag-panel');
      assert.ok(toggleAction, 'toggle-rag-panel action must be registered');

      // Open panel
      toggleAction.run({ progress: createProgressSpy(), signal: new AbortController().signal });
      assert.ok(mountedRoot, 'panel shadow root must be rendered');

      const form = mountedRoot.findByClass('rag-form');
      const input = mountedRoot.findByClass('rag-input');
      const answerBody = mountedRoot.findByClass('rag-answer-body');
      const insertBtn = mountedRoot.findByClass('rag-answer-card').children[1];

      input.value = 'API構成';
      await form.dispatchEvent('submit');

      assert.strictEqual(answerBody.textContent, 'syki はモジュラー API 構成を採用しています [1]。');

      // Click hit title -> knowledge.notes.read + editor.openTab
      const hitTitleBtn = mountedRoot.findByClass('rag-hit-title');
      assert.ok(hitTitleBtn, 'hit title button must be rendered');
      await hitTitleBtn.dispatchEvent('click');
      assert.strictEqual(editor._openedTabs.length, 1);
      assert.strictEqual(editor._openedTabs[0].title, 'Architecture Note');
      assert.strictEqual(editor._openedTabs[0].content, '# Full content of notes/arch.md');

      // Click insert button -> editor.insert + toast
      await insertBtn.dispatchEvent('click');
      assert.ok(editor.getText().includes('syki はモジュラー API 構成を採用しています [1]。'));
      assert.ok(toasts.some((t) => t.level === 'ok' && t.msg.includes('挿入')));

      // Toggle again -> disposes panel
      toggleAction.run({ progress: createProgressSpy(), signal: new AbortController().signal });
      assert.strictEqual(panelDisposed, true);

      platform.dispose();
    } finally {
      if (prevDocument === undefined) {
        delete globalThis.document;
      } else {
        globalThis.document = prevDocument;
      }
    }
  });

  // 5. PluginManager lifecycle with all 3 real plugins
  await test('platform.pluginManager.loadAll loads all 3 real plugins via dynamic import() and platform.dispose() unloads them', async () => {
    const platform = createSykiPlatform({
      editorAdapter: createFakeEditor(),
    });

    const statuses = await platform.pluginManager.loadAll([
      { id: 'translate-bar', url: pluginUrl('examples/plugins/translate-bar/plugin.js') },
      { id: 'voice-to-diagram', url: pluginUrl('examples/plugins/voice-to-diagram/plugin.js') },
      { id: 'local-rag-panel', url: pluginUrl('examples/plugins/local-rag-panel/plugin.js') },
    ]);

    assert.strictEqual(statuses.length, 3);
    for (const st of statuses) {
      assert.strictEqual(st.status, 'active', `Plugin ${st.id} failed: ${st.lastError}`);
    }

    platform.dispose();
    const afterDispose = platform.pluginManager.getStatus();
    for (const st of afterDispose) {
      assert.strictEqual(st.status, 'unloaded');
    }
  });

  const failed = results.filter((r) => !r.ok);
  console.log(`\nSummary: ${results.length - failed.length}/${results.length} passed`);
  if (failed.length > 0) {
    process.exitCode = 1;
  }
}

runTests().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});
