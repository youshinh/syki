// Unit tests for mermaid_tone.js: tone choice, the Mermaid configuration behind each tone, and the
// card decoration (with a tiny fake DOM; the real look is checked in a browser).
const assert = require('assert');

global.window = global;
const MT = require('./mermaid_tone.js');

(function testNormalizeTone() {
  assert.deepStrictEqual(MT.TONES, ['auto', 'dark', 'light', 'neutral', 'forest']);
  for (const t of MT.TONES) assert.strictEqual(MT.normalizeTone(t), t, 'the four tones of before are kept, and auto is new');
  assert.strictEqual(MT.normalizeTone(undefined), 'auto', 'no setting yet follows the look (the dark tone on the ink look the app always had)');
  assert.strictEqual(MT.normalizeTone(''), 'auto');
  assert.strictEqual(MT.normalizeTone('sepia'), 'auto', 'a value from a newer or older version falls back');
  assert.strictEqual(MT.normalizeTone(null), 'auto');
})();

(function testResolveTone() {
  assert.strictEqual(MT.resolveTone('auto', 'ink'), 'dark');
  assert.strictEqual(MT.resolveTone('auto', 'paper'), 'light', 'on paper the default tone is a light diagram');
  assert.strictEqual(MT.resolveTone('auto'), 'dark', 'no look given counts as ink');
  assert.strictEqual(MT.resolveTone(undefined, 'paper'), 'light');
  for (const t of ['dark', 'light', 'neutral', 'forest']) {
    assert.strictEqual(MT.resolveTone(t, 'ink'), t, t + ' is a fixed tone on ink');
    assert.strictEqual(MT.resolveTone(t, 'paper'), t, t + ' is a fixed tone on paper');
  }
})();

(function testToneToSave() {
  // a config saved before 'auto' existed holds 'dark'; switching to paper with the select untouched means "follow the look"
  assert.strictEqual(MT.toneToSave('dark', 'dark', 'ink', 'paper'), 'auto');
  assert.strictEqual(MT.toneToSave('dark', 'dark', 'ink', 'ink'), 'dark', 'no change of look: what the select says');
  assert.strictEqual(MT.toneToSave('dark', 'dark', 'paper', 'paper'), 'dark', 'a person who picked Dark on paper keeps it');
  assert.strictEqual(MT.toneToSave('dark', 'dark', 'paper', 'ink'), 'dark');
  assert.strictEqual(MT.toneToSave('light', 'dark', 'ink', 'paper'), 'light', 'a tone the person chose in this dialog is kept');
  assert.strictEqual(MT.toneToSave('forest', 'forest', 'ink', 'paper'), 'forest');
  assert.strictEqual(MT.toneToSave('auto', 'auto', 'ink', 'paper'), 'auto');
  assert.strictEqual(MT.toneToSave('dark', 'light', 'ink', 'paper'), 'dark', 'dark chosen now, after something else was saved, is a choice');
  assert.strictEqual(MT.toneToSave('bogus', 'dark', 'ink', 'paper'), 'auto');
  assert.strictEqual(MT.toneToSave(undefined, undefined, 'ink', 'paper'), 'auto');
})();

(function testFlip() {
  assert.strictEqual(MT.flipTone('dark'), 'light');
  assert.strictEqual(MT.flipTone('light'), 'dark');
  assert.strictEqual(MT.flipTone('neutral'), 'dark', 'a light tone flips back to dark');
  assert.strictEqual(MT.flipTone('forest'), 'dark');
  assert.strictEqual(MT.flipTone(undefined), 'light', 'auto on ink is dark, so it flips to light');
  assert.strictEqual(MT.flipTone('auto', 'ink'), 'light');
  assert.strictEqual(MT.flipTone('auto', 'paper'), 'dark', 'auto on paper is light, so it flips to dark');
})();

