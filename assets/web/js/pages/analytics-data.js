// Data layer of the Analytics page: range maths, request builders, series gap-filling and
// small derived numbers. No DOM here.
//
// Polling friendliness: every request carries explicit `since`/`until` computed on the browser
// clock from calendar boundaries (local midnight, Monday, 1st of month). `until` is the END of the
// period (usually in the future), so the URL and the echoed body stay byte-identical for hours and
// the server's ETag/304 path applies. Rows that arrive later are still inside the window.
import { get, qs } from '../lib/api.js';
import * as F from '../lib/fmt.js';
import { costText } from '../lib/shared.js';

export const DAY = 86400;
export const MAX_BUCKETS = 1500;
const sec = (d) => Math.floor(d.getTime() / 1000);
const pad2 = (n) => String(n).padStart(2, '0');

/* ------------------------------------------------------------ ranges */

export const ymd = (epoch) => {
  const d = new Date(epoch * 1000);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
};

function parseYmd(s) {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s || '');
  return m ? new Date(+m[1], +m[2] - 1, +m[3]) : null;
}

export function customError(custom) {
  const a = parseYmd(custom && custom.from), b = parseYmd(custom && custom.to);
  if (!a || !b) return 'missing';
  if (b < a) return 'order';
  if ((b - a) / 86400000 > 400) return 'long';
  return '';
}

/** A period is [start, end) in epoch seconds; `live` means "now" falls inside it. */
export function resolvePeriod(key, custom, nowMs = Date.now()) {
  const now = new Date(nowMs);
  const y = now.getFullYear(), m = now.getMonth(), d = now.getDate();
  const day = (off) => sec(new Date(y, m, d + off));
  let start, end;
  if (key === 'week') {
    const dow = (now.getDay() + 6) % 7;
    start = day(-dow); end = day(7 - dow);
  } else if (key === 'month') {
    start = sec(new Date(y, m, 1)); end = sec(new Date(y, m + 1, 1));
  } else if (key === 'd3') {
    start = day(-2); end = day(1);
  } else if (key === 'd30') {
    start = day(-29); end = day(1);
  } else if (key === 'custom' && !customError(custom)) {
    const a = parseYmd(custom.from), b = parseYmd(custom.to);
    start = sec(a); end = sec(new Date(b.getFullYear(), b.getMonth(), b.getDate() + 1));
  } else if (key === 'custom') {
    start = day(-6); end = day(1);
  } else {
    key = 'today'; start = day(0); end = day(1);
  }
  return { key, start, end, live: Math.floor(nowMs / 1000) < end };
}

/**
 * The comparison window: the same span one period earlier, cut at the same elapsed time.
 * The cut-off is rounded up to 30 min so the URL (and the request) changes at most twice an hour.
 */
export function prevWindow(p, nowSec) {
  const s = new Date(p.start * 1000);
  const y = s.getFullYear(), m = s.getMonth(), d = s.getDate();
  let ps;
  if (p.key === 'month') ps = sec(new Date(y, m - 1, 1));
  else {
    const days = { today: 1, week: 7, d3: 3, d30: 30 }[p.key] || Math.max(1, Math.round((p.end - p.start) / DAY));
    ps = sec(new Date(y, m, d - days));
  }
  const elapsed = p.live ? Math.ceil(Math.max(0, nowSec - p.start) / 1800) * 1800 : p.end - p.start;
  const pe = Math.min(ps + elapsed, p.start);
  return pe > ps ? { start: ps, end: pe } : null;
}

/** Bucket width for the main chart; keeps the point count between ~24 and ~100-170. */
export function stepFor(p) {
  const span = p.end - p.start;
  if (span <= 1.1 * DAY) return 900;
  if (span <= 3.1 * DAY) return 3600;
  if (span <= 8 * DAY) return 10800;
  if (span <= 190 * DAY) return DAY;
  return 7 * DAY;
}

/* ------------------------------------------------------------ requests */

const win = (w) => ({ realm: 'all', range: 'custom', since: w.start, until: w.end - 1 });

export const fetchRecent = (signal) => get('/usage/recent' + qs({ realm: 'all', limit: 12 }), { signal });
export const fetchAnalytics = (w, signal) => get('/usage/analytics' + qs(win(w)), { signal });
export const fetchSeries = (w, step, signal) => get('/usage/timeseries' + qs({ ...win(w), bucket_seconds: step }), { signal });
export const fetchPerf = (w, signal) => get('/usage/perf' + qs({ ...win(w), sample: w.end - w.start > 3 * DAY ? 10000 : 5000 }), { signal });

