// Accounts page: pure logic (derived status, credits shape, filtering, sorting). No DOM here.
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';

add({
  '已停用': 'Disabled', '已停用 · 任务中': 'Disabled · tasks on', '不接 API 流量，仍参与签到 / 任务': 'No API traffic, still runs check-in and tasks',
  '面板手动停用，该账号不参与轮询': 'Disabled in the panel; skipped when picking an account',
  '凭证失效': 'Re-login', '登录凭证已过期或被上游拒绝（401），需要重新登录': 'The login token expired or was rejected upstream (401). Sign in again.',
  '受限 {left}': 'Restricted {left}', '上游限制了该账号（code 11140），到期后自动恢复；期间不会接单': 'Upstream restricted this account (code 11140). It recovers automatically and takes no traffic meanwhile.',
  '保留积分': 'Reserve', '剩余积分 {a} 已不高于保留线 {b}，暂停接单；刷新积分或调低保留线后恢复': 'Remaining credits {a} are at or below the reserve {b}; paused until credits are refreshed or the reserve is lowered',
  '日限额': 'Daily cap', '今日已用 {a} / {b} token，暂停接单（网关本机 0 点恢复）': 'Used {a} / {b} tokens today; paused until the gateway host’s midnight',
  '积分限额': 'Credit cap', '今日已消费 {a} / {b} 积分，仅免费模型可用（网关本机 0 点恢复）': 'Spent {a} / {b} credits today; only free models are served until the gateway host’s midnight',
  '熔断 {left}': 'Breaker {left}', '连续失败 3 次以上触发熔断，账号本身没问题，到点自动恢复': '3+ consecutive failures opened the breaker. The account itself is fine and recovers automatically.',
  '降权 {left}': 'Degraded {left}', '连续 5 次以上未知失败，优先级被压低，到点自动恢复': '5+ consecutive unknown failures lowered its priority. Recovers automatically.',
  '余额不足 {left}': 'No credits {left}', '上游返回 402（积分不足），冷却到本机次日 04:00': 'Upstream answered 402 (insufficient credits); cooling until 04:00 gateway time',
  '冷却 {left}': 'Cooling {left}', '上游临时限流，到点自动恢复': 'Temporarily rate limited upstream; recovers automatically', '连续软限流 {n} 次，冷却时间按 2 倍递增': '{n} soft rate limits in a row; the cooldown doubles each time',
  '可用': 'Ready', '正常，可以接单': 'Healthy and taking traffic',
  '长期有效': 'No expiry', '静态 API Key，没有过期时间': 'Static API key, no expiry', '过期时间未知': 'Expiry unknown', '凭证到期：{t}': 'Token expires: {t}', '已过期': 'Expired', '{n} 天': '{n} d',
  '临期': 'Expiring', '7 天内到期': 'Expires in 7 d', '最近套餐包将于 {d} 到期（剩 {n} 天）': 'Nearest package expires {d} ({n} d left)',
  'WorkBuddy 独立桌面客户端 (WB)': 'WorkBuddy desktop client (WB)', '官方 VSCode 插件 (VSC)': 'Official VSCode extension (VSC)', '官方 CodeBuddy CLI (CLI)': 'Official CodeBuddy CLI (CLI)',
  '未命名': 'Unnamed', '会员 {p}': 'Plan {p}',
});

export const PRODUCTS = [{ id: 'workbuddy', short: 'WB' }, { id: 'vscode', short: 'VSC' }, { id: 'cli', short: 'CLI' }];
export const productOf = (a) => (PRODUCTS.some((p) => p.id === a.product) ? a.product : 'workbuddy');
export const productShort = (id) => (PRODUCTS.find((p) => p.id === id) || PRODUCTS[0]).short;
export function productLabel(id) {
  if (id === 'vscode') return t('官方 VSCode 插件 (VSC)');
  if (id === 'cli') return t('官方 CodeBuddy CLI (CLI)');
  return t('WorkBuddy 独立桌面客户端 (WB)');
}

export const accountName = (a) => a.nickname || a.uid.slice(0, 8) || t('未命名');

/** First visible character of the nickname (or uid) for the avatar chip. */
export function initial(a) {
  const s = (a.nickname || a.uid || '?').trim();
  return (Array.from(s)[0] || '?').toUpperCase();
}

/** Seconds -> "45s" / "29m" / "2h05m" / "1d03h". */
export function leftText(sec) {
  sec = Math.max(0, Math.round(sec));
  if (sec < 60) return sec + 's';
  if (sec < 3600) return Math.floor(sec / 60) + 'm';
  if (sec < 86400) return Math.floor(sec / 3600) + 'h' + String(Math.floor((sec % 3600) / 60)).padStart(2, '0') + 'm';
  return Math.floor(sec / 86400) + 'd' + String(Math.floor((sec % 86400) / 3600)).padStart(2, '0') + 'h';
}

/* ------------------------------------------------------------ status */
const BALANCE_RE = /\b402\b|insufficient credits/i;

/**
 * First matching state wins (spec precedence; restriction and balance cooldown get their own badges).
 * group: ready | limited | expired | disabled  (status filter chips)
 */
