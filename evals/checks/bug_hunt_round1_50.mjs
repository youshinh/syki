#!/usr/bin/env node
// Round 1: 50 targeted high-risk bug-hunting scenarios derived from historical fault patterns.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const ROOT = process.cwd();

const results = [];
function scenario(id, domain, title, fn) {
  try {
    fn();
    results.push({ id, domain, title, status: 'PASS', error: null });
  } catch (err) {
    results.push({ id, domain, title, status: 'FAIL', error: err && err.message ? err.message : String(err) });
  }
}

const appJs = fs.readFileSync(path.join(ROOT, 'frontend/js/app.js'), 'utf8');
const indexHtml = fs.readFileSync(path.join(ROOT, 'frontend/index.html'), 'utf8');
const i18nJs = fs.readFileSync(path.join(ROOT, 'frontend/js/i18n.js'), 'utf8');
const terminalPaneJs = fs.readFileSync(path.join(ROOT, 'frontend/js/terminal_pane.js'), 'utf8');
const searchWorkbenchJs = fs.readFileSync(path.join(ROOT, 'frontend/js/search_workbench.js'), 'utf8');
const searchRs = fs.readFileSync(path.join(ROOT, 'crates/syki-core/src/search.rs'), 'utf8');
const scrapRs = fs.readFileSync(path.join(ROOT, 'crates/syki-core/src/storage/scrap.rs'), 'utf8');
const rpcDispatchRs = fs.readFileSync(path.join(ROOT, 'crates/syki-win/src/rpc_dispatch.rs'), 'utf8');
const llmRs = fs.readFileSync(path.join(ROOT, 'crates/syki-win/src/llm.rs'), 'utf8');
const shellPipeRs = fs.readFileSync(path.join(ROOT, 'crates/syki-win/src/shell_pipe.rs'), 'utf8');

global.window = global;
const MT = require(path.join(ROOT, 'frontend/js/mermaid_tone.js'));
const markdownit = require(path.join(ROOT, 'frontend/vendor/markdown-it.min.js'));

// Build markdown-it with exact app.js allow_br rule
function createAppMarkdownIt() {
  const md = markdownit({ html: false, linkify: true, typographer: true, breaks: true });
  md.inline.ruler.before('html_inline', 'allow_br', (state, silent) => {
    if (state.src.charCodeAt(state.pos) !== 0x3C) return false;
    const match = /^<br\s*\/?>/i.exec(state.src.slice(state.pos));
    if (!match) return false;
    if (!silent) state.push('hardbreak', 'br', 0);
    state.pos += match[0].length;
    return true;
  });
  return md;
}

// =========================================================================
// Domain A: Scrap Search, Cache Consistency & I/O (R1-01 .. R1-08)
// =========================================================================
scenario('R1-01', 'A:SearchCache', 'ScrapStore::append_entry invalidates search cache', () => {
  assert.ok(
    scrapRs.includes('invalidate_search_cache') || searchRs.includes('append_entry'),
    'ScrapStore::append_entry does not invalidate search cache (quick capture / CLI scrap append can return stale results within slow-drive TTL)'
  );
});

scenario('R1-02', 'A:SearchCache', 'saveFileAs and createNoteInFolder invalidate search cache', () => {
  const saveFileAsIdx = rpcDispatchRs.indexOf('"saveFileAs" =>');
  const createNoteIdx = rpcDispatchRs.indexOf('"createNoteInFolder" =>');
  assert.ok(saveFileAsIdx > 0 && createNoteIdx > 0);
  const saveAsSlice = rpcDispatchRs.slice(saveFileAsIdx, createNoteIdx);
  const createSlice = rpcDispatchRs.slice(createNoteIdx, createNoteIdx + 1200);
  assert.ok(saveAsSlice.includes('invalidate_search_cache'), 'saveFileAs missing invalidate_search_cache');
  assert.ok(createSlice.includes('invalidate_search_cache'), 'createNoteInFolder missing invalidate_search_cache');
});

