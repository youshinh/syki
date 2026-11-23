/**
 * Additional 20 Random Real-World Plugin Scenarios E2E Verification
 * Covers scenarios not included in the initial 15-scenario suite.
 */
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const PluginRuntime = require('../../frontend/js/plugin_runtime.js');
const ApiCore = require('../../frontend/js/syki_api_core.js');
const ApiFlow = require('../../frontend/js/syki_api_flow.js');
const ApiExt = require('../../frontend/js/syki_api_ext.js');
const ApiMedia = require('../../frontend/js/syki_api_media.js');

function createHarness(overrides = {}) {
  const state = {
    tabs: [
      { id: 't1', title: 'Main.md', path: 'Main.md', text: 'ユーザは出来るだけ早く製品Aと製品Bを比較したい。\nHello world!' },
    ],
    activeTabId: 't1',
    selection: { from: 0, to: 18 },
    notes: { 'vocabulary.md': '# Vocab\n' },
    clipboardText: 'The quick brown fox jumps over the lazy dog.',
    clipboardImages: [],
    toasts: [],
    tokens: new Map(),
    panels: [],
    ptyCalls: [],
    detachGuardCount: 0,
  };
  const active = () => state.tabs.find((t) => t.id === state.activeTabId);

  const core = ApiCore.createSykiCoreApi({
    getLocale: () => overrides.locale || 'en',
    editorAdapter: {
      getText: () => active().text,
      setText: (t) => { active().text = t; },
      getSelection: () => active().text.slice(state.selection.from, state.selection.to),
      getSelectionRange: () => ({ ...state.selection }),
      getSelectionRect: () => ({ x: 300, y: 180, width: 64, height: 20 }),
      insert: (t) => { active().text += t; },
      replaceSelection: (t) => {
        const cur = active().text;
        active().text = cur.slice(0, state.selection.from) + t + cur.slice(state.selection.to);
      },
      replaceRange: (from, to, t) => {
        const cur = active().text;
        active().text = cur.slice(0, from) + t + cur.slice(to);
      },
      openTab: ({ title, content }) => {
        const id = `t${state.tabs.length + 1}`;
        state.tabs.push({ id, title, text: content });
        state.activeTabId = id;
      },
      getActiveTab: () => ({ id: active().id, title: active().title, path: active().path }),
      listTabs: () => state.tabs.map(({ id, title, path }) => ({ id, title, path })),
      getTabText: (id) => state.tabs.find((t) => t.id === id)?.text ?? null,
    },
    mountPanel: (slot, opts) => {
      state.panels.push({ slot, opts });
      return { dispose: () => {} };
    },
    toast: (msg, level) => state.toasts.push({ msg, level }),
    getTheme: () => 'paper',
    setToken: (name, val) => {
      const prev = state.tokens.get(name);
      state.tokens.set(name, val);
      return {
        dispose: () => {
          if (prev === undefined) state.tokens.delete(name);
          else state.tokens.set(name, prev);
        },
      };
    },
    clipboard: {
      read: async () => ({ text: state.clipboardText }),
      writeText: async (t) => { state.clipboardText = t; },
      writeImage: async (img) => { state.clipboardImages.push(img); },
    },
    storage: {
      get: async (_k, fb) => fb,
      set: async () => {},
    },
  });

  const flow = ApiFlow.createSykiFlowApi({
    acquireDetachGuard: () => { state.detachGuardCount++; },
    releaseDetachGuard: () => { state.detachGuardCount--; },
    searchSemantic: async (q) => [{ path: 'adr/001.md', snippet: `Past decision on ${q}`, score: 0.91 }],
    searchFulltext: async (q) => [{ path: 'GLOSSARY.md', snippet: `${q}: canonical definition`, score: 1.0 }],
    notes: {
      read: async (p) => state.notes[p] ?? '',
      list: async () => Object.keys(state.notes).map((p) => ({ path: p })),
      create: async ({ title, content, folder }) => {
        const key = folder ? `${folder}/${title}` : title;
        state.notes[key] = content;
        return key;
      },
      append: async (p, text) => { state.notes[p] = (state.notes[p] || '') + text; },
      update: async (p, text) => { state.notes[p] = text; },
    },
    queryLlm: async (req) => {
      if (overrides.queryLlm) return overrides.queryLlm(req);
      return `LLM:${req.prompt.slice(0, 24)}`;
    },
    queryDecision: async (req) => {
      if (overrides.queryDecision) return overrides.queryDecision(req);
      return { reqId: req.reqId, choiceId: req.choices[0].id, by: 'decision' };
    },
    runProgram: async (program, args, opts) => {
      if (overrides.runProgram) return overrides.runProgram(program, args, opts);
      return { allowed: true, exitCode: 0, stdout: `stdout:${program}`, stderr: '' };
    },
    openPty: async (opts) => {
      state.ptyCalls.push(opts);
      return 'pty-wt-1';
    },
  });

  const ext = ApiExt.createSykiExtApi({
    acquireDetachGuard: () => { state.detachGuardCount++; },
    releaseDetachGuard: () => { state.detachGuardCount--; },
  });

  const media = ApiMedia.createSykiMediaApi({
    acquireDetachGuard: () => { state.detachGuardCount++; },
    releaseDetachGuard: () => { state.detachGuardCount--; },
    recordVoice: async () => '明日の10時にAPI仕様書をレビューする',
    recordMeeting: async () => '田中: 来週リリース。佐藤: テスト追加担当。',
    captureScreen: async () => ({ id: 'scr-1', url: '/__ext/tmp/scr-1.png', mime: 'image/png', bytes: 2048 }),
    queryVision: async (_a, p) => `OCR_TEXT(${p}): ECONNREFUSED 127.0.0.1:5432`,
    generateImage: async () => ({ id: 'g-1', url: '/__ext/tmp/g-1.png', mime: 'image/png', bytes: 4096 }),
    renderMermaidOffscreen: async (code) => {
      if (overrides.renderMermaidOffscreen) return overrides.renderMermaidOffscreen(code);
      return { id: 'm-1', url: '/__ext/tmp/m-1.png', mime: 'image/png', bytes: 1024 };
    },
    renderHtmlToPng: async (html) => ({ id: 'hp-1', url: '/__ext/tmp/hp-1.png', mime: 'image/png', bytes: html.length }),
    renderHtmlToPdf: async (_html, opts) => opts?.outPath || 'slides.pdf',
  });

  return { state, core, flow, ext, media };
}

