const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const proxyMod = require('./backend_proxy.js');
const { createPostMessageBackendProxy, KEPT_METHODS } = proxyMod;

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
}

run().catch((err) => {
  console.error(err);
  process.exit(1);
});
