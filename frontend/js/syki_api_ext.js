(function (root, factory) {
  if (typeof module === 'object' && module.exports) {
    module.exports = factory();
  } else {
    root.SykiApiExt = factory();
  }
})(typeof globalThis !== 'undefined' ? globalThis : typeof window !== 'undefined' ? window : this, function () {
  'use strict';

  const MAX_LOOP_TURNS = 10;

  function createAbortError(reason) {
    const message = typeof reason === 'string' && reason.length > 0 ? reason : 'flow.loop aborted';
    const err = new Error(message);
    err.name = 'AbortError';
    if (reason !== undefined) {
      err.cause = reason;
    }
    return err;
  }

  function normalizeMaxTurns(maxTurns) {
    if (typeof maxTurns !== 'number' || !Number.isFinite(maxTurns) || maxTurns <= 0) {
      throw new RangeError('flow.loop requires a positive finite maxTurns');
    }
    const floored = Math.floor(maxTurns);
    if (floored < 1) {
      throw new RangeError('flow.loop requires maxTurns >= 1');
    }
    return Math.min(MAX_LOOP_TURNS, floored);
  }

  /**
   * Deliver queued Quick Capture events in FIFO order.
   * Failure in one delivery never stops subsequent events.
   */
  function drainQuickCaptureQueue(events, emitFn) {
    const result = { delivered: 0, failed: 0, errors: [] };
    if (!Array.isArray(events) || typeof emitFn !== 'function') {
      return result;
    }
    for (let i = 0; i < events.length; i++) {
      try {
        emitFn('quick-capture:saved', events[i]);
        result.delivered += 1;
      } catch (err) {
        result.failed += 1;
        result.errors.push(err);
      }
    }
    return result;
  }

  function createSykiExtApi(deps) {
    const safeDeps = deps && typeof deps === 'object' ? deps : {};

    async function loop(options) {
      if (!options || typeof options !== 'object') {
        throw new TypeError('flow.loop requires an options object');
      }
      const { init, maxTurns, step, until, signal } = options;
      if (typeof step !== 'function') {
        throw new TypeError('flow.loop requires a step function');
      }
      const limit = normalizeMaxTurns(maxTurns);

      if (signal && signal.aborted) {
        throw createAbortError(signal.reason);
      }

      if (typeof safeDeps.acquireDetachGuard === 'function') {
        safeDeps.acquireDetachGuard();
      }

      const effectiveSignal = signal || (typeof AbortController !== 'undefined' ? new AbortController().signal : undefined);

      try {
        let state = init;
        let turns = 0;

        for (let turn = 1; turn <= limit; turn++) {
          if (effectiveSignal && effectiveSignal.aborted) {
            throw createAbortError(effectiveSignal.reason);
          }

          state = await step(state, turn, effectiveSignal);
          turns = turn;

          if (effectiveSignal && effectiveSignal.aborted) {
            throw createAbortError(effectiveSignal.reason);
          }

          if (typeof until === 'function') {
            const done = await until(state, turn);
            if (done) {
              return { state, turns, completed: true };
            }
          }
        }

        return { state, turns, completed: false };
      } finally {
        if (typeof safeDeps.releaseDetachGuard === 'function') {
          safeDeps.releaseDetachGuard();
        }
      }
    }

    return {
      MAX_LOOP_TURNS,
      flow: {
        loop,
      },
      drainQuickCaptureQueue,
    };
  }

  return {
    MAX_LOOP_TURNS,
    createSykiExtApi,
    drainQuickCaptureQueue,
  };
});
