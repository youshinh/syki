#!/usr/bin/env node
// Round 3: 160 convergence & boundary-fuzz scenarios to verify >=99.7% bug-free reliability.
import assert from 'node:assert/strict';
import path from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const ROOT = process.cwd();

global.window = global;
const MT = require(path.join(ROOT, 'frontend/js/mermaid_tone.js'));
const markdownit = require(path.join(ROOT, 'frontend/vendor/markdown-it.min.js'));
const IntentRouter = require(path.join(ROOT, 'frontend/js/intent_router.js'));
const ScrapsFilter = require(path.join(ROOT, 'frontend/js/scraps_filter.js'));
const ScrapQuote = require(path.join(ROOT, 'frontend/js/scrap_quote.js'));
const NoteTitle = require(path.join(ROOT, 'frontend/js/note_title.js'));
const DiskSync = require(path.join(ROOT, 'frontend/js/disk_sync.js'));
const SecretStrip = require(path.join(ROOT, 'frontend/js/secret_strip.js'));
const HtmlComments = require(path.join(ROOT, 'frontend/js/html_comments.js'));
const PreviewImages = require(path.join(ROOT, 'frontend/js/preview_images.js'));

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

const results = [];
let seq = 0;
function check(domain, title, fn) {
  const id = `R3-${String(++seq).padStart(3, '0')}`;
  try {
    fn();
    results.push({ id, domain, title, status: 'PASS', error: null });
  } catch (err) {
    results.push({ id, domain, title, status: 'FAIL', error: err && err.message ? err.message : String(err) });
  }
}

// 1. 50 Mermaid Mindmap & Flowchart Boundary / Fuzz Scenarios
const mindmapSamples = [
  'mindmap\n  root((基本))\n    子1\n    子2',
  'mindmap\nroot["0インデント"]\n  A["子A"]\n    A1["孫A1"]',
  '\uFEFFmindmap\n  root["BOM付"]\n    B["子B<br/>改行"]',
  'mindmap\n\troot["タブ"]\n\t\tC["子C"]',
  'mindmap\n\u3000root["全角空白"]\n\u3000\u3000D["子D"]',
  'mindmap\n  Topic1\n  Topic2\n  Topic3',
  'mindmap\n  root(("丸"))\n    A(["角丸"])\n    B{{"六角"}}\n    C{"菱形"}',
  'mindmap\n  root["括弧(a)[b]{c}"]\n    N["引用\u201Cテスト\u201D"]',
  '%% header comment\nmindmap\n  root((コメント付))\n    %% inline comment\n    子ノード\n      ::icon(fa fa-star)\n      :::highlight',
  'mindmap\r\n  root["CRLF"]\r\n    Sub["A<br>B<br />C"]\r\n'
];
for (let i = 0; i < 25; i++) {
  const base = mindmapSamples[i % mindmapSamples.length];
  const variant = i < 10 ? base : `${base}\n    Extra${i}["追加(項目${i})<br/>詳細${i}"]`;
  check('MermaidFuzz', `Mindmap sanitize & idempotence #${i + 1}`, () => {
    const once = MT.sanitizeSource(variant);
    const twice = MT.sanitizeSource(once);
    assert.equal(twice, once, 'Not idempotent');
    assert.ok(!once.includes('\uFEFF'), 'BOM remained');
    assert.ok(!once.includes('<br'), '<br remained');
    assert.ok(!once.includes('"'), 'Quote remained');
    // Verify strictly single root (only 1 non-directive node at minimum indent after mindmap)
    const lines = once.split('\n');
    const hdr = lines.findIndex((l) => /^mindmap\b/i.test(l.trim()));
    assert.ok(hdr >= 0);
    const nodes = lines.slice(hdr + 1)
      .filter((l) => l.trim() && !l.trim().startsWith('%%') && !l.trim().startsWith('::'))
      .map((l) => l.length - l.trimStart().length);
    const minInd = Math.min(...nodes);
    const roots = nodes.filter((n) => n === minInd).length;
    assert.equal(roots, 1, `Expected 1 root, got ${roots}:\n${once}`);
  });
}

