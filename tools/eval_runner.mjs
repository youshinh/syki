// Unified 4-layer Acceptance Test & Functional Evaluation runner for syki-lite.
// Usage: node tools/eval_runner.mjs [--smoke=none|quick|full] [--rust|--no-rust] [--lang=en|ja] [--json] [--allow-red]
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { runStaticChecks } from '../evals/checks/static_contract.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, '..');
const MATRIX_PATH = path.join(REPO, 'evals', 'functional_matrix.json');
const REPORTS_DIR = path.join(REPO, 'evals', 'reports');

function argVal(prefix, dflt) {
  const hit = process.argv.find((a) => a.startsWith(prefix + '='));
  return hit ? hit.slice(prefix.length + 1) : dflt;
}

const SMOKE_MODE = argVal('--smoke', 'quick');
const LANG = argVal('--lang', 'en') === 'ja' ? 'ja' : 'en';
const RUN_RUST = !process.argv.includes('--no-rust');
const OUTPUT_JSON = process.argv.includes('--json');
const ALLOW_RED = process.argv.includes('--allow-red');

const matrix = JSON.parse(fs.readFileSync(MATRIX_PATH, 'utf8'));

// ---- Layer 1: Static contract ----
const staticRes = runStaticChecks(REPO);

// ---- Layer 2: JS Unit tests ----
function runJsUnit() {
  const res = spawnSync(process.execPath, [path.join(REPO, 'tools', 'run_js_tests.mjs')], {
    cwd: REPO,
    encoding: 'utf8',
    maxBuffer: 10 * 1024 * 1024,
  });
  const out = (res.stdout || '') + '\n' + (res.stderr || '');
  const byFile = {};
  for (const line of out.split(/\r?\n/)) {
    const m = /^(PASS|FAIL)\s+(?:frontend[\\/]+js[\\/]+|tests[\\/]+)([^\s]+)$/.exec(line.trim());
    if (m) {
      byFile[m[2]] = { ok: m[1] === 'PASS', status: m[1].toLowerCase() };
    }
  }
  const mSum = /(\d+)\/(\d+)\s+test file\(s\) passed/.exec(out);
  return {
    ok: res.status === 0,
    passed: mSum ? Number(mSum[1]) : Object.values(byFile).filter((x) => x.ok).length,
    total: mSum ? Number(mSum[2]) : Object.keys(byFile).length,
    files: byFile,
    rawExit: res.status,
  };
}
const jsUnitRes = runJsUnit();

// ---- Layer 3: Headless Edge Smoke Suite ----
function runSmoke(mode, lang) {
  if (mode === 'none') {
    return { mode, skipped: true, passed: 0, total: 0, flows: {} };
  }
  const tmpOut = path.join(os.tmpdir(), `syki-lite-smoke-${Date.now()}.json`);
  const args = [path.join(REPO, 'tests', 'smoke', 'run.mjs'), '--lang', lang, '--jobs', '3', '--tier', mode, '--json-out', tmpOut];
  const res = spawnSync(process.execPath, args, {
    cwd: REPO,
    encoding: 'utf8',
    maxBuffer: 10 * 1024 * 1024,
  });
  let parsed = null;
  if (fs.existsSync(tmpOut)) {
    try { parsed = JSON.parse(fs.readFileSync(tmpOut, 'utf8')); } catch { /* ignore */ }
    fs.rmSync(tmpOut, { force: true });
  }
  const flows = {};
  if (parsed && Array.isArray(parsed.flows)) {
    for (const f of parsed.flows) {
      flows[f.id] = {
        ok: f.status === 'pass' || f.status === 'xfail',
        status: f.status,
        ms: f.ms,
        step: f.step,
        message: f.message,
      };
    }
  }
  let error = null;
  if (!parsed || !parsed.total) {
    const rawMsg = ((res.stderr || '') + '\n' + (res.stdout || '')).trim();
    error = rawMsg ? rawMsg.split(/\r?\n/).slice(0, 6).join(' | ') : 'smoke runner failed without JSON output';
  }
  return {
    mode,
    skipped: false,
    passed: parsed ? parsed.passed + (parsed.xfail || 0) : 0,
    total: parsed ? parsed.total : 0,
    failed: parsed ? parsed.failed : 1,
    durationMs: parsed ? parsed.durationMs : 0,
    rawExit: res.status,
    error,
    flows,
  };
}
const smokeRes = runSmoke(SMOKE_MODE, LANG);

