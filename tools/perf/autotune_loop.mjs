// Self-improving zero-token parameter autotuning loop for syki-lite.
// Supports up to R rounds x N iterations with automatic early-stopping when gains plateau.
// Preserves 100% of UX functionality via hard test gates; reverts automatically on regression or test failure.
//
// Usage:
//   node tools/perf/autotune_loop.mjs --rounds 5 --iterations 50 --patience 6 --fast-gate
//   node tools/perf/autotune_loop.mjs --rounds 5 --iterations 50 --patience 6
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, '..', '..');

const argv = process.argv.slice(2);
function opt(name, dflt) {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] && !argv[i + 1].startsWith('--') ? argv[i + 1] : dflt;
}
function hasFlag(name) {
  return argv.includes(name);
}

const MAX_ROUNDS = Math.max(1, Math.min(5, Number(opt('--rounds', '5')) || 5));
const MAX_ITERS_PER_ROUND = Math.max(1, Math.min(50, Number(opt('--iterations', '50')) || 50));
const PATIENCE = Math.max(2, Number(opt('--patience', '6')) || 6);
const PARAMS_PATH = path.resolve(opt('--params', path.join(REPO, 'config', 'perf_params.json')));
const HISTORY_PATH = path.resolve(opt('--history', path.join(REPO, 'docs', 'tuning-history.jsonl')));
const MIN_IMPROVE = Number(opt('--min-improve', '0.015')) || 0.015;
const MAX_AXIS_REGRESSION = Number(opt('--max-axis-regression', '0.06')) || 0.06;
const SKIP_NATIVE = hasFlag('--skip-native');
const FAST_GATE = hasFlag('--fast-gate');

// High-impact priority order: WebView startup -> Keyboard hot path -> Voice responsiveness -> Secondary
const PRIORITY_KEYS = [
  'webview.browserArgs',
  'keyboard.lineNumbersThrottleMs',
  'keyboard.statusBarThrottleMs',
  'keyboard.imeSyncDebounceMs',
  'voice.sampleIntervalMs',
  'voice.barSampleMs',
  'voice.barMinVoicedMs',
  'keyboard.linkMarkDebounceMs',
  'keyboard.bulkInsertWorkLimit',
  'keyboard.findDebounceMs',
  'keyboard.linkMarkMaxChars',
  'keyboard.charMirrorFullLimit',
  'keyboard.previewResponsiveCharLimit',
  'keyboard.linkHoverDebounceMs',
  'webview.idleDetachMs',
  'webview.detachCommitMs'
];

function deepClone(obj) {
  return JSON.parse(JSON.stringify(obj));
}

function getByDot(obj, dotKey) {
  return dotKey.split('.').reduce((acc, k) => (acc ? acc[k] : undefined), obj);
}

function setByDot(obj, dotKey, val) {
  const parts = dotKey.split('.');
  let cur = obj;
  for (let i = 0; i < parts.length - 1; i++) {
    if (!cur[parts[i]] || typeof cur[parts[i]] !== 'object') cur[parts[i]] = {};
    cur = cur[parts[i]];
  }
  cur[parts[parts.length - 1]] = val;
}

function appendHistory(entry) {
  fs.mkdirSync(path.dirname(HISTORY_PATH), { recursive: true });
  fs.appendFileSync(HISTORY_PATH, JSON.stringify(entry) + '\n', 'utf8');
}

function loadTriedSignatures() {
  const seen = new Set();
  if (!fs.existsSync(HISTORY_PATH)) return seen;
  for (const line of fs.readFileSync(HISTORY_PATH, 'utf8').split(/\r?\n/)) {
    if (!line.trim()) continue;
    try {
      const row = JSON.parse(line);
      if (row.params) seen.add(JSON.stringify(row.params));
    } catch {}
  }
  return seen;
}

function runBench(paramsFile, mode = 'all') {
  const benchScript = path.join(REPO, 'tools', 'perf', 'autotune_bench.mjs');
  const effectiveMode = SKIP_NATIVE ? 'browser' : mode;
  const args = [benchScript, '--params', paramsFile, '--mode', effectiveMode];
  if (SKIP_NATIVE) args.push('--skip-native');
  const res = spawnSync(process.execPath, args, {
    cwd: REPO,
    encoding: 'utf8',
    timeout: 180000
  });
  if (res.status !== 0) {
    throw new Error(`autotune_bench failed (exit ${res.status}): ${(res.stderr || res.stdout || '').slice(-400)}`);
  }
  return JSON.parse(res.stdout);
}

