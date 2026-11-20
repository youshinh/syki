// Verifies that the 13 pruned features from syki::sok are cleanly absent from the DOM and Command Palette in syki-lite.
import { assert, openPalette, paletteTitles, waitHidden } from './lib.mjs';

const PRUNED_DOM_IDS = [
  'btn-mobile-drop',
  'jev-action-panel',
  'stat-gitsync',
  'pack-modal',
  'mobile-drop-modal',
  'cf-bridge-qr-modal',
  'tag-pick-modal',
  'deep-search-modal',
  'lesson-modal',
  'local-ai-card',
  'cfg-ocr-on-device',
  'btn-scraps-deep',
  'ctx-new-window',
];

const FORBIDDEN_PALETTE_PATTERNS = [
  /mobile\s*drop|モバイルドロップ/i,
  /\blesson(s)?\b|レッスン/i,
  /\badd\s+tag|タグ.*編集|tag\s*edit/i,
  /export\s+settings\s+package|設定パッケージ|config\s*pack/i,
  /git\s*sync|github\s*sync/i,
  /\bslot\s+agent\b|\bquick\s*actions\b/i,
];

export default {
  title: 'syki-lite prune gate: pruned DOM IDs and command palette items are completely absent',
  session: { notes: [{ title: 'note.md', content: '# syki-lite\n\nMinimal fast editor.\n' }] },

  async run(s, t) {
    t.step('pruned DOM elements do not exist in index.html');
    const present = await s.ev(`(${JSON.stringify(PRUNED_DOM_IDS)}).filter(function (id) { return !!document.getElementById(id); })`);
    assert.deepEqual(present, [], `pruned DOM IDs still present: ${present.join(', ')}`);

    t.step('command palette omits all pruned features');
    await openPalette(s);
    const titles = await paletteTitles(s);
    const offending = [];
    for (const title of titles) {
      for (const re of FORBIDDEN_PALETTE_PATTERNS) {
        if (re.test(title)) offending.push(`${title} (${re})`);
      }
    }
    assert.deepEqual(offending, [], `pruned items found in command palette: ${offending.join('; ')}`);
    await s.key('Escape');
    await waitHidden(s, 'quick-pick-modal');
  },
};