// ---- Layer 4: Rust & Perf Eval ----
function runRustEval() {
  if (!RUN_RUST) return { skipped: true, env: null, checks: {}, passed: 0, total: 0 };
  const psArgs = ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', path.join(REPO, 'evals', 'rust_and_perf_eval.ps1'), '-RepoRoot', REPO, '-Json'];
  const res = spawnSync('pwsh', psArgs, { cwd: REPO, encoding: 'utf8', maxBuffer: 5 * 1024 * 1024 });
  if (res.status !== 0 || !(res.stdout || '').trim()) {
    return { skipped: false, error: (res.stderr || res.stdout || 'pwsh failed').trim(), env: null, checks: {}, passed: 0, total: 0 };
  }
  const parsed = JSON.parse(res.stdout);
  return { skipped: false, ...parsed };
}
const rustRes = runRustEval();

// ---- Aggregate Category Scorecard ----
const categoryResults = matrix.categories.map((cat) => {
  const details = [];
  for (const id of (cat.checks.static || [])) {
    const c = staticRes.checks[id];
    details.push({ layer: 'static', id, ok: !!(c && c.ok), status: c ? c.status : 'missing', detail: c ? c.detail : 'unmapped' });
  }
  for (const file of (cat.checks.js_unit || [])) {
    const u = jsUnitRes.files[file];
    details.push({ layer: 'js_unit', id: file, ok: !!(u && u.ok), status: u ? u.status : 'missing', detail: u ? u.status : 'not found in run' });
  }
  for (const flow of (cat.checks.smoke || [])) {
    if (smokeRes.skipped) continue;
    const f = smokeRes.flows[flow];
    if (f) {
      details.push({ layer: 'smoke', id: flow, ok: f.ok, status: f.status, detail: f.ok ? `${f.ms}ms` : `${f.step}: ${String(f.message).split('\n')[0]}` });
    } else if (SMOKE_MODE === 'quick') {
      details.push({ layer: 'smoke', id: flow, ok: null, status: 'deferred_full', detail: 'runs under --smoke=full' });
    } else {
      details.push({ layer: 'smoke', id: flow, ok: false, status: 'missing', detail: 'flow missing from suite' });
    }
  }
  for (const rc of (cat.checks.rust_perf || [])) {
    if (rustRes.skipped) continue;
    const r = (rustRes.checks && rustRes.checks[rc]) || (rc.startsWith('rust.') ? staticRes.checks[rc] : null);
    if (r) {
      details.push({ layer: 'rust_perf', id: rc, ok: !!r.ok, status: r.status, detail: r.detail });
    } else {
      details.push({ layer: 'rust_perf', id: rc, ok: false, status: 'missing', detail: 'unmapped rust_perf check' });
    }
  }

  const active = details.filter((d) => d.ok !== null);
  const passedCount = active.filter((d) => d.ok === true).length;
  const hasBlockedEnv = active.some((d) => d.status === 'BLOCKED_MSVC_LINK');
  const ratio = active.length > 0 ? (passedCount / active.length) : 0;
  const earned = Number((cat.weight * ratio).toFixed(2));
  let status = 'FAIL';
  if (active.length > 0 && passedCount === active.length) status = 'PASS';
  else if (passedCount > 0) status = 'PARTIAL';
  else if (hasBlockedEnv) status = 'BLOCKED_ENV';

  return {
    id: cat.id,
    name: cat.name,
    weight: cat.weight,
    earned,
    status,
    passed_checks: passedCount,
    active_checks: active.length,
    deferred_checks: details.filter((d) => d.ok === null).length,
    owner_tasks: cat.owner_tasks,
    checks: details,
  };
});

