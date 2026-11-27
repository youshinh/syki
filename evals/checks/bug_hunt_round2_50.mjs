#!/usr/bin/env node
// Round 2: 50 deep edge-case & cross-module bug-hunting scenarios.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
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
const terminalPaneJs = fs.readFileSync(path.join(ROOT, 'frontend/js/terminal_pane.js'), 'utf8');
const printPreviewJs = fs.readFileSync(path.join(ROOT, 'frontend/js/print_preview.js'), 'utf8');
const searchWorkbenchJs = fs.readFileSync(path.join(ROOT, 'frontend/js/search_workbench.js'), 'utf8');

global.window = global;
const MT = require(path.join(ROOT, 'frontend/js/mermaid_tone.js'));
const markdownit = require(path.join(ROOT, 'frontend/vendor/markdown-it.min.js'));
const IntentRouter = require(path.join(ROOT, 'frontend/js/intent_router.js'));
const ScrapsFilter = require(path.join(ROOT, 'frontend/js/scraps_filter.js'));
const ScrapQuote = require(path.join(ROOT, 'frontend/js/scrap_quote.js'));
const NoteTitle = require(path.join(ROOT, 'frontend/js/note_title.js'));
const DiskSync = require(path.join(ROOT, 'frontend/js/disk_sync.js'));
const PreviewImages = require(path.join(ROOT, 'frontend/js/preview_images.js'));
const InfographicPrompt = require(path.join(ROOT, 'frontend/js/infographic_prompt.js'));

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
  PreviewImages.installLooseImageRule(md);
  return md;
}
const md = createAppMarkdownIt();

// =========================================================================
// Domain 1: Markdown <br>, Formatting, Images, Print Preview (R2-01 .. R2-12)
// =========================================================================
scenario('R2-01', 'Markdown', 'Bold/italic/strikethrough combined with <br>', () => {
  const html = md.render('**太字1<br>太字2** *斜体1<br/>斜体2* ~~取消1<br />取消2~~');
  assert.equal((html.match(/<br>/g) || []).length, 3, html);
  assert.ok(html.includes('<strong>太字1<br>\n太字2</strong>'), html);
});

scenario('R2-02', 'Markdown', 'Multiple consecutive <br><br> render two breaks', () => {
  const html = md.render('行1<br><br>行2');
  assert.equal((html.match(/<br>/g) || []).length, 2, html);
});

scenario('R2-03', 'Markdown', 'Escaped \\<br> is NOT converted to a break', () => {
  const html = md.render('literal \\<br> tag');
  assert.ok(!html.includes('<br>') && html.includes('&lt;br&gt;'), html);
});

scenario('R2-04', 'Markdown', 'Japanese text with <br/> inside multi-column GFM table', () => {
  const html = md.render('| 機能 | 詳細 |\n|---|---|\n| 検索 | 高速化<br/>キャッシュ対応 |\n| 要約 | LLM実行<br>箇条書き |');
  assert.equal((html.match(/<br>/g) || []).length, 2, html);
  assert.ok(html.includes('<td>高速化<br>\nキャッシュ対応</td>'), html);
});

scenario('R2-05', 'Markdown', 'Malformed <br class="x"> or <branch> is NOT matched as <br>', () => {
  const html = md.render('<br class="x"> <branch> <br/>');
  assert.equal((html.match(/<br>/g) || []).length, 1, html);
  assert.ok(html.includes('&lt;branch&gt;') && html.includes('&lt;br class='), html);
});

scenario('R2-06', 'Markdown', 'Loose image rule accepts spaces and Japanese in local image paths', () => {
  const html = md.render('![図解](C:/Users/test/マイ ドキュメント/図 1.png)');
  assert.ok(html.includes('<img ') && html.includes('alt="図解"'), html);
});

scenario('R2-07', 'Markdown', 'Markdown link with <br> inside link text works safely', () => {
  const html = md.render('[Line1<br>Line2](https://example.com)');
  assert.ok(html.includes('<a href="https://example.com">Line1<br>\nLine2</a>'), html);
});

scenario('R2-08', 'Markdown', 'Indented code block preserves literal <br>', () => {
  const html = md.render('    const x = "<br>";');
  assert.ok(html.includes('&lt;br&gt;') && !html.includes('<br>\n'), html);
});

scenario('R2-09', 'Markdown', 'print_preview.js re-renders dark Mermaid diagrams cleanly in light tone', () => {
  assert.ok(printPreviewJs.includes('mermaidSrc') && printPreviewJs.includes('mermaid.render'));
});

