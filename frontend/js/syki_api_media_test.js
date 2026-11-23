'use strict';

const assert = require('assert');

// Expose global.window to verify UMD registration alongside CommonJS module.exports
global.window = global;
const SykiApiMedia = require('./syki_api_media.js');

const {
  createSykiMediaApi,
  isAbortError,
} = SykiApiMedia;

function createGuardTracker() {
  let active = 0;
  let maxActive = 0;
  let acquireCount = 0;
  let releaseCount = 0;
  return {
    acquireDetachGuard() {
      active += 1;
      acquireCount += 1;
      if (active > maxActive) maxActive = active;
    },
    releaseDetachGuard() {
      active -= 1;
      releaseCount += 1;
      assert.ok(active >= 0, 'detach guard must never underflow');
    },
    snapshot() {
      return { active, maxActive, acquireCount, releaseCount };
    },
  };
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

async function runTests() {
  // 0. UMD / CommonJS export & zero startup probes
  {
    assert.strictEqual(global.window.SykiApiMedia, SykiApiMedia, 'UMD attaches window.SykiApiMedia');
    let called = 0;
    const api = createSykiMediaApi({
      recordVoice: async () => { called += 1; return 'x'; },
      recordMeeting: async () => { called += 1; return 'x'; },
      captureScreen: async () => { called += 1; return null; },
      queryVision: async () => { called += 1; return 'x'; },
      generateImage: async () => { called += 1; return null; },
      renderMermaidOffscreen: async () => { called += 1; return null; },
    });
    assert.ok(api.input && api.exec && api.exec.render && api.onPipeIn && api.dispatchPipeIn);
    assert.strictEqual(called, 0, 'createSykiMediaApi performs zero startup probes');
  }

  // 1. input.voice / input.meeting exclusive audio lock (AUDIO_BUSY + release + re-run)
  {
    const guard = createGuardTracker();
    let voiceDef = deferred();
    let meetingDef = deferred();
    let voiceCalls = 0;
    let meetingCalls = 0;

    const api = createSykiMediaApi({
      ...guard,
      recordVoice: () => {
        voiceCalls += 1;
        return voiceDef.promise;
      },
      recordMeeting: () => {
        meetingCalls += 1;
        return meetingDef.promise;
      },
    });

    // Start voice recording
    const firstVoice = api.input.voice();
    assert.strictEqual(guard.snapshot().active, 1, 'detach guard acquired while voice recording');

    // Concurrent voice() rejected with AUDIO_BUSY
    await assert.rejects(
      () => api.input.voice(),
      (err) => err && err.code === 'AUDIO_BUSY' && err.activeMode === 'voice'
    );

    // Concurrent meeting() rejected with AUDIO_BUSY
    await assert.rejects(
      () => api.input.meeting(),
      (err) => err && err.code === 'AUDIO_BUSY' && err.activeMode === 'voice'
    );
    assert.strictEqual(voiceCalls, 1, 'rejected voice call did not invoke recordVoice');
    assert.strictEqual(meetingCalls, 0, 'rejected meeting call did not invoke recordMeeting');

    // Complete first voice recording -> lock & guard released
    voiceDef.resolve('hello voice');
    assert.strictEqual(await firstVoice, 'hello voice');
    assert.strictEqual(guard.snapshot().active, 0, 'detach guard released after voice completes');

    // Now meeting() can run; concurrent voice() during meeting is rejected with AUDIO_BUSY
    const firstMeeting = api.input.meeting();
    await assert.rejects(
      () => api.input.voice(),
      (err) => err && err.code === 'AUDIO_BUSY' && err.activeMode === 'meeting'
    );

    // User cancels meeting (returns null) -> returns null and releases lock
    meetingDef.resolve(null);
    assert.strictEqual(await firstMeeting, null);
    assert.strictEqual(guard.snapshot().active, 0, 'detach guard released after meeting cancel');

    // Error during recording also releases lock and allows subsequent recording
    voiceDef = deferred();
    const failingVoice = api.input.voice();
    voiceDef.reject(new Error('mic disconnected'));
    await assert.rejects(() => failingVoice, /mic disconnected/);
    assert.strictEqual(guard.snapshot().active, 0, 'detach guard released after failure');

    voiceDef = deferred();
    const recoveredVoice = api.input.voice();
    voiceDef.resolve('recovered transcript');
    assert.strictEqual(await recoveredVoice, 'recovered transcript');
    assert.strictEqual(guard.snapshot().active, 0);
  }

  // 2. AbortSignal cancellation across input.voice, input.meeting, exec.vision, exec.generateImage
  {
    const guard = createGuardTracker();
    let voiceSignalSeen = null;
    let visionSignalSeen = null;
    let imageSignalSeen = null;

    const api = createSykiMediaApi({
      ...guard,
      recordVoice: ({ signal }) => {
        voiceSignalSeen = signal;
        return new Promise(() => {}); // hangs until aborted
      },
      recordMeeting: () => new Promise(() => {}),
      queryVision: (_asset, _prompt, { signal }) => {
        visionSignalSeen = signal;
        return new Promise(() => {});
      },
      generateImage: (_prompt, { signal }) => {
        imageSignalSeen = signal;
        return new Promise(() => {});
      },
    });

    // Pre-aborted signal rejects immediately without acquiring lock or calling dep
    const preAborted = new AbortController();
    preAborted.abort();
    await assert.rejects(() => api.input.voice({ signal: preAborted.signal }), isAbortError);
    assert.strictEqual(voiceSignalSeen, null, 'pre-aborted voice does not invoke recordVoice');
    assert.strictEqual(guard.snapshot().active, 0);

    // In-flight abort on input.voice releases exclusive lock and detach guard
    const voiceCtrl = new AbortController();
    const voicePromise = api.input.voice({ signal: voiceCtrl.signal });
    assert.strictEqual(voiceSignalSeen, voiceCtrl.signal);
    voiceCtrl.abort();
    await assert.rejects(() => voicePromise, isAbortError);
    assert.strictEqual(guard.snapshot().active, 0, 'detach guard released after voice abort');

    // Exclusive lock is free after abort: meeting can start and be aborted too
    const meetingCtrl = new AbortController();
    const meetingPromise = api.input.meeting({ signal: meetingCtrl.signal });
    meetingCtrl.abort();
    await assert.rejects(() => meetingPromise, isAbortError);
    assert.strictEqual(guard.snapshot().active, 0);

    // In-flight abort on exec.vision
    const visionCtrl = new AbortController();
    const visionPromise = api.exec.vision({ id: 'a1', path: 'assets/a1.png' }, 'describe', { signal: visionCtrl.signal });
    assert.strictEqual(visionSignalSeen, visionCtrl.signal);
    visionCtrl.abort();
    await assert.rejects(() => visionPromise, isAbortError);
    assert.strictEqual(guard.snapshot().active, 0);

    // In-flight abort on exec.generateImage
    const imgCtrl = new AbortController();
    const imgPromise = api.exec.generateImage('cyberpunk city', { size: '1024x1024', signal: imgCtrl.signal });
    assert.strictEqual(imageSignalSeen, imgCtrl.signal);
    imgCtrl.abort();
    await assert.rejects(() => imgPromise, isAbortError);
    assert.strictEqual(guard.snapshot().active, 0);
  }

  // 3. input.captureScreen & exec.generateImage AssetRef + exec.vision delegation
  {
    const guard = createGuardTracker();
    const sampleAsset = Object.freeze({ id: 'asset_01', path: 'assets/cap.png', mime: 'image/png', size: 2048 });
    const generatedAsset = Object.freeze({ id: 'asset_02', path: 'assets/gen.png', mime: 'image/png', size: 4096 });
    let captureCall = 0;
    let visionArgs = null;
    let genArgs = null;

    const api = createSykiMediaApi({
      ...guard,
      captureScreen: async () => {
        captureCall += 1;
        return captureCall === 1 ? sampleAsset : null;
      },
      queryVision: async (asset, prompt, opts) => {
        visionArgs = { asset, prompt, opts };
        return 'A screenshot of a terminal';
      },
      generateImage: async (prompt, opts) => {
        genArgs = { prompt, opts };
        return generatedAsset;
      },
    });

    const cap = await api.input.captureScreen();
    assert.deepStrictEqual(cap, sampleAsset, 'captureScreen returns AssetRef');
    const cancelledCap = await api.input.captureScreen();
    assert.strictEqual(cancelledCap, null, 'captureScreen returns null when cancelled');

    const visionResult = await api.exec.vision(sampleAsset, 'What is on screen?');
    assert.strictEqual(visionResult, 'A screenshot of a terminal');
    assert.strictEqual(visionArgs.asset, sampleAsset);
    assert.strictEqual(visionArgs.prompt, 'What is on screen?');

    const genResult = await api.exec.generateImage('minimalist bamboo icon', { size: '512x512' });
    assert.deepStrictEqual(genResult, generatedAsset, 'generateImage returns AssetRef');
    assert.strictEqual(genArgs.prompt, 'minimalist bamboo icon');
    assert.strictEqual(genArgs.opts.size, '512x512');

    // Invalid arguments rejected cleanly
    await assert.rejects(() => api.exec.vision(null, 'desc'), (err) => err.code === 'INVALID_ARGUMENT');
    await assert.rejects(() => api.exec.vision(sampleAsset, '   '), (err) => err.code === 'INVALID_ARGUMENT');
    await assert.rejects(() => api.exec.generateImage(''), (err) => err.code === 'INVALID_ARGUMENT');
    assert.strictEqual(guard.snapshot().active, 0);
  }

  // 4. exec.render.mermaidToPng (isolated offscreen render, zero global pollution) + htmlToPng / htmlToPdf
  {
    const guard = createGuardTracker();
    const renderedCalls = [];
    const pngAsset = { id: 'm1', path: 'assets/diagram.png', mime: 'image/png' };
    const htmlPngBlob = { type: 'image/png', bytes: 128 };
    const htmlPdfAsset = { id: 'p1', path: 'assets/doc.pdf', mime: 'application/pdf' };

    // Set host global state to verify mermaidToPng never mutates it
    global.mermaidAppliedTone = 'light';
    global.window.mermaidAppliedTone = 'light';

    const api = createSykiMediaApi({
      ...guard,
      renderMermaidOffscreen: async (code, opts) => {
        renderedCalls.push({ kind: 'mermaid', code, opts });
        return pngAsset;
      },
      renderHtmlToPng: async (html, opts) => {
        renderedCalls.push({ kind: 'htmlToPng', html, opts });
        return htmlPngBlob;
      },
      renderHtmlToPdf: async (html, opts) => {
        renderedCalls.push({ kind: 'htmlToPdf', html, opts });
        return htmlPdfAsset;
      },
    });

    // Defaults: theme='dark', scale=2
    const resDefault = await api.exec.render.mermaidToPng('graph TD; A-->B;');
    assert.deepStrictEqual(resDefault, pngAsset);
    assert.deepStrictEqual(renderedCalls[0], {
      kind: 'mermaid',
      code: 'graph TD; A-->B;',
      opts: { theme: 'dark', scale: 2 },
    });
    assert.strictEqual(global.mermaidAppliedTone, 'light', 'mermaidToPng must not mutate global mermaidAppliedTone');
    assert.strictEqual(global.window.mermaidAppliedTone, 'light', 'mermaidToPng must not mutate window.mermaidAppliedTone');

    // Custom theme & scale
    await api.exec.render.mermaidToPng('sequenceDiagram\nA->>B: Hi', { theme: 'forest', scale: 3 });
    assert.deepStrictEqual(renderedCalls[1].opts, { theme: 'forest', scale: 3 });
    assert.strictEqual(global.mermaidAppliedTone, 'light', 'global mermaidAppliedTone remains untouched');

    // Reject empty / whitespace / non-string mermaidCode
    await assert.rejects(() => api.exec.render.mermaidToPng(''), (err) => err.code === 'INVALID_MERMAID_CODE');
    await assert.rejects(() => api.exec.render.mermaidToPng('   \n\t '), (err) => err.code === 'INVALID_MERMAID_CODE');
    await assert.rejects(() => api.exec.render.mermaidToPng(null), (err) => err.code === 'INVALID_MERMAID_CODE');

    // htmlToPng & htmlToPdf delegation
    const hPng = await api.exec.render.htmlToPng('<h1>Hello</h1>', { width: 800 });
    assert.strictEqual(hPng, htmlPngBlob);
    assert.deepStrictEqual(renderedCalls[2], { kind: 'htmlToPng', html: '<h1>Hello</h1>', opts: { width: 800 } });

    const hPdf = await api.exec.render.htmlToPdf('<p>Report</p>', { landscape: true });
    assert.strictEqual(hPdf, htmlPdfAsset);
    assert.deepStrictEqual(renderedCalls[3], { kind: 'htmlToPdf', html: '<p>Report</p>', opts: { landscape: true } });

    await assert.rejects(() => api.exec.render.htmlToPng('  '), (err) => err.code === 'INVALID_HTML');
    await assert.rejects(() => api.exec.render.htmlToPdf(''), (err) => err.code === 'INVALID_HTML');
    assert.strictEqual(guard.snapshot().active, 0);

    delete global.mermaidAppliedTone;
  }

  // 5. pipe:in hook: sequential dispatch, error isolation, handled + transformedText, Disposable
  {
    const errorsLogged = [];
    const order = [];
    const api = createSykiMediaApi({
      onPipeInError: (err) => errorsLogged.push(err.message),
    });

    assert.throws(() => api.onPipeIn('not-a-fn'), (err) => err.code === 'INVALID_HANDLER');

    // Empty dispatch returns { handled: false }
    assert.deepStrictEqual(await api.dispatchPipeIn({ text: 'raw', source: 'cli' }), { handled: false });

    const d1 = api.onPipeIn((ev) => {
      order.push('h1:' + ev.source);
      throw new Error('sync boom');
    });

    const d2 = api.onPipeIn(async (ev) => {
      order.push('h2:' + ev.text);
      throw new Error('async boom');
    });

    const d3 = api.onPipeIn(async (ev) => {
      order.push('h3:' + ev.text);
      if (ev.text.startsWith('!upper ')) {
        return { handled: true, transformedText: ev.text.slice(7).toUpperCase() };
      }
      return { handled: false };
    });

    const d4 = api.onPipeIn(() => {
      order.push('h4:should-not-run-when-handled');
      return { handled: true, transformedText: 'override' };
    });

    const handledRes = await api.dispatchPipeIn({ text: '!upper hello', source: 'stdin' });
    assert.deepStrictEqual(handledRes, { handled: true, transformedText: 'HELLO' });
    assert.deepStrictEqual(order, ['h1:stdin', 'h2:!upper hello', 'h3:!upper hello']);
    assert.deepStrictEqual(errorsLogged, ['sync boom', 'async boom']);

    // Dispose h3 and h4 -> unhandled returns { handled: false } despite h1/h2 errors
    d3.dispose();
    d3.dispose(); // idempotent
    d4.dispose();
    order.length = 0;
    const unhandledRes = await api.dispatchPipeIn({ text: 'plain', source: 'clipboard' });
    assert.deepStrictEqual(unhandledRes, { handled: false });
    assert.deepStrictEqual(order, ['h1:clipboard', 'h2:plain']);

    // Dispose remaining handlers
    d1.dispose();
    d2.dispose();
    order.length = 0;
    assert.deepStrictEqual(await api.dispatchPipeIn({ text: 'after', source: 'stdin' }), { handled: false });
    assert.deepStrictEqual(order, []);
  }

  console.log('ALL SYKI API MEDIA TESTS PASSED');
}

runTests().catch((err) => {
  console.error('SYKI API MEDIA TEST FAILED:', err);
  process.exit(1);
});
