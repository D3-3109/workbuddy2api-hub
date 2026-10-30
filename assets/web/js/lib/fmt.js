// Number / time formatting. Formatters are created once and reused.
import { locale } from './i18n.js';

const cache = new Map();
function nf(opts) {
  const key = locale() + JSON.stringify(opts);
  let f = cache.get(key);
  if (!f) { f = new Intl.NumberFormat(locale(), opts); cache.set(key, f); }
  return f;
}
function df(opts) {
  const key = 'd' + locale() + JSON.stringify(opts);
  let f = cache.get(key);
  if (!f) { f = new Intl.DateTimeFormat(locale(), opts); cache.set(key, f); }
  return f;
}

const isNum = (n) => typeof n === 'number' && Number.isFinite(n);
export const DASH = '—';

export function int(n) { return isNum(n) ? nf({ maximumFractionDigits: 0 }).format(n) : DASH; }
export function dec(n, d = 1) { return isNum(n) ? nf({ minimumFractionDigits: 0, maximumFractionDigits: d }).format(n) : DASH; }
export function fixed(n, d = 2) { return isNum(n) ? nf({ minimumFractionDigits: d, maximumFractionDigits: d }).format(n) : DASH; }

/** 12,345 -> 12.3K ; 3,400,000 -> 3.4M */
export function compact(n, d = 1) {
  if (!isNum(n)) return DASH;
  const a = Math.abs(n);
  if (a < 1000) return nf({ maximumFractionDigits: a < 10 && n % 1 ? 1 : 0 }).format(n);
  return nf({ notation: 'compact', maximumFractionDigits: d }).format(n);
}

export function pct(v, d = 1) { return isNum(v) ? nf({ minimumFractionDigits: 0, maximumFractionDigits: d }).format(v) + '%' : DASH; }
export function ratio(a, b, d = 1) { return b > 0 ? pct((a / b) * 100, d) : DASH; }
export function money(n, cur = '¥') { return isNum(n) ? cur + nf({ minimumFractionDigits: 2, maximumFractionDigits: 2 }).format(n) : DASH; }

export function ms(v) {
  if (!isNum(v)) return DASH;
  if (v < 1000) return Math.round(v) + ' ms';
  if (v < 60000) return (v / 1000).toFixed(v < 10000 ? 2 : 1) + ' s';
  return Math.floor(v / 60000) + ' m ' + Math.round((v % 60000) / 1000) + ' s';
}

/** Seconds -> 2h 05m / 45s */
export function span(sec) {
  if (!isNum(sec) || sec < 0) return DASH;
  sec = Math.round(sec);
  if (sec < 60) return sec + 's';
  if (sec < 3600) return Math.floor(sec / 60) + 'm' + (sec % 60 ? String(sec % 60).padStart(2, '0') + 's' : '');
  if (sec < 86400) return Math.floor(sec / 3600) + 'h' + String(Math.floor((sec % 3600) / 60)).padStart(2, '0') + 'm';
  return Math.floor(sec / 86400) + 'd' + String(Math.floor((sec % 86400) / 3600)).padStart(2, '0') + 'h';
}

export function bytes(n) {
  if (!isNum(n)) return DASH;
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return (i ? n.toFixed(n < 10 ? 2 : 1) : n) + ' ' + u[i];
}

const toDate = (epoch) => new Date(epoch > 1e11 ? epoch : epoch * 1000);
export function time(epoch) { return isNum(epoch) && epoch > 0 ? df({ hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false }).format(toDate(epoch)) : DASH; }
export function dateTime(epoch) { return isNum(epoch) && epoch > 0 ? df({ year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false }).format(toDate(epoch)) : DASH; }
export function date(epoch) { return isNum(epoch) && epoch > 0 ? df({ year: 'numeric', month: '2-digit', day: '2-digit' }).format(toDate(epoch)) : DASH; }
export function mdHm(epoch) { return isNum(epoch) && epoch > 0 ? df({ month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false }).format(toDate(epoch)) : DASH; }
export function md(epoch) { return isNum(epoch) ? df({ month: 'numeric', day: 'numeric' }).format(toDate(epoch)) : DASH; }
export function hm(epoch) { return isNum(epoch) ? df({ hour: '2-digit', minute: '2-digit', hour12: false }).format(toDate(epoch)) : DASH; }
export function weekday(i) { return df({ weekday: 'short' }).format(new Date(2024, 0, 1 + i)); } // 0 = Monday

/** "3 分钟前" style. `tr` is i18n.t so units follow the UI language. */
export function ago(epoch, tr) {
  if (!isNum(epoch) || epoch <= 0) return DASH;
  const d = Math.max(0, Date.now() / 1000 - (epoch > 1e11 ? epoch / 1000 : epoch));
  if (d < 45) return tr('刚刚');
  if (d < 3600) return tr('{n} 分钟前', { n: Math.max(1, Math.round(d / 60)) });
  if (d < 86400) return tr('{n} 小时前', { n: Math.round(d / 3600) });
  return tr('{n} 天前', { n: Math.round(d / 86400) });
}

/** Start of the local day (epoch seconds). */
export function dayStart(offsetDays = 0) {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  d.setDate(d.getDate() + offsetDays);
  return Math.floor(d.getTime() / 1000);
}