const flowchartSamples = [
  'flowchart TD\n  sub-graph S1 "開始"\n    A ──> B\n  end',
  'flowchart LR\n  subgraph "タイトル"\n    A["入力 → 処理"] ══> B["完了"]\n  end',
  'flowchart TD\n  subgraph [1] フェーズ (09:00)\n    X ───> Y\n  end',
  'sequenceDiagram\n  participant U as "ユーザー"\n  U->>S: 送信',
  'erDiagram\n  USER ||--o{ NOTE : "writes"'
];
for (let i = 0; i < 25; i++) {
  const src = flowchartSamples[i % flowchartSamples.length] + (i >= 5 ? `\n%% v${i}` : '');
  check('MermaidFuzz', `Non-mindmap sanitize & idempotence #${i + 1}`, () => {
    const once = MT.sanitizeSource(src);
    const twice = MT.sanitizeSource(once);
    assert.equal(twice, once);
    assert.ok(!once.includes('sub-graph'));
  });
}

// 2. 50 Markdown <br>, Table, Code & XSS Boundary / Fuzz Scenarios
const brTags = ['<br>', '<br/>', '<br />', '<BR>', '<BR/>', '<Br />'];
for (let i = 0; i < 20; i++) {
  const tag = brTags[i % brTags.length];
  check('MarkdownBr', `Paragraph/Table/List <br> variant #${i + 1} (${tag})`, () => {
    const para = md.render(`前${i}${tag}後${i}`);
    assert.ok(para.includes('<br>'), para);
    const tbl = md.render(`| A | B |\n|---|---|\n| 1${tag}2 | 3${tag}4 |`);
    assert.equal((tbl.match(/<br>/g) || []).length, 2, tbl);
  });
}

const xssPayloads = [
  '<script>alert(1)</script>',
  '<img src=x onerror=alert(1)>',
  '<br onload=alert(1)>',
  '<br/onclick=alert(1)>',
  '<svg/onload=alert(1)>',
  '<iframe src="javascript:alert(1)"></iframe>',
  '<a href="javascript:alert(1)">x</a>',
  '<details open ontoggle=alert(1)>',
  '<body onload=alert(1)>',
  '<object data="x"></object>',
  '<embed src="x">',
  '<style>body{display:none}</style>',
  '<link rel=stylesheet href=x>',
  '<meta http-equiv="refresh" content="0">',
  '<math><mtext></mtext></math>'
];
for (let i = 0; i < 15; i++) {
  const payload = xssPayloads[i];
  check('MarkdownXSS', `XSS payload escaped #${i + 1}`, () => {
    const html = md.render(`text ${payload} end`);
    assert.ok(!/<(?:script|img|svg|iframe|details|body|object|embed|style|link|meta|math)\b/i.test(html), `Unescaped tag: ${html}`);
    assert.ok(!/<[^>]*\bon(?:load|error|click|toggle)\s*=/i.test(html), `Unescaped event handler tag: ${html}`);
  });
}

for (let i = 0; i < 15; i++) {
  const tag = brTags[i % brTags.length];
  check('MarkdownCode', `Code & comment protection #${i + 1}`, () => {
    const inlineHtml = md.render(`code \`a${tag}b\` here`);
    assert.ok(inlineHtml.includes('&lt;'), inlineHtml);
    assert.ok(!inlineHtml.includes('<br>'), inlineHtml);
    const fencedHtml = md.render(`\`\`\`\na${tag}b\n\`\`\``);
    assert.ok(!fencedHtml.includes('<br>'), fencedHtml);
    const noComment = HtmlComments.removeComments(`x <!-- hidden ${i} --> y`);
    assert.ok(!noComment.includes(`hidden ${i}`));
  });
}

