/**
 * Official Sample Plugin: Gemini Media
 * Bridges syki with `gemini-api` CLI for PDF/Video/YouTube analysis,
 * speaker-diarized transcription, and text-to-speech (TTS) synthesis.
 */

function extractTarget(syki) {
  const sel = String(syki.editor.getSelection() || '').trim();
  let raw = sel;
  if (!raw) {
    const lines = String(syki.editor.getText() || '').split(/\r?\n/);
    for (const line of lines) {
      const t = line.trim();
      if (t) {
        raw = t;
        break;
      }
    }
  }
  if (!raw) return '';
  if (raw.startsWith('[') && raw.endsWith(')')) {
    const mid = raw.indexOf('](');
    if (mid > 0) raw = raw.slice(mid + 2, -1).trim();
  }
  if ((raw.startsWith('"') && raw.endsWith('"')) || (raw.startsWith("'") && raw.endsWith("'"))) {
    raw = raw.slice(1, -1);
  }
  return raw.trim();
}

function formatShellError(err, res) {
  if (res && res.stderr && String(res.stderr).trim()) {
    const lines = String(res.stderr).trim().split(/\r?\n/).filter((l) => l.trim());
    return lines[lines.length - 1] || 'gemini-api failed';
  }
  const msg = String((err && err.message) || err || 'gemini-api failed');
  if (/ENOENT|not found|cannot find/i.test(msg)) {
    return 'gemini-api CLI が見つかりません（go install で導入してください）';
  }
  return msg;
}