const totalScore = Number(categoryResults.reduce((s, c) => s + c.earned, 0).toFixed(1));
const totalWeight = matrix.total_weight;
const allGreen =
  staticRes.passed === staticRes.total &&
  jsUnitRes.ok &&
  (smokeRes.skipped || (smokeRes.failed === 0 && smokeRes.total > 0)) &&
  (rustRes.skipped || (rustRes.total > 0 && rustRes.passed === rustRes.total));

const summary = {
  generated_at: new Date().toISOString(),
  options: { smoke: SMOKE_MODE, rust: RUN_RUST, lang: LANG },
  total_score: totalScore,
  total_weight: totalWeight,
  all_green: allGreen,
  layers: {
    static: { passed: staticRes.passed, total: staticRes.total, checks: staticRes.checks },
    js_unit: { passed: jsUnitRes.passed, total: jsUnitRes.total, ok: jsUnitRes.ok },
    smoke: { mode: smokeRes.mode, passed: smokeRes.passed, total: smokeRes.total, failed: smokeRes.failed || 0, durationMs: smokeRes.durationMs || 0, error: smokeRes.error || null, flows: smokeRes.flows },
    rust_perf: { skipped: rustRes.skipped, env: rustRes.env || null, passed: rustRes.passed || 0, total: rustRes.total || 0, checks: rustRes.checks || {} },
  },
  categories: categoryResults,
};

// Render Markdown report
const mdLines = [
  `# syki-lite Acceptance & Functional Evaluation Report`,
  ``,
  `- **Generated**: ${summary.generated_at}`,
  `- **Options**: smoke=\`${SMOKE_MODE}\`, rust=\`${RUN_RUST}\`, lang=\`${LANG}\``,
  `- **Overall Score**: **${totalScore} / ${totalWeight}** (${allGreen ? 'GREEN' : 'RED BASELINE / IN PROGRESS'})`,
  ``,
  `## Layer Summary`,
  `- **Layer 1 Static Contract**: ${staticRes.passed}/${staticRes.total} passed`,
  `- **Layer 2 JS Unit Tests**: ${jsUnitRes.passed}/${jsUnitRes.total} passed`,
  `- **Layer 3 Edge Smoke (${SMOKE_MODE})**: ${smokeRes.skipped ? 'skipped' : (smokeRes.error ? `ERROR (${smokeRes.error})` : `${smokeRes.passed}/${smokeRes.total} passed (${(smokeRes.durationMs / 1000).toFixed(1)}s)`)}`,
  `- **Layer 4 Rust & Perf**: ${rustRes.skipped ? 'skipped' : `${rustRes.passed}/${rustRes.total} passed (rustc=${rustRes.env?.rustc || 'none'}, msvc_link=${rustRes.env?.msvc_link_count ?? '?'})`}`,
  ``,
  `## Feature Readiness Scorecard`,
  `| Feature | Name | Status | Score | Checks | Owner |`,
  `|---|---|---|---:|---:|---|`,
  ...categoryResults.map((c) => `| ${c.id} | ${c.name} | ${c.status} | ${c.earned.toFixed(1)}/${c.weight} | ${c.passed_checks}/${c.active_checks} | ${c.owner_tasks.join(', ')} |`),
  ``,
];

if (!smokeRes.skipped) {
  const failedFlows = Object.entries(smokeRes.flows).filter(([, f]) => !f.ok);
  if (failedFlows.length) {
    mdLines.push(`## Smoke Failures (${failedFlows.length})`, '');
    for (const [id, f] of failedFlows) {
      mdLines.push(`- \`${id}\` (${f.step}): ${String(f.message).split('\n')[0]}`);
    }
    mdLines.push('');
  }
}

fs.mkdirSync(REPORTS_DIR, { recursive: true });
fs.writeFileSync(path.join(REPORTS_DIR, 'latest.json'), JSON.stringify(summary, null, 2) + '\n', 'utf8');
fs.writeFileSync(path.join(REPORTS_DIR, 'latest.md'), mdLines.join('\n'), 'utf8');

if (OUTPUT_JSON) {
  console.log(JSON.stringify(summary, null, 2));
} else {
  console.log(mdLines.join('\n'));
}

process.exit(allGreen || ALLOW_RED ? 0 : 1);
