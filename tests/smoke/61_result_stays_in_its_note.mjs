// A result that arrives later belongs to the note it was asked from, not to whichever note is on screen when it arrives (UX review
// B11). The primary pane has ONE textarea that shows every note in turn, so a command-bar result (Ctrl+E) and a Quick Actions
// result (Ctrl+J) that held "the editor" and the offsets from when they started used to be spliced into the note the person had
// switched to. The runner and the Quick Actions backend are stand-ins that answer only when the flow says so.
import { assert, clickSelector, selectInEditor, waitFocus, waitHidden, waitShown } from './lib.mjs';

const A = 'zeta\nalpha\nmike\nyankee\nbravo\n';
const B = 'B-line-1\nB-line-2\nB-line-3\nB-line-4\nB-line-5\nB-line-6\n';

const RUNNER = `window.__ran = [];
window.backend.runCommandFilterAsync = function (id, cmd, input) { window.__ran.push({ id: id, cmd: cmd, input: input }); return Promise.resolve(null); };`;

const activeId = async (s) => (await s.state()).activeTabId;
const tabById = async (s, id) => (await s.state()).tabs.find((tab) => tab.id === id);

// A real click on a note's tab, then wait until it is the one on screen.
async function goToTab(s, id) {
  await clickSelector(s, `.tab-item[data-tab-id="${id}"]`);
  await s.waitFor(`window.__explore.state().activeTabId === ${JSON.stringify(id)}`);
}

export default {
  title: 'command bar results go to the note they were asked from',
  session: { notes: [{ title: 'a.md', content: A }, { title: 'b.md', content: B }] },

  async run(s, t) {
    const [tabA, tabB] = (await s.state()).tabs.map((tab) => tab.id);
    await s.ev(RUNNER);
    await goToTab(s, tabA);

    t.step('command bar: run over a selection in a.md, then open b.md before the answer comes');
    await selectInEditor(s, 'alpha\nmike\nyankee');
    await s.key('e', { ctrl: true });
    await waitShown(s, 'cli-filter-bar');
    await waitFocus(s, 'cli-filter-input');
    await s.type('sort');
    await s.key('Enter');
    await s.waitFor('window.__ran.length === 1');
    await goToTab(s, tabB);
    await s.ev(`window.__onCliFilterResult(window.__ran[0].id, { exitCode: 0, output: 'alpha\\nmike\\nyankee\\nSORTED-EXTRA\\n', error: '' }, '')`);
    await waitHidden(s, 'cli-filter-bar');

    t.step('neither note took the output; it is in the result note, and the person is told why');
    await s.waitFor("window.__explore.state().tabs.some(function (t) { return /^\\[CLI\\]/.test(t.title); })");
    let st = await s.state();
    assert.equal((await tabById(s, tabA)).content, A, 'a.md is as it was');
    assert.equal((await tabById(s, tabA)).dirty, false, 'a.md is not marked as changed');
    assert.equal((await tabById(s, tabB)).content, B, 'b.md is as it was: nothing was spliced into it at a.md\'s offsets');
    assert.equal((await tabById(s, tabB)).dirty, false, 'b.md is not marked as changed');
    const result = st.tabs.find((tab) => /^\[CLI\]/.test(tab.title));
    assert.ok(result.content.includes('SORTED-EXTRA'), 'the output was not lost: it is in the result note');
    assert.ok(st.toasts.length > 0, 'a toast says what happened');

    t.step('the same with the setting "no result note" (the output would only go into the note)');
    await s.ev("SykiBridge.getConfig().cli = Object.assign({}, SykiBridge.getConfig().cli, { openResultInNewTab: false })");
    await goToTab(s, tabA);
    await selectInEditor(s, 'alpha\nmike\nyankee');
    await s.key('e', { ctrl: true });
    await waitShown(s, 'cli-filter-bar');
    await waitFocus(s, 'cli-filter-input');
    await s.type('sort');
    await s.key('Enter');
    await s.waitFor('window.__ran.length === 2');
    await goToTab(s, tabB);
    const tabsBefore = (await s.state()).tabs.length;
    await s.ev(`window.__onCliFilterResult(window.__ran[1].id, { exitCode: 0, output: 'X1\\nX2\\n', error: '' }, '')`);
    await s.waitFor(`window.__explore.state().tabs.length === ${tabsBefore + 1}`);
    assert.equal((await tabById(s, tabA)).content, A, 'a.md is as it was');
    assert.equal((await tabById(s, tabB)).content, B, 'b.md is as it was');
    assert.equal((await tabById(s, tabB)).dirty, false);

    t.step('control: when the person stays in a.md the output lands there, below the selected lines');
    await goToTab(s, tabA);
    await selectInEditor(s, 'alpha\nmike\nyankee');
    await s.key('e', { ctrl: true });
    await waitShown(s, 'cli-filter-bar');
    await waitFocus(s, 'cli-filter-input');
    await s.type('sort');
    await s.key('Enter');
    await s.waitFor('window.__ran.length === 3');
    await s.ev(`window.__onCliFilterResult(window.__ran[2].id, { exitCode: 0, output: 'OUT1\\nOUT2\\n', error: '' }, '')`);
    await s.waitFor(`window.__explore.state().tabs.filter(function (t) { return t.id === ${JSON.stringify(tabA)}; })[0].content.indexOf('OUT1') !== -1`);
    assert.equal((await tabById(s, tabA)).content, 'zeta\nalpha\nmike\nyankee\nOUT1\nOUT2\nbravo\n', 'the output went below the selected lines of a.md');
    assert.equal((await tabById(s, tabB)).content, B, 'b.md untouched');
    assert.equal(await activeId(s), tabA);
  },
};