function selectModeForChanges(changes) {
  if (SKIP_NATIVE) return 'browser';
  const hasWebview = changes.some((c) => c.key.startsWith('webview.'));
  const hasBrowser = changes.some((c) => !c.key.startsWith('webview.'));
  if (hasWebview && !hasBrowser) return 'native';
  if (hasBrowser && !hasWebview) return 'browser';
  return 'all';
}

function mergeAxisScores(baseScores, measuredScores, mode) {
  const startup = mode === 'browser' ? baseScores.startup : measuredScores.startup;
  const keyboard = mode === 'native' ? baseScores.keyboard : measuredScores.keyboard;
  const voice = mode === 'native' ? baseScores.voice : measuredScores.voice;
  const processing = mode === 'native' ? baseScores.processing : measuredScores.processing;
  const composite = Number((startup * 0.35 + keyboard * 0.35 + voice * 0.20 + processing * 0.10).toFixed(2));
  const memoryGuardOk = mode === 'browser' ? baseScores.memoryGuardOk : measuredScores.memoryGuardOk;
  return { startup, keyboard, voice, processing, composite, memoryGuardOk };
}

function averageScores(a, b) {
  const r2 = (n) => Number((n / 2).toFixed(2));
  return {
    startup: r2(a.startup + b.startup),
    keyboard: r2(a.keyboard + b.keyboard),
    voice: r2(a.voice + b.voice),
    processing: r2(a.processing + b.processing),
    composite: r2(a.composite + b.composite),
    memoryGuardOk: Boolean(a.memoryGuardOk && b.memoryGuardOk)
  };
}

function runUxGate(fullOverride = false) {
  const useFast = FAST_GATE && !fullOverride;
  const steps = useFast
    ? [
        { name: 'js_unit_tests', cmd: process.execPath, args: ['tools/run_js_tests.mjs'] },
        { name: 'static_contract', cmd: process.execPath, args: ['evals/checks/static_contract.mjs'] },
        { name: 'smoke_main', cmd: process.execPath, args: ['tests/smoke/run.mjs', '01'] }
      ]
    : [
        { name: 'cargo_test_workspace', cmd: 'cargo', args: ['test', '--workspace'] },
        { name: 'js_unit_tests', cmd: process.execPath, args: ['tools/run_js_tests.mjs'] },
        { name: 'static_contract', cmd: process.execPath, args: ['evals/checks/static_contract.mjs'] },
        { name: 'smoke_suite', cmd: process.execPath, args: ['tests/smoke/run.mjs'] }
      ];

  const results = [];
  for (const st of steps) {
    const t0 = performance.now();
    const res = spawnSync(st.cmd, st.args, {
      cwd: REPO,
      encoding: 'utf8',
      timeout: 300000
    });
    const ok = res.status === 0;
    results.push({
      name: st.name,
      ok,
      ms: Math.round(performance.now() - t0),
      tail: ok ? '' : (res.stderr || res.stdout || '').slice(-300)
    });
    if (!ok) {
      return { ok: false, failedStep: st.name, results };
    }
  }
  return { ok: true, results };
}

function orderedKeys(bounds) {
  const present = Object.keys(bounds || {}).filter((k) => !(SKIP_NATIVE && k.startsWith('webview.')));
  const ordered = PRIORITY_KEYS.filter((k) => present.includes(k));
  for (const k of present) {
    if (!ordered.includes(k)) ordered.push(k);
  }
  return ordered;
}

function generateCandidate(doc, tried, roundIndex, iterIndex) {
  const bounds = doc.bounds || {};
  const keys = orderedKeys(bounds);
  if (!keys.length) return null;

  for (let attempt = 0; attempt < 60; attempt++) {
    const candDoc = deepClone(doc);
    const changes = [];
    // Round 1 focuses on single high-impact knobs; later rounds also test 2-knob combinations
    const numMut = (roundIndex === 1 && attempt < 25) ? 1 : (attempt < 20 ? 1 : 2);

    for (let m = 0; m < numMut; m++) {
      const key = keys[(iterIndex - 1 + (roundIndex - 1) * 7 + attempt * 3 + m * 5) % keys.length];
      const spec = bounds[key];
      const cur = getByDot(candDoc.params, key);
      let next = cur;

      if (Array.isArray(spec.choices) && spec.choices.length > 1) {
        const idx = spec.choices.indexOf(cur);
        next = spec.choices[(idx + 1 + attempt) % spec.choices.length];
      } else if (typeof spec.min === 'number' && typeof spec.max === 'number') {
        const step = spec.step || Math.max(1, Math.round((spec.max - spec.min) / 6));
        // For throttle/debounce starting at 0, explore upward first; for voice intervals, explore downward first
        const preferUp = key.endsWith('ThrottleMs') || key.endsWith('DebounceMs');
        const dir = (attempt + m) % 2 === 0 ? (preferUp ? 1 : -1) : (preferUp ? -1 : 1);
        const mult = 1 + (attempt % 3);
        next = Math.min(spec.max, Math.max(spec.min, Number(cur) + dir * step * mult));
      }

      if (next !== cur) {
        setByDot(candDoc.params, key, next);
        changes.push({ key, from: cur, to: next });
      }
    }

    if (!changes.length) continue;
    const sig = JSON.stringify(candDoc.params);
    if (!tried.has(sig)) {
      tried.add(sig);
      return { candDoc, changes };
    }
  }
  return null;
}