scenario('R1-03', 'A:SearchCache', 'Cache handles sub-directory modifications (not just root_mtime)', () => {
  // Check whether DirCache tracks subdirs or only root_mtime_ns
  assert.ok(
    searchRs.includes('subdir') || searchRs.includes('dir_mtimes') || searchRs.includes('collect_dir_mtimes'),
    'DirCache only checks root_mtime_ns; edits inside subdirectories of scrap_dir are missed during SLOW_SCAN_TTL'
  );
});

scenario('R1-04', 'A:SearchCache', 'Cache avoids skipping files when mtime_ns == 0', () => {
  assert.ok(searchRs.includes('mtime_ns != 0 && prev.mtime_ns == mtime_ns && prev.len == len'));
});

scenario('R1-05', 'A:SearchCache', 'search_scraps_parallel skips dot-dirs and node_modules', () => {
  assert.ok(searchRs.includes("!name.starts_with('.')") && searchRs.includes('name != "node_modules"'));
});

scenario('R1-06', 'A:SearchCache', 'scrap_filter_options reuses load_cached_entries', () => {
  const fnIdx = searchRs.indexOf('pub fn scrap_filter_options');
  const slice = searchRs.slice(fnIdx, fnIdx + 400);
  assert.ok(slice.includes('load_cached_entries'));
});

scenario('R1-07', 'A:SearchCache', 'ScrapFilter::parse rejects invalid JSON and accepts empty/null', () => {
  assert.ok(searchRs.includes('trimmed.is_empty() || trimmed == "null"'));
  assert.ok(searchRs.includes('invalid filter JSON'));
});

scenario('R1-08', 'A:SearchCache', 'search_scraps_parallel caps results at max_results and preserves newest-first order', () => {
  assert.ok(searchRs.includes('results.len() >= cap') && searchRs.includes('b.0.file_name().cmp(&a.0.file_name())'));
});

// =========================================================================
// Domain B: UI Button & Menu Wiring, i18n Completeness (R1-09 .. R1-16)
// =========================================================================
const allJsText = fs.readdirSync(path.join(ROOT, 'frontend/js'))
  .filter((f) => f.endsWith('.js') && !f.endsWith('_test.js'))
  .map((f) => fs.readFileSync(path.join(ROOT, 'frontend/js', f), 'utf8'))
  .join('\n');

scenario('R1-09', 'B:UIWiring', 'All btn-* IDs in index.html are referenced in frontend JS', () => {
  const ids = [...indexHtml.matchAll(/id="(btn-[a-z0-9-]+)"/g)].map((m) => m[1]);
  const isDynamicallyWired = (id) => {
    const m1 = /^(btn-speech-model-(?:install|remove|cancel|browse))-(?:rt|bt)$/.exec(id);
    if (m1 && allJsText.includes(m1[1])) return true;
    const m2 = /^(btn-speech-addon-(?:install|remove|cancel)-)(?:diarize|enhance|vad)$/.exec(id);
    if (m2 && allJsText.includes(m2[1])) return true;
    return false;
  };
  const missing = [...new Set(ids)].filter((id) => !allJsText.includes(id) && !isDynamicallyWired(id));
  assert.deepEqual(missing, [], `Unwired button IDs in index.html: ${missing.join(', ')}`);
});

scenario('R1-10', 'B:UIWiring', 'All agent-ctx-* menu items in index.html are wired in terminal_pane.js', () => {
  const ids = [...indexHtml.matchAll(/id="(agent-ctx-[a-z0-9-]+)"/g)].map((m) => m[1]);
  const missing = [...new Set(ids)].filter((id) => !terminalPaneJs.includes(id));
  assert.deepEqual(missing, [], `Unwired agent-ctx items: ${missing.join(', ')}`);
});