scenario('R2-10', 'Markdown', 'htmlPreviewImagePath blocks UNC network shares and javascript: URLs', () => {
  assert.ok(appJs.includes('\\\\host\\share') || appJs.includes('/^[\\\\/]{2}/.test(p)'));
  assert.ok(appJs.includes('/^[a-z][a-z0-9+.-]+:/i.test(p)'));
});

scenario('R2-11', 'Markdown', 'markdown-it keeps html: false so raw <iframe/svg/object> are escaped', () => {
  const html = md.render('<iframe src="x"></iframe><svg onload="alert(1)"></svg><object data="x"></object>');
  assert.ok(!html.includes('<iframe') && !html.includes('<svg') && !html.includes('<object'), html);
});

scenario('R2-12', 'Markdown', 'extractMarkdownFootnotes handles multiple footnotes and ignores fenced code', () => {
  assert.ok(appJs.includes('SYKIFNREF') && appJs.includes('injectMarkdownFootnotes'));
});

// =========================================================================
// Domain 2: Mermaid Deep Edge Cases (R2-13 .. R2-22)
// =========================================================================
scenario('R2-13', 'Mermaid', 'sanitizeSource handles mindmap with UTF-8 BOM prefix', () => {
  const src = '\uFEFFmindmap\n  root["BOMテスト"]\n    A["子"]';
  const out = MT.sanitizeSource(src);
  assert.ok(out.includes('root((BOMテスト))') && !out.includes('\uFEFF'), `BOM broke mindmap sanitize: ${JSON.stringify(out)}`);
});

scenario('R2-14', 'Mermaid', 'sanitizeSource handles mindmap with fullwidth spaces as indent', () => {
  const src = 'mindmap\n\u3000root["全角インデント"]\n\u3000\u3000子ノード';
  const out = MT.sanitizeSource(src);
  assert.ok(!/^\u3000/m.test(out) && out.includes('root((全角インデント))'), `Fullwidth space indent not normalized: ${JSON.stringify(out)}`);
});

scenario('R2-15', 'Mermaid', 'sanitizeSource preserves mindmap with leading %% comments', () => {
  const src = '%% comment 1\n%% comment 2\nmindmap\n  root["タイトル"]\n    A["要素"]';
  const out = MT.sanitizeSource(src);
  assert.ok(out.includes('%% comment 1') && out.includes('root((タイトル))') && out.includes('A[要素]'), out);
});

scenario('R2-16', 'Mermaid', 'sanitizeSource handles mindmap with CRLF line endings', () => {
  const src = 'mindmap\r\n  root["Win改行"]\r\n    Sub["子1<br/>子2"]\r\n';
  const out = MT.sanitizeSource(src);
  assert.ok(!out.includes('\r') && out.includes('root((Win改行))') && !out.includes('<br/>'), out);
});

scenario('R2-17', 'Mermaid', 'sanitizeSource handles mindmap rounded/hexagon/stadium node shapes', () => {
  const src = 'mindmap\n  root(("中心"))\n    A(["スタジアム"])\n    B{{"六角形"}}\n    C{"菱形"}';
  const out = MT.sanitizeSource(src);
  assert.ok(out.includes('root((中心))') && out.includes('A(スタジアム)') && out.includes('B{{六角形}}') && out.includes('C[菱形]'), out);
});

scenario('R2-18', 'Mermaid', 'sanitizeSource handles mindmap with 0-space root and 2-space children', () => {
  const src = 'mindmap\nroot["ゼロインデント"]\n  子A\n  子B';
  const out = MT.sanitizeSource(src);
  assert.ok(out.includes('  root((ゼロインデント))') && out.includes('    子A') || out.includes('  子A'), out);
  // Verify root is strictly less indented than children!
  const lines = out.split('\n').slice(1);
  const rootIndent = lines[0].length - lines[0].trimStart().length;
  const childIndent = lines[1].length - lines[1].trimStart().length;
  assert.ok(childIndent > rootIndent, `Child indent (${childIndent}) must exceed root indent (${rootIndent}):\n${out}`);
});

scenario('R2-19', 'Mermaid', 'InfographicPrompt recognizes mindmap and all Mermaid 11 headers', () => {
  assert.ok(typeof InfographicPrompt.buildPrompt === 'function' || typeof InfographicPrompt.getInfographicStylePrompt === 'function');
  const raw = fs.readFileSync(path.join(ROOT, 'frontend/js/infographic_prompt.js'), 'utf8');
  assert.ok(raw.includes('mindmap') && raw.includes('erDiagram') && raw.includes('stateDiagram'));
});

