// Verifies kept syki-lite features requested explicitly by the user:
// - F11: Ctrl+K inline selection rewrite + undo
// - F12: InfographicPrompt API & backend.generateImageAsync seam
// - F23: AI OCR only (backend.queryVisionAsync present, WinRT #cfg-ocr-on-device absent)
import { assert, focusEditor, selectInEditor, submitAsk, waitFocus, waitIdle, waitNote, waitShown } from './lib.mjs';

const ORIG_LINE = 'This sentence is way too wordy and redundant for a crisp engineering note.';
const NOTE = `# Draft\n\n${ORIG_LINE}\n`;

export default {
  title: 'syki-lite core AI: Ctrl+K rewrite (F11), infographic prompt (F12), and AI-OCR-only (F23)',
  session: {
    notes: [{ title: 'draft.md', content: NOTE }],
    llm: { mode: 'ok', delayMs: 80, reply: 'A crisp engineering sentence.' },
  },

  async run(s, t) {
    t.step('F11: Ctrl+K rewrites selected text in place and supports Ctrl+Z restore');
    await selectInEditor(s, ORIG_LINE);
    await s.key('k', { ctrl: true });
    await waitShown(s, 'inline-prompt-bar');
    await waitFocus(s, 'inline-prompt-input');
    await submitAsk(s, 'make concise');
    await waitNote(s, "text.indexOf('A crisp engineering sentence.') !== -1");
    await waitIdle(s);
    let content = (await s.state()).tabs[0].content;
    assert.ok(content.includes('A crisp engineering sentence.'), 'rewritten text replaced the selection');
    assert.ok(!content.includes(ORIG_LINE), 'original selected line was replaced by Ctrl+K');

    await focusEditor(s);
    for (let i = 0; i < 3 && (await s.state()).tabs[0].content !== NOTE; i++) {
      const before = (await s.state()).tabs[0].content;
      await s.key('z', { ctrl: true });
      await s.waitFor(`window.__explore.state().tabs[0].content !== ${JSON.stringify(before)}`, { timeout: 2000 });
    }
    assert.equal((await s.state()).tabs[0].content, NOTE, 'Ctrl+Z restored the original note before Ctrl+K');
    await s.waitFor("window.__explore.state().pendingLlm === 0");

    t.step('F12: InfographicPrompt builds Mermaid image prompts and generateImageAsync seam is present');
    const infoCheck = await s.ev(`(function () {
      var ip = window.InfographicPrompt;
      var built = ip ? ip.buildImagePrompt('graph LR\\n  A[Start] --> B[End]', '# Plan') : null;
      return {
        hasGlobal: !!(ip && typeof ip.buildImagePrompt === 'function'),
        kind: built && built.kind,
        promptLen: built && built.prompt ? built.prompt.length : 0,
        hasBackendFn: !!(window.backend && typeof window.backend.generateImageAsync === 'function'),
      };
    })()`);
    assert.equal(infoCheck.hasGlobal, true, 'window.InfographicPrompt.buildImagePrompt exists');
    assert.equal(infoCheck.kind, 'mermaid', 'Mermaid input classified as mermaid infographic');
    assert.ok(infoCheck.promptLen > 50, `built infographic prompt has detail (${infoCheck.promptLen} chars)`);
    assert.equal(infoCheck.hasBackendFn, true, 'backend.generateImageAsync seam is available');

    t.step('F23: AI OCR backend is available and WinRT on-device OCR checkbox is removed');
    await s.key(',', { ctrl: true });
    await waitShown(s, 'settings-modal');
    const ocrCheck = await s.ev(`({
      hasVisionBackend: !!(window.backend && typeof window.backend.queryVisionAsync === 'function'),
      winrtCheckbox: document.getElementById('cfg-ocr-on-device'),
    })`);
    assert.equal(ocrCheck.hasVisionBackend, true, 'backend.queryVisionAsync exists for AI OCR');
    assert.equal(ocrCheck.winrtCheckbox, null, 'WinRT #cfg-ocr-on-device toggle is absent in syki-lite');
    await s.key('Escape');
  },
};
