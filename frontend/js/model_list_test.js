const assert = require('assert');
const ModelList = require('./model_list.js');

(async () => {
  // 1. listUrl
  assert.strictEqual(
    ModelList.listUrl('https://generativelanguage.googleapis.com/v1beta/openai', 'k1'),
    'https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000&key=k1'
  );
  assert.strictEqual(
    ModelList.listUrl('https://api.anthropic.com', 'ak'),
    'https://api.anthropic.com/v1/models?limit=1000'
  );
  assert.strictEqual(
    ModelList.listUrl('https://api.anthropic.com/v1/', 'ak'),
    'https://api.anthropic.com/v1/models?limit=1000'
  );
  assert.strictEqual(
    ModelList.listUrl('https://api.openai.com/v1', 'ok'),
    'https://api.openai.com/v1/models'
  );
  assert.strictEqual(
    ModelList.listUrl('http://127.0.0.1:11434/v1', ''),
    'http://127.0.0.1:11434/api/tags'
  );
  assert.strictEqual(
    ModelList.listUrl('http://127.0.0.1:8080', ''),
    'http://127.0.0.1:8080/v1/models'
  );

  // 2. parse (Anthropic / OpenAI / Gemini / Ollama)
  const ant = ModelList.parse({
    data: [
      { id: 'claude-sonnet-4-5-20250929', display_name: 'Claude Sonnet 4.5' },
      { id: 'gpt-4o' },
    ],
  });
  assert.deepStrictEqual(ant, [
    { id: 'claude-sonnet-4-5-20250929', label: 'Claude Sonnet 4.5 (claude-sonnet-4-5-20250929)', methods: [] },
    { id: 'gpt-4o', label: '', methods: [] },
  ]);

  const gem = ModelList.parse({
    models: [
      { name: 'models/gemini-2.5-flash', displayName: 'Gemini 2.5 Flash', supportedGenerationMethods: ['generateContent'] },
      { name: 'models/imagen-3.0', displayName: 'Imagen 3', supportedGenerationMethods: ['predict'] },
    ],
  });
  assert.strictEqual(gem[0].id, 'gemini-2.5-flash');
  assert.deepStrictEqual(ModelList.filterFor('text', gem).map(m => m.id), ['gemini-2.5-flash']);
  assert.deepStrictEqual(ModelList.filterFor('image', gem).map(m => m.id), ['imagen-3.0']);

  // 3. filterFor excludes non-text OpenAI models
  const oai = ModelList.parse({
    data: [{ id: 'gpt-5' }, { id: 'whisper-1' }, { id: 'dall-e-3' }, { id: 'text-embedding-3-small' }],
  });
  assert.deepStrictEqual(ModelList.filterFor('text', oai).map(m => m.id), ['gpt-5']);
  assert.deepStrictEqual(ModelList.filterFor('image', oai).map(m => m.id), ['dall-e-3']);
  assert.deepStrictEqual(ModelList.filterFor('embed', oai).map(m => m.id), ['text-embedding-3-small']);

  // 4. fetchModels prefers backend.fetchModels when no fetchImpl is passed
  const origBackend = global.backend;
  try {
    let called = null;
    global.backend = {
      fetchModels: async (baseUrl, apiKey) => {
        called = { baseUrl, apiKey };
        return { data: [{ id: 'claude-opus-4-1', display_name: 'Claude Opus 4.1' }] };
      },
    };
    const list = await ModelList.fetchModels('https://api.anthropic.com', 'sk-ant-test');
    assert.deepStrictEqual(called, { baseUrl: 'https://api.anthropic.com', apiKey: 'sk-ant-test' });
    assert.strictEqual(list.length, 1);
    assert.strictEqual(list[0].id, 'claude-opus-4-1');
  } finally {
    global.backend = origBackend;
  }

  console.log('model_list_test.js: all assertions passed');
})();