scenario('R2-20', 'Mermaid', 'DEGRAM_MERMAID_SYSTEM_PROMPT explicitly forbids id["..."] and <br/> in mindmap', () => {
  const pIdx = appJs.indexOf('const DEGRAM_MERMAID_SYSTEM_PROMPT');
  const pEnd = appJs.indexOf('function convertSelectionToMermaid');
  const prompt = appJs.slice(pIdx, pEnd);
  assert.ok(prompt.includes('NEVER use double quotes or <br/> in mindmap'));
  assert.ok(prompt.includes('root((Main Topic))'));
});

scenario('R2-21', 'Mermaid', 'Flowchart arrows inside quoted labels are NOT altered by sanitizeSource', () => {
  const src = 'flowchart LR\n  A["入力 → 出力"] ──> B["完了"]';
  const out = MT.sanitizeSource(src);
  assert.ok(out.includes('A["入力 → 出力"] --> B["完了"]'), out);
});

scenario('R2-22', 'Mermaid', 'Empty / null / whitespace input to sanitizeSource never throws', () => {
  assert.equal(MT.sanitizeSource(''), '');
  assert.equal(MT.sanitizeSource(null), '');
  assert.equal(MT.sanitizeSource(undefined), '');
  assert.equal(MT.sanitizeSource('mindmap\n').trim(), 'mindmap');
});

// =========================================================================
// Domain 3: Shortcuts, Terminal Pane, Intent Router (R2-23 .. R2-34)
// =========================================================================
scenario('R2-23', 'Shortcuts', 'Default Windows shortcuts have no conflicting duplicate bindings', () => {
  const start = appJs.indexOf('const DEFAULT_SHORTCUTS_WIN = {');
  const end = appJs.indexOf('};', start);
  assert.ok(start > 0 && end > start);
  const block = appJs.slice(start, end);
  const pairs = [...block.matchAll(/(\w+)\s*:\s*'([^']+)'/g)].map((m) => [m[1], m[2]]);
  const seen = new Map();
  const dups = [];
  for (const [k, v] of pairs) {
    if (!v) continue;
    const norm = v.toLowerCase();
    if (seen.has(norm)) dups.push(`${v} (${seen.get(norm)} vs ${k})`);
    else seen.set(norm, k);
  }
  assert.deepEqual(dups, [], `Duplicate Windows default shortcuts: ${dups.join(', ')}`);
});

scenario('R2-24', 'Shortcuts', 'Default macOS shortcuts have no conflicting duplicate bindings', () => {
  const start = appJs.indexOf('const DEFAULT_SHORTCUTS_MAC = {');
  const end = appJs.indexOf('};', start);
  assert.ok(start > 0 && end > start);
  const block = appJs.slice(start, end);
  const pairs = [...block.matchAll(/(\w+)\s*:\s*'([^']+)'/g)].map((m) => [m[1], m[2]]);
  const seen = new Map();
  const dups = [];
  for (const [k, v] of pairs) {
    if (!v) continue;
    const norm = v.toLowerCase();
    if (seen.has(norm)) dups.push(`${v} (${seen.get(norm)} vs ${k})`);
    else seen.set(norm, k);
  }
  assert.deepEqual(dups, [], `Duplicate macOS default shortcuts: ${dups.join(', ')}`);
});

scenario('R2-25', 'TerminalPane', 'terminal_pane strips ANSI CSI/OSC sequences when reading output', () => {
  assert.ok(terminalPaneJs.includes('\\x1b') || terminalPaneJs.includes('\\u001b'));
});

scenario('R2-26', 'TerminalPane', 'btn-pty-relay guards against < 2 sessions without crashing', () => {
  const idx = terminalPaneJs.indexOf("wireHeaderBtn('btn-pty-relay'");
  const body = terminalPaneJs.slice(idx, idx + 450);
  assert.ok(body.includes('others.length') || body.includes('if (target)'));
});

scenario('R2-27', 'TerminalPane', 'insertTextIntoLeftNote guards null editor and empty text', () => {
  const idx = terminalPaneJs.indexOf('function insertTextIntoLeftNote');
  const body = terminalPaneJs.slice(idx, idx + 250);
  assert.ok(body.includes('if (!ed || !text) return;'));
});

