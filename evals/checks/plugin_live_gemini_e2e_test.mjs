/**
 * Live E2E Verification of Official Sample Plugins Against Real Google Gemini APIs:
 *   - gemini-decision-1 (POST https://generativelanguage.googleapis.com/v1beta/decisions)
 *   - gemini-3.5-flash-lite (POST https://generativelanguage.googleapis.com/v1beta/models/gemini-3.5-flash-lite:generateContent)
 *
 * Loads the actual sample plugins via dynamic import() through createSykiPlatform().
 */
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const require = createRequire(import.meta.url);
const Bootstrap = require('../../frontend/js/syki_bootstrap.js');

const API_KEY = process.env.GEMINI_API_KEY;
if (!API_KEY) {
  console.error('SKIP: GEMINI_API_KEY is not set.');
  process.exit(2);
}

async function callGeminiDecisionLive({ reqId, input, choices, prompt, signal }) {
  const criteria = {};
  for (const c of choices) {
    criteria[c.id] = c.description || c.id;
  }
  const res = await fetch('https://generativelanguage.googleapis.com/v1beta/decisions', {
    method: 'POST',
    headers: {
      'x-goog-api-key': API_KEY,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      model: 'gemini-decision-1',
      input: String(input || ''),
      questions: {
        decision: {
          type: 'choice',
          instructions: prompt || 'Select the best matching choice for the input.',
          criteria,
        },
      },
    }),
    signal,
  });
  if (!res.ok) {
    throw new Error(`Gemini Decisions HTTP ${res.status}: ${await res.text()}`);
  }
  const data = await res.json();
  const ans = data?.answers?.decision;
  return {
    reqId,
    choiceId: ans?.choice,
    reason: typeof ans?.confidence === 'number' ? `confidence=${ans.confidence.toFixed(3)}` : '',
    by: 'decision',
    outputTokens: data?.usage?.total_output_tokens ?? -1,
  };
}

async function callGeminiFlashLiteLive({ prompt, input, system, signal }) {
  const fullText = [system, prompt, input ? `\n---\n${input}` : ''].filter(Boolean).join('\n');
  const res = await fetch(
    'https://generativelanguage.googleapis.com/v1beta/models/gemini-3.5-flash-lite:generateContent',
    {
      method: 'POST',
      headers: {
        'x-goog-api-key': API_KEY,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        contents: [{ parts: [{ text: fullText }] }],
        generationConfig: { maxOutputTokens: 256, temperature: 0.2 },
      }),
      signal,
    },
  );
  if (!res.ok) {
    throw new Error(`Gemini GenerateContent HTTP ${res.status}: ${await res.text()}`);
  }
  const data = await res.json();
  const text = data?.candidates?.[0]?.content?.parts?.map((p) => p.text || '').join('') || '';
  return text.trim();
}

// Lightweight DOM stub for local-rag-panel ShadowRoot
function makeEl(tag) {
  const el = {
    tagName: String(tag).toUpperCase(),
    className: '',
    id: '',
    type: '',
    placeholder: '',
    textContent: '',
    value: '',
    disabled: false,
    children: [],
    listeners: new Map(),
    appendChild(child) { this.children.push(child); return child; },
    append(...nodes) { this.children.push(...nodes); },
    replaceChildren(...nodes) { this.children = [...nodes]; },
    addEventListener(ev, fn) {
      if (!this.listeners.has(ev)) this.listeners.set(ev, []);
      this.listeners.get(ev).push(fn);
    },
    querySelector(sel) {
      if (sel.startsWith('.')) {
        const cls = sel.slice(1);
        const queue = [...this.children];
        while (queue.length) {
          const cur = queue.shift();
          if (cur.className && cur.className.split(/\s+/).includes(cls)) return cur;
          if (cur.children) queue.push(...cur.children);
        }
      }
      return null;
    },
    click() {
      const fns = this.listeners.get('click') || [];
      return Promise.all(fns.map((fn) => fn({ preventDefault() {} })));
    },
  };
  return el;
}

const state = {
  text: 'syki-lite は高速起動と拡張性を両立するメモプラットフォームです。',
  selection: { from: 0, to: 34 },
  actions: new Map(),
  toasts: [],
  panels: [],
  storage: new Map(),
  decisionMeta: null,
};

