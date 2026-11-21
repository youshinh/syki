#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { execSync } from 'node:child_process';

const args = process.argv.slice(2);
const opt = (name, def = '') => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] ? args[i + 1] : def;
};

const inFile = opt('--in');
const jsonlFile = opt('--jsonl', 'docs/perf-history.jsonl');
const mdFile = opt('--md', 'docs/PERF_HISTORY.md');
const summaryFile = opt('--summary', '');

if (!inFile || !fs.existsSync(inFile)) {
  console.error('Missing --in <bench.json>');
  process.exit(1);
}

const raw = fs.readFileSync(inFile, 'utf8').replace(/^\uFEFF/, '');
const bench = JSON.parse(raw);

function git(cmd, fallback = 'unknown') {
  try {
    return execSync(cmd, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim() || fallback;
  } catch {
    return fallback;
  }
}

const commit = (process.env.GITHUB_SHA || git('git rev-parse --short HEAD')).slice(0, 7);
const ref = process.env.GITHUB_REF_NAME || git('git rev-parse --abbrev-ref HEAD');
const runner = process.env.RUNNER_OS
  ? `${process.env.RUNNER_OS}-${process.env.RUNNER_ARCH || 'X64'}`
  : `${process.platform}-${process.arch}`;

const entry = {
  timestamp: new Date().toISOString(),
  commit,
  ref,
  runner,
  sykiExeMb: bench.sykiExeMb ?? null,
  sykiCliExeMb: bench.sykiCliExeMb ?? null,
  coldAppReadyMs: bench.coldAppReadyMs ?? null,
  warmAppReadyMedianMs: bench.warmAppReadyMedianMs ?? null,
  attachedPwsMb: bench.attached?.totalPwsMb ?? null,
  attachedWsMb: bench.attached?.totalWsMb ?? null,
  suspendedPwsMb: bench.suspendedLowMem?.totalPwsMb ?? null,
  detachedPwsMb: bench.detached?.totalPwsMb ?? null,
  appOnlyPwsMb: bench.detached?.appPwsMb ?? bench.attached?.appPwsMb ?? null,
  reattachWakeMs: bench.reattachWakeMs ?? null,
};

fs.mkdirSync(path.dirname(path.resolve(jsonlFile)), { recursive: true });
fs.appendFileSync(jsonlFile, JSON.stringify(entry) + '\n', 'utf8');

const rows = fs
  .readFileSync(jsonlFile, 'utf8')
  .split(/\r?\n/)
  .filter(Boolean)
  .map((line) => JSON.parse(line));

const fmt = (v, unit = '') => (v === null || v === undefined ? '-' : `${v}${unit}`);
const delta = (cur, prev, unit = '') => {
  if (cur == null || prev == null) return '';
  const d = Math.round((cur - prev) * 100) / 100;
  return d === 0 ? ' (±0)' : ` (${d > 0 ? '+' : ''}${d}${unit})`;
};

const lines = [
  '# syki-lite Performance & Memory History',
  '',
  'CI / デプロイごとに同一指標で記録した起動速度・省メモリ・バイナリサイズの履歴です。',
  '',
  '| Date (UTC) | Commit | Ref | Runner | Warm `app_ready` (ms) | Cold `app_ready` (ms) | Attached PWS (MB) | Detached PWS (MB) | `syki.exe` (MB) |',
  '|---|---|---|---|---:|---:|---:|---:|---:|',
];

for (let i = rows.length - 1; i >= 0; i--) {
  const r = rows[i];
  const prev = i > 0 ? rows[i - 1] : null;
  const dateStr = String(r.timestamp || '').slice(0, 19).replace('T', ' ');
  lines.push(
    `| ${dateStr} | \`${r.commit}\` | ${r.ref} | ${r.runner} | ${fmt(r.warmAppReadyMedianMs)}${prev ? delta(r.warmAppReadyMedianMs, prev.warmAppReadyMedianMs) : ''} | ${fmt(r.coldAppReadyMs)} | ${fmt(r.attachedPwsMb)}${prev ? delta(r.attachedPwsMb, prev.attachedPwsMb) : ''} | ${fmt(r.detachedPwsMb)} | ${fmt(r.sykiExeMb)} |`
  );
}

const mdContent = lines.join('\n') + '\n';
fs.mkdirSync(path.dirname(path.resolve(mdFile)), { recursive: true });
fs.writeFileSync(mdFile, mdContent, 'utf8');

if (summaryFile) {
  const latest = rows[rows.length - 1];
  const prev = rows.length > 1 ? rows[rows.length - 2] : null;
  const summary = [
    '### syki-lite Deploy Perf & Memory Gate',
    `- **Commit**: \`${latest.commit}\` (${latest.ref})`,
    `- **Warm \`app_ready\`**: **${fmt(latest.warmAppReadyMedianMs, ' ms')}**${prev ? delta(latest.warmAppReadyMedianMs, prev.warmAppReadyMedianMs, ' ms') : ''}`,
    `- **Cold \`app_ready\`**: ${fmt(latest.coldAppReadyMs, ' ms')}`,
    `- **Attached PWS**: **${fmt(latest.attachedPwsMb, ' MB')}**${prev ? delta(latest.attachedPwsMb, prev.attachedPwsMb, ' MB') : ''}`,
    `- **Detached PWS**: ${fmt(latest.detachedPwsMb, ' MB')} (app-only: ${fmt(latest.appOnlyPwsMb, ' MB')})`,
    `- **Binary size**: \`syki.exe\` ${fmt(latest.sykiExeMb, ' MB')} / \`syki-cli.exe\` ${fmt(latest.sykiCliExeMb, ' MB')}`,
    '',
  ].join('\n');
  fs.appendFileSync(summaryFile, summary, 'utf8');
}

console.log(`Recorded perf history entry for ${entry.commit} -> ${jsonlFile}, ${mdFile}`);