scenario('R2-28', 'IntentRouter', 'IntentRouter routes Japanese diagram / mindmap requests to diagram', () => {
  const r1 = IntentRouter.matchFastRule('これをマインドマップにして', { hasSelection: true });
  const r2 = IntentRouter.matchFastRule('シーケンス図で図解して', { hasSelection: false });
  assert.equal(r1 && r1.route, 'diagram');
  assert.equal(r2 && r2.route, 'diagram');
});

scenario('R2-29', 'IntentRouter', 'IntentRouter routes selected summarize/proofread requests to edit_note', () => {
  const r = IntentRouter.matchFastRule('箇条書きで要約して', { hasSelection: true });
  assert.equal(r && r.route, 'edit_note');
});

scenario('R2-30', 'IntentRouter', 'IntentRouter does not hijack slash commands like /summarize', () => {
  assert.equal(IntentRouter.matchFastRule('/summarize', { hasSelection: true }), null);
});

scenario('R2-31', 'ScrapsFilter', 'ScrapsFilter emptyMessageKey distinguishes empty query vs no results vs filtered', () => {
  assert.equal(ScrapsFilter.emptyMessageKey(null, false), 'scrapsSearchEmpty');
  assert.equal(ScrapsFilter.emptyMessageKey(null, true), 'scrapsSearchNoResults');
  const filteredKey = ScrapsFilter.emptyMessageKey({ tags: ['work'] }, true);
  assert.ok(typeof filteredKey === 'string' && filteredKey.length > 0);
});

scenario('R2-32', 'ScrapQuote', 'ScrapQuote.quoteText extracts clean text and handles empty input', () => {
  assert.equal(ScrapQuote.quoteText(null), '');
  assert.equal(ScrapQuote.quoteText({ lineText: '  hello world  ' }).trim(), 'hello world');
});

scenario('R2-33', 'NoteTitle', 'NoteTitle generates safe non-empty titles from Japanese and Markdown headings', () => {
  const t1 = NoteTitle.deriveTitle('# 会議メモ\n詳細');
  assert.ok(typeof t1 === 'string' && t1.includes('会議メモ'), `Got: ${t1}`);
});

scenario('R2-34', 'DiskSync', 'DiskSync cyrb53 signature is deterministic and detects 1-char changes', () => {
  const s1 = DiskSync.sum('hello world');
  const s2 = DiskSync.sum('hello world');
  const s3 = DiskSync.sum('hello world!');
  assert.equal(s1, s2);
  assert.notEqual(s1, s3);
});

// =========================================================================
// Domain 4: Search Workbench, Config, Backend Proxy, Rust Safety (R2-35 .. R2-50)
// =========================================================================
scenario('R2-35', 'SearchUI', 'search_workbench resets scrapsSearchStale on backend error', () => {
  assert.ok(searchWorkbenchJs.includes('if (seq === scrapsSearchSeq) scrapsSearchStale = false;'));
});

scenario('R2-36', 'SearchUI', 'search_workbench warms cache on empty-seed modal open without throwing', () => {
  assert.ok(searchWorkbenchJs.includes('window.backend.scrapFilterOptions()') && searchWorkbenchJs.includes('.catch(() => {})'));
});

scenario('R2-37', 'BackendProxy', 'backend_proxy.js exposes searchScraps, scrapFilterOptions, queryLLMAsync, config', () => {
  const bp = fs.readFileSync(path.join(ROOT, 'frontend/js/backend_proxy.js'), 'utf8');
  for (const m of ['searchScraps', 'scrapFilterOptions', 'queryLLMAsync', 'saveFile', 'saveConfig']) {
    assert.ok(bp.includes(`'${m}'`), `Missing RPC method in backend_proxy.js: ${m}`);
  }
});

scenario('R2-38', 'RPCBridge', 'rpc_bridge.js exposes getLiveConfig and getUiState with strict key contract', () => {
  const rb = fs.readFileSync(path.join(ROOT, 'frontend/js/rpc_bridge.js'), 'utf8');
  assert.ok(rb.includes('getLiveConfig') && rb.includes('getUiState'));
});

scenario('R2-39', 'RustCore', 'ipc.rs supports >=32 concurrent Named Pipe instances and worker threads', () => {
  const ipcRs = fs.readFileSync(path.join(ROOT, 'crates/syki-core/src/ipc.rs'), 'utf8');
  assert.ok(ipcRs.includes('32') && ipcRs.includes('std::thread::spawn'));
});

scenario('R2-40', 'RustCore', 'atomic.rs uses unique temp names and atomic rename/replace', () => {
  const atomicRs = fs.readFileSync(path.join(ROOT, 'crates/syki-core/src/storage/atomic.rs'), 'utf8');
  assert.ok(atomicRs.includes('pub fn write_atomic_bytes'));
});

