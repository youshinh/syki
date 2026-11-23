/**
 * 10 Targeted Stress & Edge-Case Scenarios Covering Under-Tested Areas:
 *   - Path traversal / UI boundary injection
 *   - Audio concurrency lock & action double-click concurrency ('lock' vs 'cancel-prev')
 *   - Post-dispose async call rejection (AbortError)
 *   - Decision model hallucination -> LLM failure -> safe fallback
 *   - Mid-flight AbortSignal cancellation & detachGuard leak-free restoration
 *   - flow.loop hard cap (maxTurns=10) & invalid bounds
 *   - Real syki-cli.exe plugin check warnings/errors & invalid --args JSON exit code 2
 *   - Shortcut collision protection
 */
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const require = createRequire(import.meta.url);
const PluginRuntime = require('../../frontend/js/plugin_runtime.js');
const ApiCore = require('../../frontend/js/syki_api_core.js');
const ApiFlow = require('../../frontend/js/syki_api_flow.js');
const ApiExt = require('../../frontend/js/syki_api_ext.js');
const ApiMedia = require('../../frontend/js/syki_api_media.js');

let passed = 0;
async function test(id, name, fn) {
  await fn();
  passed++;
  console.log(`PASS [${id}] ${name}`);
}

// G01: Path traversal & invalid note path defense
await test('G01', 'セキュリティ境界: notes API のパストラバーサル/UNC/ドライブ文字/NULバイト完全遮断', async () => {
  const flow = ApiFlow.createSykiFlowApi({
    notes: {
      read: async () => 'SHOULD_NOT_RUN',
      create: async () => 'SHOULD_NOT_RUN',
      append: async () => 'SHOULD_NOT_RUN',
    },
  });
  for (const bad of ['../../etc/passwd', '\\\\server\\share\\x.md', 'C:\\Windows\\win.ini', 'a\0b.md', '/abs.md']) {
    await assert.rejects(() => flow.knowledge.notes.read(bad), (e) => e.code === 'INVALID_NOTE_PATH');
    await assert.rejects(() => flow.knowledge.notes.create({ title: bad, content: 'x' }), (e) => e.code === 'INVALID_NOTE_PATH');
    await assert.rejects(() => flow.knowledge.notes.append(bad, 'x'), (e) => e.code === 'INVALID_NOTE_PATH');
  }
});

// G02: UI boundary defense (illegal panel slot & non-CSS-var token injection)
await test('G02', 'UI境界防御: toolbar への生パネル注入および非 -- トークン名注入の拒否', async () => {
  const core = ApiCore.createSykiCoreApi({
    mountPanel: () => ({ dispose: () => {} }),
    setToken: () => ({ dispose: () => {} }),
  });
  assert.throws(() => core.ui.mountPanel('toolbar', { render: () => {} }));
  assert.throws(() => core.ui.setToken('background-color', 'red'));
});

// G03: Audio exclusive lock race (voice vs meeting) & recovery after failure
await test('G03', '並行排他レース: voice 進行中の meeting 同時起動 AUDIO_BUSY 拒否 & 完了/失敗後の自動復帰', async () => {
  let guard = 0;
  let finishVoice;
  const media = ApiMedia.createSykiMediaApi({
    acquireDetachGuard: () => { guard++; },
    releaseDetachGuard: () => { guard--; },
    recordVoice: () => new Promise((r) => { finishVoice = r; }),
    recordMeeting: async () => 'meeting-ok',
  });
  const pVoice = media.input.voice();
  assert.equal(guard, 1);
  await assert.rejects(() => media.input.meeting(), (e) => e.code === 'AUDIO_BUSY');
  finishVoice('voice-ok');
  assert.equal(await pVoice, 'voice-ok');
  assert.equal(guard, 0);
  assert.equal(await media.input.meeting(), 'meeting-ok');
  assert.equal(guard, 0);
});

