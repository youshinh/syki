'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const IntentRouter = require('./intent_router.js');

function makeMemoryStorage() {
  const map = new Map();
  return {
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => map.set(k, String(v))
  };
}

test('matchFastRule: 0ms deterministic short-circuits and slash bypass', () => {
  assert.equal(IntentRouter.matchFastRule('/summarize'), null);
  assert.equal(IntentRouter.matchFastRule('@skill do something'), null);

  const grepRes = IntentRouter.matchFastRule('grep rpc_dispatch');
  assert.equal(grepRes.route, 'search_grep');
  assert.equal(grepRes.by, 'rule');
  assert.equal(grepRes.query, 'rpc_dispatch');

  const semRes = IntentRouter.matchFastRule('昨日のタイムアウトの件を意味検索して');
  assert.equal(semRes.route, 'search_semantic');
  assert.equal(semRes.query, '昨日のタイムアウトの件');

  const diagRes = IntentRouter.matchFastRule('認証フローを図にして');
  assert.equal(diagRes.route, 'diagram');

  const agentRes = IntentRouter.matchFastRule('エージェントにテスト修正を依頼');
  assert.equal(agentRes.route, 'send_agent');

  const editSel = IntentRouter.matchFastRule('議事録にして', { hasSelection: true });
  assert.equal(editSel.route, 'edit_note');
  assert.equal(editSel.by, 'rule');

  assert.equal(IntentRouter.matchFastRule('議事録にして', { hasSelection: false }), null);

  const bulletSel = IntentRouter.matchFastRule('箇条書きにまとめて', { hasSelection: true });
  assert.equal(bulletSel.route, 'edit_note');

  const cliRes = IntentRouter.matchFastRule('ping確認して、疎通状態整理して', { hasSelection: true });
  assert.equal(cliRes.route, 'run_cli');
  assert.equal(cliRes.by, 'rule');

  const infoRes = IntentRouter.matchFastRule('この構成をインフォグラフィックにして', { hasSelection: true });
  assert.equal(infoRes.route, 'infographic');
  assert.equal(infoRes.by, 'rule');

  assert.equal(IntentRouter.INTENT_CHOICES.length, 8);
});

test('extractSearchQuery strips Japanese and English search wrappers', () => {
  assert.equal(IntentRouter.extractSearchQuery('grep foo_bar'), 'foo_bar');
  assert.equal(IntentRouter.extractSearchQuery('起動速度のメモを探して'), '起動速度');
  assert.equal(IntentRouter.extractSearchQuery('search for auth middleware'), 'auth middleware');
  assert.equal(IntentRouter.extractSearchQuery('検索して'), '検索して');
});

test('IntentHistory: lazy load, cap at 16, and recordCorrection', () => {
  const storage = makeMemoryStorage();
  for (let i = 0; i < 20; i++) {
    IntentRouter.recordIntent({ id: `id_${i}`, text: `item ${i}`, route: 'ask_ai', by: 'decision' }, storage);
  }
  const list = IntentRouter.loadHistory(storage);
  assert.equal(list.length, 16);
  assert.equal(list[0].id, 'id_4');

  const ok = IntentRouter.recordCorrection('id_19', 'diagram', storage);
  assert.equal(ok, true);
  const hint = IntentRouter.formatHistoryHint(IntentRouter.loadHistory(storage), 5);
  assert.match(hint, /diagram \(corrected from ask_ai\)/);
});

test('buildDecisionPayload includes UI state and recent history', () => {
  const payload = IntentRouter.buildDecisionPayload(
    'これを短くまとめて',
    {
      source: 'bar_voice',
      hasSelection: true,
      selectionChars: 42,
      selectionPreview: '長い文章のサンプルテキスト',
      activeFile: 'notes.md',
      hasEditorContent: true,
      rightPane: 'agent',
      agentRunning: true
    },
    [{ text: '箇条書きにして', route: 'ask_ai', correctedTo: 'edit_note' }]
  );
  assert.match(payload, /source=bar_voice/);
  assert.match(payload, /hasSelection=true\(42ch\)/);
  assert.match(payload, /agentRunning=true/);
  assert.match(payload, /edit_note \(corrected from ask_ai\)/);
  assert.match(payload, /これを短くまとめて/);
});

test('routeIntent: calls queryDecision for ambiguous input and falls back safely', async () => {
  const storage = makeMemoryStorage();
  let called = 0;
  const res = await IntentRouter.routeIntent({
    input: '先週の会議メモどこだっけ',
    uiState: { source: 'bar_text', hasSelection: false, rightPane: 'closed', agentRunning: false },
    enabled: true,
    storage,
    queryDecision: async () => {
      called++;
      return { choiceId: 'search_semantic', by: 'decision', ok: true };
    }
  });
  assert.equal(called, 1);
  assert.equal(res.route, 'search_semantic');
  assert.equal(res.by, 'decision');
  assert.ok(res.historyId);

  const disabledRes = await IntentRouter.routeIntent({
    input: '何でもいい',
    enabled: false,
    queryDecision: async () => { throw new Error('should not be called'); }
  });
  assert.equal(disabledRes.route, 'ask_ai');
  assert.equal(disabledRes.by, 'bypass');

  const errRes = await IntentRouter.routeIntent({
    input: '曖昧な入力',
    enabled: true,
    storage,
    queryDecision: async () => { throw new Error('network timeout'); }
  });
  assert.equal(errRes.route, 'ask_ai');
  assert.equal(errRes.by, 'fallback');
});