let passed = 0;
async function run(id, title, fn) {
  await fn();
  passed++;
  console.log(`PASS [${id}] ${title}`);
}

// 1. UC#3
await run('R01 (UC#3)', 'スクショOCR → 過去スクラップ意味検索 → 解決策カード新タブ生成', async () => {
  const { core, flow, media } = createHarness();
  const cap = await media.input.captureScreen();
  const errText = await media.exec.vision(cap, 'Extract error text');
  const hits = await flow.knowledge.searchSemantic(errText);
  core.editor.openTab({ title: 'ErrorFix.md', content: `${errText}\nRef: ${hits[0].path}` });
  assert.ok(core.editor.getText().includes('ECONNREFUSED'));
  assert.ok(core.editor.getText().includes('adr/001.md'));
});

// 2. UC#5
await run('R02 (UC#5)', 'クリップボード英文読取 → 翻訳 → 引用ブロック挿入', async () => {
  const { core, flow } = createHarness({ queryLlm: async () => '素早い茶色のキツネ' });
  const clip = await core.sys.clipboard.read();
  const ja = await flow.exec.llm('Translate to JA', { input: clip.text });
  core.editor.insert(`\n> ${clip.text}\n> 訳: ${ja}\n`);
  assert.ok(core.editor.getText().includes('訳: 素早い茶色のキツネ'));
});

