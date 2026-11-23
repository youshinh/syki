'use strict';

const assert = require('assert');

// Expose global.window to verify UMD registration alongside CommonJS module.exports
global.window = global;
const SykiApiFlow = require('./syki_api_flow.js');

const {
  createSykiFlowApi,
  validateNotePath,
  extractChoiceId,
} = SykiApiFlow;

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

async function runTests() {
  // 0. UMD / CommonJS export & 0ms startup (zero probes on init)
  {
    assert.strictEqual(global.window.SykiApiFlow, SykiApiFlow, 'UMD attaches window.SykiApiFlow');
    let networkCalls = 0;
    const api = createSykiFlowApi({
      queryDecision() { networkCalls += 1; throw new Error('must not probe on init'); },
      queryLlm() { networkCalls += 1; throw new Error('must not probe on init'); },
      searchSemantic() { networkCalls += 1; },
    });
    assert.ok(api.knowledge && api.exec && api.flow);
    assert.strictEqual(networkCalls, 0, 'createSykiFlowApi performs zero startup probes');
  }

  // 1. knowledge: semantic/fulltext/notes/assets + strict path boundary contract
  {
    let noteOps = 0;
    const api = createSykiFlowApi({
      searchSemantic: async (q, opts) => [{ id: 's1', q, opts }],
      searchFulltext: async (q, opts) => [{ id: 'f1', q, opts }],
      updateSemanticIndex: async () => ({ indexed: 42 }),
      notes: {
        read: async (p) => { noteOps += 1; return `content:${p}`; },
        list: async (o) => { noteOps += 1; return [o.dir || '']; },
        create: async (p, c) => { noteOps += 1; return { p, c }; },
        append: async (p, c) => { noteOps += 1; return { p, c }; },
        update: async (p, c) => { noteOps += 1; return { p, c }; },
      },
      assets: {
        save: async (blob, ext) => ({ size: blob.length, ext }),
      },
    });

    const sem = await api.knowledge.searchSemantic('rust', { limit: 5 });
    assert.strictEqual(sem[0].q, 'rust');
    const ft = await api.knowledge.searchFulltext('sqlite', { limit: 3 });
    assert.strictEqual(ft[0].q, 'sqlite');
    const idx = await api.knowledge.updateSemanticIndex();
    assert.strictEqual(idx.indexed, 42);

    assert.strictEqual(await api.knowledge.notes.read('daily/2026-11-22.md'), 'content:daily/2026-11-22.md');
    assert.deepStrictEqual(await api.knowledge.notes.list({ dir: 'daily' }), ['daily']);
    assert.deepStrictEqual(await api.knowledge.notes.create('inbox/a.md', 'hi'), { p: 'inbox/a.md', c: 'hi' });
    assert.deepStrictEqual(await api.knowledge.notes.append('inbox/a.md', '\nmore'), { p: 'inbox/a.md', c: '\nmore' });
    assert.deepStrictEqual(await api.knowledge.notes.update('inbox/a.md', 'new'), { p: 'inbox/a.md', c: 'new' });
    assert.deepStrictEqual(await api.knowledge.assets.save(Buffer.from('abc'), '.PNG'), { size: 3, ext: 'png' });

    const opsBeforeInvalid = noteOps;
    const badPaths = [
      '',
      '   ',
      '../secret.md',
      'a/../../etc/passwd',
      'a\\..\\b.md',
      '/etc/passwd',
      '\\Windows\\System32\\cmd.exe',
      'C:\\Users\\secret.txt',
      'D:notes.md',
      'note.md:stream',
      'bad\0name.md',
    ];
    for (const bad of badPaths) {
      await assert.rejects(() => api.knowledge.notes.read(bad), (err) => err.code === 'INVALID_NOTE_PATH');
      await assert.rejects(() => api.knowledge.notes.create(bad, 'x'), (err) => err.code === 'INVALID_NOTE_PATH');
      await assert.rejects(() => api.knowledge.notes.append(bad, 'x'), (err) => err.code === 'INVALID_NOTE_PATH');
      await assert.rejects(() => api.knowledge.notes.update(bad, 'x'), (err) => err.code === 'INVALID_NOTE_PATH');
    }
    await assert.rejects(() => api.knowledge.notes.list({ dir: '../outside' }), (err) => err.code === 'INVALID_NOTE_PATH');
    await assert.rejects(() => api.knowledge.assets.save(Buffer.from('x'), '../exe'), (err) => err.code === 'INVALID_ASSET_EXT');
    assert.strictEqual(noteOps, opsBeforeInvalid, 'invalid paths must never reach backend deps.notes');
    assert.strictEqual(validateNotePath('sub\\note.md'), 'sub/note.md');
  }

  // 2. exec.llm: stripSecrets default ON, abort signal, and detach guard lifecycle
  {
    const guard = createGuardTracker();
    const received = [];
    const api = createSykiFlowApi({
      ...guard,
      stripSecrets: (s) => (typeof s === 'string' ? s.replace(/sk-[A-Za-z0-9-]+/g, '[MASKED]') : s),
      queryLlm: async (req) => {
        assert.ok(guard.snapshot().active > 0, 'detach guard must be held during queryLlm');
        received.push(req);
        return `ok:${req.prompt}`;
      },
    });

    const out1 = await api.exec.llm('Check sk-SECRET123456', { input: 'token sk-INPUT999999' });
    assert.strictEqual(out1, 'ok:Check [MASKED]');
    assert.strictEqual(received[0].prompt, 'Check [MASKED]');
    assert.strictEqual(received[0].input, 'token [MASKED]');

    await api.exec.llm('Raw sk-KEEP123456', { stripSecrets: false });
    assert.strictEqual(received[1].prompt, 'Raw sk-KEEP123456');

    // Immediate abort before invoking queryLlm
    const ac = new AbortController();
    ac.abort();
    await assert.rejects(
      () => api.exec.llm('will abort', { signal: ac.signal }),
      (err) => err.name === 'AbortError'
    );
    assert.strictEqual(guard.snapshot().active, 0, 'detach guard must be released after AbortError');
  }

  // 3. exec.shell & exec.pty: array args enforcement + structured PolicyDecision errors
  {
    const guard = createGuardTracker();
    const api = createSykiFlowApi({
      ...guard,
      runProgram: async (program, args, opts) => {
        assert.ok(guard.snapshot().active > 0, 'detach guard held during runProgram');
        if (program === 'rm') {
          return { allowed: false, action: 'deny', reason: 'destructive command' };
        }
        if (program === 'git' && args[0] === 'push') {
          return { decision: { allowed: false, action: 'ask', reason: 'remote mutation requires confirmation' } };
        }
        return { allowed: true, exitCode: 0, stdout: `${program}:${args.join(',')}`, cwd: opts.cwd };
      },
      openPty: async (opts) => ({ sessionId: 'pty-1', shell: opts.shell || 'pwsh' }),
    });

    const okRes = await api.exec.shell('git', ['status', '-s'], { cwd: '/repo' });
    assert.strictEqual(okRes.stdout, 'git:status,-s');

    await assert.rejects(
      () => api.exec.shell('git', 'status -s'),
      (err) => err instanceof TypeError && err.code === 'INVALID_ARGUMENT'
    );

    await assert.rejects(
      () => api.exec.shell('rm', ['-rf', '/']),
      (err) => err.code === 'POLICY_DENIED' && err.decision && err.decision.allowed === false
    );

    await assert.rejects(
      () => api.exec.shell('git', ['push', 'origin', 'main']),
      (err) => err.code === 'POLICY_ASK' && err.decision && err.decision.action === 'ask'
    );

    const pty = await api.exec.pty.open({ shell: 'pwsh' });
    assert.strictEqual(pty.sessionId, 'pty-1');
    assert.strictEqual(guard.snapshot().active, 0);
  }

  // 4. flow.steps & flow.parallel: progress labels, abort propagation, detach guard
  {
    const guard = createGuardTracker();
    const api = createSykiFlowApi(guard);
    const labels = [];

    const result = await api.flow.steps(
      1,
      [
        { label: 'double', run: async (x) => x * 2 },
        { label: 'add-ten', run: async (x) => x + 10 },
      ],
      { progress: { update: (label) => labels.push(label) } }
    );
    assert.strictEqual(result, 12);
    assert.deepStrictEqual(labels, ['double', 'add-ten']);

    // Abort mid-pipeline
    const ac = new AbortController();
    let step2Ran = false;
    await assert.rejects(
      () => api.flow.steps(
        'start',
        [
          { label: 's1', run: async (v) => { ac.abort(); return v; } },
          { label: 's2', run: async () => { step2Ran = true; return 'never'; } },
        ],
        { signal: ac.signal }
      ),
      (err) => err.name === 'AbortError'
    );
    assert.strictEqual(step2Ran, false);

    const par = await api.flow.parallel([
      async () => 'a',
      Promise.resolve('b'),
      () => 'c',
    ]);
    assert.deepStrictEqual(par, ['a', 'b', 'c']);
    assert.strictEqual(guard.snapshot().active, 0);
  }

  // 5. flow.decide: Checklist #3, #4, #5, #6, #7 (rule short-circuit, strict extraction,
  //    stripSecrets, decision->llm->fallback, timeout/abort, parallel reqId demux)
  {
    // 5a. Rule-first 0ms zero-network short-circuit
    let modelCalls = 0;
    const guard = createGuardTracker();
    const api = createSykiFlowApi({
      ...guard,
      queryDecision: async () => { modelCalls += 1; return { choiceId: 'memo' }; },
      queryLlm: async () => { modelCalls += 1; return 'memo'; },
    });

    const ruled = await api.flow.decide({
      input: 'TODO: fix login crash',
      choices: [{ id: 'task' }, { id: 'bug' }, { id: 'memo' }],
      rule: (text) => (text.startsWith('TODO:') ? 'task' : null),
      fallback: 'memo',
    });
    assert.deepStrictEqual(ruled, { choiceId: 'task', by: 'rule' });
    assert.strictEqual(modelCalls, 0, 'rule short-circuit must make zero model calls');
    assert.strictEqual(guard.snapshot().active, 0);
  }

  {
    // 5b. Strict choice extraction (JSON / fenced JSON / plain / hallucination -> fallback)
    const valid = new Set(['task', 'idea', 'bug', 'memo']);
    assert.strictEqual(extractChoiceId('bug', valid), 'bug');
    assert.strictEqual(extractChoiceId('"idea"', valid), 'idea');
    assert.strictEqual(extractChoiceId('```json\n{"choiceId": "task"}\n```', valid), 'task');
    assert.strictEqual(extractChoiceId('{"id":"memo"}', valid), 'memo');
    assert.strictEqual(extractChoiceId('I think this is a task probably', valid), null, 'reject loose prose');
    assert.strictEqual(extractChoiceId('hallucinated_id', valid), null);
    assert.strictEqual(extractChoiceId('', valid), null);

    const api = createSykiFlowApi({
      queryDecision: async ({ reqId }) => ({ reqId, text: '```json\n{"choiceId":"not_a_real_choice"}\n```' }),
      queryLlm: async () => 'still_invalid_choice',
    });
    const fb = await api.flow.decide({
      input: 'ambiguous text',
      choices: ['task', 'memo'],
      fallback: 'memo',
    });
    assert.strictEqual(fb.choiceId, 'memo');
    assert.strictEqual(fb.by, 'fallback');
    assert.ok(typeof fb.reason === 'string' && fb.reason.length > 0);
  }

  {
    // 5c. Default stripSecrets ON before Decision model
    let captured = null;
    const api = createSykiFlowApi({
      queryDecision: async (req) => {
        captured = req;
        return { reqId: req.reqId, choiceId: 'secret_risk' };
      },
    });
    const res = await api.flow.decide({
      input: 'key=sk-LIVE9876543210 https://alice:pw123@example.com/api?token=TOK999',
      prompt: 'Classify Bearer eyJhbGciOiJIUzI1NiJ9',
      choices: ['safe', 'secret_risk'],
      fallback: 'safe',
    });
    assert.strictEqual(res.choiceId, 'secret_risk');
    assert.strictEqual(res.by, 'decision');
    assert.ok(!captured.input.includes('sk-LIVE9876543210'), 'API key must be stripped');
    assert.ok(!captured.input.includes('pw123'), 'URL password must be stripped');
    assert.ok(!captured.input.includes('TOK999'), 'URL token query param must be stripped');
    assert.ok(!captured.prompt.includes('eyJhbGciOiJIUzI1NiJ9'), 'Bearer token in prompt must be stripped');
  }

  {
    // 5d. Decision unavailable / disabled -> LLM fallback -> safe fallback when both missing
    const apiLlmFallback = createSykiFlowApi({
      capabilities: { decisionModel: false },
      queryDecision: async () => { throw new Error('should not be called when disabled'); },
      queryLlm: async ({ reqId }) => ({ reqId, text: '```json\n{"choiceId":"translate"}\n```' }),
    });
    const r1 = await apiLlmFallback.flow.decide({
      input: 'Hello world',
      choices: ['translate', 'paste_raw'],
      fallback: 'paste_raw',
    });
    assert.strictEqual(r1.choiceId, 'translate');
    assert.strictEqual(r1.by, 'llm');

    const apiNoModels = createSykiFlowApi({});
    const r2 = await apiNoModels.flow.decide({
      input: 'Hello world',
      choices: ['translate', 'paste_raw'],
      fallback: 'paste_raw',
    });
    assert.strictEqual(r2.choiceId, 'paste_raw');
    assert.strictEqual(r2.by, 'fallback');
  }

  {
    // 5e. AbortSignal immediate cancellation & timeout cleanup (no zombie requests)
    let internalAborted = false;
    const guard = createGuardTracker();
    const api = createSykiFlowApi({
      ...guard,
      queryDecision: ({ signal }) => new Promise((_resolve, reject) => {
        signal.addEventListener('abort', () => {
          internalAborted = true;
          reject(SykiApiFlow.createAbortError());
        }, { once: true });
      }),
    });

    // External caller abort -> AbortError
    const ac = new AbortController();
    setTimeout(() => ac.abort(), 15);
    await assert.rejects(
      () => api.flow.decide({
        input: 'slow',
        choices: ['a', 'b'],
        fallback: 'b',
        signal: ac.signal,
        timeoutMs: 5000,
      }),
      (err) => err.name === 'AbortError'
    );
    assert.strictEqual(internalAborted, true, 'external abort must propagate to backend signal');

    // Internal timeout -> aborts backend signal and returns fallback safely
    internalAborted = false;
    const timed = await api.flow.decide({
      input: 'hang',
      choices: ['a', 'b'],
      fallback: 'b',
      timeoutMs: 25,
    });
    assert.strictEqual(internalAborted, true, 'timeout must abort backend signal (no zombie)');
    assert.deepStrictEqual(timed, { choiceId: 'b', reason: 'timeout', by: 'fallback' });
    assert.strictEqual(guard.snapshot().active, 0);
  }

  {
    // 5f. Parallel flow.decide reqId demultiplexing (out-of-order responses + mismatch rejection)
    const guard = createGuardTracker();
    const api = createSykiFlowApi({
      ...guard,
      queryDecision: async ({ reqId, input }) => {
        const delay = input === 'first' ? 40 : 5;
        await new Promise((r) => setTimeout(r, delay));
        return { reqId, choiceId: input === 'first' ? 'bug' : 'idea' };
      },
    });

    const [rFirst, rSecond] = await api.flow.parallel([
      () => api.flow.decide({ input: 'first', choices: ['bug', 'idea', 'memo'], fallback: 'memo' }),
      () => api.flow.decide({ input: 'second', choices: ['bug', 'idea', 'memo'], fallback: 'memo' }),
    ]);
    assert.strictEqual(rFirst.choiceId, 'bug');
    assert.strictEqual(rSecond.choiceId, 'idea');
    assert.ok(rFirst.reqId !== rSecond.reqId, 'parallel calls get distinct reqIds');
    assert.strictEqual(guard.snapshot().active, 0);

    // Mismatched reqId response must be rejected to fallback
    const apiMismatch = createSykiFlowApi({
      queryDecision: async () => ({ reqId: 'wrong_req_id', choiceId: 'bug' }),
    });
    const bad = await apiMismatch.flow.decide({
      input: 'test',
      choices: ['bug', 'memo'],
      fallback: 'memo',
    });
    assert.strictEqual(bad.choiceId, 'memo');
    assert.strictEqual(bad.by, 'fallback');
    assert.ok(bad.reason.includes('reqId_mismatch'));
  }

  console.log('PASS: all syki_api_flow tests passed');
}

runTests().catch((err) => {
  console.error('FAIL:', err);
  process.exit(1);
});
