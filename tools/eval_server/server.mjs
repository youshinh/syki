// Static HTTP server for syki headless-Edge smoke & acceptance evaluations.
// Serves frontend/ untouched and injects window.__DOCSHOT_BOOT + /__eval/mock.js into "/" in memory.
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { buildBoot } from './demo.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
export const REPO = path.resolve(HERE, '..', '..');
const FRONTEND = path.join(REPO, 'frontend');
const MOCK_JS = path.join(HERE, 'mock_backend.js');

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.png': 'image/png',
  '.svg': 'image/svg+xml',
  '.woff2': 'font/woff2',
  '.json': 'application/json; charset=utf-8',
  '.ico': 'image/x-icon',
  '.pdf': 'application/pdf',
};

function demoDiagramSvg() {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="480" height="270" viewBox="0 0 480 270">
<rect width="480" height="270" fill="#f4f3ef"/>
<rect x="24" y="96" width="120" height="78" rx="10" fill="#fff" stroke="#556b2f" stroke-width="3"/>
<rect x="180" y="96" width="120" height="78" rx="10" fill="#fff" stroke="#556b2f" stroke-width="3"/>
<rect x="336" y="96" width="120" height="78" rx="10" fill="#fff" stroke="#556b2f" stroke-width="3"/>
<path d="M144 135h36M300 135h36" stroke="#556b2f" stroke-width="3" fill="none"/>
<path d="M172 127l8 8-8 8M328 127l8 8-8 8" stroke="#556b2f" stroke-width="3" fill="none"/>
<text x="84" y="141" font-family="Segoe UI, sans-serif" font-size="18" text-anchor="middle" fill="#2b2a26">Editor</text>
<text x="240" y="141" font-family="Segoe UI, sans-serif" font-size="18" text-anchor="middle" fill="#2b2a26">Rust Core</text>
<text x="396" y="141" font-family="Segoe UI, sans-serif" font-size="18" text-anchor="middle" fill="#2b2a26">Note</text>
<text x="240" y="40" font-family="Segoe UI, sans-serif" font-size="20" font-weight="bold" text-anchor="middle" fill="#2b2a26">syki</text>
</svg>`;
}

const MINIMAL_PDF = Buffer.from(
  '%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n',
  'ascii',
);

function send(res, status, type, body) {
  res.writeHead(status, { 'Content-Type': type, 'Cache-Control': 'no-store' });
  res.end(body);
}

function loadPerfParams() {
  const p = process.env.SYKI_PERF_PARAMS_FILE || path.join(REPO, 'config', 'perf_params.json');
  try {
    const parsed = JSON.parse(fs.readFileSync(p, 'utf8'));
    return parsed && parsed.params ? parsed.params : parsed;
  } catch {
    return null;
  }
}

export function startServer({ title }) {
  const indexTemplate = () => fs.readFileSync(path.join(FRONTEND, 'index.html'), 'utf8');

  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://127.0.0.1');
    const pathname = decodeURIComponent(url.pathname);

    if (pathname === '/' || pathname === '/index.html') {
      const boot = buildBoot(Object.fromEntries(url.searchParams), title);
      const perfParams = loadPerfParams();
      const perfScript = perfParams
        ? '<script>window.__sykiPreload = Object.assign(window.__sykiPreload || {}, { perf: ' +
          JSON.stringify(perfParams).replace(/</g, '\\u003c') +
          ' });</script>\n'
        : '';
      const inject =
        perfScript +
        '<script>window.__DOCSHOT_BOOT = ' + JSON.stringify(boot).replace(/</g, '\\u003c') + ';</script>\n' +
        '<script src="/__eval/mock.js"></script>\n';
      let html = indexTemplate();
      html = html.replace(/<title>[^<]*<\/title>/, '<title>' + title + '</title>');
      html = html.replace('<head>', '<head>\n' + inject);
      return send(res, 200, MIME['.html'], html);
    }

    if (pathname === '/__eval/mock.js') {
      return send(res, 200, MIME['.js'], fs.readFileSync(MOCK_JS));
    }

    if (pathname === '/api/image' || pathname === '/api/local-asset') {
      return send(res, 200, 'image/svg+xml', demoDiagramSvg());
    }

    if (pathname === '/api/print/preview.pdf') {
      return send(res, 200, MIME['.pdf'], MINIMAL_PDF);
    }

    if (pathname.startsWith('/__eval/')) return send(res, 404, 'text/plain', 'not found');

    const rel = pathname.replace(/^\/+/, '');
    const file = path.resolve(FRONTEND, rel);
    if (!file.startsWith(FRONTEND + path.sep) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
      return send(res, 404, 'text/plain', 'not found');
    }
    const type = MIME[path.extname(file).toLowerCase()] || 'application/octet-stream';
    return send(res, 200, type, fs.readFileSync(file));
  });

  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve({ server, port: server.address().port }));
  });
}