// 3. UC#6
await run('R03 (UC#6)', '音声独り言 → TODO抽出 → ドキュメント先頭へピン留め挿入 (replaceRange 0..0)', async () => {
  const { core, flow, media } = createHarness({ queryLlm: async () => '- [ ] 明日10時 API仕様書レビュー\n' });
  const voice = await media.input.voice();
  const todo = await flow.exec.llm('Extract TODO', { input: voice });
  core.editor.replaceRange(0, 0, todo);
  assert.ok(core.editor.getText().startsWith('- [ ] 明日10時 API仕様書レビュー'));
});

// 4. UC#13
await run('R04 (UC#13)', '比較メモ → HTMLカードPNG化 → クリップボード画像コピー', async () => {
  const { state, core, media } = createHarness();
  const png = await media.exec.render.htmlToPng('<table><tr><td>A</td><td>B</td></tr></table>');
  await core.sys.clipboard.writeImage(png);
  assert.equal(state.clipboardImages.length, 1);
  assert.equal(state.clipboardImages[0].mime, 'image/png');
});

// 5. UC#15
await run('R05 (UC#15)', '選択セクション → スライド16:9 HTML → PDF出力', async () => {
  const { core, media } = createHarness();
  const pdf = await media.exec.render.htmlToPdf(`<section>${core.editor.getSelection()}</section>`, { outPath: 'deck.pdf' });
  assert.equal(pdf, 'deck.pdf');
});

// 6. UC#18
await run('R06 (UC#18)', 'Mermaid構文エラー自動修復ループ (flow.loop × render失敗捕捉 × exec.llm)', async () => {
  let attempt = 0;
  const { flow, ext, media } = createHarness({
    renderMermaidOffscreen: async (code) => {
      attempt++;
      if (attempt === 1) throw new Error('Syntax error on line 2');
      return { id: 'm-ok', url: '/__ext/tmp/m-ok.png', mime: 'image/png', bytes: code.length };
    },
    queryLlm: async () => 'flowchart TD\n  A --> B',
  });
  const res = await ext.flow.loop({
    init: { code: 'flowchart TD\n  A --->??? B', img: null },
    maxTurns: 3,
    step: async (st, _t, signal) => {
      try {
        const img = await media.exec.render.mermaidToPng(st.code);
        return { code: st.code, img };
      } catch (err) {
        const fixed = await flow.exec.llm('Fix Mermaid syntax', { input: `${st.code}\nERR:${err.message}`, signal });
        return { code: fixed, img: null };
      }
    },
    until: (st) => st.img !== null,
  });
  assert.equal(res.completed, true);
  assert.equal(res.turns, 2);
});

// 7. UC#23
await run('R07 (UC#23)', '過去ADRとの矛盾チェック (searchSemantic + flow.decide conflict警告)', async () => {
  const { state, core, flow } = createHarness({
    queryDecision: async (req) => ({ reqId: req.reqId, choiceId: 'conflict', reason: 'Reverses ADR-001', by: 'decision' }),
  });
  const past = await flow.knowledge.searchSemantic(core.editor.getSelection());
  const d = await flow.flow.decide({
    input: `${core.editor.getSelection()}\nPAST:${past[0].snippet}`,
    choices: [{ id: 'consistent', description: 'ok' }, { id: 'conflict', description: 'contradicts' }],
    fallback: 'consistent',
  });
  if (d.choiceId === 'conflict') core.ui.toast(`矛盾の可能性: ${d.reason}`, 'warn');
  assert.equal(state.toasts[0].level, 'warn');
});

// 8. UC#28
await run('R08 (UC#28)', '専門用語ツールチップ (getSelectionRect + searchFulltext + floatingパネル)', async () => {
  const { state, core, flow } = createHarness();
  const rect = core.editor.getSelectionRect();
  const hits = await flow.knowledge.searchFulltext('Seam');
  core.ui.mountPanel('floating', { x: rect.x, y: rect.y + rect.height, title: hits[0].snippet, render: () => {} });
  assert.equal(state.panels[0].slot, 'floating');
  assert.equal(state.panels[0].opts.y, 200);
});