export function statusOf(a) {
  const mk = (key, tone, label, tip, group, rank) => ({ key, tone, label, tip, group, rank });
  if (!a.enabled) {
    return mk('disabled', '', a.tasksEnabled ? t('已停用 · 任务中') : t('已停用'),
      a.tasksEnabled ? t('不接 API 流量，仍参与签到 / 任务') : t('面板手动停用，该账号不参与轮询'), 'disabled', 90);
  }
  if (a.tokenExpired || a.expiresIn === 'expired') return mk('expired', 'bad', t('凭证失效'), t('登录凭证已过期或被上游拒绝（401），需要重新登录'), 'expired', 80);
  if (a.restrictedFor > 0) return mk('restricted', 'bad', t('受限 {left}', { left: leftText(a.restrictedFor) }), t('上游限制了该账号（code 11140），到期后自动恢复；期间不会接单'), 'limited', 70);
  if (a.reserveBlocked) {
    const remain = creditsOf(a);
    return mk('reserve', 'warn', t('保留积分'), t('剩余积分 {a} 已不高于保留线 {b}，暂停接单；刷新积分或调低保留线后恢复', { a: F.int(remain ? Math.trunc(remain.remain) : 0), b: F.int(a.reserveCredits) }), 'limited', 60);
  }
  if (a.dailyLimitBlocked) return mk('daily', 'warn', t('日限额'), t('今日已用 {a} / {b} token，暂停接单（网关本机 0 点恢复）', { a: F.int(a.dailyTokensToday), b: F.int(a.dailyTokenLimit) }), 'limited', 50);
  if (a.creditLimitReached) return mk('creditcap', 'warn', t('积分限额'), t('今日已消费 {a} / {b} 积分，仅免费模型可用（网关本机 0 点恢复）', { a: F.fixed(a.dailyCreditsToday || 0, 2), b: F.int(a.dailyCreditLimit) }), 'limited', 40);
  if (a.breakerFor > 0) return mk('breaker', 'bad', t('熔断 {left}', { left: leftText(a.breakerFor) }), t('连续失败 3 次以上触发熔断，账号本身没问题，到点自动恢复'), 'limited', 30);
  if (a.degradeFor > 0) return mk('degrade', 'warn', t('降权 {left}', { left: leftText(a.degradeFor) }), t('连续 5 次以上未知失败，优先级被压低，到点自动恢复'), 'limited', 20);
  if (a.inCooldown) {
    if (BALANCE_RE.test(a.lastError || '')) return mk('balance', 'warn', t('余额不足 {left}', { left: leftText(a.cooldownFor) }), t('上游返回 402（积分不足），冷却到本机次日 04:00'), 'limited', 15);
    const tip = a.softStreak > 1 ? t('连续软限流 {n} 次，冷却时间按 2 倍递增', { n: a.softStreak }) : t('上游临时限流，到点自动恢复');
    return mk('cooldown', 'warn', t('冷却 {left}', { left: leftText(a.cooldownFor) }), tip, 'limited', 10);
  }
  return mk('ready', 'good', t('可用'), t('正常，可以接单'), 'ready', 0);
}

/* ----------------------------------------------------------- credits */
const num = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : null);

/** Normalised credits snapshot or null (never queried). Tolerates both `size` and `total`. */
export function creditsOf(a) {
  const c = a.credits;
  if (!c || typeof c !== 'object') return null;
  const remain = num(c.remain);
  if (remain == null) return null;
  const size = num(c.size) ?? num(c.total);
  const used = num(c.used) ?? (size != null ? Math.max(0, size - remain) : null);
  const ee = c.earliest_expiring && typeof c.earliest_expiring === 'object' ? c.earliest_expiring : null;
  return {
    remain, size, used,
    ratio: size != null && size > 0 ? Math.max(0, Math.min(1, remain / size)) : null,
    updatedAt: num(c.updated_at) ?? num(c.updatedAt),
    expDays: ee ? num(ee.days_left) : null,
    expEnd: ee ? ee.cycle_end_time : null,
    expRemain: ee ? num(ee.remain) : null,
  };
}

/** Badge for packages about to expire: { tone, label, tip } or null. */
export function expiringOf(a) {
  const c = creditsOf(a);
  if (!c || c.expDays == null) return null;
  const d = c.expDays, end = c.expEnd ? String(c.expEnd).slice(5, 10) : '';
  if (d >= 0 && d <= 3) return { tone: 'bad', label: t('临期'), tip: t('最近套餐包将于 {d} 到期（剩 {n} 天）', { d: end, n: F.dec(d, 1) }) };
  if (d > 3 && d <= 7) return { tone: 'warn', label: t('7 天内到期'), tip: t('最近套餐包将于 {d} 到期（剩 {n} 天）', { d: end, n: Math.round(d) }) };
  return null;
}

