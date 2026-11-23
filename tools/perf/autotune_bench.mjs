// Multi-axis performance benchmark for syki-lite autotuning pipeline.
// Measures:
//   1. Startup / Screen-to-Interactive (35%): native WebView2 cold/warm app_ready + T0..T3 trace + hotkey wake
//   2. Keyboard responsiveness (35%): typing p95, main-thread heartbeat max, bulk insert latency, find sync
//   3. Voice responsiveness (20%): deterministic VAD pause detection overshoot + anchor roundtrip
//   4. Processing & memory (10%): scriptMs, layoutMs, heapMb + PWS memory guards
//
// Usage:
//   node tools/perf/autotune_bench.mjs [--params config/perf_params.json] [--skip-native] [--out report.json]
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { startExplore, sleep } from '../explore/kit.mjs';
import { generateNote } from './gen_note.mjs';

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

const PARAMS_PATH = path.resolve(opt('--params', path.join(REPO, 'config', 'perf_params.json')));
const SKIP_NATIVE = hasFlag('--skip-native');
const MODE = opt('--mode', SKIP_NATIVE ? 'browser' : 'all'); // all | native | browser
const OUT_PATH = opt('--out', '');

const round2 = (n) => Number(Number(n || 0).toFixed(2));
function median(arr) {
  if (!arr.length) return 0;
  const s = arr.slice().sort((a, b) => a - b);
  return s[Math.floor(s.length / 2)];
}
function p95(arr) {
  if (!arr.length) return 0;
  const s = arr.slice().sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(s.length * 0.95))];
}

function loadParamsJson(p) {
  const raw = fs.readFileSync(p, 'utf8');
  const parsed = JSON.parse(raw);
  return parsed && parsed.params ? parsed.params : parsed;
}

function parseTraceFile(tracePath) {
  if (!fs.existsSync(tracePath)) return null;
  const lines = fs.readFileSync(tracePath, 'utf8').split(/\r?\n/).filter(Boolean);
  const byPhase = {};
  for (const line of lines) {
    try {
      const row = JSON.parse(line);
      if (!row.phase || typeof row.ms !== 'number') continue;
      if (!byPhase[row.phase]) byPhase[row.phase] = [];
      byPhase[row.phase].push(row.ms);
    } catch {}
  }
  const out = {};
  for (const [k, vals] of Object.entries(byPhase)) {
    out[k] = {
      firstMs: round2(vals[0]),
      warmMedianMs: round2(vals.length > 1 ? median(vals.slice(1)) : vals[0]),
      samples: vals.length
    };
  }
  return out;
}

function runNativeBench(paramsPath) {
  const guiExe = path.join(REPO, 'target', 'release', 'syki.exe');
  const cliExe = path.join(REPO, 'target', 'release', 'syki-cli.exe');
  if (MODE === 'browser' || SKIP_NATIVE || !fs.existsSync(guiExe) || !fs.existsSync(cliExe)) {
    return { skipped: true, reason: MODE === 'browser' ? 'mode=browser' : (SKIP_NATIVE ? 'flag' : 'release binary missing') };
  }

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'syki-autotune-native-'));
  const outJson = path.join(tmpDir, 'native.json');
  const traceFile = path.join(tmpDir, 'trace.jsonl');
  try {
    const env = {
      ...process.env,
      SYKI_PERF_PARAMS_FILE: paramsPath,
      SYKI_PERF_TRACE_FILE: traceFile
    };
    const benchScript = path.join(REPO, 'tools', 'perf', 'native_load_bench.ps1');
    const res = spawnSync(
      'pwsh',
      ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', benchScript, '-OutJson', outJson],
      { cwd: REPO, env, encoding: 'utf8', timeout: 120000 }
    );
    if (res.status !== 0 || !fs.existsSync(outJson)) {
      return {
        skipped: false,
        error: `native_load_bench failed (exit ${res.status}): ${(res.stderr || res.stdout || '').slice(-300)}`
      };
    }
    const data = JSON.parse(fs.readFileSync(outJson, 'utf8'));
    const probeRes = spawnSync(guiExe, ['--perf-probe'], {
      cwd: REPO,
      env,
      encoding: 'utf8',
      timeout: 15000
    });
    let hotkeyWakeMs = 0;
    try {
      const probe = JSON.parse((probeRes.stdout || '').trim());
      hotkeyWakeMs = Number(probe.hotkeyWakeMs || 0);
    } catch {}

    return {
      skipped: false,
      coldAppReadyMs: round2(data.coldAppReadyMs),
      warmAppReadyMs: round2(data.warmAppReadyMedianMs),
      reattachWakeMs: round2(data.reattachWakeMs ?? 0),
      hotkeyWakeMs: round2(hotkeyWakeMs),
      attachedPwsMb: round2(data.attached?.totalPwsMb ?? 0),
      detachedPwsMb: round2(data.detached?.totalPwsMb ?? 0),
      trace: parseTraceFile(traceFile)
    };
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
}