// 9. UC#31
await run('R09 (UC#31)', 'ワンボタン会議フルパイプライン (input.meeting → flow.steps進捗更新 → 議事録生成)', async () => {
  const progressLog = [];
  const { flow, media } = createHarness({ queryLlm: async () => '## 決定事項\n- 来週リリース' });
  const out = await flow.flow.steps(
    null,
    [
      { label: '会議録音文字起こし中', run: () => media.input.meeting() },
      { label: '議事録構造化中', run: (t, signal) => flow.exec.llm('Structure meeting', { input: t, signal }) },
    ],
    { progress: { update: (l) => progressLog.push(l) } },
  );
  assert.deepEqual(progressLog, ['会議録音文字起こし中', '議事録構造化中']);
  assert.ok(out.includes('来週リリース'));
});

// 10. UC#38
await run('R10 (UC#38)', '会議決定事項 → adrフォルダにADRノート作成 (notes.create folder付き)', async () => {
  const { state, flow } = createHarness();
  const created = await flow.knowledge.notes.create({ title: '0010-ext.md', folder: 'adr', content: '# ADR 0010' });
  assert.equal(created, 'adr/0010-ext.md');
  assert.equal(state.notes['adr/0010-ext.md'], '# ADR 0010');
});

// 11. UC#45
await run('R11 (UC#45)', 'Git Diff取得 → コミットメッセージ生成 → クリップボードコピー', async () => {
  const { state, core, flow } = createHarness({
    runProgram: async () => ({ allowed: true, exitCode: 0, stdout: '+pub mod ext_store;', stderr: '' }),
    queryLlm: async () => 'feat(ext): add ext_store module',
  });
  const diff = await flow.exec.shell('git', ['diff', '--staged']);
  const msg = await flow.exec.llm('Generate commit msg', { input: diff.stdout });
  await core.sys.clipboard.writeText(msg);
  assert.equal(state.clipboardText, 'feat(ext): add ext_store module');
});

// 12. UC#47
await run('R12 (UC#47)', '3観点並列コードレビュー (flow.parallel × 3 LLM → 新タブ統合)', async () => {
  const { core, flow } = createHarness({ queryLlm: async (req) => `REVIEW[${req.prompt}]` });
  const [sec, perf, read] = await flow.flow.parallel([
    flow.exec.llm('Security'),
    flow.exec.llm('Performance'),
    flow.exec.llm('Readability'),
  ]);
  core.editor.openTab({ title: 'CodeReview.md', content: [sec, perf, read].join('\n') });
  assert.ok(core.editor.getText().includes('REVIEW[Security]'));
  assert.ok(core.editor.getText().includes('REVIEW[Readability]'));
});

// 13. UC#49
await run('R13 (UC#49)', 'タスクメモ → Git Worktree付きPTYターミナル即起動', async () => {
  const { state, flow } = createHarness();
  const ptyId = await flow.exec.pty.open({ worktree: true, cwd: 'C:/repo' });
  assert.equal(ptyId, 'pty-wt-1');
  assert.equal(state.ptyCalls[0].worktree, true);
});

// 14. UC#54
await run('R14 (UC#54)', '表記ゆれ一括統一 (editor.getText → editor.setText)', async () => {
  const { core } = createHarness();
  const normalized = core.editor.getText().replace(/ユーザ(?!ー)/g, 'ユーザー').replace(/出来る/g, 'できる');
  core.editor.setText(normalized);
  assert.ok(core.editor.getText().includes('ユーザーはできるだけ早く'));
});