export function tokenExpiry(a, nowSec = Date.now() / 1000) {
  if (!a.expiresAt || a.expiresAt <= 0) return { text: '—', tone: '', tip: a.apiKey ? t('静态 API Key，没有过期时间') : t('过期时间未知') };
  const left = a.expiresAt - nowSec;
  const tip = t('凭证到期：{t}', { t: F.dateTime(a.expiresAt) });
  if (left <= 0 || a.tokenExpired) return { text: t('已过期'), tone: 'bad', tip };
  return { text: left >= 172800 ? t('{n} 天', { n: Math.floor(left / 86400) }) : leftText(left), tone: left < 6 * 3600 ? 'warn' : '', tip };
}

/** Error chip data: omitted for empty errors and for model-level 429s (those show as cooldown pills). */
export function errorOf(a) {
  const e = a.lastError;
  if (!e || e === 'HTTP 429 (model throttled)') return null;
  const full = a.lastErrorDetail || e;
  const code = /"code":(\d+)/.exec(full);
  const label = (e.length > 120 ? e.slice(0, 120) + '…' : e) + (code && !e.includes('code') ? ' · code:' + code[1] : '');
  return { label, tip: full };
}

/* --------------------------------------------------- filter / search */
export const FILTERS = ['all', 'ready', 'limited', 'expired', 'disabled'];

export function countGroups(list) {
  const out = { all: list.length, ready: 0, limited: 0, expired: 0, disabled: 0 };
  for (const a of list) out[statusOf(a).group]++;
  return out;
}

export function matchQuery(a, q) {
  return (a.nickname || '').toLowerCase().includes(q) || a.uid.toLowerCase().includes(q) || (a.note || '').toLowerCase().includes(q);
}

export function summarize(list) {
  const s = { total: list.length, enabled: 0, ready: 0, expired: 0, remain: 0, size: 0, withCredits: 0, missing: 0 };
  for (const a of list) {
    if (a.enabled) s.enabled++;
    const g = statusOf(a);
    if (g.key === 'ready') s.ready++;
    if (g.key === 'expired') s.expired++;
    const c = creditsOf(a);
    if (c) { s.remain += c.remain; s.size += c.size || 0; s.withCredits++; } else s.missing++;
  }
  return s;
}

/* --------------------------------------------------------------- sort */
const GET = {
  name: (a) => (a.nickname || a.uid).toLowerCase(),
  status: (a) => statusOf(a).rank,
  credits: (a) => { const c = creditsOf(a); return c ? c.remain : null; },
  creditExp: (a) => { const c = creditsOf(a); return c ? c.expDays : null; },
  tokenExp: (a) => (a.expiresAt > 0 ? a.expiresAt : null),
  weight: (a) => a.weight || 1,
  usage: (a) => (typeof a.dailyTokensToday === 'number' ? a.dailyTokensToday : null),
  added: (a) => (typeof a.addedAt === 'number' ? a.addedAt : null),
};
export const SORT_KEYS = ['default', 'name', 'status', 'credits', 'creditExp', 'tokenExp', 'weight', 'usage', 'added'];
export const SORT_DIR = { name: 'asc', status: 'desc', credits: 'desc', creditExp: 'asc', tokenExp: 'asc', weight: 'desc', usage: 'desc', added: 'desc' };

/** Stable sort; blanks always sink. `default` = enabled first, higher weight first, then server order. */
export function sortAccounts(list, key, dir) {
  const rows = list.map((a, i) => ({ a, i }));
  if (key === 'default' || !GET[key]) {
    rows.sort((x, y) => (Number(y.a.enabled) - Number(x.a.enabled)) || ((y.a.weight || 1) - (x.a.weight || 1)) || (x.i - y.i));
    return rows.map((r) => r.a);
  }
  const get = GET[key], m = dir === 'asc' ? 1 : -1;
  for (const r of rows) r.v = get(r.a);
  rows.sort((x, y) => {
    if (x.v == null && y.v == null) return x.i - y.i;
    if (x.v == null) return 1;
    if (y.v == null) return -1;
    const d = typeof x.v === 'string' ? x.v.localeCompare(y.v, undefined, { numeric: true }) : x.v - y.v;
    return d * m || x.i - y.i;
  });
  return rows.map((r) => r.a);
}

/** Signature of everything the list renders for one account (poll diffing). */
export function rowSig(a) {
  const c = a.credits, ee = c && c.earliest_expiring;
  return [a.nickname, a.note, a.enabled, a.tasksEnabled, a.weight, a.product, a.realm, a.apiKey, a.tokenExpired, a.expiresAt, a.restrictedFor,
    a.reserveBlocked, a.dailyLimitBlocked, a.creditLimitReached, a.breakerFor, a.degradeFor, a.inCooldown, a.cooldownFor, a.softStreak, a.inFlight,
    a.lastError, a.hasProxy, a.proxySlot, a.hasRefreshToken, a.dailyTokensToday, a.dailyCreditsToday, a.dailyTokenLimit, a.dailyCreditLimit, a.reserveCredits,
    a.modelCooldowns && a.modelCooldowns.length, a.disabledModels && a.disabledModels.length, c && c.remain, c && (c.size ?? c.total),
    ee && ee.days_left, a.tier && a.tier.package, a.tier && a.tier.paid].join('|');
}
