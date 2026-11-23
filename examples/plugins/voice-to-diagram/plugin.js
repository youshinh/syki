/**
 * Official Sample Plugin: Voice to Diagram
 * Demonstrates: input.voice -> ui.review -> flow.decide (Decision model) -> exec.llm
 *               -> exec.render.mermaidToPng -> knowledge.assets.save -> editor.insert
 */

const DIAGRAM_CHOICES = [
  { id: 'flowchart', description: '処理フロー・手順・条件分岐・システム構成図 (flowchart TD)' },
  { id: 'sequence', description: '複数主体やAPI間の時系列のやり取り (sequenceDiagram)' },
  { id: 'state', description: '状態遷移・ライフサイクル (stateDiagram-v2)' },
  { id: 'er', description: 'データモデル・テーブル関係 (erDiagram)' },
];

const DIAGRAM_SYNTAX_HINT = {
  flowchart: 'flowchart TD',
  sequence: 'sequenceDiagram',
  state: 'stateDiagram-v2',
  er: 'erDiagram',
};

function stripMermaidFence(raw) {
  const trimmed = String(raw || '').trim();
  const match = trimmed.match(/^```(?:mermaid)?\s*\r?\n([\s\S]*?)\r?\n```$/i);
  return match ? match[1].trim() : trimmed;
}

export default {
  id: 'voice-to-diagram',
  name: '音声→Mermaid図解',
  apiVersion: 1,
  permissions: ['llm', 'decision', 'voice'],

  init(syki) {
    syki.ui.registerAction({
      id: 'run',
      label: '音声→図',
      icon: 'mic',
      slot: 'toolbar',
      priority: 'primary',
      concurrency: 'lock',
      async run({ progress, signal }) {
        const result = await syki.flow.steps(
          null,
          [
            {
              label: '音声を録音・文字起こし中...',
              run: async () => syki.input.voice({ signal }),
            },
            {
              label: '文字起こし内容を確認中...',
              run: async (transcript) => {
                if (!transcript || !transcript.trim()) return null;
                return syki.ui.review(transcript, {
                  title: '文字起こし内容を確認',
                  okLabel: '図解を生成',
                });
              },
            },
            {
              label: '最適な図の種別を判定中...',
              run: async (reviewed) => {
                if (!reviewed || !reviewed.trim()) return null;
                const decision = await syki.flow.decide({
                  input: reviewed,
                  choices: DIAGRAM_CHOICES,
                  engine: 'decision',
                  fallback: 'flowchart',
                  stripSecrets: true,
                  signal,
                });
                return { text: reviewed, diagramType: decision.choiceId };
              },
            },
            {
              label: 'Mermaid 図を生成中...',
              run: async (ctx) => {
                if (!ctx) return null;
                const header = DIAGRAM_SYNTAX_HINT[ctx.diagramType] || 'flowchart TD';
                const prompt = [
                  `以下の説明を Mermaid の \`${header}\` 記法のみで図解してください。`,
                  '説明文やコードフェンスは不要で、純粋な Mermaid コードのみを返してください。',
                  '括弧や記号を含むラベルは必ずダブルクォートで囲ってください。',
                ].join('\n');
                const raw = await syki.exec.llm(prompt, {
                  input: ctx.text,
                  stripSecrets: true,
                  signal,
                });
                const code = stripMermaidFence(raw);
                if (!code) return null;
                return { code, diagramType: ctx.diagramType };
              },
            },
            {
              label: 'PNG 画像化＆アセット保存中...',
              run: async (ctx) => {
                if (!ctx) return null;
                const asset = await syki.exec.render.mermaidToPng(ctx.code, {
                  theme: syki.ui.getTheme(),
                  signal,
                });
                const savedPath = await syki.knowledge.assets.save(asset, 'png');
                return { code: ctx.code, savedPath, diagramType: ctx.diagramType };
              },
            },
          ],
          { progress, signal }
        );

        if (!result) {
          syki.ui.toast('図解生成を中断しました', 'info');
          return;
        }

        const block = [
          '',
          `![図解 (${result.diagramType})](${result.savedPath})`,
          '',
          '```mermaid',
          result.code,
          '```',
          '',
        ].join('\n');

        syki.editor.insert(block);
        syki.ui.toast(`Mermaid 図 (${result.diagramType}) を挿入しました`, 'ok');
      },
    });
  },
};
