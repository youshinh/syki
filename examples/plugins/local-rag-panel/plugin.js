/**
 * Official Sample Plugin: Local RAG Panel
 * Demonstrates: knowledge.searchSemantic, ui.mountPanel('side-panel') with Shadow DOM, exec.llm
 */

export default {
  id: 'local-rag-panel',
  name: 'ローカルRAGパネル',
  apiVersion: 1,
  permissions: ['llm', 'semantic'],

  init(syki) {
    let panelHandle = null;

    const openPanel = () => {
      if (panelHandle) {
        panelHandle.dispose();
        panelHandle = null;
        return;
      }

      panelHandle = syki.ui.mountPanel('side-panel', {
        title: 'ローカルRAG検索',
        render(root) {
          const container = document.createElement('div');
          container.className = 'rag-root';

          const header = document.createElement('form');
          header.className = 'rag-form';

          const input = document.createElement('input');
          input.type = 'search';
          input.className = 'rag-input';
          input.placeholder = 'ローカルノートへ質問...';

          const submitBtn = document.createElement('button');
          submitBtn.type = 'submit';
          submitBtn.className = 'rag-btn rag-btn-primary';
          submitBtn.textContent = '検索';

          header.append(input, submitBtn);

          const statusEl = document.createElement('div');
          statusEl.className = 'rag-status';
          statusEl.textContent = '意味検索＋ローカルRAG要約を実行します。';

          const answerSection = document.createElement('section');
          answerSection.className = 'rag-answer-card';
          answerSection.hidden = true;

          const answerBody = document.createElement('pre');
          answerBody.className = 'rag-answer-body';

          const insertBtn = document.createElement('button');
          insertBtn.type = 'button';
          insertBtn.className = 'rag-btn';
          insertBtn.textContent = '回答をエディタへ挿入';

          answerSection.append(answerBody, insertBtn);

          const hitsList = document.createElement('ul');
          hitsList.className = 'rag-hits';

          container.append(header, statusEl, answerSection, hitsList);
          root.appendChild(container);

          let latestAnswer = '';
          let activeController = null;

          insertBtn.addEventListener('click', () => {
            if (!latestAnswer) return;
            syki.editor.insert(`\n${latestAnswer}\n`);
            syki.ui.toast('RAG 回答をエディタへ挿入しました', 'ok');
          });

          header.addEventListener('submit', async (ev) => {
            ev.preventDefault();
            const query = input.value.trim();
            if (!query) {
              statusEl.textContent = '検索キーワードまたは質問を入力してください。';
              return;
            }

            if (activeController) {
              activeController.abort();
            }
            activeController = new AbortController();
            const { signal } = activeController;

            submitBtn.disabled = true;
            answerSection.hidden = true;
            hitsList.replaceChildren();
            statusEl.textContent = 'ローカルノートを意味検索中...';

            try {
              const hits = await syki.knowledge.searchSemantic(query, { limit: 5 });
              if (signal.aborted) return;

              if (!hits || hits.length === 0) {
                statusEl.textContent = '関連するノートが見つかりませんでした。';
                return;
              }

              for (const hit of hits) {
                const li = document.createElement('li');
                li.className = 'rag-hit';

                const titleBtn = document.createElement('button');
                titleBtn.type = 'button';
                titleBtn.className = 'rag-hit-title';
                titleBtn.textContent = hit.title || hit.path;
                titleBtn.addEventListener('click', async () => {
                  const content = await syki.knowledge.notes.read(hit.path);
                  syki.editor.openTab({ title: hit.title || hit.path, content });
                });

                const snippet = document.createElement('p');
                snippet.className = 'rag-hit-snippet';
                snippet.textContent = hit.snippet;

                li.append(titleBtn, snippet);
                hitsList.appendChild(li);
              }

              statusEl.textContent = `${hits.length} 件のノートをもとに回答を生成中...`;

              const context = hits
                .map((h, i) => `[${i + 1}] ${h.path}\n${h.snippet}`)
                .join('\n\n');

              const prompt = [
                `質問: ${query}`,
                '以下のローカルノート抜粋のみを根拠に、日本語で簡潔に回答してください。',
                '根拠となるノート番号（例: [1]）を文末に明記してください。',
              ].join('\n');

              const answer = await syki.exec.llm(prompt, {
                input: context,
                stripSecrets: true,
                signal,
              });
              if (signal.aborted) return;

              latestAnswer = answer.trim();
              answerBody.textContent = latestAnswer;
              answerSection.hidden = !latestAnswer;
              statusEl.textContent = `${hits.length} 件のノートから回答を生成しました。`;
            } catch (err) {
              if (!signal.aborted) {
                statusEl.textContent = `エラー: ${err instanceof Error ? err.message : String(err)}`;
              }
            } finally {
              submitBtn.disabled = false;
            }
          });
        },
      });
    };

    syki.ui.registerAction({
      id: 'toggle-rag-panel',
      label: 'RAG検索',
      icon: 'search',
      slot: 'toolbar',
      priority: 'secondary',
      run() {
        openPanel();
      },
    });
  },
};