// 15. UC#55
await run('R15 (UC#55)', '1記事 → X / LinkedIn / メルマガ 3媒体並列変換 → 3新タブ生成', async () => {
  const { core, flow } = createHarness({ queryLlm: async (req) => `POST(${req.prompt})` });
  const src = core.editor.getText();
  const [x, li, mail] = await flow.flow.parallel([
    flow.exec.llm('X-thread', { input: src }),
    flow.exec.llm('LinkedIn', { input: src }),
    flow.exec.llm('Newsletter', { input: src }),
  ]);
  core.editor.openTab({ title: 'X.md', content: x });
  core.editor.openTab({ title: 'LinkedIn.md', content: li });
  core.editor.openTab({ title: 'Mail.md', content: mail });
  assert.equal(core.editor.listTabs().length, 4);
});

// 16. UC#62
await run('R16 (UC#62)', 'CSV選択 → DuckDB集計 (未承認時はPOLICY_ASK構造化エラー)', async () => {
  const { flow } = createHarness({
    runProgram: async () => ({ allowed: false, decision: { verdict: 'ask', ruleId: 'prog.unapproved', reason: 'duckdb not approved' } }),
  });
  await assert.rejects(
    () => flow.exec.shell('duckdb', ['-c', 'SELECT 1']),
    (err) => err.code === 'POLICY_ASK' && err.decision.ruleId === 'prog.unapproved',
  );
});

// 17. UC#65
await run('R17 (UC#65)', '危険シェル呼び出し拒否 (POLICY_DENIED構造化エラー)', async () => {
  const { flow } = createHarness({
    runProgram: async () => ({ allowed: false, decision: { verdict: 'deny', ruleId: 'prog.dangerous_shell', reason: 'blocked' } }),
  });
  await assert.rejects(
    () => flow.exec.shell('cmd.exe', ['/c', 'dir']),
    (err) => err.code === 'POLICY_DENIED' && err.decision.verdict === 'deny',
  );
});

// 18. UC#75
await run('R18 (UC#75)', '英単語選択 → 意味生成 → vocabulary.md 自動追記', async () => {
  const { state, flow } = createHarness({ queryLlm: async () => '- **orthogonal**: 直交する' });
  const entry = await flow.exec.llm('Define word', { input: 'orthogonal' });
  await flow.knowledge.notes.append('vocabulary.md', `${entry}\n`);
  assert.ok(state.notes['vocabulary.md'].includes('orthogonal'));
});

// 19. UC#83
await run('R19 (UC#83)', '時間帯自動テーマトークン切替 (setToken → dispose復元 + theme:change発火)', async () => {
  const { state, core } = createHarness();
  let seenTheme = null;
  core.sys.on('theme:change', ({ theme }) => { seenTheme = theme; });
  const d = core.ui.setToken('--bg-main', '#181a1f');
  assert.equal(state.tokens.get('--bg-main'), '#181a1f');
  d.dispose();
  assert.equal(state.tokens.has('--bg-main'), false);
  core.sys.emit('theme:change', { theme: 'ink' });
  assert.equal(seenTheme, 'ink');
});

// 20. Combo#26/47
await run('R20 (Combo#26/47)', '日英バイリンガル(sys.locale) + 朝ルーティン複数コマンド連結 (sys.runCommand 3連鎖)', async () => {
  const { core } = createHarness({ locale: 'en' });
  assert.equal(core.sys.locale, 'en');
  const log = [];
  core.sys.registerCommand({ id: 'morning.daily', title: 'Daily', run: () => log.push('daily') });
  core.sys.registerCommand({ id: 'morning.todo', title: 'Todo', run: () => log.push('todo') });
  core.sys.registerCommand({ id: 'morning.cal', title: 'Cal', run: () => log.push('cal') });
  await core.sys.runCommand('morning.daily');
  await core.sys.runCommand('morning.todo');
  await core.sys.runCommand('morning.cal');
  assert.deepEqual(log, ['daily', 'todo', 'cal']);
});

console.log(`\n=== ALL ${passed}/20 ADDITIONAL RANDOM SCENARIOS PASSED ===`);