async function runBrowserBench(paramsPath, params) {
  const prevEnv = process.env.SYKI_PERF_PARAMS_FILE;
  process.env.SYKI_PERF_PARAMS_FILE = paramsPath;
  const t0 = performance.now();
  const s = await startExplore({ title: 'syki-autotune' });
  const cdpReadyMs = round2(performance.now() - t0);

  try {
    await s.page.cdp.send('Performance.enable');
    // Install heartbeat monitor (10ms interval)
    await s.ev(`(function () {
      window.__atHb = { max: 0, last: performance.now() };
      window.__atHbId = setInterval(function () {
        var now = performance.now();
        var gap = now - window.__atHb.last;
        if (gap > window.__atHb.max) window.__atHb.max = gap;
        window.__atHb.last = now;
      }, 10);
      return 1;
    })()`);

    // Load a realistic ~65KB markdown note (1500 lines) with code blocks & links
    const noteText = generateNote({ lines: 1500, seed: 20261004 });
    await s.ev(`(function (txt) {
      var ed = document.getElementById('editor');
      ed.value = txt;
      ed.dispatchEvent(new Event('input', { bubbles: true }));
      return ed.value.length;
    })(${JSON.stringify(noteText)})`);
    await sleep(300);

    // 1. Keyboard typing p95 + heartbeat
    await s.ev(`(function () {
      var ed = document.getElementById('editor');
      ed.focus();
      var mid = Math.floor(ed.value.length / 2);
      ed.setSelectionRange(mid, mid);
      window.__atHb.max = 0;
      window.__atHb.last = performance.now();
      return mid;
    })()`);

    const typingSamples = [];
    for (let i = 0; i < 20; i++) {
      const k0 = performance.now();
      await s.key('a');
      await s.ev(`new Promise(function (r) {
        var done = false;
        var finish = function () { if (!done) { done = true; r(1); } };
        requestAnimationFrame(function () { setTimeout(finish, 0); });
        setTimeout(finish, 80);
      })`);
      typingSamples.push(performance.now() - k0);
    }
    const heartbeatMaxMs = Number(await s.ev('Math.round(window.__atHb.max)')) || 0;

    // 2. Bulk insert latency (1500 lines)
    const bulkLines = Array.from({ length: 1500 }, (_, i) => `Line ${i}: syki autotune payload text`).join('\n');
    const b0 = performance.now();
    await s.ev(`(function (payload) {
      var ed = document.getElementById('editor');
      ed.focus();
      if (window.BulkInsert && window.BulkInsert.wanted(payload, ed.value)) {
        window.BulkInsert.exec(ed, payload);
      } else {
        document.execCommand('insertText', false, payload);
      }
      return ed.value.length;
    })(${JSON.stringify(bulkLines)})`);
    await s.ev('new Promise(function (r) { requestAnimationFrame(function () { requestAnimationFrame(function () { r(1); }); }); })');
    const bulkInsertMs = round2(performance.now() - b0);

    // 3. Deterministic Voice Input benchmark (VAD pause overshoot + anchor roundtrip)
    const sampleIntervalMs = Number(params?.voice?.sampleIntervalMs || 200);
    const barSampleMs = Number(params?.voice?.barSampleMs || 100);
    const barMinVoicedMs = Number(params?.voice?.barMinVoicedMs || 200);

    const voiceResult = await s.ev(`(function (sampleMs, barSampleMs, minVoicedMs) {
      // A) BarVoice VAD pause detection overshoot (speech stops at t=1200ms, silence threshold=1500ms -> ideal stop at 2700ms)
      var overshoots = [];
      var tCompute0 = performance.now();
      if (window.BarVoice && window.BarVoice.createListenState && window.BarVoice.listenUpdate) {
        for (var phase = 0; phase < 5; phase++) {
          var st = window.BarVoice.createListenState({ silenceSec: 1.5, noSpeechSec: 6, maxSec: 60, rmsThreshold: 0.015 });
          var offset = Math.round((phase / 5) * barSampleMs);
          var stoppedAt = 6000;
          for (var t = offset; t <= 4000; t += barSampleMs) {
            var rms = (t >= 100 && t <= 1200) ? 0.08 : 0.002;
            var reason = window.BarVoice.listenUpdate(st, rms, t);
            if (reason) { stoppedAt = t; break; }
          }
          overshoots.push(Math.max(0, stoppedAt - 2700));
        }
      }
      var vadComputeMs = performance.now() - tCompute0;

      // B) VoiceInput anchor insert -> transcribe replace roundtrip on active editor
      var tAnc0 = performance.now();
      var ed = document.getElementById('editor');
      var orig = ed.value;
      var marker = '[音声入力中... #at01]';
      var pos = Math.min(500, orig.length);
      ed.value = orig.slice(0, pos) + marker + orig.slice(pos);
      ed.dispatchEvent(new Event('input', { bubbles: true }));
      var idx = ed.value.indexOf(marker);
      if (idx >= 0) {
        ed.value = ed.value.slice(0, idx) + '音声入力テスト結果' + ed.value.slice(idx + marker.length);
        ed.dispatchEvent(new Event('input', { bubbles: true }));
      }
      var anchorRoundtripMs = performance.now() - tAnc0;

      // C) Effective worst-case silence detection jitter from VoiceInput sampleIntervalMs
      var avgOvershoot = overshoots.length ? overshoots.reduce(function (a, b) { return a + b; }, 0) / overshoots.length : barSampleMs;
      var effectiveVadDelayMs = avgOvershoot + (sampleMs * 0.5);
      return {
        vadOvershootMs: effectiveVadDelayMs,
        vadComputeMs: vadComputeMs,
        anchorRoundtripMs: anchorRoundtripMs
      };
    })(${sampleIntervalMs}, ${barSampleMs}, ${barMinVoicedMs})`);

    // 4. Processing metrics via CDP Performance + Heap
    const [{ metrics }, heap] = await Promise.all([
      s.page.cdp.send('Performance.getMetrics'),
      s.page.cdp.send('Runtime.getHeapUsage')
    ]);
    const mmap = {};
    for (const m of metrics) mmap[m.name] = m.value;

    await s.ev('clearInterval(window.__atHbId); 1');

    return {
      cdpReadyMs,
      keyboard: {
        typingMedianMs: round2(median(typingSamples)),
        typingP95Ms: round2(p95(typingSamples)),
        heartbeatMaxMs: round2(heartbeatMaxMs),
        bulkInsertMs
      },
      voice: {
        vadOvershootMs: round2(voiceResult.vadOvershootMs),
        vadComputeMs: round2(voiceResult.vadComputeMs),
        anchorRoundtripMs: round2(voiceResult.anchorRoundtripMs)
      },
      processing: {
        scriptMs: round2((mmap.ScriptDuration || 0) * 1000),
        layoutMs: round2((mmap.LayoutDuration || 0) * 1000),
        taskMs: round2((mmap.TaskDuration || 0) * 1000),
        heapMb: round2((heap.usedSize || mmap.JSHeapUsedSize || 0) / (1024 * 1024)),
        nodes: Math.round(mmap.Nodes || 0)
      }
    };
  } finally {
    if (prevEnv === undefined) delete process.env.SYKI_PERF_PARAMS_FILE;
    else process.env.SYKI_PERF_PARAMS_FILE = prevEnv;
    await s.close();
  }
}