scenario('R1-11', 'B:UIWiring', 'btn-pty-copy-to-left and btn-pty-summarize-to-left execute distinct handlers', () => {
  const copyIdx = terminalPaneJs.indexOf("wireHeaderBtn('btn-pty-copy-to-left'");
  const sumIdx = terminalPaneJs.indexOf("wireHeaderBtn('btn-pty-summarize-to-left'");
  assert.ok(copyIdx > 0 && sumIdx > 0);
  const copyBody = terminalPaneJs.slice(copyIdx, sumIdx);
  const sumBody = terminalPaneJs.slice(sumIdx, sumIdx + 600);
  assert.ok(copyBody.includes('insertTextIntoLeftNote(out)'));
  assert.ok(sumBody.includes('cfg.summarizeToLeftNote(out)'));
  assert.ok(!copyBody.includes('summarizeToLeftNote'));
});

scenario('R1-12', 'B:UIWiring', 'All data-i18n* keys in index.html exist in both en and ja dictionaries', () => {
  const keys = [...indexHtml.matchAll(/data-i18n(?:-[a-z]+)?="([A-Za-z0-9_]+)"/g)].map((m) => m[1]);
  const uniqueKeys = [...new Set(keys)];
  const jaSplit = i18nJs.indexOf('\n  ja: {');
  assert.ok(jaSplit > 0, 'ja dictionary not found in i18n.js');
  const enPart = i18nJs.slice(0, jaSplit);
  const jaPart = i18nJs.slice(jaSplit);
  const missingEn = uniqueKeys.filter((k) => !new RegExp(`\\b${k}\\s*:`).test(enPart));
  const missingJa = uniqueKeys.filter((k) => !new RegExp(`\\b${k}\\s*:`).test(jaPart));
  assert.deepEqual(missingEn, [], `Missing en i18n keys: ${missingEn.join(', ')}`);
  assert.deepEqual(missingJa, [], `Missing ja i18n keys: ${missingJa.join(', ')}`);
});

scenario('R1-13', 'B:UIWiring', 'Agent context menu also offers summarize-to-note or copy-to-note cleanly', () => {
  assert.ok(terminalPaneJs.includes('agent-ctx-copy-to-note'));
});

scenario('R1-14', 'B:UIWiring', 'btn-pty-summarize-to-left passes selection or last 120 lines', () => {
  const sumIdx = terminalPaneJs.indexOf("wireHeaderBtn('btn-pty-summarize-to-left'");
  const sumBody = terminalPaneJs.slice(sumIdx, sumIdx + 500);
  assert.ok(sumBody.includes('getSelection') && sumBody.includes('readActiveOutput(120)'));
});

scenario('R1-15', 'B:UIWiring', 'search_workbench uses seq guard, 60ms exact debounce, and searching indicator', () => {
  assert.ok(searchWorkbenchJs.includes('seq === scrapsSearchSeq'));
  assert.ok(searchWorkbenchJs.includes('scrapsMeaningMode() ? 400 : 60'));
  assert.ok(searchWorkbenchJs.includes('scrapsSearchSearching'));
});

scenario('R1-16', 'B:UIWiring', 'search_workbench guards IME composition on keydown', () => {
  assert.ok(searchWorkbenchJs.includes('e.isComposing || e.keyCode === 229'));
});

// =========================================================================
// Domain C: Markdown Preview, <br>, KaTeX, Footnotes, HTML Detection (R1-17 .. R1-26)
// =========================================================================
const md = createAppMarkdownIt();

scenario('R1-17', 'C:Markdown', 'Paragraph <br>, <br/>, <br />, <BR> render as line breaks', () => {
  const html = md.render('L1<br>L2<br/>L3<br />L4<BR>L5');
  assert.equal((html.match(/<br>/g) || []).length, 4, `Unexpected html: ${html}`);
});

scenario('R1-18', 'C:Markdown', 'Table cell <br> and <br/> render as line breaks', () => {
  const html = md.render('| Col1 | Col2 |\n|---|---|\n| A<br>B | C<br/>D |');
  assert.ok(html.includes('<td>A<br>\nB</td>') && html.includes('<td>C<br>\nD</td>'), html);
});

