'use strict';

const assert = require('assert');
const {
  normalizeCssColorToHex,
  extractQcThemeSnapshot,
  syncQcThemeIfNeeded,
} = require('./ext_theme_sync.js');

function makeStyle(vars) {
  return {
    getPropertyValue(name) {
      return Object.prototype.hasOwnProperty.call(vars, name) ? vars[name] : '';
    },
  };
}

function makeWindow({ hasUserCss = true, safeMode = false, isPaper = false, vars = {} } = {}) {
  const classes = new Set(isPaper ? ['look-paper'] : ['dark-theme']);
  const body = {
    classList: {
      contains(cls) {
        return classes.has(cls);
      },
    },
  };
  return {
    __sykiPreload: { ext: { hasUserCss, safeMode } },
    document: { body, documentElement: body },
    getComputedStyle() {
      return makeStyle(vars);
    },
  };
}

// 1. normalizeCssColorToHex: rgb(), rgba(), #rgb, #rrggbb, invalid
assert.strictEqual(normalizeCssColorToHex('rgb(25, 26, 24)'), '#191a18');
assert.strictEqual(normalizeCssColorToHex('rgba(85, 107, 47, 0.9)'), '#556b2f');
assert.strictEqual(normalizeCssColorToHex('rgb(255 255 255 / 1)'), '#ffffff');
assert.strictEqual(normalizeCssColorToHex(' #AbC '), '#aabbcc');
assert.strictEqual(normalizeCssColorToHex('#112233'), '#112233');
assert.strictEqual(normalizeCssColorToHex('#112233cc'), '#112233');
assert.strictEqual(normalizeCssColorToHex('transparent'), null);
assert.strictEqual(normalizeCssColorToHex(''), null);

// 2. extractQcThemeSnapshot: normalizes all 8 CSS custom properties
{
  const style = makeStyle({
    '--bg-modal': 'rgb(24, 26, 25)',
    '--bg-field': ' #212320 ',
    '--border-color': 'rgb(54, 57, 52)',
    '--bg-btn': '#282A26',
    '--accent-color': 'rgb(85, 107, 47)',
    '--text-main': '#ebeeec',
    '--text-on-accent': '#fff',
    '--text-muted': 'rgba(144, 149, 142, 0.85)',
  });
  const snap = extractQcThemeSnapshot(style, true);
  assert.deepStrictEqual(snap, {
    isDark: true,
    bg: '#181a19',
    field: '#212320',
    border: '#363934',
    btn: '#282a26',
    btnSave: '#556b2f',
    text: '#ebeeec',
    textOnSave: '#ffffff',
    hint: '#90958e',
  });
}

// 3. syncQcThemeIfNeeded: skips when hasUserCss is false or safeMode is true
{
  let called = 0;
  const winFalse = makeWindow({ hasUserCss: false, vars: { '--bg-modal': '#111111' } });
  assert.strictEqual(syncQcThemeIfNeeded(winFalse, () => { called++; }), false);
  assert.strictEqual(called, 0);

  const winSafe = makeWindow({ hasUserCss: true, safeMode: true, vars: { '--bg-modal': '#111111' } });
  assert.strictEqual(syncQcThemeIfNeeded(winSafe, () => { called++; }), false);
  assert.strictEqual(called, 0);
}

// 4. syncQcThemeIfNeeded: calls saveFn on first sync, skips unchanged, calls again on change
{
  const vars = {
    '--bg-modal': 'rgb(30, 30, 30)',
    '--accent-color': '#556b2f',
  };
  const win = makeWindow({ hasUserCss: true, isPaper: false, vars });
  const saved = [];

  assert.strictEqual(syncQcThemeIfNeeded(win, (s) => saved.push(s)), true);
  assert.strictEqual(saved.length, 1);
  assert.deepStrictEqual(saved[0], {
    isDark: true,
    bg: '#1e1e1e',
    btnSave: '#556b2f',
  });

  // Unchanged -> skipped
  assert.strictEqual(syncQcThemeIfNeeded(win, (s) => saved.push(s)), false);
  assert.strictEqual(saved.length, 1);

  // Changed -> synced again
  vars['--accent-color'] = 'rgb(0, 116, 194)';
  assert.strictEqual(syncQcThemeIfNeeded(win, (s) => saved.push(s)), true);
  assert.strictEqual(saved.length, 2);
  assert.strictEqual(saved[1].btnSave, '#0074c2');
}

console.log('ext_theme_sync_test.js: all assertions passed');