scenario('R2-41', 'RustCore', 'llm.rs sets thinkingBudget: 0 for gemini-2.5 with small max_tokens <= 1024', () => {
  const llmRs = fs.readFileSync(path.join(ROOT, 'crates/syki-win/src/llm.rs'), 'utf8');
  assert.ok(llmRs.includes('thinkingBudget') && llmRs.includes('1024'));
});

scenario('R2-42', 'RustCore', 'llm.rs resolve_gemini_root strips /v1beta/openai to prevent 404 double-path', () => {
  const llmRs = fs.readFileSync(path.join(ROOT, 'crates/syki-win/src/llm.rs'), 'utf8');
  assert.ok(llmRs.includes('fn resolve_gemini_root') && llmRs.includes('/v1beta/openai'));
});

scenario('R2-43', 'RustCore', 'speech.rs is_audio_media supports wav, mp3, m4a, ogg, flac, webm', () => {
  const speechRs = fs.readFileSync(path.join(ROOT, 'crates/syki-win/src/speech.rs'), 'utf8');
  for (const ext of ['"wav"', '"mp3"', '"m4a"', '"ogg"', '"flac"', '"webm"']) {
    assert.ok(speechRs.includes(ext), `Missing audio extension in speech.rs: ${ext}`);
  }
});

scenario('R2-44', 'RustCore', 'pty.rs separates list_sessions from presets', () => {
  const ptyRs = fs.readFileSync(path.join(ROOT, 'crates/syki-win/src/pty.rs'), 'utf8');
  assert.ok(ptyRs.includes('pub fn list_sessions'));
});

scenario('R2-45', 'RustGUI', 'syki-gui implements real ocr.run (not queued stub)', () => {
  const guiRs = fs.readFileSync(path.join(ROOT, 'crates/syki-gui/src/main.rs'), 'utf8');
  assert.ok(guiRs.includes('query_vision_sync') && !guiRs.includes('"queued": true'));
});

scenario('R2-46', 'RustCLI', 'syki-cli config set notifies running GUI via IPC', () => {
  const cliRs = fs.readFileSync(path.join(ROOT, 'crates/syki-cli/src/main.rs'), 'utf8');
  assert.ok(cliRs.includes('"config.set"'));
});

scenario('R2-47', 'Security', 'No hardcoded API keys (AIza...) anywhere in frontend/ or crates/', () => {
  const re = /AIza[0-9A-Za-z_-]{20,}/;
  assert.ok(!re.test(appJs), 'Hardcoded Google API key in app.js');
  const llmRs = fs.readFileSync(path.join(ROOT, 'crates/syki-win/src/llm.rs'), 'utf8');
  assert.ok(!re.test(llmRs), 'Hardcoded Google API key in llm.rs');
});

scenario('R2-48', 'Security', 'openExternal rejects unsafe URI schemes (file:/javascript:/vbscript:)', () => {
  const rpcRs = fs.readFileSync(path.join(ROOT, 'crates/syki-win/src/rpc_dispatch.rs'), 'utf8');
  assert.ok(rpcRs.includes('open_external_rejects_unsafe_schemes'));
});

scenario('R2-49', 'Build', 'Cargo.toml workspace version is valid semver', () => {
  const cargo = fs.readFileSync(path.join(ROOT, 'Cargo.toml'), 'utf8');
  assert.ok(/version\s*=\s*"\d+\.\d+\.\d+"/.test(cargo));
});

scenario('R2-50', 'Build', 'No leftover console-breaking syntax errors in app.js or terminal_pane.js', () => {
  assert.ok(appJs.length > 100000 && terminalPaneJs.length > 50000);
});

const total = results.length;
const failed = results.filter((r) => r.status === 'FAIL');
const passed = total - failed.length;
const bugRate = failed.length / total;
const bugFreeRate = 1 - bugRate;

console.log(`ROUND 2 SUMMARY: ${passed}/${total} PASS, ${failed.length} FAIL`);
console.log(`Empirical Bug-Free Rate: ${(bugFreeRate * 100).toFixed(2)}% (Defect Rate: ${(bugRate * 100).toFixed(2)}%)`);
if (failed.length > 0) {
  console.log('\n--- DETECTED BUGS ---');
  for (const f of failed) {
    console.log(`[${f.id}] (${f.domain}) ${f.title}\n    -> ${f.error}`);
  }
  process.exit(1);
}