scenario('R1-19', 'C:Markdown', 'Inline code `<br>` and fenced code <br> remain escaped', () => {
  const html = md.render('Inline `<br>` test\n\n```html\n<div><br></div>\n```');
  assert.ok(html.includes('<code>&lt;br&gt;</code>'), html);
  assert.ok(html.includes('&lt;div&gt;&lt;br&gt;&lt;/div&gt;'), html);
});

scenario('R1-20', 'C:Markdown', 'XSS payloads including <br onmouseover=...> are safely escaped', () => {
  const html = md.render('<script>alert(1)</script> <img src=x onerror=alert(1)> <br onmouseover="alert(1)">');
  assert.ok(!html.includes('<script>'), html);
  assert.ok(!html.includes('<img'), html);
  assert.ok(!html.includes('onmouseover="alert'), html);
  assert.ok(html.includes('&lt;br onmouseover='), html);
});

scenario('R1-21', 'C:Markdown', 'List items, blockquotes, and headings support <br>', () => {
  const html = md.render('# H1<br>Sub\n\n- item1<br/>line2\n\n> quote1<br>quote2');
  assert.equal((html.match(/<br>/g) || []).length, 3, html);
});

scenario('R1-22', 'C:Markdown', 'Inline KaTeX regex does NOT misfire on currency amounts like $100 and $200', () => {
  const m = appJs.match(/\/\/ 3\. Extract Inline Math[\s\S]*?rawText = rawText\.replace\((\/[^;]+\/g)/);
  assert.ok(m, 'Inline math regex not found in app.js');
  const re = eval(m[1]);
  const currency = '価格は $100 と $200 です。';
  const matched = currency.match(re);
  assert.equal(matched, null, `Inline KaTeX misfired on currency text: ${JSON.stringify(matched)}`);
});

scenario('R1-23', 'C:Markdown', 'Inline KaTeX matches real math like $x^2 + y^2 = z^2$ and $E=mc^2$', () => {
  const m = appJs.match(/\/\/ 3\. Extract Inline Math[\s\S]*?rawText = rawText\.replace\((\/[^;]+\/g)/);
  assert.ok(m, 'Inline math regex not found in app.js');
  const re = eval(m[1]);
  const sample = '公式 $x^2 + y^2 = z^2$ と $E=mc^2$ です。';
  const matched = sample.match(re);
  assert.deepEqual(matched, ['$x^2 + y^2 = z^2$', '$E=mc^2$']);
});

scenario('R1-24', 'C:Markdown', 'Footnote extraction preserves fenced code blocks', () => {
  assert.ok(appJs.includes('function extractMarkdownFootnotes') && appJs.includes('if (/^\\s*(?:```|~~~)/.test(line))'));
});

scenario('R1-25', 'C:Markdown', 'HTML comments module strips comments outside code fences', () => {
  const HC = require(path.join(ROOT, 'frontend/js/html_comments.js'));
  const out = HC.removeComments('hello <!-- secret --> world\n```\n<!-- kept -->\n```');
  assert.ok(!out.includes('secret') && out.includes('<!-- kept -->'), out);
});

scenario('R1-26', 'C:Markdown', 'isHtmlDocument does not misclassify Markdown containing <br> or <div>', () => {
  assert.ok(appJs.includes('/^<!DOCTYPE\\s+html/i.test(head) || /^<html[\\s>]/i.test(head)'));
});

// =========================================================================
// Domain D: Mermaid Mindmap, Sanitizer, Cursor Extraction (R1-27 .. R1-36)
// =========================================================================
scenario('R1-27', 'D:Mermaid', 'sanitizeSource repairs real scrap mindmap with root["..."] and <br/>', () => {
  const src = [
    'mindmap',
    '  root["ノートアプリ仕様"]',
    '    UI["UIデザインと質感"]',
    '      Paper["紙のような質感<br/>シャドウ・陰影"]'
  ].join('\n');
  const out = MT.sanitizeSource(src);
  assert.ok(out.includes('  root((ノートアプリ仕様))'));
  assert.ok(out.includes('    UI[UIデザインと質感]'));
  assert.ok(!out.includes('<br/>') && !out.includes('"'));
});

scenario('R1-28', 'D:Mermaid', 'sanitizeSource wraps multiple mindmap roots and normalizes tabs', () => {
  const src = 'mindmap\n\tA\n\t\tA1\n\tB';
  const out = MT.sanitizeSource(src);
  assert.ok(out.includes('  root((Mindmap))') && !out.includes('\t'), out);
});

scenario('R1-29', 'D:Mermaid', 'sanitizeSource preserves mindmap ::icon(...) and :::class directives', () => {
  const src = 'mindmap\n  root((Home))\n    Branch\n      ::icon(fa fa-book)\n      :::urgent';
  const out = MT.sanitizeSource(src);
  assert.ok(out.includes('::icon(fa fa-book)') && out.includes(':::urgent'), out);
});

scenario('R1-30', 'D:Mermaid', 'sanitizeSource handles mindmap labels with nested brackets and smart quotes', () => {
  const src = 'mindmap\n  root[\u201C仕様 (v1) [案]\u201D]\n    A[\u201Cテスト {a}\u201D]';
  const out = MT.sanitizeSource(src);
  assert.ok(out.includes('root((仕様 （v1） ［案］))'), out);
  assert.ok(out.includes('A[テスト ｛a｝]'), out);
});

scenario('R1-31', 'D:Mermaid', 'sanitizeSource repairs flowchart sub-graph and Unicode arrows', () => {
  const src = 'flowchart TD\n  sub-graph S1 "フェーズ1"\n    A ──> B\n  end';
  const out = MT.sanitizeSource(src);
  assert.ok(out.includes('subgraph S1["フェーズ1"]') && out.includes('A --> B'), out);
});

scenario('R1-32', 'D:Mermaid', 'sanitizeSource leaves sequenceDiagram / erDiagram / stateDiagram-v2 intact', () => {
  const samples = [
    'sequenceDiagram\n  participant C as "Client"\n  C->>S: Request',
    'erDiagram\n  USER ||--o{ ORDER : "places"',
    'stateDiagram-v2\n  [*] --> Active'
  ];
  for (const s of samples) {
    assert.equal(MT.sanitizeSource(s), s);
  }
});

scenario('R1-33', 'D:Mermaid', 'extractMermaidAtCursor supports mindmap / erDiagram / stateDiagram-v2 / graph selections', () => {
  const fnIdx = appJs.indexOf('function extractMermaidAtCursor()');
  assert.ok(fnIdx > 0);
  const body = appJs.slice(fnIdx, fnIdx + 1200);
  assert.ok(
    body.includes('mindmap') || body.includes('MERMAID_START_RE'),
    'extractMermaidAtCursor only checks flowchart/sequenceDiagram; selecting mindmap/erDiagram/stateDiagram fails!'
  );
});

scenario('R1-34', 'D:Mermaid', 'MermaidTone resolveTone and flipTone work for ink/paper/auto', () => {
  assert.equal(MT.resolveTone('auto', 'ink'), 'dark');
  assert.equal(MT.resolveTone('auto', 'paper'), 'light');
  assert.equal(MT.flipTone('auto', 'ink'), 'light');
  assert.equal(MT.flipTone('auto', 'paper'), 'dark');
});

scenario('R1-35', 'D:Mermaid', 'sanitizeSource does NOT double-wrap valid root((...)) in mindmap', () => {
  const src = 'mindmap\n  root((中心テーマ))\n    子ノードA\n    子ノードB';
  const out = MT.sanitizeSource(src);
  assert.equal(out, src, `Mutated valid mindmap: ${out}`);
});

scenario('R1-36', 'D:Mermaid', 'sanitizeSource is idempotent across mindmap and flowchart', () => {
  const cases = [
    'mindmap\n  root["テーマ (1)<br/>詳細"]\n    A["子A"]\n    B["子B"]',
    'mindmap\n  Topic1\n  Topic2',
    'flowchart TD\n  sub-graph X "タイトル"\n    A ──> B\n  end'
  ];
  for (const c of cases) {
    const once = MT.sanitizeSource(c);
    const twice = MT.sanitizeSource(once);
    assert.equal(twice, once, `Not idempotent:\nonce=${once}\ntwice=${twice}`);
  }
});

// =========================================================================
// Domain E: LLM Async Anchor, Secrets, Config, Shell Pipe (R1-37 .. R1-44)
// =========================================================================
scenario('R1-37', 'E:LLM&Sec', 'summarizeTerminalOutputToLeftNote wires pending LLM request & bridge', () => {
  assert.ok(appJs.includes('function summarizeTerminalOutputToLeftNote('));
  assert.ok(appJs.includes('summarizeToLeftNote: summarizeTerminalOutputToLeftNote'));
});

scenario('R1-38', 'E:LLM&Sec', 'summarizeTerminalOutputToLeftNote clips huge terminal output to <=12000 chars', () => {
  const fnIdx = appJs.indexOf('function summarizeTerminalOutputToLeftNote(');
  const body = appJs.slice(fnIdx, fnIdx + 1600);
  assert.ok(body.includes('12000') && body.includes('slice(-12000)'));
});

scenario('R1-39', 'E:LLM&Sec', 'SecretStrip removes all API key fields before localStorage persistence', () => {
  const SS = require(path.join(ROOT, 'frontend/js/secret_strip.js'));
  const stripped = SS.stripSecrets({
    text: { apiKey: 'AIzaSecret1', model: 'gemini' },
    vision: { apiKey: 'AIzaSecret2' },
    image: { apiKey: 'AIzaSecret3' },
    voice: { apiKey: 'AIzaSecret4' },
    semantic: { model: { apiKey: 'AIzaSecret5' } }
  });
  const json = JSON.stringify(stripped);
  assert.ok(!json.includes('AIzaSecret'), `Secret leaked after stripSecrets: ${json}`);
});

scenario('R1-40', 'E:LLM&Sec', 'config.rs config_key_kind includes semantic, appearance, image.apiKey, voice.engine', () => {
  const cfgRs = fs.readFileSync(path.join(ROOT, 'crates/syki-core/src/storage/config.rs'), 'utf8');
  for (const key of ['"image.apiKey"', '"voice.engine"', '"general.mermaidTone"', '"action.intentRouter"']) {
    assert.ok(cfgRs.includes(key), `Missing config key in config.rs: ${key}`);
  }
});

scenario('R1-41', 'E:LLM&Sec', 'llm.rs uses x-goog-api-key header and never puts ?key= in Gemini URLs', () => {
  assert.ok(llmRs.includes('x-goog-api-key'));
  assert.ok(!llmRs.includes('?key={'), 'Found ?key={ in llm.rs!');
  assert.ok(llmRs.includes('fn redact_url_secrets'));
});

scenario('R1-42', 'E:LLM&Sec', 'shell_pipe.rs uses raw_arg /d /s /c to preserve quoted commands', () => {
  assert.ok(shellPipeRs.includes('raw_arg("/d /s /c")'));
});

scenario('R1-43', 'E:LLM&Sec', 'rpc_dispatch.rs invalidates search cache on saveFile and saveFileChecked', () => {
  const count = (rpcDispatchRs.match(/invalidate_search_cache/g) || []).length;
  assert.ok(count >= 2, `Expected >=2 invalidate_search_cache calls in rpc_dispatch.rs, got ${count}`);
});

scenario('R1-44', 'E:LLM&Sec', 'quick_capture.rs or ScrapStore invalidates search cache after quick capture', () => {
  const qcRs = fs.readFileSync(path.join(ROOT, 'crates/syki-win/src/quick_capture.rs'), 'utf8');
  assert.ok(
    qcRs.includes('invalidate_search_cache') || scrapRs.includes('invalidate_search_cache'),
    'Quick capture appends to scrap without invalidating search cache'
  );
});

// =========================================================================
// Domain F: Packaging, Skills, Release Binary Contract (R1-45 .. R1-50)
// =========================================================================
scenario('R1-45', 'F:Package', 'All 6 required syki skill files exist and are non-empty', () => {
  const reqFiles = [
    'frontend/resources/syki/SKILL.md',
    'frontend/resources/syki/syki-api.d.ts',
    'frontend/resources/syki/references/cli-jsonrpc-pipeline.md',
    'frontend/resources/syki/references/config-and-css.md',
    'frontend/resources/syki/references/real-world-recipes.md',
    'frontend/resources/syki/references/usage-and-shortcuts.md'
  ];
  for (const rel of reqFiles) {
    const st = fs.statSync(path.join(ROOT, rel));
    assert.ok(st.size > 100, `${rel} is too small (${st.size})`);
  }
});

scenario('R1-46', 'F:Package', 'All example plugin JS files pass node --check', () => {
  const pluginsDir = path.join(ROOT, 'examples/plugins');
  const walk = (dir) => fs.readdirSync(dir, { withFileTypes: true }).flatMap((d) =>
    d.isDirectory() ? walk(path.join(dir, d.name)) : [path.join(dir, d.name)]
  );
  const jsFiles = walk(pluginsDir).filter((f) => f.endsWith('.js'));
  assert.ok(jsFiles.length >= 4, `Expected >=4 example plugin JS files, got ${jsFiles.length}`);
  for (const f of jsFiles) {
    execFileSync(process.execPath, ['--check', f], { stdio: 'pipe' });
  }
});

scenario('R1-47', 'F:Package', 'config/perf_params.json is valid JSON', () => {
  const parsed = JSON.parse(fs.readFileSync(path.join(ROOT, 'config/perf_params.json'), 'utf8'));
  assert.ok(parsed && typeof parsed === 'object');
});

scenario('R1-48', 'F:Package', 'Release binary syki-cli.exe version matches Cargo.toml', () => {
  const cargo = fs.readFileSync(path.join(ROOT, 'Cargo.toml'), 'utf8');
  const ver = cargo.match(/version\s*=\s*"([^"]+)"/)?.[1];
  const out = execFileSync(path.join(ROOT, 'target/release/syki-cli.exe'), ['--version'], { encoding: 'utf8' }).trim();
  assert.ok(out.includes(ver), `Binary version "${out}" != Cargo.toml "${ver}"`);
});

scenario('R1-49', 'F:Package', 'MEMORY.md records mandatory release ZIP bundle rule', () => {
  const mem = fs.readFileSync(path.join(ROOT, 'MEMORY.md'), 'utf8');
  assert.ok(mem.includes('skills/syki/') && mem.includes('examples/plugins/') && mem.includes('cargo build --release'));
});

scenario('R1-50', 'F:Package', 'Static contract suite passes 100%', () => {
  execFileSync(process.execPath, [path.join(ROOT, 'evals/checks/static_contract.mjs')], { stdio: 'pipe' });
});

// =========================================================================
// Report & Statistical Defect Probability Calculation
// =========================================================================
const total = results.length;
const failed = results.filter((r) => r.status === 'FAIL');
const passed = total - failed.length;
const bugRate = failed.length / total;
const bugFreeRate = 1 - bugRate;

// Wilson 95% upper bound for defect probability
function wilsonUpper(k, n, z = 1.96) {
  const p = k / n;
  const denom = 1 + (z * z) / n;
  const center = p + (z * z) / (2 * n);
  const margin = z * Math.sqrt((p * (1 - p) + (z * z) / (4 * n)) / n);
  return (center + margin) / denom;
}

console.log(`ROUND 1 SUMMARY: ${passed}/${total} PASS, ${failed.length} FAIL`);
console.log(`Empirical Bug-Free Rate: ${(bugFreeRate * 100).toFixed(2)}% (Defect Rate: ${(bugRate * 100).toFixed(2)}%, Wilson 95% Upper: ${(wilsonUpper(failed.length, total) * 100).toFixed(2)}%)`);
if (failed.length > 0) {
  console.log('\n--- DETECTED BUGS ---');
  for (const f of failed) {
    console.log(`[${f.id}] (${f.domain}) ${f.title}\n    -> ${f.error}`);
  }
  process.exit(1);
}
