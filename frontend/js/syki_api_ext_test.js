'use strict';

const assert = require('assert');
const { MAX_LOOP_TURNS, createSykiExtApi, drainQuickCaptureQueue } = require('./syki_api_ext.js');

async function testFlowLoopNormalConvergence() {
  let guardCount = 0;
  let maxGuardObserved = 0;
  const api = createSykiExtApi({
    acquireDetachGuard() {
      guardCount += 1;
      maxGuardObserved = Math.max(maxGuardObserved, guardCount);
    },
    releaseDetachGuard() {
      guardCount -= 1;
    },
  });

  const res = await api.flow.loop({
    init: 0,
    maxTurns: 5,
    step: async (state, turn) => {
      assert.strictEqual(guardCount, 1, 'detachGuard must be held during step');
      return state + turn;
    },
    until: (state) => state >= 6,
  });

  assert.deepStrictEqual(res, { state: 6, turns: 3, completed: true });
  assert.strictEqual(maxGuardObserved, 1);
  assert.strictEqual(guardCount, 0, 'detachGuard must be released after loop');
}

async function testFlowLoopClampsMaxTurnsTo10AndRejectsInvalid() {
  assert.strictEqual(MAX_LOOP_TURNS, 10);
  const api = createSykiExtApi();

  const res = await api.flow.loop({
    init: 0,
    maxTurns: 99,
    step: (state) => state + 1,
    until: () => false,
  });

  assert.deepStrictEqual(res, { state: 10, turns: 10, completed: false });

  for (const invalid of [undefined, null, 0, -3, NaN, Infinity, 0.5]) {
    let threw = false;
    try {
      await api.flow.loop({
        init: 0,
        maxTurns: invalid,
        step: (s) => s,
      });
    } catch (err) {
      threw = true;
      assert.ok(err instanceof RangeError, `expected RangeError for ${invalid}`);
    }
    assert.strictEqual(threw, true, `maxTurns=${invalid} must be rejected`);
  }
}

async function testFlowLoopAbortSignalAndGuardRelease() {
  let guardCount = 0;
  const api = createSykiExtApi({
    acquireDetachGuard() {
      guardCount += 1;
    },
    releaseDetachGuard() {
      guardCount -= 1;
    },
  });

  // Pre-aborted signal rejects immediately without leaving guard acquired.
  const preAborted = new AbortController();
  preAborted.abort('cancelled-before-start');
  let preErr = null;
  try {
    await api.flow.loop({
      init: 0,
      maxTurns: 5,
      step: () => {
        throw new Error('should not run');
      },
      signal: preAborted.signal,
    });
  } catch (err) {
    preErr = err;
  }
  assert.ok(preErr, 'pre-aborted signal must throw');
  assert.strictEqual(preErr.name, 'AbortError');
  assert.strictEqual(guardCount, 0);

  // Mid-loop abort stops immediately and releases detachGuard in finally.
  const midController = new AbortController();
  let executedTurns = 0;
  let midErr = null;
  try {
    await api.flow.loop({
      init: 0,
      maxTurns: 10,
      signal: midController.signal,
      step: async (state, turn) => {
        executedTurns = turn;
        if (turn === 2) {
          midController.abort('stop-at-2');
        }
        return state + 1;
      },
      until: () => false,
    });
  } catch (err) {
    midErr = err;
  }
  assert.ok(midErr, 'mid-loop abort must throw');
  assert.strictEqual(midErr.name, 'AbortError');
  assert.strictEqual(executedTurns, 2);
  assert.strictEqual(guardCount, 0, 'detachGuard must be released on AbortError');

  // Step error also releases detachGuard.
  let stepErr = null;
  try {
    await api.flow.loop({
      init: 0,
      maxTurns: 3,
      step: () => {
        throw new Error('boom');
      },
    });
  } catch (err) {
    stepErr = err;
  }
  assert.ok(stepErr && stepErr.message === 'boom');
  assert.strictEqual(guardCount, 0, 'detachGuard must be released on step error');
}

function testDrainQuickCaptureQueueOrderedAndFaultTolerant() {
  const api = createSykiExtApi();
  const events = [
    { text: 'first', assetPaths: [], scrapPath: 'a.md' },
    { text: 'bad-second', assetPaths: ['x.png'], scrapPath: 'b.md' },
    { text: 'third', assetPaths: [], scrapPath: 'c.md' },
  ];
  const received = [];

  const summary = api.drainQuickCaptureQueue(events, (topic, ev) => {
    assert.strictEqual(topic, 'quick-capture:saved');
    if (ev.text === 'bad-second') {
      throw new Error('listener boom');
    }
    received.push(ev.text);
  });

  assert.deepStrictEqual(received, ['first', 'third']);
  assert.strictEqual(summary.delivered, 2);
  assert.strictEqual(summary.failed, 1);
  assert.strictEqual(summary.errors.length, 1);

  // Also verify direct exported function handles empty / non-array safely.
  const emptyRes = drainQuickCaptureQueue(null, () => {});
  assert.deepStrictEqual(emptyRes, { delivered: 0, failed: 0, errors: [] });
}

async function main() {
  await testFlowLoopNormalConvergence();
  await testFlowLoopClampsMaxTurnsTo10AndRejectsInvalid();
  await testFlowLoopAbortSignalAndGuardRelease();
  testDrainQuickCaptureQueueOrderedAndFaultTolerant();
  console.log('syki_api_ext_test.js: all tests passed');
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