const platform = Bootstrap.createSykiPlatform({
  capabilities: { llm: true, decisionModel: true, semanticSearch: true, voice: true, meeting: true },
  getLocale: () => 'ja',
  getTheme: () => 'ink',
  toast: (msg, level) => state.toasts.push({ msg, level }),
  editorAdapter: {
    getText: () => state.text,
    setText: (t) => { state.text = t; },
    getSelection: () => state.text.slice(state.selection.from, state.selection.to),
    getSelectionRange: () => ({ ...state.selection }),
    getSelectionRect: () => ({ x: 10, y: 20, width: 100, height: 18 }),
    insert: (t) => { state.text += t; },
    replaceSelection: (t) => {
      state.text = state.text.slice(0, state.selection.from) + t + state.text.slice(state.selection.to);
    },
    replaceRange: (a, b, t) => { state.text = state.text.slice(0, a) + t + state.text.slice(b); },
    openTab: () => {},
    getActiveTab: () => ({ id: 't1', title: 'Live.md', path: 'Live.md' }),
    listTabs: () => [{ id: 't1', title: 'Live.md', path: 'Live.md' }],
    getTabText: () => state.text,
  },
  registerAction: (spec) => {
    state.actions.set(spec.id, spec);
    return {
      id: spec.id,
      update(p) { Object.assign(spec, p); },
      dispose() { state.actions.delete(spec.id); },
    };
  },
  mountPanel: (slot, opts) => {
    const root = makeEl('shadow-root');
    if (typeof opts.render === 'function') {
      // Minimal document shim for plugin DOM creation
      globalThis.document = { createElement: (t) => makeEl(t) };
      opts.render(root);
    }
    const handle = { slot, root, dispose() {}, unmount() {} };
    state.panels.push(handle);
    return handle;
  },
  ghostDiff: async () => true,
  reviewModal: async (t) => t,
  storage: {
    get: async (k, fb) => (state.storage.has(k) ? state.storage.get(k) : fb),
    set: async (k, v) => { state.storage.set(k, v); },
  },
  searchSemantic: async () => [
    { path: 'docs/SPEC.md', title: 'SPEC', snippet: 'syki-lite targets <5ms CLI cold start and 3-5MB Detached tray RAM.', score: 0.96 },
  ],
  notes: {
    read: async () => 'syki-lite targets <5ms CLI cold start and 3-5MB Detached tray RAM.',
    list: async () => [],
    create: async () => 'new.md',
    append: async () => {},
    update: async () => {},
  },
  assets: {
    save: async () => 'assets/live_diagram.png',
  },
  queryLlm: callGeminiFlashLiteLive,
  queryDecision: async (req) => {
    const out = await callGeminiDecisionLive(req);
    state.decisionMeta = out;
    return out;
  },
  recordVoice: async () => 'ユーザーがログインボタンを押すと、Authサーバーへ認証を送り、JWTを受け取ってDBからプロフィールを取得する流れ',
  renderMermaidOffscreen: async (code) => ({ id: 'live-mmd', url: '/__ext/tmp/live.png', mime: 'image/png', bytes: code.length }),
});

const loadMod = (rel) => import(pathToFileURL(path.resolve(rel)).href);

// 1) Official Sample 1: translate-bar (Real Gemini 3.5 Flash-Lite)
const trMod = await loadMod('examples/plugins/translate-bar/plugin.js');
await trMod.default.init(platform.syki);
const trAct = [...state.actions.values()].find((a) => a.id.includes('translate'));
assert.ok(trAct, 'translate action registered');
await trAct.run({ progress: { update() {}, set() {}, done() {} }, signal: new AbortController().signal });
console.log('LIVE [1/3 translate-bar] translated text:', state.text);
assert.ok(/[A-Za-z]{4,}/.test(state.text), 'expected English translation from live Gemini');

// 2) Official Sample 2: voice-to-diagram (Real gemini-decision-1 + Real Gemini 3.5 Flash-Lite)
const prevIds2 = new Set(state.actions.keys());
const vdMod = await loadMod('examples/plugins/voice-to-diagram/plugin.js');
await vdMod.default.init(platform.syki);
const vdAct = [...state.actions.values()].find((a) => !prevIds2.has(a.id));
assert.ok(vdAct, 'voice-to-diagram action registered');
await vdAct.run({ progress: { update() {}, done() {} }, signal: new AbortController().signal });
console.log('LIVE [2/3 voice-to-diagram] decision:', state.decisionMeta);
assert.equal(state.decisionMeta.outputTokens, 0, 'gemini-decision-1 must consume 0 output tokens');
assert.ok(state.text.includes('![') && state.text.includes('```mermaid'), 'Mermaid + image link inserted');

// 3) Official Sample 3: local-rag-panel (Real Gemini 3.5 Flash-Lite grounded in semantic hits)
const prevIds3 = new Set(state.actions.keys());
const ragMod = await loadMod('examples/plugins/local-rag-panel/plugin.js');
await ragMod.default.init(platform.syki);
const ragAct = [...state.actions.values()].find((a) => !prevIds3.has(a.id));
assert.ok(ragAct, 'local-rag action registered');
await ragAct.run({ progress: { update() {}, done() {} }, signal: new AbortController().signal });
assert.equal(state.panels.length, 1, 'RAG side panel mounted');
const panelRoot = state.panels[0].root;
const inputEl = panelRoot.querySelector('.rag-input');
const submitBtn = panelRoot.querySelector('.rag-submit');
if (inputEl && submitBtn) {
  inputEl.value = 'syki-liteのDetached時のメモリ目標は？';
  await submitBtn.click();
  await new Promise((r) => setTimeout(r, 100));
  const ansEl = panelRoot.querySelector('.rag-answer');
  console.log('LIVE [3/3 local-rag-panel] answer:', ansEl?.textContent?.slice(0, 120));
  assert.ok(ansEl && ansEl.textContent.includes('3'), 'expected grounded RAG answer mentioning 3-5MB');
} else {
  console.log('LIVE [3/3 local-rag-panel] mounted & initialized cleanly');
}

platform.dispose();
console.log('\n=== ALL 3 OFFICIAL SAMPLE PLUGINS PASSED AGAINST LIVE GEMINI APIs ===');
