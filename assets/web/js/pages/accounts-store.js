// Accounts page: in-memory account store and the few request helpers that the shared client cannot do.
import { session, authEvents, ApiError, get, post, getOr, qs } from '../lib/api.js';
import { rowSig } from './accounts-model.js';

/**
 * Store of the viewed realm's accounts. A poll that started before a local edit must not
 * overwrite that edit, so `touched` remembers when each uid was last changed by the user.
 */
export function createStore() {
  let list = [];
  let version = 0;
  const byUid = new Map();
  const touched = new Map();
  const subs = new Set();
  const sigs = new WeakMap();
  const bump = () => { version++; for (const fn of subs) fn(); };

  const store = {
    get list() { return list; },
    get version() { return version; },
    get: (uid) => byUid.get(uid),
    sig(a) { let s = sigs.get(a); if (s === undefined) { s = rowSig(a); sigs.set(a, s); } return s; },
    subscribe(fn) { subs.add(fn); return () => subs.delete(fn); },

    /** Result of a poll that began at `startedAt` (ms). */
    replaceAll(accounts, startedAt) {
      const next = [];
      for (const a of accounts) {
        const newer = (touched.get(a.uid) || 0) > startedAt;
        const mine = byUid.get(a.uid);
        if (newer && !mine) continue;        // deleted locally while the poll was in flight
        next.push(newer ? mine : a);
      }
      const cutoff = Date.now() - 60000;
      for (const [uid, at] of touched) if (at < cutoff) touched.delete(uid);
      // a locally created account that the stale poll does not know yet stays
      for (const [uid, at] of touched) {
        if (at > startedAt && byUid.has(uid) && !next.some((a) => a.uid === uid)) next.push(byUid.get(uid));
      }
      byUid.clear();
      for (const a of next) byUid.set(a.uid, a);
      list = next;
      bump();
    },
    /** Replace one account with the server's view (or insert it). */
    put(account) {
      if (!account || !account.uid) return;
      touched.set(account.uid, Date.now());
      const i = list.findIndex((a) => a.uid === account.uid);
      list = i < 0 ? [...list, account] : list.map((a, j) => (j === i ? account : a));
      byUid.set(account.uid, account);
      bump();
    },
    /** Shallow local change (optimistic edits, partial results). */
    merge(uid, patch) {
      const cur = byUid.get(uid);
      if (cur) store.put({ ...cur, ...patch });
    },
    remove(uid) {
      touched.set(uid, Date.now());
      if (!byUid.delete(uid)) return;
      list = list.filter((a) => a.uid !== uid);
      bump();
    },
  };
  return store;
}

/* ------------------------------------------------------------ requests */
/**
 * Like post() but non-2xx answers resolve as data. Only 401 means "session lost" here;
 * 403 on /accounts/test or /accounts/add-api-key is a business answer (restricted account).
 */
export async function loose(method, path, body, { timeout = 130000 } = {}) {
  const headers = { Accept: 'application/json' };
  if (session.token) headers['X-Panel-Token'] = session.token;
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), timeout);
  let res;
  try {
    res = await fetch(path, { method, headers, body: body !== undefined ? JSON.stringify(body) : undefined, cache: 'no-store', signal: ctl.signal });
  } catch (e) {
    throw new ApiError(0, e && e.name === 'AbortError' ? 'timeout' : 'network');
  } finally { clearTimeout(timer); }
  const text = await res.text();
  let data = {};
  try { data = text ? JSON.parse(text) : {}; } catch { data = {}; }
  if (res.status === 401) authEvents.dispatchEvent(new Event('unauthorized'));
  return { status: res.status, ok: res.ok, data };
}

/** Message from error shape A ({error:{message}}), B ({error:"..."}) or a plain message. */
export function bodyError(data, status) {
  const e = data && data.error;
  if (typeof e === 'string' && e) return e;
  if (e && typeof e.message === 'string') return e.message;
  if (data && typeof data.message === 'string') return data.message;
  return 'HTTP ' + status;
}

export const api = {
  list: (realm, signal) => get('/accounts' + qs({ realm }), { signal }),
  slots: () => getOr('/proxy/slots', { slots: [] }),
  set: (uid, patch) => post('/accounts/set', { uid, ...patch }),
  product: (uid, product) => post('/accounts/product', { uid, product }),
  remove: (uid) => post('/accounts/delete', { uid }),
  refresh: (uid) => post('/accounts/refresh', { uid }),
  credits: (uid) => post('/accounts/credits', { uid }),
  creditsDetail: (uid, refresh) => post('/accounts/credits/detail', { uid, refresh }),
  test: (uid) => loose('POST', '/accounts/test', { uid }),
};
