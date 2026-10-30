// HTTP client for the management API. One place owns the panel token, 401
// handling, timeouts and request de-duplication.

const TOKEN_KEY = 'wb-proxy-panel-token';

export const authEvents = new EventTarget();

export const session = {
  get token() { try { return sessionStorage.getItem(TOKEN_KEY) || ''; } catch { return ''; } },
  set(token) { try { token ? sessionStorage.setItem(TOKEN_KEY, token) : sessionStorage.removeItem(TOKEN_KEY); } catch { /* storage blocked */ } },
  clear() { this.set(''); },
};

export class ApiError extends Error {
  constructor(status, message, body) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.body = body;
  }
  get unsupported() { return this.status === 501; }
  // Panel auth answers 401 only; 403 is an ordinary business refusal (e.g. a restricted account).
  get unauthorized() { return this.status === 401; }
}

function messageOf(status, body, text) {
  if (body && typeof body === 'object') {
    const e = body.error;
    if (typeof e === 'string') return e;
    if (e && typeof e.message === 'string') return e.message;
    if (typeof body.message === 'string') return body.message;
    if (typeof body.msg === 'string') return body.msg;
  }
  if (text && text.length < 200 && !text.startsWith('<')) return text;
  return 'HTTP ' + status;
}

async function send(method, path, { body, signal, timeout = method === 'GET' ? 30000 : 120000, headers } = {}) {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(new DOMException('timeout', 'TimeoutError')), timeout);
  if (signal) {
    if (signal.aborted) ctl.abort(signal.reason);
    else signal.addEventListener('abort', () => ctl.abort(signal.reason), { once: true });
  }
  const h = { Accept: 'application/json', ...headers };
  const token = session.token;
  if (token) h['X-Panel-Token'] = token;
  if (body !== undefined) h['Content-Type'] = 'application/json';
  let res;
  try {
    res = await fetch(path, {
      method,
      headers: h,
      body: body !== undefined ? JSON.stringify(body) : undefined,
      // `no-cache` lets the browser revalidate with If-None-Match (usage endpoints send ETags).
      cache: 'no-cache',
      signal: ctl.signal,
    });
  } catch (e) {
    clearTimeout(timer);
    if (e && e.name === 'AbortError' && signal && signal.aborted) throw e;
    if (e && (e.name === 'TimeoutError' || e.name === 'AbortError')) throw new ApiError(0, 'timeout');
    throw new ApiError(0, 'network');
  }
  clearTimeout(timer);
  const text = await res.text();
  let data = null;
  if (text) { try { data = JSON.parse(text); } catch { data = null; } }
  if (!res.ok) {
    const err = new ApiError(res.status, messageOf(res.status, data, text), data);
    if (err.unauthorized && path !== '/panel/login') authEvents.dispatchEvent(new Event('unauthorized'));
    throw err;
  }
  return data === null && text ? { raw: text } : (data ?? {});
}

const inflight = new Map();

/** GET with in-flight de-duplication: concurrent callers share one request. */
export function get(path, opts = {}) {
  if (opts.signal) return send('GET', path, opts);
  const hit = inflight.get(path);
  if (hit) return hit;
  const p = send('GET', path, opts).finally(() => inflight.delete(path));
  inflight.set(path, p);
  return p;
}

export function post(path, body = {}, opts = {}) { return send('POST', path, { ...opts, body }); }

/** Like get(), but resolves `fallback` for endpoints this build answers with 501. */
export async function getOr(path, fallback, opts) {
  try { return await get(path, opts); }
  catch (e) { if (e instanceof ApiError && e.unsupported) return fallback; throw e; }
}

export function qs(params) {
  const q = new URLSearchParams();
  for (const k in params) { const v = params[k]; if (v !== undefined && v !== null && v !== '') q.set(k, v); }
  const s = q.toString();
  return s ? '?' + s : '';
}

/** Authenticated download (a plain <a href> cannot send the token header). */
export async function download(path, filename) {
  const h = {};
  if (session.token) h['X-Panel-Token'] = session.token;
  const res = await fetch(path, { headers: h, cache: 'no-store' });
  if (!res.ok) throw new ApiError(res.status, 'HTTP ' + res.status);
  const blob = await res.blob();
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url; a.download = filename;
  document.body.appendChild(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 4000);
}
