// Layer 1 static contract & compatibility fixture verification checks for syki-lite.
// Run standalone: node evals/checks/static_contract.mjs [--json]
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const DEFAULT_ROOT = path.resolve(HERE, '..', '..');

const DELETED_PRUNED_JS_FILES = [
  'frontend/js/agent_risk.js', 'frontend/js/agent_risk_test.js',
  'frontend/js/auto_selector.js', 'frontend/js/auto_selector_test.js',
  'frontend/js/config_pack.js', 'frontend/js/config_pack_test.js',
  'frontend/js/jev_action.js', 'frontend/js/jev_action_test.js',
  'frontend/js/lessons.js', 'frontend/js/lessons_test.js',
  'frontend/js/mermaid_repair.js',
  'frontend/js/slot_agent.js', 'frontend/js/slot_agent_test.js',
  'frontend/js/slot_snippets.js', 'frontend/js/slot_snippets_test.js',
  'frontend/js/tag_edit.js', 'frontend/js/tag_edit_test.js',
];

const PRUNED_HTML_IDS = [
  'btn-mobile-drop', 'jev-action-panel', 'stat-gitsync', 'pack-modal',
  'mobile-drop-modal', 'cf-bridge-qr-modal', 'tag-pick-modal',
  'deep-search-modal', 'lesson-modal', 'local-ai-card', 'cfg-ocr-on-device',
  'btn-scraps-deep', 'ctx-new-window',
];

const PRUNED_APP_JS_CALLS = [
  'startMobileDrop', 'jevPredict', 'jevExecuteAsync', 'runSlotAgentAsync',
  'lessonPlan', 'tagEdit', 'deepSearchPlan', 'packExport', 'triggerGitSync',
  'testDiscordBridgeConnection', 'setupOllamaGemma4Async', 'startOllamaService',
];

function walkFiles(dir, extFilter) {
  const out = [];
  if (!fs.existsSync(dir)) return out;
  for (const ent of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, ent.name);
    if (ent.isDirectory()) out.push(...walkFiles(p, extFilter));
    else if (!extFilter || extFilter(ent.name)) out.push(p);
  }
  return out;
}

function checkDeletedJsFiles(repoRoot) {
  const remaining = DELETED_PRUNED_JS_FILES.filter((rel) => fs.existsSync(path.join(repoRoot, rel)));
  return {
    ok: remaining.length === 0,
    status: remaining.length === 0 ? 'pass' : 'fail',
    detail: remaining.length === 0 ? '0/17 pruned JS files present' : `remaining: ${remaining.join(', ')}`,
  };
}

function checkHtmlPrunedIds(repoRoot) {
  const htmlPath = path.join(repoRoot, 'frontend', 'index.html');
  if (!fs.existsSync(htmlPath)) return { ok: false, status: 'fail', detail: 'frontend/index.html missing' };
  const html = fs.readFileSync(htmlPath, 'utf8');
  const present = PRUNED_HTML_IDS.filter((id) => new RegExp(`\\bid\\s*=\\s*["']${id}["']`).test(html));
  return {
    ok: present.length === 0,
    status: present.length === 0 ? 'pass' : 'fail',
    detail: present.length === 0 ? '0 pruned IDs in index.html' : `still present (${present.length}): ${present.join(', ')}`,
  };
}

function checkAppJsPrunedCalls(repoRoot) {
  const appPath = path.join(repoRoot, 'frontend', 'js', 'app.js');
  if (!fs.existsSync(appPath)) return { ok: false, status: 'fail', detail: 'frontend/js/app.js missing' };
  const src = fs.readFileSync(appPath, 'utf8');
  const hits = PRUNED_APP_JS_CALLS.filter((sym) => new RegExp(`\\b${sym}\\b`).test(src));
  return {
    ok: hits.length === 0,
    status: hits.length === 0 ? 'pass' : 'fail',
    detail: hits.length === 0 ? '0 pruned backend calls in app.js' : `still present (${hits.length}): ${hits.join(', ')}`,
  };
}

