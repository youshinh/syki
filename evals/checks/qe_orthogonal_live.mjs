#!/usr/bin/env node
// Quality Engineering (Taguchi Orthogonal / FMEA Interaction & Noise Factor) Real-Browser Stress
// + Production Gemini API (gemini-flash-lite-latest) E2E Verification.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { startExplore, sleep } from '../../tools/explore/kit.mjs';
import { click, shown, waitShown, waitHidden, rpc } from '../../tests/smoke/lib.mjs';

const MODEL = 'gemini-flash-lite-latest';

function loadProdApiKey() {
  const cfgPath = path.join(process.env.APPDATA || '', 'syki-sok', 'config.json');
  if (!fs.existsSync(cfgPath)) throw new Error('Production config.json not found');
  const cfg = JSON.parse(fs.readFileSync(cfgPath, 'utf8'));
  const apiKey = String(cfg?.text?.apiKey || '').trim();
  if (!apiKey) throw new Error('Production Gemini API key missing');
  return apiKey;
}

async function callGeminiFlashLite({ apiKey, systemPrompt, userPrompt, maxOutputTokens = 1024 }) {
  const url = `https://generativelanguage.googleapis.com/v1beta/models/${encodeURIComponent(MODEL)}:generateContent`;
  const body = {
    contents: [{ role: 'user', parts: [{ text: userPrompt }] }],
    generationConfig: { maxOutputTokens, temperature: 0.2 },
  };
  if (systemPrompt) body.systemInstruction = { parts: [{ text: systemPrompt }] };
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'x-goog-api-key': apiKey },
    body: JSON.stringify(body),
  });
  if (!res.ok) {
    const errText = await res.text();
    throw new Error(`Gemini (${MODEL}) HTTP ${res.status}: ${errText.slice(0, 220)}`);
  }
  const data = await res.json();
  const parts = data?.candidates?.[0]?.content?.parts || [];
  const text = parts.filter((p) => !p.thought && typeof p.text === 'string').map((p) => p.text).join('');
  if (!text.trim()) throw new Error('Gemini returned empty text');
  return text.trim();
}

const appJs = fs.readFileSync(path.join(process.cwd(), 'frontend/js/app.js'), 'utf8');
const promptStart = appJs.indexOf('const DEGRAM_MERMAID_SYSTEM_PROMPT = `');
const promptEnd = appJs.indexOf('`;\n\n  function convertSelectionToMermaid', promptStart);
const DEGRAM_PROMPT = appJs.slice(promptStart + 'const DEGRAM_MERMAID_SYSTEM_PROMPT = `'.length, promptEnd);

const results = [];
async function run(id, factor, title, fn) {
  const t0 = Date.now();
  try {
    const detail = await fn();
    results.push({ id, factor, title, ok: true, ms: Date.now() - t0 });
    console.log(`PASS ${id} [${Date.now() - t0}ms] (${factor}) ${title}${detail ? ': ' + detail : ''}`);
  } catch (err) {
    const msg = err && err.message ? err.message : String(err);
    results.push({ id, factor, title, ok: false, ms: Date.now() - t0, error: msg });
    console.log(`FAIL ${id} [${Date.now() - t0}ms] (${factor}) ${title}\n    -> ${msg}`);
  }
}