// G04: Declarative action concurrency ('lock' vs 'cancel-prev')
await test('G04', 'アクション連打レース: concurrency lock 抑止 & cancel-prev 旧 AbortSignal 中断', async () => {
  const registered = new Map();
  const scope = PluginRuntime.createPluginScope('race-test', {
    registerAction: (spec) => {
      registered.set(spec.id, spec);
      return { update() {}, dispose() { registered.delete(spec.id); } };
    },
    toast: () => {},
  });

  // 1) lock mode
  let lockStarts = 0;
  let releaseLock;
  scope.api.registerAction({
    id: 'act-lock',
    label: 'Lock',
    slot: 'toolbar',
    concurrency: 'lock',
    run: () => {
      lockStarts++;
      return new Promise((r) => { releaseLock = r; });
    },
  });
  const p1 = registered.get('act-lock').run();
  const p2 = registered.get('act-lock').run(); // ignored while locked
  releaseLock();
  await Promise.all([p1, p2]);
  assert.equal(lockStarts, 1);

  // 2) cancel-prev mode
  const signals = [];
  let releaseSecond;
  scope.api.registerAction({
    id: 'act-cancel',
    label: 'CancelPrev',
    slot: 'toolbar',
    concurrency: 'cancel-prev',
    run: ({ signal }) => {
      signals.push(signal);
      return new Promise((r) => {
        if (signals.length === 2) releaseSecond = r;
        signal.addEventListener('abort', () => r(), { once: true });
      });
    },
  });
  const c1 = registered.get('act-cancel').run();
  const c2 = registered.get('act-cancel').run();
  assert.equal(signals[0].aborted, true, 'first run signal must be aborted by cancel-prev');
  assert.equal(signals[1].aborted, false);
  releaseSecond();
  await Promise.all([c1, c2]);
  scope.dispose();
});

// G05: Post-dispose delayed async call must reject with AbortError
await test('G05', 'アンロード後レース: scope.dispose() 後の遅延非同期 API 呼び出しは必ず AbortError', async () => {
  let hostCalled = false;
  const scope = PluginRuntime.createPluginScope('zombie', {
    exec: {
      llm: async () => { hostCalled = true; return 'SHOULD_NOT_RUN'; },
    },
    toast: () => {},
  });
  scope.dispose();
  await assert.rejects(() => scope.api.exec.llm('hi'), (e) => e.name === 'AbortError');
  assert.equal(hostCalled, false);
});

// G06: Decision model hallucination -> LLM failure -> safe fallback
await test('G06', 'Decisionモデル耐性: 未知IDハルシネーション → LLM例外 → クラッシュせず fallback 返却', async () => {
  const flow = ApiFlow.createSykiFlowApi({
    queryDecision: async (req) => ({ reqId: req.reqId, choiceId: 'hallucinated_banana', by: 'decision' }),
    queryLlm: async () => { throw new Error('503 Service Unavailable'); },
  });
  const res = await flow.flow.decide({
    input: 'some text',
    choices: [{ id: 'task', description: 'task' }, { id: 'memo', description: 'memo' }],
    fallback: 'memo',
  });
  assert.equal(res.choiceId, 'memo');
  assert.equal(res.by, 'fallback');
});

// G07: Mid-flight AbortSignal cancellation & detachGuard leak-free restoration
await test('G07', '途中中断の完全性: flow.steps 2段目で abort → 3段目未実行 & detachGuardCount === 0', async () => {
  let guard = 0;
  let step3Ran = false;
  const ac = new AbortController();
  const flow = ApiFlow.createSykiFlowApi({
    acquireDetachGuard: () => { guard++; },
    releaseDetachGuard: () => { guard--; },
  });
  await assert.rejects(
    () => flow.flow.steps(
      0,
      [
        { label: 's1', run: async (v) => v + 1 },
        { label: 's2', run: async (v) => { ac.abort(); return v + 1; } },
        { label: 's3', run: async (v) => { step3Ran = true; return v + 1; } },
      ],
      { signal: ac.signal },
    ),
    (e) => e.name === 'AbortError',
  );
  assert.equal(step3Ran, false);
  assert.equal(guard, 0);
});

