#!/usr/bin/env node
// Live Production Gemini API + Real Headless Edge Browser E2E Verification.
// Uses the real API key from %APPDATA%\syki-sok\config.json (never logged).
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { startExplore, sleep } from '../../tools/explore/kit.mjs';
import { rpc } from '../../tests/smoke/lib.mjs';

function loadProdGeminiConfig() {
  const cfgPath = path.join(process.env.APPDATA || '', 'syki-sok', 'config.json');
  if (!fs.existsSync(cfgPath)) throw new Error('Production config.json not found');
  const cfg = JSON.parse(fs.readFileSync(cfgPath, 'utf8'));
  const apiKey = String(cfg?.text?.apiKey || '').trim();
  const model = String(cfg?.text?.model || 'gemini-2.5-flash').trim();
  if (!apiKey) throw new Error('Production Gemini API key missing in config.json');
  return { apiKey, model };
}

async function callGeminiLive({ apiKey, model, systemPrompt, userPrompt, maxOutputTokens = 1024 }) {
  const url = `https://generativelanguage.googleapis.com/v1beta/models/${encodeURIComponent(model)}:generateContent`;
  const genCfg = { maxOutputTokens, temperature: 0.2 };
  if (model.startsWith('gemini-2.5') && maxOutputTokens <= 1024) {
    genCfg.thinkingConfig = { thinkingBudget: 0 };
  }
  const body = {
    contents: [{ role: 'user', parts: [{ text: userPrompt }] }],
    generationConfig: genCfg,
  };
  if (systemPrompt) {
    body.systemInstruction = { parts: [{ text: systemPrompt }] };
  }
  const res = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'x-goog-api-key': apiKey },
    body: JSON.stringify(body),
  });
  if (!res.ok) {
    const errText = await res.text();
    throw new Error(`Gemini HTTP ${res.status}: ${errText.slice(0, 200)}`);
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
async function run(id, title, fn) {
  const t0 = Date.now();
  try {
    const info = await fn();
    results.push({ id, title, ok: true, ms: Date.now() - t0 });
    console.log(`PASS ${id} [${Date.now() - t0}ms] ${title}: ${info || 'ok'}`);
  } catch (err) {
    results.push({ id, title, ok: false, ms: Date.now() - t0, error: err.message });
    console.log(`FAIL ${id} [${Date.now() - t0}ms] ${title}\n    -> ${err.message}`);
  }
}

async function renderInBrowser(s, content, waitMermaid = true) {
  await s.ev(`(function(){
    var ed = document.getElementById('editor');
    ed.value = ${JSON.stringify(content)};
    ed.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  const ui = (await rpc(s, 'getUiState()')).ok;
  if (ui.preview !== 'full') await rpc(s, 'setUiState({ preview: "full" })');
  await sleep(200);
  if (waitMermaid) {
    await s.waitFor("!document.querySelector('#preview-pane pre code.language-mermaid')", { timeout: 10000 });
    await sleep(100);
  }
  return await s.ev(`(function(){
    var p = document.getElementById('preview-pane');
    return {
      svgCount: p.querySelectorAll('pre.mermaid-card > svg').length,
      errorCount: p.querySelectorAll('.mermaid-error').length,
      errorTexts: Array.from(p.querySelectorAll('.mermaid-error')).map(function(e){ return e.textContent.slice(0, 140); }),
      brCount: p.querySelectorAll('br').length,
      tdBrCount: p.querySelectorAll('td br').length,
      textLen: (p.textContent || '').length
    };
  })()`);
}

async function main() {
  const prod = loadProdGeminiConfig();
  const s = await startExplore({ lang: 'ja' });
  try {
    // L01: Production Gemini generates MINDMAP using real DEGRAM_MERMAID_SYSTEM_PROMPT -> Real browser SVG check
    await run('L01', 'Live Gemini API mindmap generation -> real mermaid.min.js SVG render', async () => {
      const generated = await callGeminiLive({
        ...prod,
        systemPrompt: DEGRAM_PROMPT,
        userPrompt: '以下の機能一覧・構成要素をマインドマップ（mindmap）で図解してください:\nノートアプリsykiの構成要素: UI（紙の質感・タブ・ミニマル）、コア（Rust・WebView2・5MB軽量）、機能（高速検索・AI要約・音声入力・ショートカット）。',
      });
      assert.ok(/```mermaid[\s\S]*mindmap/i.test(generated), `Did not generate mindmap fence:\n${generated.slice(0, 200)}`);
      const info = await renderInBrowser(s, generated, true);
      assert.equal(info.errorCount, 0, `Mermaid error on live API output: ${info.errorTexts.join('; ')}\nCode:\n${generated}`);
      assert.ok(info.svgCount >= 1, `Expected >=1 SVG, got ${info.svgCount}`);
      return `svg=${info.svgCount}, chars=${generated.length}`;
    });

    // L02: Production Gemini generates sequenceDiagram -> real browser SVG render
    await run('L02', 'Live Gemini API sequenceDiagram generation -> real mermaid.min.js SVG render', async () => {
      const generated = await callGeminiLive({
        ...prod,
        systemPrompt: DEGRAM_PROMPT,
        userPrompt: 'クライアントがAPIサーバーにWebhookリクエストを送り、サーバーがDBに保存して200 OKを返す対話フローを図解してください。',
      });
      const info = await renderInBrowser(s, generated, true);
      assert.equal(info.errorCount, 0, `Mermaid error: ${info.errorTexts.join('; ')}\nCode:\n${generated}`);
      assert.ok(info.svgCount >= 1);
      return `svg=${info.svgCount}`;
    });

    // L03: Production Gemini generates flowchart with Japanese labels -> real browser SVG render
    await run('L03', 'Live Gemini API flowchart generation -> real mermaid.min.js SVG render', async () => {
      const generated = await callGeminiLive({
        ...prod,
        systemPrompt: DEGRAM_PROMPT,
        userPrompt: 'プルリクエスト作成 -> 自動テスト実行 -> 成功ならマージ、失敗なら修正して再テストする手順を図解してください。',
      });
      const info = await renderInBrowser(s, generated, true);
      assert.equal(info.errorCount, 0, `Mermaid error: ${info.errorTexts.join('; ')}\nCode:\n${generated}`);
      assert.ok(info.svgCount >= 1);
      return `svg=${info.svgCount}`;
    });

    // L04: Production Gemini summarizes real terminal output (same style as user screenshot)
    await run('L04', 'Live Gemini API PTY terminal output summary (not raw paste)', async () => {
      const termLog = [
        'SKIP  20_企画フェーズ\\202611(情報システム内容検証中)\\詳細説明_月末シミュレーション.html  対象外(Include=0)',
        'SKIP  20_企画フェーズ\\202611(情報システム内容検証中)\\簡易説明_モック.html  対象外(Include=0)',
        'SAME  10_現状資料\\202611\\仕様書\\02_業務とデータの流れ.md',
        'SAME  10_現状資料\\202611\\仕様書\\03_原価計算の仕組み.md',
        'SAME  10_現状資料\\202611\\仕様書\\06_原価計算ロジック仕様.md',
        'ERR   30_テスト\\broken_case.md  権限エラー (Access Denied)'
      ].join('\n');
      const prompt = `以下のターミナル出力を読み、要点を簡潔なMarkdown箇条書きで要約してください。\n- 実行内容と結果（成功/失敗/件数など）\n- 重要な変更・警告・エラー（あれば具体的なファイル名や原因）\n- 次のアクション（必要なら）\n余計な前置きや挨拶は不要で、ノートにそのまま貼れるMarkdownのみを出力してください。\n\n\`\`\`\n${termLog}\n\`\`\``;
      const summary = await callGeminiLive({ ...prod, userPrompt: prompt, maxOutputTokens: 768 });
      assert.ok(summary.includes('-') || summary.includes('*'), `Not bullet summary: ${summary}`);
      assert.ok(summary.includes('broken_case.md') || summary.includes('権限') || summary.includes('ERR'), `Missed error detail: ${summary}`);
      assert.ok(!summary.startsWith('```\nSKIP'), 'Returned raw log instead of summary');
      const info = await renderInBrowser(s, summary, false);
      assert.ok(info.textLen > 20);
      return `summary_chars=${summary.length}`;
    });

    // L05: Production Gemini outputs Markdown table with <br> -> real browser <td><br></td> render
    await run('L05', 'Live Gemini API Markdown table with <br> -> real <td><br></td> DOM render', async () => {
      const tableMd = await callGeminiLive({
        ...prod,
        userPrompt: '2列3行のMarkdownテーブルを1つだけ出力してください。セル内改行には必ず <br> を2回以上使ってください。前置き不要。',
        maxOutputTokens: 512,
      });
      assert.ok(/<br\s*\/?>/i.test(tableMd), `Model did not emit <br>: ${tableMd}`);
      const info = await renderInBrowser(s, tableMd, false);
      assert.ok(info.tdBrCount >= 2, `Expected >=2 <br> in <td>, got ${info.tdBrCount}\nMD:\n${tableMd}`);
      return `td_br=${info.tdBrCount}`;
    });

    // L06: Production Gemini generates ER diagram -> real browser SVG render
    await run('L06', 'Live Gemini API erDiagram generation -> real mermaid.min.js SVG render', async () => {
      const generated = await callGeminiLive({
        ...prod,
        systemPrompt: DEGRAM_PROMPT,
        userPrompt: 'データベースのテーブル定義: ユーザー(User)は複数のノート(Note)を持ち、各ノートは複数のタグ(Tag)と紐づく。ER図にしてください。',
      });
      const info = await renderInBrowser(s, generated, true);
      assert.equal(info.errorCount, 0, `Mermaid error: ${info.errorTexts.join('; ')}\nCode:\n${generated}`);
      assert.ok(info.svgCount >= 1);
      return `svg=${info.svgCount}`;
    });
  } finally {
    await s.close();
  }

  const failed = results.filter((r) => !r.ok);
  console.log(`\nLIVE API + REAL BROWSER SUMMARY: ${results.length - failed.length}/${results.length} PASS, ${failed.length} FAIL`);
  if (failed.length > 0) process.exit(1);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
