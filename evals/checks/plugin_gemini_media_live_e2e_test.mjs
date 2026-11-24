/**
 * Live E2E Verification of Official Plugin `gemini-media` against real `gemini-api` CLI
 * using `process.env.GEMINI_API_KEY`.
 */
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';

const require = createRequire(import.meta.url);
const Bootstrap = require('../../frontend/js/syki_bootstrap.js');

if (!process.env.GEMINI_API_KEY || !process.env.GEMINI_API_KEY.trim()) {
  console.error('SKIP: GEMINI_API_KEY is not set.');
  process.exit(2);
}

function resolveGeminiApiBin() {
  const gopath = process.env.GOPATH || path.join(os.homedir(), 'go');
  const candidate = path.join(gopath, 'bin', process.platform === 'win32' ? 'gemini-api.exe' : 'gemini-api');
  return fs.existsSync(candidate) ? candidate : 'gemini-api';
}

function runProgram(program, args, opts = {}) {
  const bin = program === 'gemini-api' ? resolveGeminiApiBin() : program;
  return new Promise((resolve, reject) => {
    const child = spawn(bin, args || [], {
      cwd: opts.cwd || process.cwd(),
      env: process.env,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (d) => { stdout += d.toString(); });
    child.stderr.on('data', (d) => { stderr += d.toString(); });
    child.on('error', reject);
    if (opts.stdin !== undefined && opts.stdin !== null) {
      child.stdin.write(String(opts.stdin));
    }
    child.stdin.end();
    child.on('close', (code) => {
      resolve({ exitCode: code ?? 1, stdout, stderr });
    });
  });
}

const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'syki-gemini-media-e2e-'));
try {
  let noteText = '';
  let selection = '';
  const openedTabs = [];
  const commands = new Map();

  const editorAdapter = {
    getText: () => noteText,
    setText: (t) => { noteText = String(t); },
    getSelection: () => selection,
    insertAtCursor: (t) => { noteText += String(t); },
    openTab: (tab) => { openedTabs.push(tab); return tab; },
  };
  const platform = Bootstrap.createSykiPlatform({
    pluginId: 'gemini-media',
    permissions: ['shell:gemini-api'],
    ...editorAdapter,
    editor: editorAdapter,
    registerCommand: (cmd) => {
      commands.set(cmd.id, cmd.handler || cmd.run);
      return { dispose() { commands.delete(cmd.id); } };
    },
    runProgram,
  });

  const pluginUrl = pathToFileURL(path.resolve('examples/plugins/gemini-media/plugin.js')).href;
  const mod = await import(pluginUrl);
  await mod.default.init(platform.syki);

  // 1. Live analyze test
  const sampleMd = path.join(tmpDir, 'fact.md');
  fs.writeFileSync(sampleMd, '# Project Phoenix\nCodeword: SYKI-FAST-2026\n', 'utf8');
  platform.syki.editor.setText(sampleMd);
  const analyzeOut = await platform.syki.sys.runCommand('gemini-media:analyze', {
    target: sampleMd,
    prompt: 'What is the Codeword in this file? Reply concisely.',
  });
  assert.ok(analyzeOut && analyzeOut.includes('SYKI-FAST-2026'), `unexpected analyze output: ${analyzeOut}`);
  console.log('PASS: gemini-media:analyze live');

  // 2. Live TTS test
  selection = 'テスト音声です。サイキの動作確認を行います。';
  noteText = selection;
  const wavOut = path.join(tmpDir, 'speech.wav');
  const ttsPath = await platform.syki.sys.runCommand('gemini-media:tts', {
    text: 'テスト音声です。サイキの動作確認を行います。',
    out: wavOut,
  });
  assert.ok(fs.existsSync(ttsPath), `WAV not found at ${ttsPath}`);
  const wavBuf = fs.readFileSync(ttsPath);
  assert.ok(wavBuf.length > 1000, 'WAV file too small');
  assert.equal(wavBuf.subarray(0, 4).toString('ascii'), 'RIFF', 'Expected RIFF WAV header');
  console.log(`PASS: gemini-media:tts live (${wavBuf.length} bytes)`);

  // 3. Live transcribe test on generated WAV
  selection = ttsPath;
  noteText = ttsPath;
  const transcriptOut = path.join(tmpDir, 'transcript.md');
  const outPath = await platform.syki.sys.runCommand('gemini-media:transcribe', { target: ttsPath, format: 'md', out: transcriptOut });
  assert.ok(fs.existsSync(outPath), `Transcript not found at ${outPath}`);
  const transcript = fs.readFileSync(outPath, 'utf8');
  assert.ok(transcript.trim().length > 0, 'Transcript should not be empty');
  console.log('PASS: gemini-media:transcribe live');

  console.log('ALL 3 LIVE GEMINI-MEDIA E2E CHECKS PASSED');
} finally {
  fs.rmSync(tmpDir, { recursive: true, force: true });
}