/** Activity pattern: last 28 days in hourly buckets; the URL only changes once an hour. */
export function heatWindow(nowSec) {
  const hour = Math.floor(nowSec / 3600) * 3600;
  return { start: hour - 28 * DAY, end: hour + 3600 };
}
export const fetchHeat = (nowSec, signal) => fetchSeries(heatWindow(nowSec), 3600, signal);

/* ------------------------------------------------------------ series */

const SRC = ['requests', 'errors', 'prompt_tokens', 'completion_tokens', 'reasoning_tokens', 'cached_tokens', 'total_tokens', 'credit'];
const DST = ['req', 'err', 'prompt', 'completion', 'reasoning', 'cached', 'total', 'credit'];

/**
 * The Rust series is sparse (empty buckets are omitted). Rebuild a dense grid from the period
 * start to "now", anchored on the server's own bucket phase so the x axis is true time.
 */
export function fillSeries(res, p, nowSec) {
  const step = Math.max(60, Math.round(res.bucket_seconds) || 3600);
  const rows = Array.isArray(res.series) ? res.series : [];
  const anchor = rows.length ? rows[0].at : p.start;
  const s0 = anchor + Math.floor((p.start - anchor) / step) * step;
  const last = Math.min(nowSec, p.end - 1);
  const idx = rows.map((r) => Math.round((r.at - s0) / step));
  let n = Math.max(1, Math.floor((last - s0) / step) + 1);
  for (const i of idx) if (i + 1 > n) n = i + 1;
  n = Math.min(n, MAX_BUCKETS);
  const out = { step, s0, n, at: new Array(n) };
  for (const k of DST) out[k] = new Array(n).fill(0);
  for (let i = 0; i < n; i++) out.at[i] = s0 + i * step;
  rows.forEach((r, j) => {
    const i = idx[j];
    if (i < 0 || i >= n) return;
    for (let c = 0; c < SRC.length; c++) out[DST[c]][i] += r[SRC[c]] || 0;
  });
  return out;
}

export const sum = (a) => { let s = 0; for (let i = 0; i < a.length; i++) s += a[i] || 0; return s; };
export function peak(a) {
  let best = 0, at = -1;
  for (let i = 0; i < a.length; i++) if (a[i] > best) { best = a[i]; at = i; }
  return { value: best, index: at };
}

/** Per-bucket cost is not stored; spread the window total by credit share (tokens if no credits). */
export function allocCost(f, total) {
  const out = new Array(f.n).fill(0);
  if (!(total > 0)) return out;
  const basis = sum(f.credit) > 0 ? f.credit : f.total;
  const all = sum(basis);
  if (!(all > 0)) return out;
  for (let i = 0; i < f.n; i++) out[i] = (basis[i] / all) * total;
  return out;
}

/** Fill gaps in a ratio series with the last known value and scale to 0.1..1 for a sparkline. */
export function rateSpark(values) {
  const known = values.filter((v) => v != null);
  if (known.length < 2) return [];
  let lastV = known[0];
  const lo = Math.min(...known), hi = Math.max(...known);
  const span = hi - lo || 1;
  return values.map((v) => { if (v != null) lastV = v; return 0.1 + ((lastV - lo) / span) * 0.9; });
}

/* ------------------------------------------------------------ metrics */

export const METRICS = ['tokens', 'requests', 'credit', 'cost'];

export function metricOf(stat, metric) {
  if (!stat) return 0;
  switch (metric) {
    case 'requests': return stat.requests || 0;
    case 'credit': return stat.credit || 0;
    case 'cost': return stat.cost_cny || 0;
    default: return stat.total_tokens || 0;
  }
}

export function fmtMetric(metric, v, full = false) {
  if (v == null || !Number.isFinite(v)) return F.DASH;
  switch (metric) {
    case 'cost': return costText(v);
    case 'credit': return full || Math.abs(v) < 1e4 ? F.dec(v, full ? 3 : 1) : F.compact(v);
    case 'requests': return full || v < 1e5 ? F.int(v) : F.compact(v);
    default: return full ? F.int(v) : F.compact(v);
  }
}

/** Full number for a tooltip, only when the compact text hides digits. */
export function fullTip(v, d = 0) {
  return Number.isFinite(v) && Math.abs(v) >= 1000 ? F.dec(v, d) : '';
}

/* ------------------------------------------------------------ misc */

/** Cheap content signature (FNV-1a over the JSON text) used to skip identical renders. */
export function sigOf(obj) {
  const s = JSON.stringify(obj);
  let hsh = 2166136261;
  for (let i = 0; i < s.length; i++) hsh = Math.imul(hsh ^ s.charCodeAt(i), 16777619);
  return s.length + ':' + (hsh >>> 0).toString(36);
}

export const errorRate = (s) => {
  const total = (s.requests || 0) + (s.errors || 0);
  return total > 0 ? ((s.errors || 0) / total) * 100 : null;
};
