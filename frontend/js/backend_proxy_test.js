const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const proxyMod = require('./backend_proxy.js');
const { createPostMessageBackendProxy, createReqIdDemux, KEPT_METHODS } = proxyMod;

function makeMockWebView() {
  const sent = [];
  const handlers = new Set();
  return {
    sent,
    postMessage(msg) {
      sent.push(msg);
    },
    addEventListener(type, fn) {
      if (type === 'message') handlers.add(fn);
    },
    removeEventListener(type, fn) {
      if (type === 'message') handlers.delete(fn);
    },
    emit(data) {
      for (const fn of Array.from(handlers)) {
        fn({ data });
      }
    },
    listenerCount() {
      return handlers.size;
    }
  };
}

async function run() {
  // 1. 0-arg RPC request sends {} params and resolves result
  {
    const wv = makeMockWebView();
    const backend = createPostMessageBackendProxy({ webview: wv, targetWindow: {} });
    const p = backend.getAppVersion();
    assert.deepStrictEqual(wv.sent, [{ id: '1', method: 'getAppVersion', params: {} }]);
    wv.emit({ id: '1', ok: true, result: '2.0.0-lite' });
    assert.strictEqual(await p, '2.0.0-lite');
  }

  // 2. Positional arguments serialize as array and handle JSON string responses
  {
    const wv = makeMockWebView();
    const backend = createPostMessageBackendProxy({ webview: wv, targetWindow: {} });
    const p = backend.saveFileChecked('C:\\notes\\a.md', 'hello', 'UTF-8', 'sig-1');
    assert.deepStrictEqual(wv.sent, [{
      id: '1',
      method: 'saveFileChecked',
      params: ['C:\\notes\\a.md', 'hello', 'UTF-8', 'sig-1']
    }]);
    wv.emit(JSON.stringify({ id: '1', ok: true, result: { saved: true, newSig: 'sig-2' } }));
    assert.deepStrictEqual(await p, { saved: true, newSig: 'sig-2' });
  }

  // 3. Structured conflict error preserves err.code and err.detail; malformed JSON is ignored
  {
    const wv = makeMockWebView();
    const backend = createPostMessageBackendProxy({ webview: wv, targetWindow: {} });
    const p = backend.saveFileChecked('C:\\notes\\a.md', 'x', 'UTF-8', 'old');
    wv.emit('{not valid json');
    wv.emit({ id: '1', ok: false, error: 'conflict', result: { code: 'conflict', currentSig: 'sig-9' } });
    await assert.rejects(p, (err) => {
      assert.strictEqual(err.message, 'conflict');
      assert.strictEqual(err.code, 'conflict');
      assert.deepStrictEqual(err.detail, { code: 'conflict', currentSig: 'sig-9' });
      return true;
    });
  }

  // 4. Timeout rejects stalled requests and __dispose cleans listeners + pending promises
  {
    const wv = makeMockWebView();
    const timed = createPostMessageBackendProxy({ webview: wv, targetWindow: {}, timeoutMs: 20 });
    await assert.rejects(timed.getConfig(), /RPC timeout after 20ms: getConfig/);
    timed.__dispose();
    assert.strictEqual(wv.listenerCount(), 0);

    const wv2 = makeMockWebView();
    const disposable = createPostMessageBackendProxy({ webview: wv2, targetWindow: {} });
    assert.strictEqual(wv2.listenerCount(), 1);
    const inflight = disposable.getSession();
    disposable.__dispose();
    assert.strictEqual(wv2.listenerCount(), 0);
    await assert.rejects(inflight, /backend proxy disposed/);
    await assert.rejects(disposable.getAppVersion(), /backend proxy disposed/);
  }

  // 5. RpcEvent routing to global callbacks and onEvent subscribe/unsubscribe
  {
    const wv = makeMockWebView();
    const calls = [];
    const targetWindow = {
      __onDiskTextSeen(p, res) { calls.push(['disk', p, res]); },
      __sykiBeforeQuit() { calls.push(['flush']); },
      __onLLMResult(reqId, result, err) { calls.push(['llm', reqId, result, err]); },
      __semanticProgress(fd, ft, cd, ct) { calls.push(['sem', fd, ft, cd, ct]); },
      __onPtyData(id, b64) { calls.push(['pty', id, b64]); },
      __onVoiceResult(reqId, text, err, cachePath, refineErr) { calls.push(['voice', reqId, text, err, cachePath, refineErr]); }
    };
    const backend = createPostMessageBackendProxy({ webview: wv, targetWindow });
    const customEvents = [];
    const unsub = backend.onEvent('file:changed', (payload) => customEvents.push(payload));

    wv.emit({ topic: 'file:changed', payload: { path: 'a.md', sig: 's2' } });
    wv.emit({ topic: 'host:flushSession', payload: {} });
    wv.emit({ topic: 'llm:result', payload: { reqId: 'r1', result: 'ok', error: '' } });
    wv.emit({ topic: 'semantic:progress', payload: { filesDone: 2, filesTotal: 5, chunksDone: 8, chunksTotal: 20 } });
    wv.emit({ topic: 'pty:data', payload: { id: 'p1', b64: 'YWJj' } });
    wv.emit({ topic: 'voice:result', payload: { reqId: 'v1', text: 'hi', error: '', cachePath: 'c.wav', refineErr: '' } });

    unsub();
    wv.emit({ topic: 'file:changed', payload: { path: 'b.md', sig: 's3' } });

    assert.deepStrictEqual(calls, [
      ['disk', 'a.md', { path: 'a.md', sig: 's2' }],
      ['flush'],
      ['llm', 'r1', 'ok', ''],
      ['sem', 2, 5, 8, 20],
      ['pty', 'p1', 'YWJj'],
      ['voice', 'v1', 'hi', '', 'c.wav', ''],
      ['disk', 'b.md', { path: 'b.md', sig: 's3' }]
    ]);
    assert.deepStrictEqual(customEvents, [{ path: 'a.md', sig: 's2' }]);
  }

  // 6. Pruned methods are strictly excluded from KEPT_METHODS and proxy object
  {
    const pruned = [
      'startMobileDrop', 'jevPredict', 'jevExecuteAsync', 'runSlotAgentAsync',
      'lessonPlan', 'tagEdit', 'deepSearchPlan', 'packExport', 'triggerGitSync',
      'testDiscordBridgeConnection', 'setupOllamaGemma4Async', 'startOllamaService'
    ];
    for (const name of pruned) {
      assert.ok(!KEPT_METHODS.includes(name), `pruned method present in KEPT_METHODS: ${name}`);
    }
    const wv = makeMockWebView();
    const backend = createPostMessageBackendProxy({ webview: wv, targetWindow: {} });
    for (const name of pruned) {
      assert.strictEqual(backend[name], undefined, `pruned method exposed on proxy: ${name}`);
    }
  }

  // 7. Existing window.backend (e.g. mock_backend.js) is never overwritten on load; live WebView2 attaches cleanly
  {
    const code = fs.readFileSync(path.join(__dirname, 'backend_proxy.js'), 'utf8');
    const sentinel = { existingMock: true };
    const ctxWithMock = {
      window: null,
      chrome: { webview: makeMockWebView() },
      backend: sentinel
    };
    ctxWithMock.window = ctxWithMock;
    vm.runInNewContext(code, ctxWithMock);
    assert.strictEqual(ctxWithMock.backend, sentinel);

    const wv = makeMockWebView();
    const ctxWebView = {
      window: null,
      chrome: { webview: wv }
    };
    ctxWebView.window = ctxWebView;
    vm.runInNewContext(code, ctxWebView);
    assert.ok(ctxWebView.backend && typeof ctxWebView.backend.getAppVersion === 'function');
    assert.strictEqual(wv.listenerCount(), 1);
  }

  // 8. createReqIdDemux resolves parallel & out-of-order reqIds and handles AbortSignal cancellation
  {
    const cancelledIds = [];
    const demux = createReqIdDemux({
      onCancel(reqId, topic) {
        cancelledIds.push([reqId, topic]);
      }
    });

    const p1 = demux.wait('req-1', { topic: 'llm:decisionResult' });
    const p2 = demux.wait('req-2', { topic: 'llm:decisionResult' });
    const p3 = demux.wait('req-3', { topic: 'llm:result' });
    assert.strictEqual(demux.pendingCount(), 3);

    // Out-of-order arrival: req-3 -> req-2 -> req-1
    assert.strictEqual(demux.dispatch('llm:result', { reqId: 'req-3', result: 'hello', error: '' }), true);
    // Mismatched topic must NOT settle req-2
    assert.strictEqual(demux.dispatch('llm:result', { reqId: 'req-2', result: 'wrong-topic' }), false);
    assert.strictEqual(demux.dispatch('llm:decisionResult', { reqId: 'req-2', choiceId: 'bug', by: 'model', ok: true }), true);
    assert.strictEqual(demux.dispatch('llm:decisionResult', { reqId: 'req-1', choiceId: 'task', by: 'model', ok: true }), true);

    const [r1, r2, r3] = await Promise.all([p1, p2, p3]);
    assert.strictEqual(r1.choiceId, 'task');
    assert.strictEqual(r2.choiceId, 'bug');
    assert.strictEqual(r3.result, 'hello');
    assert.strictEqual(demux.pendingCount(), 0);

    // AbortSignal cancellation (in-flight)
    const ac = new AbortController();
    const pAbort = demux.wait('req-abort', { topic: 'llm:decisionResult', signal: ac.signal });
    assert.strictEqual(demux.pendingCount(), 1);
    ac.abort('user clicked stop');
    await assert.rejects(pAbort, (err) => {
      assert.strictEqual(err.name, 'AbortError');
      assert.match(err.message, /user clicked stop/);
      return true;
    });
    assert.strictEqual(demux.pendingCount(), 0);

    // Pre-aborted AbortSignal rejects immediately without leaking pending entry
    const preAc = new AbortController();
    preAc.abort('already aborted');
    await assert.rejects(
      demux.wait('req-pre-abort', { topic: 'llm:result', signal: preAc.signal }),
      (err) => err.name === 'AbortError'
    );
    assert.strictEqual(demux.pendingCount(), 0);
    assert.deepStrictEqual(cancelledIds, [
      ['req-abort', 'llm:decisionResult'],
      ['req-pre-abort', 'llm:result']
    ]);
  }

  // 9. Backend proxy integrates reqId demux for parallel llm:result / llm:decisionResult + AbortSignal
  {
    const wv = makeMockWebView();
    const decisionHookCalls = [];
    const targetWindow = {
      __onLLMDecisionResult(reqId, choiceId, reason, by, ok) {
        decisionHookCalls.push({ reqId, choiceId, reason, by, ok });
      }
    };
    const backend = createPostMessageBackendProxy({ webview: wv, targetWindow });
    assert.strictEqual(typeof backend.queryDecisionAsync, 'function');
    assert.strictEqual(typeof backend.cancelLLMRequest, 'function');

    const ac = new AbortController();
    const d1 = backend.waitForEvent('llm:decisionResult', 'd-1');
    const d2 = backend.waitForEvent('llm:decisionResult', 'd-2');
    const d3 = backend.waitForEvent('llm:decisionResult', 'd-3', { signal: ac.signal });
    const l1 = backend.waitForEvent('llm:result', 'l-1');

    // Cancel d-3 before any event arrives
    ac.abort('cancelled d-3');
    await assert.rejects(d3, (err) => err.name === 'AbortError');

    // Emit out-of-order events over WebView2 bridge
    wv.emit({ topic: 'llm:decisionResult', payload: { reqId: 'd-2', choiceId: 'memo', reason: 'plain note', by: 'model', ok: true } });
    wv.emit({ topic: 'llm:result', payload: { reqId: 'l-1', result: 'generated text', error: '' } });
    wv.emit({ topic: 'llm:decisionResult', payload: { reqId: 'd-1', choiceId: 'task', reason: 'has due date', by: 'model', ok: true } });
    // Late event for cancelled d-3 is harmlessly ignored by demux
    wv.emit({ topic: 'llm:decisionResult', payload: { reqId: 'd-3', choiceId: 'bug', reason: 'late', by: 'model', ok: true } });

    const [resD1, resD2, resL1] = await Promise.all([d1, d2, l1]);
    assert.strictEqual(resD1.choiceId, 'task');
    assert.strictEqual(resD2.choiceId, 'memo');
    assert.strictEqual(resL1.result, 'generated text');
    assert.strictEqual(backend.reqDemux.pendingCount(), 0);
    assert.strictEqual(decisionHookCalls.length, 3);
    assert.strictEqual(decisionHookCalls[0].reqId, 'd-2');
    assert.strictEqual(decisionHookCalls[1].reqId, 'd-1');

    // __dispose rejects any remaining waitForEvent promises
    const dangling = backend.waitForEvent('llm:result', 'l-dangling');
    backend.__dispose();
    await assert.rejects(dangling, /backend proxy disposed/);
  }
}

run().catch((err) => {
  console.error(err);
  process.exit(1);
});