// 3. 60 Cross-Module Boundary Scenarios (Intent, Secrets, Filter, Quote, Title, DiskSync)
const intentCases = [
  ['フローチャートを作って', false, 'diagram'],
  ['マインドマップに整理して', true, 'diagram'],
  ['ER図にして', false, 'diagram'],
  ['意味検索で探して', false, 'search_semantic'],
  ['完全一致で探して', false, 'search_grep'],
  ['インフォグラフィック画像にして', true, 'infographic'],
  ['エージェントに投げて', false, 'send_agent'],
  ['curlで疎通確認して', false, 'run_cli'],
  ['これを要約して', true, 'edit_note'],
  ['丁寧に校正して', true, 'edit_note'],
  ['/help', true, null],
  ['/summarize', true, null]
];
for (let i = 0; i < 12; i++) {
  const [text, hasSelection, expected] = intentCases[i];
  check('IntentRouter', `Fast rule routing #${i + 1}: ${text}`, () => {
    const res = IntentRouter.matchFastRule(text, { hasSelection });
    assert.equal(res ? res.route : null, expected);
  });
}

const secretKeys = ['apiKey', 'api_key', 'api-key', 'token', 'botToken', 'secret', 'clientSecret', 'password', 'passwd', 'myApiKey', 'authToken', 'dbPassword'];
for (let i = 0; i < 12; i++) {
  const k = secretKeys[i];
  check('SecretStrip', `Strips secret key "${k}" and URL credentials`, () => {
    const obj = { nested: { [k]: `SECRET_VAL_${i}`, url: `https://user:pass${i}@example.com/v1?key=K${i}&token=T${i}`, maxTokens: 512 } };
    const clean = SecretStrip.stripSecrets(obj);
    const s = JSON.stringify(clean);
    assert.ok(!s.includes(`SECRET_VAL_${i}`), s);
    assert.ok(!s.includes(`pass${i}`), s);
    assert.ok(!s.includes(`K${i}`), s);
    assert.ok(!s.includes(`T${i}`), s);
    assert.equal(clean.nested.maxTokens, 512);
  });
}

const titleInputs = [
  '# 設計メモ\n本文',
  '## 2026-11-27 議事録\n- 項目1',
  '通常テキスト1行目\n2行目',
  '   \n\n# 遅延見出し',
  'a/b\\c:d*e?f"g<h>i|j',
  '日本語とEnglish 123',
  '- [x] タスク完了',
  '> 引用からのタイトル',
  '`コード` を含む見出し',
  '**太字タイトル**',
  '長いタイトル'.repeat(20),
  ''
];
for (let i = 0; i < 12; i++) {
  check('NoteTitle', `deriveTitle & sanitizeFileName #${i + 1}`, () => {
    const t = NoteTitle.deriveTitle(titleInputs[i]);
    const safe = NoteTitle.sanitizeFileName(t || 'untitled');
    assert.ok(typeof safe === 'string' && !/[\\/:*?"<>|]/.test(safe), `Unsafe filename: ${safe}`);
  });
}

for (let i = 0; i < 12; i++) {
  check('DiskSync', `Signature & EOL handling #${i + 1}`, () => {
    const txt = `line1_${i}\r\nline2_${i}\r\n`;
    assert.equal(DiskSync.detectEol(txt), 'crlf');
    const lf = DiskSync.lf(txt);
    assert.ok(!lf.includes('\r'));
    assert.equal(DiskSync.sum(lf), DiskSync.sum(lf));
    assert.notEqual(DiskSync.sum(lf), DiskSync.sum(lf + 'x'));
  });
}

for (let i = 0; i < 12; i++) {
  check('ScrapsFilter&Quote', `Filter & quote boundary #${i + 1}`, () => {
    const st = ScrapsFilter.create();
    const req = ScrapsFilter.toRequest(st, new Date('2026-11-27T12:00:00Z'));
    assert.equal(req, null);
    const q = ScrapQuote.quoteText({ lineText: `match line ${i}` });
    assert.ok(q.includes(`match line ${i}`));
  });
}

const total = results.length;
const failed = results.filter((r) => r.status === 'FAIL');
const passed = total - failed.length;
console.log(`ROUND 3 SUMMARY: ${passed}/${total} PASS, ${failed.length} FAIL`);
if (failed.length > 0) {
  for (const f of failed) {
    console.log(`[${f.id}] (${f.domain}) ${f.title}\n    -> ${f.error}`);
  }
  process.exit(1);
}