(function testMermaidConfig() {
  const dark = MT.mermaidConfig('dark');
  assert.strictEqual(dark.theme, 'dark');
  assert.strictEqual(dark.startOnLoad, false);
  assert.strictEqual(dark.securityLevel, 'strict', 'the security level must never depend on the tone');
  assert.deepStrictEqual(dark.themeVariables, { darkMode: true }, 'without a page to read the tokens from, Mermaid keeps its own dark colors');
  // the three colors come from the page's tokens (app.js: readMermaidPalette), values as the card has them on the ink look
  const palette = { background: '#252526', primary: '#007acc', text: '#d4d4d4' };
  assert.deepStrictEqual(MT.mermaidConfig('dark', palette).themeVariables, {
    darkMode: true, background: '#252526', primaryColor: '#007acc', textColor: '#d4d4d4'
  }, 'the dark tone is the configuration the app has always used');
  assert.deepStrictEqual(Object.keys(MT.mermaidConfig('dark', palette).themeVariables), ['darkMode', 'background', 'primaryColor', 'textColor']);
  assert.deepStrictEqual(MT.mermaidConfig('auto', palette, 'ink'), MT.mermaidConfig('dark', palette), 'auto on ink is the dark tone');
  assert.strictEqual(MT.mermaidConfig('auto', palette, 'paper').theme, 'default', 'auto on paper is the light tone');
  assert.strictEqual(MT.mermaidConfig('auto', palette, 'paper').themeVariables, undefined);
  assert.strictEqual(MT.mermaidConfig('dark', palette, 'paper').theme, 'dark', 'an explicit dark tone stays dark on paper');
  assert.strictEqual(MT.mermaidConfig('light').theme, 'default');
  assert.strictEqual(MT.mermaidConfig('neutral').theme, 'neutral');
  assert.strictEqual(MT.mermaidConfig('forest').theme, 'forest');
  for (const t of ['light', 'neutral', 'forest']) {
    const c = MT.mermaidConfig(t);
    assert.strictEqual(c.themeVariables, undefined, t + ': no dark overrides on a light tone');
    assert.strictEqual(c.securityLevel, 'strict');
    assert.strictEqual(c.startOnLoad, false);
  }
  assert.strictEqual(MT.mermaidConfig('bogus').theme, 'dark', 'a bogus tone is auto, which is dark on the default (ink) look');
})();

(function testCardClasses() {
  assert.deepStrictEqual(MT.cardClasses('light'), ['mermaid-card', 'tone-light']);
  assert.deepStrictEqual(MT.cardClasses('nope'), ['mermaid-card', 'tone-dark']);
  assert.deepStrictEqual(MT.cardClasses('auto', 'paper'), ['mermaid-card', 'tone-light'], 'the card carries the tone that is drawn, never "auto" (print_preview.js looks for tone-dark)');
  assert.deepStrictEqual(MT.cardClasses('auto', 'ink'), ['mermaid-card', 'tone-dark']);
})();

(function testDecorate() {
  const classes = new Set(['tone-dark']);
  const children = [];
  let handler = null;
  const btn = {
    attrs: {},
    setAttribute(k, v) { this.attrs[k] = v; },
    addEventListener(type, fn) { if (type === 'click') handler = fn; }
  };
  const container = {
    ownerDocument: { createElement: (tag) => { assert.strictEqual(tag, 'button'); return btn; } },
    classList: {
      remove: (...names) => names.forEach((n) => classes.delete(n)),
      add: (n) => classes.add(n)
    },
    appendChild: (c) => children.push(c)
  };
  let flips = 0;
  MT.decorate(container, 'light', 'Switch colors', () => { flips++; });
  assert.ok(classes.has('tone-light') && classes.has('mermaid-card'));
  assert.ok(!classes.has('tone-dark'), 'the previous tone class is removed');
  assert.strictEqual(children.length, 1);
  assert.strictEqual(btn.type, 'button', 'must not submit or navigate anything');
  assert.strictEqual(btn.className, 'mermaid-tone-btn');
  assert.strictEqual(btn.title, 'Switch colors');
  assert.strictEqual(btn.attrs['aria-label'], 'Switch colors');
  assert.ok(/<svg/.test(btn.innerHTML) && !/[\u{1F300}-\u{1FAFF}]/u.test(btn.innerHTML), 'a line SVG icon, no emoji');
  let stopped = 0;
  let prevented = 0;
  handler({ preventDefault() { prevented++; }, stopPropagation() { stopped++; } });
  assert.strictEqual(flips, 1);
  assert.strictEqual(stopped, 1, 'the click must not reach the preview pane link handler');
  assert.strictEqual(prevented, 1);
  MT.decorate(null, 'dark', 't', () => {}); // must not throw
})();

