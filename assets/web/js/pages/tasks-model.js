// Pure helpers for the Tasks page: eligibility, today's status, task/travel presentation.
import { add, t } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { setAttr } from '../lib/dom.js';
import { badge } from '../lib/ui.js';

add({
  '未知': 'Unknown', '已停用': 'Disabled', '需重新登录': 'Need re-login', '令牌已过期': 'Token expired', '风控 {s}': 'Restricted {s}',
  '请先选择账号': 'Select an account first', '每日活跃仅适用于国际版账号': 'Daily activity is for global-region accounts only',
  '仅适用于国内版账号': 'For China-region accounts only', '企业版或 API Key 账号没有成长体系': 'Enterprise and API-key accounts have no growth system',
  '该账号未开启任务，请在账号页启用': 'Tasks are off for this account; turn them on in the accounts page',
  '打卡成功': 'Checked in', '失败': 'Failed', '已完成': 'Done', '无记录': 'No record', '已领': 'Claimed', '未领': 'Not claimed',
  '今天还没有记录（没有记录不等于失败）': 'Nothing recorded today yet (no record is not a failure)',
  '已领取': 'Claimed', '待领奖': 'Ready to claim', '进行中': 'In progress', '未接取': 'Not accepted',
  '+{n} 积分': '+{n} credits', '+{n} 能量': '+{n} energy',
  '旅行中': 'Traveling', '今日已完成': 'Done today', '在家待命': 'At home', '次日 00:00 刷新': 'Resets at 00:00', '旅行状态查询失败': 'Travel status unavailable',
  '奖励 +{n} 积分': 'Reward +{n} credits',
  '可签到': 'Can check in', '不可签到': 'No check-in', '可做成长任务': 'Growth tasks on', '无成长体系': 'No growth system',
  '任务已开启': 'Tasks enabled', '任务未开启': 'Tasks off', '可每日活跃': 'Can chat daily', '不可每日活跃': 'No daily chat',
  '定时': 'Scheduled', '手动': 'Manual', '添加账号时': 'When account added', '导入': 'Imported',
  '账号或区域风控暂停中，剩余 {s}': 'Account or region is paused by risk control, {s} left',
});

export const shortUid = (uid) => String(uid || '').slice(0, 8);
export const realmOf = (a) => (a && a.realm === 'cn' ? 'cn' : 'intl');
/** Same rule the old UI used: tasksEnabled when present, else enabled. */
export const taskActive = (a) => (a.tasksEnabled != null ? !!a.tasksEnabled : !!a.enabled);

