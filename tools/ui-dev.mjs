// Dev helper for the dashboard in assets/web: serves it from disk (no rebuild, no cache)
// exactly like the gateway's /ui handler, and proxies every other request to a running gateway.
//
//   node tools/ui-dev.mjs [listen-port=8790] [gateway-port=8788]
//
// Then open http://127.0.0.1:<listen-port>/ and edit files under assets/web; a reload picks them up.
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const web = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'assets', 'web');
const listen = Number(process.argv[2] || 8790);
const upstream = Number(process.argv[3] || 8788);

// Same order as gateway/src/ui.rs CSS_ORDER; any other css/*.css follows alphabetically.
const CSS_ORDER = ['tokens', 'base', 'layout', 'components', 'pages'];
const MIME = { '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.html': 'text/html; charset=utf-8', '.json': 'application/json; charset=utf-8', '.svg': 'image/svg+xml' };

function cssBundle() {
  const dir = path.join(web, 'css');
  const have = fs.existsSync(dir) ? fs.readdirSync(dir).filter((f) => f.endsWith('.css')) : [];
  const ordered = CSS_ORDER.map((n) => `${n}.css`).filter((f) => have.includes(f));
  const rest = have.filter((f) => !ordered.includes(f)).sort();
  return [...ordered, ...rest].map((f) => fs.readFileSync(path.join(dir, f), 'utf8')).join('\n');
}

http.createServer((req, res) => {
  const { pathname } = new URL(req.url, 'http://localhost');
  const send = (type, body) => { res.writeHead(200, { 'Content-Type': type, 'Cache-Control': 'no-store' }); res.end(body); };
  if (req.method === 'GET') {
    try {
      if (pathname === '/' || pathname === '/dashboard' || pathname === '/ui') {
        return send(MIME['.html'], fs.readFileSync(path.join(web, 'index.html'), 'utf8').replace('__WB_UI_LANGUAGE__', 'zh'));
      }
      if (pathname === '/ui/app.css') return send(MIME['.css'], cssBundle());
      if (pathname.startsWith('/ui/') && !pathname.includes('..')) {
        const file = path.join(web, pathname.slice(4));
        if (fs.existsSync(file) && fs.statSync(file).isFile()) return send(MIME[path.extname(file)] || 'application/octet-stream', fs.readFileSync(file));
        res.writeHead(404); return res.end('not found');
      }
    } catch (e) { res.writeHead(500); return res.end(String(e)); }
  }
  const proxied = http.request({ host: '127.0.0.1', port: upstream, path: req.url, method: req.method, headers: req.headers }, (r) => {
    res.writeHead(r.statusCode, r.headers);
    r.pipe(res);
  });
  proxied.on('error', () => { res.writeHead(502); res.end('gateway not reachable on port ' + upstream); });
  req.pipe(proxied);
}).listen(listen, '127.0.0.1', () => console.log(`dashboard dev server http://127.0.0.1:${listen}  ->  gateway :${upstream}`));