function computeScores(native, browser) {
  // Lower score is better (1.0 = reference baseline).
  const startupScore = (!native.skipped && !native.error && native.warmAppReadyMs > 0)
    ? (native.warmAppReadyMs / 400) * 0.65 +
      (native.hotkeyWakeMs / 15) * 0.20 +
      (native.coldAppReadyMs / 7000) * 0.15
    : (browser.cdpReadyMs / 400);

  const keyboardScore =
    (browser.keyboard.typingP95Ms / 25) * 0.55 +
    (browser.keyboard.heartbeatMaxMs / 40) * 0.25 +
    (browser.keyboard.bulkInsertMs / 80) * 0.20;

  const voiceScore =
    (browser.voice.vadOvershootMs / 150) * 0.55 +
    (browser.voice.anchorRoundtripMs / 30) * 0.45;

  const processingScore =
    ((browser.processing.scriptMs + browser.processing.layoutMs) / 500) * 0.60 +
    (browser.processing.heapMb / 60) * 0.40;

  const composite =
    startupScore * 0.35 +
    keyboardScore * 0.35 +
    voiceScore * 0.20 +
    processingScore * 0.10;

  const memoryGuardOk =
    native.skipped || native.error
      ? true
      : (native.attachedPwsMb <= 140.0 && native.detachedPwsMb <= Math.max(140.0, native.attachedPwsMb + 5.0));

  return {
    startup: round2(startupScore),
    keyboard: round2(keyboardScore),
    voice: round2(voiceScore),
    processing: round2(processingScore),
    composite: round2(composite),
    memoryGuardOk
  };
}

async function main() {
  const params = loadParamsJson(PARAMS_PATH);
  const native = runNativeBench(PARAMS_PATH);
  const browser = MODE === 'native'
    ? {
        skipped: true,
        cdpReadyMs: 0,
        keyboard: { typingMedianMs: 0, typingP95Ms: 0, heartbeatMaxMs: 0, bulkInsertMs: 0 },
        voice: { vadOvershootMs: 0, vadComputeMs: 0, anchorRoundtripMs: 0 },
        processing: { scriptMs: 0, layoutMs: 0, taskMs: 0, heapMb: 0, nodes: 0 }
      }
    : await runBrowserBench(PARAMS_PATH, params);
  const scores = computeScores(native, browser);

  const report = {
    timestamp: new Date().toISOString(),
    mode: MODE,
    paramsPath: path.relative(REPO, PARAMS_PATH),
    params,
    native,
    browser,
    scores
  };

  const jsonStr = JSON.stringify(report, null, 2);
  if (OUT_PATH) {
    fs.mkdirSync(path.dirname(path.resolve(OUT_PATH)), { recursive: true });
    fs.writeFileSync(OUT_PATH, jsonStr + '\n', 'utf8');
  }
  process.stdout.write(jsonStr + '\n');
}

main().catch((err) => {
  console.error('[autotune_bench] fatal:', err?.stack || err);
  process.exit(1);
});
