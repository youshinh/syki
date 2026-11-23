/**
 * Official Sample Plugin: Translate Bar
 * Demonstrates: ui.registerSettings, settings:change, ActionHandle.update, exec.llm, ui.diffConfirm
 */

const LANG_LABELS = {
  en: 'EN',
  ja: 'JA',
  zh: 'ZH',
  ko: 'KO',
};

const TONE_INSTRUCTIONS = {
  natural: '自然で読みやすい文体で翻訳してください。',
  concise: '簡潔で要点を押さえた文体で翻訳してください。',
  formal: '丁寧でビジネス向けの文体で翻訳してください。',
};

export default {
  id: 'translate-bar',
  name: '翻訳ミニバー',
  apiVersion: 1,
  permissions: ['llm'],

  async init(syki) {
    syki.ui.registerSettings([
      {
        key: 'targetLang',
        label: '翻訳先言語',
        type: 'select',
        defaultValue: 'en',
        options: [
          { value: 'en', label: 'English (EN)' },
          { value: 'ja', label: '日本語 (JA)' },
          { value: 'zh', label: '中文 (ZH)' },
          { value: 'ko', label: '한국어 (KO)' },
        ],
      },
      {
        key: 'tone',
        label: '文体',
        type: 'select',
        defaultValue: 'natural',
        options: [
          { value: 'natural', label: '自然 (Natural)' },
          { value: 'concise', label: '簡潔 (Concise)' },
          { value: 'formal', label: '丁寧 (Formal)' },
        ],
      },
    ]);

    let targetLang = await syki.sys.storage.get('targetLang', 'en');
    let tone = await syki.sys.storage.get('tone', 'natural');

    const formatBadge = (lang) => LANG_LABELS[lang] || String(lang).toUpperCase();

    const action = syki.ui.registerAction({
      id: 'translate-selection',
      label: '翻訳',
      icon: 'languages',
      badge: formatBadge(targetLang),
      slot: 'toolbar',
      priority: 'primary',
      concurrency: 'lock',
      async run({ progress, signal }) {
        const selection = syki.editor.getSelection();
        const hasSelection = selection.trim().length > 0;
        const source = hasSelection ? selection : syki.editor.getText();

        if (!source.trim()) {
          syki.ui.toast('翻訳するテキストがありません', 'warn');
          return;
        }

        const badge = formatBadge(targetLang);
        if (typeof progress.update === 'function') {
          progress.update(`${badge} へ翻訳中...`);
        } else if (typeof progress.set === 'function') {
          progress.set(`${badge} へ翻訳中...`);
        }

        const toneHint = TONE_INSTRUCTIONS[tone] || TONE_INSTRUCTIONS.natural;
        const prompt = [
          `以下のテキストを ${badge} に翻訳してください。`,
          toneHint,
          'Markdown の構造・コードブロック・インラインコードは保持し、翻訳結果のみを出力してください。',
        ].join('\n');

        const translated = await syki.exec.llm(prompt, {
          input: source,
          stripSecrets: true,
          signal,
        });

        if (!translated || !translated.trim()) {
          syki.ui.toast('翻訳結果が空でした', 'warn');
          return;
        }

        const accepted = await syki.ui.diffConfirm(source, translated, {
          title: `翻訳結果の確認 (${badge})`,
        });
        if (!accepted) {
          syki.ui.toast('翻訳をキャンセルしました', 'info');
          return;
        }

        if (hasSelection) {
          syki.editor.replaceSelection(translated);
        } else {
          syki.editor.setText(translated);
        }
        syki.ui.toast(`${badge} への翻訳を適用しました`, 'ok');
      },
    });

    syki.sys.on('settings:change', ({ key, value }) => {
      if (key === 'targetLang' && typeof value === 'string') {
        targetLang = value;
        action.update({ badge: formatBadge(targetLang) });
      } else if (key === 'tone' && typeof value === 'string') {
        tone = value;
      }
    });
  },
};