function checkHtmlAssetsResolve(repoRoot) {
  const htmlPath = path.join(repoRoot, 'frontend', 'index.html');
  if (!fs.existsSync(htmlPath)) return { ok: false, status: 'fail', detail: 'frontend/index.html missing' };
  const html = fs.readFileSync(htmlPath, 'utf8');
  const refs = [];
  const tagRe = /<(?:script|link)\b[^>]*(?:\bsrc|\bhref)\s*=\s*["']([^"']+)["'][^>]*>/gi;
  let m;
  while ((m = tagRe.exec(html))) {
    const raw = m[1];
    if (!raw || /^(?:https?:|data:|mailto:|#|\/)/i.test(raw)) continue;
    const clean = raw.split(/[?#]/)[0];
    refs.push(clean);
  }
  const missing = [...new Set(refs)].filter((rel) => !fs.existsSync(path.join(repoRoot, 'frontend', rel)));
  return {
    ok: missing.length === 0,
    status: missing.length === 0 ? 'pass' : 'fail',
    detail: missing.length === 0 ? `all ${refs.length} local asset refs resolve` : `missing assets (${missing.length}): ${missing.join(', ')}`,
  };
}

function checkHtmlDivBalance(repoRoot) {
  const htmlPath = path.join(repoRoot, 'frontend', 'index.html');
  if (!fs.existsSync(htmlPath)) return { ok: false, status: 'fail', detail: 'frontend/index.html missing' };
  const html = fs.readFileSync(htmlPath, 'utf8');
  const stripped = html
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/<script\b[^>]*>[\s\S]*?<\/script>/gi, '')
    .replace(/<style\b[^>]*>[\s\S]*?<\/style>/gi, '');
  let depth = 0;
  let minDepth = 0;
  const divRe = /<\/?div\b[^>]*>/gi;
  let m;
  while ((m = divRe.exec(stripped))) {
    if (m[0].startsWith('</')) {
      depth--;
      if (depth < minDepth) minDepth = depth;
    } else if (!m[0].endsWith('/>')) {
      depth++;
    }
  }
  const ok = depth === 0 && minDepth >= 0;
  return {
    ok,
    status: ok ? 'pass' : 'fail',
    detail: ok ? 'balanced <div> tags' : `unbalanced <div> tags (finalDepth=${depth}, minDepth=${minDepth})`,
  };
}

function checkJsSyntax(repoRoot) {
  const jsDir = path.join(repoRoot, 'frontend', 'js');
  if (!fs.existsSync(jsDir)) return { ok: false, status: 'fail', detail: 'frontend/js missing' };
  const files = fs.readdirSync(jsDir).filter((f) => f.endsWith('.js')).sort();
  const bad = [];
  for (const f of files) {
    const res = spawnSync(process.execPath, ['--check', path.join(jsDir, f)], { encoding: 'utf8' });
    if (res.status !== 0) bad.push(`${f}: ${(res.stderr || '').trim().split('\n')[0]}`);
  }
  return {
    ok: bad.length === 0,
    status: bad.length === 0 ? 'pass' : 'fail',
    detail: bad.length === 0 ? `${files.length} JS file(s) pass node --check` : `syntax errors: ${bad.join('; ')}`,
  };
}

function checkBackendProxyPresent(repoRoot) {
  const proxyPath = path.join(repoRoot, 'frontend', 'js', 'backend_proxy.js');
  const htmlPath = path.join(repoRoot, 'frontend', 'index.html');
  if (!fs.existsSync(proxyPath)) {
    return { ok: false, status: 'fail', detail: 'frontend/js/backend_proxy.js not created yet (#04)' };
  }
  const src = fs.readFileSync(proxyPath, 'utf8');
  const html = fs.existsSync(htmlPath) ? fs.readFileSync(htmlPath, 'utf8') : '';
  const idxProxy = html.indexOf('js/backend_proxy.js');
  const idxPlatform = html.indexOf('js/platform.js');
  const hasPostMessage = /postMessage|webview\.postMessage/i.test(src);
  const ok = idxProxy >= 0 && idxPlatform > idxProxy && hasPostMessage;
  return {
    ok,
    status: ok ? 'pass' : 'fail',
    detail: ok ? 'backend_proxy.js present and ordered before platform.js' : `proxyOrder=${idxProxy}<${idxPlatform}, hasPostMessage=${hasPostMessage}`,
  };
}

function checkFixturesValid(repoRoot) {
  const fx = path.join(repoRoot, 'evals', 'fixtures');
  const probs = [];
  try {
    const cfg = JSON.parse(fs.readFileSync(path.join(fx, 'config_v1_legacy.json'), 'utf8'));
    if (!cfg.general || !cfg.semantic || !cfg.ocr || !cfg.gitSync) probs.push('config_v1_legacy.json missing expected sections');

    const s17 = path.join(fx, 'scraps', '2026-09-17.md');
    const s15 = path.join(fx, 'scraps', '2026-09-15.md');
    if (!fs.existsSync(s17) || !fs.existsSync(s15)) probs.push('UTF-8 scrap fixtures missing');

    const sjis = fs.readFileSync(path.join(fx, 'scraps', 'shift_jis_note.md'));
    const decodedUtf8 = new TextDecoder('utf-8', { fatal: false }).decode(sjis);
    if (!decodedUtf8.includes('\uFFFD')) probs.push('shift_jis_note.md unexpectedly valid UTF-8');

    const chunksPath = path.join(fx, 'semindex', 'chunks.jsonl');
    const vecsPath = path.join(fx, 'semindex', 'vectors.bin');
    const man = JSON.parse(fs.readFileSync(path.join(fx, 'semindex', 'manifest.json'), 'utf8'));
    const vecs = fs.readFileSync(vecsPath);
    if (vecs.subarray(0, 4).toString('ascii') !== 'MDSV') probs.push('vectors.bin bad magic');
    if (vecs.readUInt32LE(4) !== 1 || vecs.readUInt32LE(8) !== 4) probs.push('vectors.bin bad header');
    if (vecs.length !== 16 + man.vecs * man.dim * 4) probs.push('vectors.bin size mismatch');
    if (fs.statSync(chunksPath).size !== man.metas_bytes) probs.push('manifest.json metas_bytes mismatch');

    const ipc = JSON.parse(fs.readFileSync(path.join(fx, 'ipc_auth_vectors.json'), 'utf8'));
    if (!Array.isArray(ipc.cases) || ipc.cases.length < 8) probs.push('ipc_auth_vectors.json incomplete');

    const wav = fs.readFileSync(path.join(fx, 'audio', 'tone_44100_stereo.wav'));
    if (wav.subarray(0, 4).toString('ascii') !== 'RIFF' || wav.subarray(8, 12).toString('ascii') !== 'WAVE') probs.push('audio wav bad header');

    const png = fs.readFileSync(path.join(fx, 'images', 'sample_ocr.png'));
    if (!png.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))) probs.push('sample_ocr.png bad magic');
  } catch (err) {
    probs.push(err.message);
  }
  return {
    ok: probs.length === 0,
    status: probs.length === 0 ? 'pass' : 'fail',
    detail: probs.length === 0 ? 'all 7 fixture sets valid' : probs.join('; '),
  };
}

function checkRustWorkspace(repoRoot) {
  const cargoPath = path.join(repoRoot, 'Cargo.toml');
  if (!fs.existsSync(cargoPath)) {
    return { ok: false, status: 'pending', detail: 'Cargo.toml not created yet (#01)' };
  }
  const txt = fs.readFileSync(cargoPath, 'utf8');
  const needed = ['syki-core', 'syki-win', 'syki-gui', 'syki-cli'];
  const missing = needed.filter((c) => !txt.includes(c));
  return {
    ok: missing.length === 0,
    status: missing.length === 0 ? 'pass' : 'fail',
    detail: missing.length === 0 ? 'Cargo workspace defines all 4 crates' : `missing crates: ${missing.join(', ')}`,
  };
}

function checkRustStaticPattern(repoRoot, pattern, label) {
  const cratesDir = path.join(repoRoot, 'crates');
  if (!fs.existsSync(cratesDir)) {
    return { ok: false, status: 'pending', detail: `crates/ not created yet (#01) for ${label}` };
  }
  const rsFiles = walkFiles(cratesDir, (n) => n.endsWith('.rs') || n === 'Cargo.toml');
  const hits = [];
  for (const p of rsFiles) {
    const txt = fs.readFileSync(p, 'utf8');
    if (pattern.test(txt)) hits.push(path.relative(repoRoot, p));
  }
  return {
    ok: hits.length === 0,
    status: hits.length === 0 ? 'pass' : 'fail',
    detail: hits.length === 0 ? `0 ${label} occurrences across ${rsFiles.length} Rust file(s)` : `${label} found in: ${hits.join(', ')}`,
  };
}

export function runStaticChecks(repoRoot = DEFAULT_ROOT) {
  const checks = {
    'prune.deleted_js_files': checkDeletedJsFiles(repoRoot),
    'prune.html_pruned_ids': checkHtmlPrunedIds(repoRoot),
    'prune.app_js_pruned_calls': checkAppJsPrunedCalls(repoRoot),
    'frontend.html_assets_resolve': checkHtmlAssetsResolve(repoRoot),
    'frontend.html_div_balance': checkHtmlDivBalance(repoRoot),
    'frontend.js_syntax': checkJsSyntax(repoRoot),
    'seam.backend_proxy_present': checkBackendProxyPresent(repoRoot),
    'fixtures.v1_valid': checkFixturesValid(repoRoot),
    'rust.workspace_present': checkRustWorkspace(repoRoot),
    'rust.zero_powershell_static': checkRustStaticPattern(repoRoot, /powershell(?:\.exe)?/i, 'powershell'),
    'rust.zero_macos_static': checkRustStaticPattern(repoRoot, /target_os\s*=\s*"macos"|\b(?:objc|cocoa|core_foundation)\b/i, 'macOS/objc'),
  };
  const keys = Object.keys(checks);
  const passed = keys.filter((k) => checks[k].ok).length;
  return { checks, passed, total: keys.length };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const res = runStaticChecks(DEFAULT_ROOT);
  if (process.argv.includes('--json')) {
    console.log(JSON.stringify(res, null, 2));
  } else {
    for (const [id, c] of Object.entries(res.checks)) {
      console.log(`${c.ok ? 'PASS' : c.status.toUpperCase()}  ${id}: ${c.detail}`);
    }
    console.log(`\n${res.passed}/${res.total} static contract check(s) passed.`);
  }
  process.exit(res.passed === res.total ? 0 : 1);
}