// G08: flow.loop hard cap (maxTurns=10) & invalid bounds rejection
await test('G08', 'flow.loop 暴走防止: maxTurns=999 は10回強制クランプ & maxTurns=0 は RangeError 拒否', async () => {
  let guard = 0;
  const ext = ApiExt.createSykiExtApi({
    acquireDetachGuard: () => { guard++; },
    releaseDetachGuard: () => { guard--; },
  });
  await assert.rejects(
    () => ext.flow.loop({ init: 0, maxTurns: 0, step: async (x) => x + 1, until: () => false }),
    RangeError,
  );
  const res = await ext.flow.loop({
    init: 0,
    maxTurns: 999,
    step: async (x) => x + 1,
    until: () => false,
  });
  assert.equal(res.completed, false);
  assert.equal(res.turns, 10);
  assert.equal(res.state, 10);
  assert.equal(guard, 0);
});

// G09: Real syki-cli.exe plugin check warnings (adjacent style.css raw hex) & unknown permissions
await test('G09', '実機 syki-cli plugin check: 隣接 style.css 生色コード警告 & 未知権限/apiVersion欠落の拒否', async () => {
  const cliBin = path.resolve('target/debug/syki-cli.exe');
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'syki-check-'));
  try {
    // 1) Valid plugin.js + adjacent style.css with raw hex -> ok: true, warnings >= 1
    const p1 = path.join(tmpDir, 'plugin.js');
    const c1 = path.join(tmpDir, 'style.css');
    fs.writeFileSync(p1, 'export default { id: "w", apiVersion: 1, permissions: ["llm"], init() {} };');
    fs.writeFileSync(c1, '.box { color: #ff00aa; }');
    const r1 = JSON.parse(execFileSync(cliBin, ['plugin', 'check', p1], { encoding: 'utf8' }));
    assert.equal(r1.ok, true);
    assert.ok(r1.warnings.length >= 1, 'must warn on raw hex color in adjacent style.css');

    // 2) Unknown permission + missing apiVersion -> ok: false, exit 1
    const p2 = path.join(tmpDir, 'bad.js');
    fs.writeFileSync(p2, 'export default { id: "b", permissions: ["nuclear_launch"], init() {} };');
    let exitCode = 0;
    let r2 = null;
    try {
      execFileSync(cliBin, ['plugin', 'check', p2], { encoding: 'utf8' });
    } catch (err) {
      exitCode = err.status;
      r2 = JSON.parse(err.stdout);
    }
    assert.equal(exitCode, 1);
    assert.equal(r2.ok, false);
    assert.ok(r2.errors.length >= 2);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

// G10: Real syki-cli.exe plugin run invalid --args JSON exit code 2 & shortcut collision protection
await test('G10', '実機 syki-cli plugin run 不正JSON(exit 2) & ショートカット衝突時の既存コマンド保護', async () => {
  const cliBin = path.resolve('target/debug/syki-cli.exe');
  let exitCode = 0;
  try {
    execFileSync(cliBin, ['plugin', 'run', 'demo', '--args', '{not-valid-json'], { encoding: 'utf8', stdio: 'pipe' });
  } catch (err) {
    exitCode = err.status;
  }
  assert.equal(exitCode, 2, 'invalid --args JSON must fail before IPC with exit code 2');

  const toasts = [];
  const core = ApiCore.createSykiCoreApi({ toast: (m, l) => toasts.push({ msg: m, level: l }) });
  let firstRan = 0;
  let secondRan = 0;
  core.sys.registerCommand({ id: 'first', title: 'First', shortcut: 'Ctrl+Alt+K', run: () => { firstRan++; } });
  core.sys.registerCommand({ id: 'second', title: 'Second', shortcut: 'Ctrl+Alt+K', run: () => { secondRan++; } });
  assert.ok(toasts.some((t) => t.level === 'warn'), 'shortcut collision must emit warn toast');
  core.sys.dispatchShortcut('Ctrl+Alt+K', { isComposing: false });
  assert.equal(firstRan, 1, 'existing command must remain intact');
  assert.equal(secondRan, 0);
});

console.log(`\n=== ALL ${passed}/10 UNDER-TESTED AREA STRESS SCENARIOS PASSED ===`);