async function setNoteAndPreview(s, content, waitMermaid = false) {
  await s.ev(`(function(){
    var ed = document.getElementById('editor');
    ed.value = ${JSON.stringify(content)};
    ed.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  const ui = (await rpc(s, 'getUiState()')).ok;
  if (ui.preview !== 'full') await rpc(s, 'setUiState({ preview: "full" })');
  await sleep(180);
  if (waitMermaid) {
    await s.waitFor("!document.querySelector('#preview-pane pre code.language-mermaid')", { timeout: 10000 });
    await sleep(80);
  }
}

async function main() {
  const apiKey = loadProdApiKey();
  const s = await startExplore({ lang: 'ja' });

  try {
    // =========================================================================
    // Part 1: Find & Replace x Noise Factors (Replacement $, Empty Regex, Case)
    // =========================================================================
    await run('QE-01', 'FindReplace×Noise', 'Literal Replace All with "$&", "$$", "$1" does NOT expand special replacement patterns', async () => {
      await s.ev(`(function(){
        var ed = document.getElementById('editor');
        ed.value = 'price=FOO and FOO';
        ed.dispatchEvent(new Event('input', { bubbles: true }));
      })()`);
      await click(s, 'btn-find');
      await waitShown(s, 'find-replace-bar');
      // Ensure replace row visible and regex OFF
      const repHidden = await s.ev("document.getElementById('replace-row').classList.contains('hidden')");
      if (repHidden) await click(s, 'btn-toggle-replace');
      const regexOn = await s.ev("document.getElementById('btn-find-regex').getAttribute('aria-pressed') === 'true'");
      if (regexOn) await click(s, 'btn-find-regex');
      await s.ev(`(function(){
        var fi = document.getElementById('find-input');
        var ri = document.getElementById('replace-input');
        fi.value = 'FOO'; fi.dispatchEvent(new Event('input', { bubbles: true }));
        ri.value = '$100 $& $$'; ri.dispatchEvent(new Event('input', { bubbles: true }));
      })()`);
      await click(s, 'btn-replace-all');
      await sleep(80);
      const val = await s.ev("document.getElementById('editor').value");
      assert.equal(val, 'price=$100 $& $$ and $100 $& $$', `Special $ pattern was expanded! Got: ${val}`);
    });

    await run('QE-02', 'FindReplace×Noise', 'Regex Replace All with zero-length pattern "^" completes without infinite loop', async () => {
      await s.ev(`(function(){
        var ed = document.getElementById('editor');
        ed.value = 'line1\\nline2\\nline3';
        ed.dispatchEvent(new Event('input', { bubbles: true }));
      })()`);
      const regexOn = await s.ev("document.getElementById('btn-find-regex').getAttribute('aria-pressed') === 'true'");
      if (!regexOn) await click(s, 'btn-find-regex');
      await s.ev(`(function(){
        var fi = document.getElementById('find-input');
        var ri = document.getElementById('replace-input');
        fi.value = '^'; fi.dispatchEvent(new Event('input', { bubbles: true }));
        ri.value = '> '; ri.dispatchEvent(new Event('input', { bubbles: true }));
      })()`);
      await click(s, 'btn-replace-all');
      await sleep(100);
      const val = await s.ev("document.getElementById('editor').value");
      assert.ok(val.startsWith('> line1'), `Unexpected result: ${val}`);
      await click(s, 'btn-find-regex'); // turn regex off
      await click(s, 'btn-find-close');
      await waitHidden(s, 'find-replace-bar');
    });

    await run('QE-03', 'FindReplace×Noise', 'Literal Find with regex metacharacters "a[0]+(b).*" matches literally without crashing', async () => {
      await s.ev(`(function(){
        var ed = document.getElementById('editor');
        ed.value = 'exact a[0]+(b).* match';
        ed.dispatchEvent(new Event('input', { bubbles: true }));
      })()`);
      await click(s, 'btn-find');
      await waitShown(s, 'find-replace-bar');
      await s.ev(`(function(){
        var fi = document.getElementById('find-input');
        fi.value = 'a[0]+(b).*'; fi.dispatchEvent(new Event('input', { bubbles: true }));
      })()`);
      await s.waitFor("(document.getElementById('find-count')?.textContent || '').includes('1')", { timeout: 2000 });
      await click(s, 'btn-find-close');
      await waitHidden(s, 'find-replace-bar');
    });

    // =========================================================================
    // Part 2: LLM Async × Tab Switch / Anchor Deletion Race
    // =========================================================================
    await run('QE-04', 'LLM×TabSwitch', 'LLM result arriving after switching to another tab updates ONLY the origin tab, never the active tab', async () => {
      await s.ev(`(function(){
        var ed = document.getElementById('editor');
        ed.value = 'TAB_A_BODY';
        ed.setSelectionRange(0, ed.value.length);
        ed.dispatchEvent(new Event('input', { bubbles: true }));
      })()`);
      // Set delayed LLM response
      await s.setLlm({ delayMs: 400, reply: 'MERMAID_OR_AI_RESULT_FOR_A' });
      // Trigger inline LLM ask on Tab A
      await s.ev(`(function(){
        var btn = document.getElementById('btn-header-llm');
        if (btn) btn.click();
      })()`);
      await waitShown(s, 'inline-prompt-bar');
      await s.ev(`(function(){
        var inp = document.getElementById('inline-prompt-input');
        inp.value = 'translate to English';
        inp.dispatchEvent(new Event('input', { bubbles: true }));
        inp.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
      })()`);
      await sleep(80);
      // Immediately create and switch to Tab B while LLM is still in flight
      await click(s, 'btn-new-file');
      await sleep(80);
      await s.ev(`(function(){
        var ed = document.getElementById('editor');
        ed.value = 'TAB_B_PRISTINE';
        ed.dispatchEvent(new Event('input', { bubbles: true }));
      })()`);
      await sleep(550);
      const tabBVal = await s.ev("document.getElementById('editor').value");
      assert.equal(tabBVal, 'TAB_B_PRISTINE', `Tab B was corrupted by Tab A LLM reply! Got: ${tabBVal}`);
      const st = await s.state();
      const tabA = st.tabs.find((t) => t.content.includes('MERMAID_OR_AI_RESULT_FOR_A'));
      assert.ok(tabA, 'Tab A did not receive its LLM result in background');
      await s.setLlm({ delayMs: 0, reply: 'OK' });
    });

    await run('QE-05', 'LLM×ManualEdit', 'Deleting waiting anchor manually before LLM reply arrives does not throw or duplicate text', async () => {
      await s.ev(`(function(){
        var ed = document.getElementById('editor');
        ed.value = 'KEEP_ME';
        ed.setSelectionRange(0, ed.value.length);
        ed.dispatchEvent(new Event('input', { bubbles: true }));
      })()`);
      await s.setLlm({ delayMs: 250, reply: 'LATE_REPLY' });
      await click(s, 'btn-header-llm');
      await waitShown(s, 'inline-prompt-bar');
      await s.ev(`(function(){
        var inp = document.getElementById('inline-prompt-input');
        inp.value = 'rewrite';
        inp.dispatchEvent(new Event('input', { bubbles: true }));
        inp.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
      })()`);
      await sleep(60);
      // User manually clears the anchor
      await s.ev(`(function(){
        var ed = document.getElementById('editor');
        ed.value = 'USER_EDITED_CLEAN';
        ed.dispatchEvent(new Event('input', { bubbles: true }));
      })()`);
      await sleep(350);
      const val = await s.ev("document.getElementById('editor').value");
      assert.ok(val.includes('USER_EDITED_CLEAN'), `User edit lost: ${val}`);
      await s.setLlm({ delayMs: 0, reply: 'OK' });
    });

    // =========================================================================
    // Part 3: Side Preview × Rapid Typing × Mermaid DOM Leftover Cleanup
    // =========================================================================
    await run('QE-06', 'SidePreview×MermaidRace', 'Rapid edits between broken and valid mindmap leave 0 stray Mermaid error nodes in document.body', async () => {
      await rpc(s, 'setUiState({ preview: "side" })');
      await sleep(120);
      const frames = [
        '```mermaid\nmindmap\n  root((A\n```',
        '```mermaid\nflowchart TD\n  A ---> ---> ERR\n```',
        '```mermaid\nmindmap\n  root((FinalValid))\n    Child1\n    Child2\n```'
      ];
      for (const f of frames) {
        await s.ev(`(function(){
          var ed = document.getElementById('editor');
          ed.value = ${JSON.stringify(f)};
          ed.dispatchEvent(new Event('input', { bubbles: true }));
        })()`);
        await sleep(60);
      }
      await s.waitFor("document.querySelectorAll('#secondary-preview-pane pre.mermaid-card > svg').length === 1", { timeout: 10000 });
      await sleep(150);
      const stray = await s.ev("document.querySelectorAll('body > [id^=\"dmermaid-\"], body > [id^=\"mermaid-svg-\"]').length");
      assert.equal(stray, 0, `Stray Mermaid error elements left in body: ${stray}`);
      await rpc(s, 'setUiState({ preview: "off" })');
      await sleep(80);
    });

    // =========================================================================
    // Part 4: Complex Multi-Factor Markdown Interaction (Table + <br> + URL + Footnote + XSS + KaTeX)
    // =========================================================================
    await run('QE-07', 'Markdown×MultiFactor', 'Single table combining <br>, bold, URL:port, file:line, version:num, currency, KaTeX, footnote, XSS', async () => {
      const combo = [
        '| 項目 | 内容 |',
        '|---|---|',
        '| 1 | **太字1<br>太字2** と https://example.com:8080/a.js |',
        '| 2 | src/main.rs:42 と v1.2.3:45 と $100〜$200 |',
        '| 3 | 数式 $x^2+1$ と 脚注[^n1] と <img src=x onerror=alert(1)> |',
        '',
        '```',
        'コード内 [^n1] と <br> と $x^2$',
        '```',
        '',
        '[^n1]: テーブルからの脚注'
      ].join('\n');
      await setNoteAndPreview(s, combo, false);
      const info = await s.ev(`(function(){
        var p = document.getElementById('preview-pane');
        return {
          tdBr: p.querySelectorAll('td br').length,
          nestedA: p.querySelectorAll('a a').length,
          vscode: Array.from(p.querySelectorAll('a.vscode-jump-link')).map(function(a){return a.textContent;}),
          katex: p.querySelectorAll('.katex').length,
          fnRef: p.querySelectorAll('sup.footnote-ref').length,
          imgs: p.querySelectorAll('img').length,
          text: p.textContent
        };
      })()`);
      assert.equal(info.tdBr, 1, `tdBr=${info.tdBr}`);
      assert.equal(info.nestedA, 0, `nestedA=${info.nestedA}`);
      assert.deepEqual(info.vscode, ['src/main.rs:42'], `vscode=${JSON.stringify(info.vscode)}`);
      assert.equal(info.katex, 1, `katex=${info.katex}`);
      assert.equal(info.fnRef, 1, `fnRef=${info.fnRef}`);
      assert.equal(info.imgs, 0, `XSS img leaked!`);
      assert.ok(info.text.includes('$100') && info.text.includes('$200') && info.text.includes('v1.2.3:45'));
    });

    await run('QE-08', 'Markdown×EmojiSurrogate', 'Emoji surrogate pairs + <br/> + Japanese headings render cleanly without corruption', async () => {
      await setNoteAndPreview(s, '# 🚀 テスト🎉\n\n👨‍👩‍👧‍👦 絵文字<br/>改行🇯🇵', false);
      const info = await s.ev("({ br: document.querySelectorAll('#preview-pane br').length, txt: document.getElementById('preview-pane').textContent })");
      assert.equal(info.br, 1);
      assert.ok(info.txt.includes('🚀') && info.txt.includes('👨‍👩‍👧‍👦') && info.txt.includes('🇯🇵'));
    });

    // =========================================================================
    // Part 5: Split Editor × Dual Pane Editing × Preview Transition
    // =========================================================================
    await run('QE-09', 'SplitEditor×State', 'Switching Full Preview -> Side Preview -> Split Editor -> Off never loses tab content', async () => {
      await rpc(s, 'setUiState({ preview: "off", splitMode: false })');
      await sleep(80);
      await s.ev(`(function(){
        var ed = document.getElementById('editor');
        ed.value = 'STATE_SURVIVAL_CHECK';
        ed.dispatchEvent(new Event('input', { bubbles: true }));
      })()`);
      await rpc(s, 'setUiState({ preview: "full" })');
      await sleep(80);
      await rpc(s, 'setUiState({ preview: "side" })');
      await sleep(80);
      await rpc(s, 'setUiState({ preview: "off", splitMode: true })');
      await sleep(80);
      await rpc(s, 'setUiState({ splitMode: false })');
      await sleep(80);
      const val = await s.ev("document.getElementById('editor').value");
      assert.equal(val, 'STATE_SURVIVAL_CHECK');
    });

    // =========================================================================
    // Part 6: Production Gemini API (gemini-flash-lite-latest) × Real Browser
    // =========================================================================
    await run('QE-L01', `ProdAPI(${MODEL})`, 'Mindmap generation -> real mermaid.min.js SVG render (0 errors)', async () => {
      const generated = await callGeminiFlashLite({
        apiKey,
        systemPrompt: DEGRAM_PROMPT,
        userPrompt: '以下の機能一覧をマインドマップ（mindmap）で図解してください:\nsykiの構成: エディタ（高速・Markdown・テーブル<br>）、AI（要約・図解・音声）、検索（インメモリキャッシュ・意味検索）。',
      });
      assert.ok(/```mermaid[\s\S]*mindmap/i.test(generated), `Not mindmap:\n${generated.slice(0, 200)}`);
      await setNoteAndPreview(s, generated, true);
      const info = await s.ev("({ svg: document.querySelectorAll('#preview-pane pre.mermaid-card > svg').length, err: document.querySelectorAll('#preview-pane .mermaid-error').length, errTxt: Array.from(document.querySelectorAll('#preview-pane .mermaid-error')).map(e=>e.textContent).join(';') })");
      assert.equal(info.err, 0, `Mermaid error: ${info.errTxt}\nCode:\n${generated}`);
      assert.ok(info.svg >= 1);
      return `svg=${info.svg}, chars=${generated.length}`;
    });

    await run('QE-L02', `ProdAPI(${MODEL})`, 'PTY terminal output summary (error-first, concise, not truncated)', async () => {
      const termLog = [
        'SKIP  20_企画フェーズ\\202611(情報システム内容検証中)\\詳細説明_月末シミュレーション.html  対象外(Include=0)',
        'SKIP  20_企画フェーズ\\202611(情報システム内容検証中)\\簡易説明_モック.html  対象外(Include=0)',
        'SAME  10_現状資料\\202611\\仕様書\\02_業務とデータの流れ.md',
        'SAME  10_現状資料\\202611\\仕様書\\03_原価計算の仕組み.md',
        'SAME  10_現状資料\\202611\\仕様書\\06_原価計算ロジック仕様.md',
        'ERR   30_テスト\\broken_case.md  権限エラー (Access Denied)'
      ].join('\n');
      const prompt = `以下のターミナル出力を読み、要点を簡潔なMarkdown箇条書きで要約してください。\n- 結論と件数（成功/失敗/スキップ等）\n- エラーや警告がある場合は最優先でファイル名と原因を明記\n- 次のアクション（必要なら1〜2行）\n冗長なパス列挙や前置きは省き、ノートにそのまま貼れるMarkdownのみを出力してください。\n\n\`\`\`\n${termLog}\n\`\`\``;
      const summary = await callGeminiFlashLite({ apiKey, userPrompt: prompt, maxOutputTokens: 768 });
      assert.ok(summary.includes('-') || summary.includes('*'), `Not bullet list: ${summary}`);
      assert.ok(summary.includes('broken_case.md') && (summary.includes('権限') || summary.includes('Access Denied')), `Missed error detail: ${summary}`);
      await setNoteAndPreview(s, summary, false);
      return `chars=${summary.length}`;
    });

    await run('QE-L03', `ProdAPI(${MODEL})`, 'Markdown table with <br> -> real <td><br></td> DOM render', async () => {
      const tableMd = await callGeminiFlashLite({
        apiKey,
        userPrompt: '2列3行のMarkdownテーブルを1つだけ出力してください。セル内改行に <br> を2回以上使ってください。前置き不要。',
        maxOutputTokens: 512,
      });
      await setNoteAndPreview(s, tableMd, false);
      const tdBr = await s.ev("document.querySelectorAll('#preview-pane td br').length");
      assert.ok(tdBr >= 2, `Expected >=2 td br, got ${tdBr}\nMD:\n${tableMd}`);
      return `td_br=${tdBr}`;
    });

    await run('QE-L04', `ProdAPI(${MODEL})`, 'sequenceDiagram generation -> real mermaid.min.js SVG render', async () => {
      const generated = await callGeminiFlashLite({
        apiKey,
        systemPrompt: DEGRAM_PROMPT,
        userPrompt: 'ユーザーがログイン画面で認証情報を送信し、AuthAPIがDBで検証してJWTを返すシーケンス図を作成してください。',
      });
      await setNoteAndPreview(s, generated, true);
      const info = await s.ev("({ svg: document.querySelectorAll('#preview-pane pre.mermaid-card > svg').length, err: document.querySelectorAll('#preview-pane .mermaid-error').length })");
      assert.equal(info.err, 0, `Mermaid error on:\n${generated}`);
      assert.ok(info.svg >= 1);
      return `svg=${info.svg}`;
    });

    await run('QE-L05', `ProdAPI(${MODEL})`, 'erDiagram generation -> real mermaid.min.js SVG render', async () => {
      const generated = await callGeminiFlashLite({
        apiKey,
        systemPrompt: DEGRAM_PROMPT,
        userPrompt: 'プロジェクト(Project)は複数のタスク(Task)を持ち、タスクは担当者(User)に割り当てられる。ER図にしてください。',
      });
      await setNoteAndPreview(s, generated, true);
      const info = await s.ev("({ svg: document.querySelectorAll('#preview-pane pre.mermaid-card > svg').length, err: document.querySelectorAll('#preview-pane .mermaid-error').length })");
      assert.equal(info.err, 0, `Mermaid error on:\n${generated}`);
      assert.ok(info.svg >= 1);
      return `svg=${info.svg}`;
    });

    await run('QE-10', 'RuntimeHealth', 'Zero uncaught JS exceptions across all QE interaction & live API tests', async () => {
      const st = await s.state();
      const uncaught = st.consoleErrors.filter((e) => e.startsWith('Uncaught:'));
      assert.deepEqual(uncaught, [], `Uncaught: ${uncaught.join(' | ')}`);
    });
  } finally {
    await s.close();
  }

  const failed = results.filter((r) => !r.ok);
  console.log(`\nQE ORTHOGONAL + LIVE (${MODEL}) SUMMARY: ${results.length - failed.length}/${results.length} PASS, ${failed.length} FAIL`);
  if (failed.length > 0) process.exit(1);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