// sanitizeSource regressions (sub-graph, Unicode arrows, missing [] brackets, smart quotes)
(() => {
  const input1 = [
    'flowchart TD',
    '  sub-graph S1["[1] 事前チェック初回稼働 (05:40-05:42)"]',
    '    S1_Start([初回稼働確認]) ──> S1_Res{結果判定}',
    '    S1_Res ──>|一部問題あり| S1_Check6[6つのタスク]',
    '    S5_Verify ──> S5_Jdg[残りは外部倉庫側の反映待ちと推定<br>→ 本日の対応は不要と判断]',
    '  end'
  ].join('\n');
  const out1 = MT.sanitizeSource(input1);
  assert.ok(out1.includes('  subgraph S1["[1] 事前チェック初回稼働 (05:40-05:42)"]'));
  assert.ok(!out1.includes('sub-graph'));
  assert.ok(out1.includes('S1_Start([初回稼働確認]) --> S1_Res{結果判定}'));
  assert.ok(out1.includes('S1_Res -->|一部問題あり| S1_Check6[6つのタスク]'));
  assert.ok(out1.includes('S5_Verify --> S5_Jdg[残りは外部倉庫側の反映待ちと推定<br>→ 本日の対応は不要と判断]'));

  const input2 = [
    'flowchart TD',
    '  subgraph S1 "タイトル"',
    '  end',
    '  subgraph "タイトルのみ"',
    '  end',
    '  subgraph [1] 事前チェック初回稼働 (05:40-05:42)',
    '  end',
    '  subgraph S2[事前チェック (05:40)]',
    '  end',
    '  subgraph S3[\u201D[1] 事前チェック\u201D]',
    '  end'
  ].join('\n');
  const out2 = MT.sanitizeSource(input2);
  assert.ok(out2.includes('subgraph S1["タイトル"]'));
  assert.ok(out2.includes('subgraph sg_1["タイトルのみ"]'));
  assert.ok(out2.includes('subgraph sg_2["[1] 事前チェック初回稼働 (05:40-05:42)"]'));
  assert.ok(out2.includes('subgraph S2["事前チェック (05:40)"]'));
  assert.ok(out2.includes('subgraph S3["[1] 事前チェック"]'));
})();

// sanitizeSource mindmap regressions (flowchart-style id["..."], <br/>, multiple roots, tabs)
(() => {
  const realMindmap = [
    'mindmap',
    '  root["ノートアプリ仕様"]',
    '    UI["UIデザインと質感"]',
    '      Paper["紙のような質感と立体感<br/>枠線排除・シャドウ・綴じ目陰影"]',
    '      CtrlL["Ctrl+L<br/>カーソル位置のAI (高速)"]'
  ].join('\n');
  const out = MT.sanitizeSource(realMindmap);
  assert.ok(out.includes('  root((ノートアプリ仕様))'), 'root["..."] becomes root((...))');
  assert.ok(out.includes('    UI[UIデザインと質感]'), 'id["..."] drops quotes');
  assert.ok(!out.includes('<br/>'), '<br/> is replaced for htmlLabels:false mindmap');
  assert.ok(!out.includes('"'), 'no double quotes left in mindmap nodes');
  assert.ok(out.includes('（高速）'), 'ASCII parens inside label become fullwidth');

  const multiRoot = [
    'mindmap',
    '\tTopicA',
    '\t\tSub1',
    '\tTopicB'
  ].join('\n');
  const fixedMulti = MT.sanitizeSource(multiRoot);
  assert.ok(fixedMulti.includes('  root((Mindmap))'), 'multiple roots wrapped in single root');
  assert.ok(fixedMulti.includes('    TopicA'));
  assert.ok(fixedMulti.includes('      Sub1'));
  assert.ok(fixedMulti.includes('    TopicB'));

  // sequenceDiagram redundant deactivate across alt/else branches
  const seqWithDupDeact = [
    'sequenceDiagram',
    '  activate AuthAPI',
    '  alt OK',
    '    deactivate AuthAPI',
    '  else NG',
    '    deactivate AuthAPI',
    '  end'
  ].join('\n');
  const fixedSeq = MT.sanitizeSource(seqWithDupDeact);
  assert.strictEqual((fixedSeq.match(/deactivate AuthAPI/g) || []).length, 1, 'drops redundant second deactivate');
})();

console.log('mermaid_tone tests passed');