export default {
  id: 'gemini-media',
  name: 'Gemini Media',
  apiVersion: 1,
  permissions: ['shell:gemini-api'],

  async init(syki) {
    syki.ui.registerSettings([
      {
        key: 'analyzePrompt',
        label: 'メディア解析プロンプト',
        type: 'text',
        defaultValue: '内容を構造化して日本語の Markdown で要約してください。',
      },
      {
        key: 'transcribeFormat',
        label: '文字起こし形式',
        type: 'select',
        defaultValue: 'md',
        options: [
          { value: 'md', label: 'Markdown (md)' },
          { value: 'text', label: 'Plain Text (text)' },
          { value: 'srt', label: 'Subtitles (srt)' },
          { value: 'json', label: 'JSON (json)' },
        ],
      },
      {
        key: 'ttsVoice',
        label: 'TTS 音声',
        type: 'select',
        defaultValue: 'Kore',
        options: [
          { value: 'Kore', label: 'Kore' },
          { value: 'Puck', label: 'Puck' },
          { value: 'Charon', label: 'Charon' },
          { value: 'Aoede', label: 'Aoede' },
        ],
      },
      {
        key: 'openInNewTab',
        label: '結果を新しいタブで開く',
        type: 'boolean',
        defaultValue: true,
      },
    ]);

    let analyzePrompt = await syki.sys.storage.get(
      'analyzePrompt',
      '内容を構造化して日本語の Markdown で要約してください。'
    );
    let transcribeFormat = await syki.sys.storage.get('transcribeFormat', 'md');
    let ttsVoice = await syki.sys.storage.get('ttsVoice', 'Kore');
    let openInNewTab = await syki.sys.storage.get('openInNewTab', true);

    syki.sys.on('settings:change', ({ key, value }) => {
      if (key === 'analyzePrompt' && typeof value === 'string') analyzePrompt = value;
      if (key === 'transcribeFormat' && typeof value === 'string') transcribeFormat = value;
      if (key === 'ttsVoice' && typeof value === 'string') ttsVoice = value;
      if (key === 'openInNewTab' && typeof value === 'boolean') openInNewTab = value;
    });

    async function runAnalyze(opts = {}) {
      const target = (opts && typeof opts.target === 'string' && opts.target.trim()) || extractTarget(syki);
      if (!target) {
        syki.ui.toast('解析対象のファイルパスまたは URL を選択してください', 'warn');
        return null;
      }
      const prompt = (opts && typeof opts.prompt === 'string' && opts.prompt.trim()) || analyzePrompt;
      let res;
      try {
        res = await syki.exec.shell('gemini-api', ['analyze', '-i', target, prompt], {
          timeoutMs: 120000,
          signal: opts.signal,
        });
      } catch (err) {
        const msg = formatShellError(err);
        syki.ui.toast(msg, 'error');
        throw new Error(msg);
      }
      if (!res || res.exitCode !== 0) {
        const msg = formatShellError(null, res);
        syki.ui.toast(msg, 'error');
        throw new Error(msg);
      }
      const out = String(res.stdout || '').trim();
      if (openInNewTab) {
        syki.editor.openTab({ title: 'Gemini Analysis.md', content: out });
      } else {
        syki.editor.replaceSelection('\n' + out + '\n');
      }
      syki.ui.toast('メディア解析が完了しました', 'ok');
      return out;
    }

    async function runTranscribe(opts = {}) {
      const target = (opts && typeof opts.target === 'string' && opts.target.trim()) || extractTarget(syki);
      if (!target) {
        syki.ui.toast('文字起こし対象の音声・動画パスを選択してください', 'warn');
        return null;
      }
      const fmt = (opts && typeof opts.format === 'string' && opts.format.trim()) || transcribeFormat;
      const args = ['transcribe', '-i', target, '--format', fmt];
      if (opts && typeof opts.out === 'string' && opts.out.trim()) {
        args.push('--out', opts.out.trim());
      }
      let res;
      try {
        res = await syki.exec.shell('gemini-api', args, {
          timeoutMs: 120000,
          signal: opts.signal,
        });
      } catch (err) {
        const msg = formatShellError(err);
        syki.ui.toast(msg, 'error');
        throw new Error(msg);
      }
      if (!res || res.exitCode !== 0) {
        const msg = formatShellError(null, res);
        syki.ui.toast(msg, 'error');
        throw new Error(msg);
      }
      const artifactPath = String(res.stdout || '').trim().split(/\r?\n/)[0] || '';
      if (openInNewTab) {
        syki.editor.openTab({
          title: 'Transcript.' + fmt,
          content: artifactPath ? `文字起こし出力: ${artifactPath}\n` : '文字起こしが完了しました。\n',
        });
      } else if (artifactPath) {
        syki.editor.replaceSelection(`\n文字起こし出力: ${artifactPath}\n`);
      }
      syki.ui.toast('文字起こしが完了しました', 'ok');
      return artifactPath;
    }

    async function runTts(opts = {}) {
      const raw = String(
        (opts && typeof opts.text === 'string' && opts.text) ||
        syki.editor.getSelection() ||
        syki.editor.getText() ||
        ''
      ).trim();
      if (!raw) {
        syki.ui.toast('読み上げるテキストを選択または入力してください', 'warn');
        return null;
      }
      const safeText = typeof syki.sys.stripSecrets === 'function'
        ? syki.sys.stripSecrets(raw)
        : raw;
      const voice = (opts && typeof opts.voice === 'string' && opts.voice.trim()) || ttsVoice;
      const args = ['tts', '--stdin', '--voice', voice];
      if (opts && typeof opts.out === 'string' && opts.out.trim()) {
        args.push('--out', opts.out.trim());
      }
      let res;
      try {
        res = await syki.exec.shell('gemini-api', args, {
          stdin: safeText,
          timeoutMs: 120000,
          signal: opts.signal,
        });
      } catch (err) {
        const msg = formatShellError(err);
        syki.ui.toast(msg, 'error');
        throw new Error(msg);
      }
      if (!res || res.exitCode !== 0) {
        const msg = formatShellError(null, res);
        syki.ui.toast(msg, 'error');
        throw new Error(msg);
      }
      const wavPath = String(res.stdout || '').trim().split(/\r?\n/)[0] || '';
      if (wavPath) {
        syki.editor.replaceSelection(`\n[音声出力](${wavPath})\n`);
      }
      syki.ui.toast('音声合成が完了しました', 'ok');
      return wavPath;
    }

    syki.ui.registerAction({
      id: 'analyze-media',
      label: 'Media解析',
      icon: 'sparkles',
      badge: 'AI',
      slot: 'toolbar',
      priority: 'secondary',
      concurrency: 'lock',
      async run({ progress, signal }) {
        if (progress && typeof progress.update === 'function') {
          progress.update('gemini-api で解析中...');
        }
        return await runAnalyze({ signal });
      },
    });

    syki.sys.registerCommand({ id: 'gemini-media:analyze', run: runAnalyze, exposeRpc: true });
    syki.sys.registerCommand({ id: 'gemini-media:transcribe', run: runTranscribe, exposeRpc: true });
    syki.sys.registerCommand({ id: 'gemini-media:tts', run: runTts, exposeRpc: true });
  },
};