const pad = (n) => String(n).padStart(2, '0');
export function localDay(d = new Date()) { return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`; }
const isToday = (stamp, day) => typeof stamp === 'string' && stamp.startsWith(day);
/** "10-31 09:00" from either `2026-10-31T09:00:00+08:00` or `2026-10-31 09:00:00` (no time-zone conversion, as recorded). */
export const stampShort = (ts) => String(ts || '').replace('T', ' ').slice(5, 16);

/* ---------------- eligibility ---------------- */
const no = (why) => ({ ok: false, why });
const YES = { ok: true, why: '' };

/** can('dailyChat' | 'checkin' | 'growth', account) -> { ok, why } */
export function can(action, a) {
  if (!a) return no(t('请先选择账号'));
  if (action === 'dailyChat') {
    if (realmOf(a) !== 'intl' || !a.canDailyChat) return no(t('每日活跃仅适用于国际版账号'));
  } else {
    if (realmOf(a) !== 'cn') return no(t('仅适用于国内版账号'));
    if (!(action === 'checkin' ? a.canCheckin : a.canReportActivity)) return no(t('企业版或 API Key 账号没有成长体系'));
  }
  return taskActive(a) ? YES : no(t('该账号未开启任务，请在账号页启用'));
}

/** Disable an action button while another action runs or when the verdict says it cannot apply; the tooltip says why. */
export function gate(el, store, verdict, tip) {
  const busy = store.busy();
  const off = !verdict.ok || (!!busy && busy.el !== el);
  if (el.disabled !== off) el.disabled = off;
  setAttr(el, 'data-tip', !verdict.ok ? verdict.why : tip || null);
}

/** Accounts of the viewed realm and the subset the picker targets. */
export function scopeOf(view, accounts, sel) {
  const list = accounts.filter((a) => realmOf(a) === view);
  const target = sel === 'all' ? list : list.filter((a) => a.uid === sel);
  return { list, target };
}

/** Eligibility badges for one account (what it can do and whether tasks are on). */
export function eligibilityBadges(view, a) {
  const yes = (text, tip) => badge(text, 'good', { dot: true, tip });
  const off = (text, tip) => badge(text, '', { dot: true, outline: true, tip });
  const out = [];
  if (view === 'cn') {
    out.push(a.canCheckin ? yes(t('可签到')) : off(t('不可签到'), t('企业版或 API Key 账号没有成长体系')));
    out.push(a.canReportActivity ? yes(t('可做成长任务')) : off(t('无成长体系'), t('企业版或 API Key 账号没有成长体系')));
  } else {
    out.push(a.canDailyChat ? yes(t('可每日活跃')) : off(t('不可每日活跃')));
  }
  out.push(taskActive(a) ? yes(t('任务已开启')) : badge(t('任务未开启'), 'warn', { dot: true, tip: t('该账号未开启任务，请在账号页启用') }));
  return out;
}

/** Account health flags (disabled, relogin, restriction countdown rounded to the minute). */
export function flagBadges(a) {
  const out = [];
  if (a.enabled === false) out.push(badge(t('已停用'), '', { sm: true, outline: true }));
  if (a.needRelogin) out.push(badge(t('需重新登录'), 'bad', { sm: true }));
  else if (a.tokenExpired) out.push(badge(t('令牌已过期'), 'warn', { sm: true }));
  if (a.restrictedFor > 0) out.push(badge(t('风控 {s}', { s: F.span(Math.ceil(a.restrictedFor / 60) * 60) }), 'warn', { sm: true }));
  return out;
}

/* ---------------- today's status ---------------- */
/** uid -> { checkin: row, daily_chat: row }, first (= newest) row per task. Rows come newest first. */
export function todayIndex(rows) {
  const m = new Map();
  for (const r of rows) {
    if (!r.uid || !r.task) continue;
    let e = m.get(r.uid);
    if (!e) { e = {}; m.set(r.uid, e); }
    if (!e[r.task]) e[r.task] = r;
  }
  return m;
}

/** The newest same-day event decides; the account's last* marker only back-fills when there is no event. */
export function todayStatus(realm, a, index, day) {
  const ev = (index.get(a.uid) || {})[realm === 'cn' ? 'checkin' : 'daily_chat'];
  if (ev) return { key: ev.ok ? 'ok' : 'failed', ts: ev.ts, msg: ev.message };
  const marker = realm === 'cn' ? a.lastCheckin : a.lastDailyChat;
  if (isToday(marker, day)) return { key: 'done', ts: marker };
  return { key: 'none' };
}
export const isDone = (st) => st.key === 'ok' || st.key === 'done';

export function statusBadge(realm, st) {
  const cn = realm === 'cn';
  let el;
  if (st.key === 'ok') el = badge(cn ? t('已领') : t('打卡成功'), 'good', { dot: true });
  else if (st.key === 'done') el = badge(cn ? t('已领') : t('已完成'), 'good', { dot: true });
  else if (st.key === 'failed') el = badge(t('失败'), 'bad', { dot: true });
  else el = badge(cn ? t('未领') : t('无记录'), '', { dot: true });
  const tip = st.key === 'none' ? t('今天还没有记录（没有记录不等于失败）') : [stampShort(st.ts), st.msg].filter(Boolean).join(' · ');
  if (tip) el.setAttribute('data-tip', tip);
  return el;
}

/* ---------------- growth tasks ---------------- */
const toNum = (v) => { if (v == null || v === '') return null; const n = Number(v); return Number.isFinite(n) ? n : null; };
const RANK = { claimable: 0, doing: 1, todo: 2, other: 3, claimed: 4 };

export function taskView(tk) {
  const cur = toNum(tk.current) ?? 0;
  const target = toNum(tk.target) ?? 1;
  const done = tk.status === 'claimed' || tk.status === 'completed';
  const reached = done || (target > 0 && cur >= target);
  let key, label, kind;
  if (tk.status === 'claimed') { key = 'claimed'; label = t('已领取'); kind = ''; }
  else if (reached) { key = 'claimable'; label = t('待领奖'); kind = 'good'; }
  else if (tk.status === 'accepted' || tk.status === 'in_progress') { key = 'doing'; label = t('进行中'); kind = 'info'; }
  else if (tk.status === 'not_accepted') { key = 'todo'; label = t('未接取'); kind = 'warn'; }
  else { key = 'other'; label = String(tk.status || '—'); kind = 'outline'; }
  const pct = done ? 100 : target > 0 ? Math.max(0, Math.min(100, Math.round((cur / target) * 100))) : 0;
  return { key, label, kind, cur, target, done, pct, rank: RANK[key], claimable: key === 'claimable' };
}

/** Upstream reward fields are raw (number, string or null): show positive numbers and non-empty text. */
function amount(v) {
  const n = toNum(v);
  if (n != null) return n > 0 ? F.dec(n, 2) : null;
  return typeof v === 'string' && v.trim() ? v.trim() : null;
}
export function rewardTexts(tk) {
  const credit = amount(tk.reward_credit), energy = amount(tk.reward_energy);
  const out = [];
  if (credit) out.push({ kind: 'credit', text: t('+{n} 积分', { n: credit }) });
  if (energy) out.push({ kind: 'energy', text: t('+{n} 能量', { n: energy }) });
  return out;
}
export const num = toNum;

export function catState(travel) {
  const s = travel && travel.state;
  if (s === 'arrived') {
    const r = amount(travel.reward_credit);
    return { label: t('待领奖'), kind: 'good', desc: r ? t('奖励 +{n} 积分', { n: r }) : '' };
  }
  if (s === 'traveling') return { label: t('旅行中'), kind: 'info', desc: '' };
  if (travel && travel.daily_limit_reached) return { label: t('今日已完成'), kind: '', desc: t('次日 00:00 刷新') };
  if (!s || s === 'unknown') return { label: t('未知'), kind: '', desc: t('旅行状态查询失败') };
  return { label: t('在家待命'), kind: '', desc: '' };
}

/** Show a restriction message with a live remaining time (returns seconds or null). */
export function restrictionSeconds(msg) {
  const m = /剩余\s*(\d+)\s*s/.exec(String(msg || ''));
  return m ? Number(m[1]) : null;
}
export function restrictionText(seconds) { return t('账号或区域风控暂停中，剩余 {s}', { s: F.span(seconds) }); }

export const TRIGGER_LABEL = () => ({ scheduler: t('定时'), manual: t('手动'), account_add: t('添加账号时'), account_import: t('导入'), unknown: t('未知') });