function evaluateCandidate(bestScores, candScores) {
  if (!candScores.memoryGuardOk) {
    return { accept: false, reason: 'memory_guard_failed' };
  }
  const relImprove = (bestScores.composite - candScores.composite) / Math.max(0.001, bestScores.composite);
  if (relImprove < MIN_IMPROVE) {
    return { accept: false, reason: `insufficient_improvement (${(relImprove * 100).toFixed(2)}%)` };
  }
  for (const axis of ['startup', 'keyboard', 'voice']) {
    const base = Math.max(0.001, bestScores[axis]);
    const reg = (candScores[axis] - base) / base;
    if (reg > MAX_AXIS_REGRESSION) {
      return { accept: false, reason: `${axis}_regressed (+${(reg * 100).toFixed(1)}%)` };
    }
  }
  return { accept: true, relImprove };
}

function main() {
  const initialRaw = fs.readFileSync(PARAMS_PATH, 'utf8');
  let bestDoc = JSON.parse(initialRaw);
  const tried = loadTriedSignatures();
  tried.add(JSON.stringify(bestDoc.params));

  console.error(
    `[autotune] start: maxRounds=${MAX_ROUNDS}, maxIters/round=${MAX_ITERS_PER_ROUND}, ` +
    `patience=${PATIENCE}, skipNative=${SKIP_NATIVE}, fastGate=${FAST_GATE}`
  );
  let bestReport = runBench(PARAMS_PATH);
  const initialScores = deepClone(bestReport.scores);
  console.error(
    `[autotune] baseline composite=${bestReport.scores.composite} ` +
    `(startup=${bestReport.scores.startup}, keyboard=${bestReport.scores.keyboard}, ` +
    `voice=${bestReport.scores.voice}, proc=${bestReport.scores.processing})`
  );

  appendHistory({
    timestamp: new Date().toISOString(),
    kind: 'baseline',
    status: 'BASELINE',
    scores: bestReport.scores,
    native: bestReport.native,
    browser: bestReport.browser,
    params: bestDoc.params
  });

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'syki-autotune-loop-'));
  let totalAdopted = 0;
  let totalEvaluated = 0;
  const roundSummaries = [];
  let stopReason = 'completed_max_rounds';

  try {
    for (let round = 1; round <= MAX_ROUNDS; round++) {
      let roundAdopted = 0;
      let roundEvaluated = 0;
      let staleStreak = 0;
      const roundStartScore = bestReport.scores.composite;
      console.error(`[autotune] === Round ${round}/${MAX_ROUNDS} (current best=${roundStartScore}) ===`);

      for (let iter = 1; iter <= MAX_ITERS_PER_ROUND; iter++) {
        const proposal = generateCandidate(bestDoc, tried, round, iter);
        if (!proposal) {
          console.error(`[autotune] R${round} I${iter}: search space exhausted`);
          stopReason = 'search_space_exhausted';
          break;
        }
        const { candDoc, changes } = proposal;
        const changeDesc = changes.map((c) => `${c.key}: ${c.from} -> ${c.to}`).join(', ');
        console.error(`[autotune] R${round} I${iter}/${MAX_ITERS_PER_ROUND} testing: ${changeDesc}`);

        const candFile = path.join(tmpDir, `r${round}-i${iter}.json`);
        fs.writeFileSync(candFile, JSON.stringify(candDoc, null, 2) + '\n', 'utf8');
        const mode = selectModeForChanges(changes);

        let candReport;
        try {
          candReport = runBench(candFile, mode);
        } catch (err) {
          staleStreak++;
          roundEvaluated++;
          totalEvaluated++;
          console.error(`[autotune] R${round} I${iter} bench error: ${err.message}`);
          appendHistory({
            timestamp: new Date().toISOString(),
            kind: 'candidate',
            round,
            iteration: iter,
            mode,
            status: 'BENCH_ERROR',
            changes,
            error: err.message,
            params: candDoc.params
          });
          if (staleStreak >= PATIENCE) {
            console.error(`[autotune] R${round} early-stop after ${staleStreak} non-improving iterations`);
            break;
          }
          continue;
        }

        roundEvaluated++;
        totalEvaluated++;
        const mergedFirst = mergeAxisScores(bestReport.scores, candReport.scores, mode);
        let verdict = evaluateCandidate(bestReport.scores, mergedFirst);

        // Confirmation run to eliminate single-run WebView/Edge noise before adopting
        let finalScores = mergedFirst;
        if (verdict.accept) {
          console.error(
            `[autotune] R${round} I${iter} [${mode}] tentative win (${mergedFirst.composite} vs ${bestReport.scores.composite}) -> confirmation bench...`
          );
          try {
            const confirmReport = runBench(candFile, mode);
            const mergedConfirm = mergeAxisScores(bestReport.scores, confirmReport.scores, mode);
            finalScores = averageScores(mergedFirst, mergedConfirm);
            verdict = evaluateCandidate(bestReport.scores, finalScores);
          } catch (err) {
            verdict = { accept: false, reason: `confirm_bench_error: ${err.message}` };
          }
        }

        if (!verdict.accept) {
          staleStreak++;
          console.error(
            `[autotune] R${round} I${iter} REJECTED (${finalScores.composite} vs ${bestReport.scores.composite}): ${verdict.reason} [stale ${staleStreak}/${PATIENCE}]`
          );
          appendHistory({
            timestamp: new Date().toISOString(),
            kind: 'candidate',
            round,
            iteration: iter,
            status: 'REJECTED',
            reason: verdict.reason,
            changes,
            scores: finalScores,
            params: candDoc.params
          });
          if (staleStreak >= PATIENCE) {
            console.error(`[autotune] R${round} early-stop after ${staleStreak} non-improving iterations`);
            break;
          }
          continue;
        }

        const prevBestRaw = JSON.stringify(bestDoc, null, 2) + '\n';
        fs.writeFileSync(PARAMS_PATH, JSON.stringify(candDoc, null, 2) + '\n', 'utf8');
        console.error(`[autotune] R${round} I${iter} confirmed +${(verdict.relImprove * 100).toFixed(2)}% -> UX gate...`);

        const gate = runUxGate(false);
        if (!gate.ok) {
          fs.writeFileSync(PARAMS_PATH, prevBestRaw, 'utf8');
          staleStreak++;
          console.error(`[autotune] R${round} I${iter} REVERTED (UX gate failed at ${gate.failedStep})`);
          appendHistory({
            timestamp: new Date().toISOString(),
            kind: 'candidate',
            round,
            iteration: iter,
            status: 'REVERTED_UX_GATE',
            failedStep: gate.failedStep,
            changes,
            scores: finalScores,
            params: candDoc.params
          });
          if (staleStreak >= PATIENCE) break;
          continue;
        }

        bestDoc = candDoc;
        bestReport = { ...candReport, scores: finalScores };
        roundAdopted++;
        totalAdopted++;
        staleStreak = 0;
        console.error(`[autotune] R${round} I${iter} ADOPTED! new best composite=${bestReport.scores.composite}`);
        appendHistory({
          timestamp: new Date().toISOString(),
          kind: 'candidate',
          round,
          iteration: iter,
          status: 'ADOPTED',
          improvementPct: Number((verdict.relImprove * 100).toFixed(2)),
          changes,
          scores: finalScores,
          gateResults: gate.results,
          params: candDoc.params
        });
      }

      const roundGainPct = Number((((roundStartScore - bestReport.scores.composite) / Math.max(0.001, roundStartScore)) * 100).toFixed(2));
      roundSummaries.push({
        round,
        evaluated: roundEvaluated,
        adopted: roundAdopted,
        startScore: roundStartScore,
        endScore: bestReport.scores.composite,
        gainPct: roundGainPct
      });

      if (roundAdopted === 0) {
        stopReason = `plateau_at_round_${round}`;
        console.error(`[autotune] Round ${round} produced 0 adoptions -> stopping early (no further gain).`);
        break;
      }
      if (stopReason === 'search_space_exhausted') break;
    }
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }

  const totalGainPct = Number(
    (((initialScores.composite - bestReport.scores.composite) / Math.max(0.001, initialScores.composite)) * 100).toFixed(2)
  );
  const summary = {
    stopReason,
    roundsCompleted: roundSummaries.length,
    totalEvaluated,
    totalAdopted,
    initialScores,
    bestScores: bestReport.scores,
    totalGainPct,
    roundSummaries,
    bestParams: bestDoc.params,
    historyPath: path.relative(REPO, HISTORY_PATH)
  };
  process.stdout.write(JSON.stringify(summary, null, 2) + '\n');
}

main();
