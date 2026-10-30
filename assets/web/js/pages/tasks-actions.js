// Task actions. The runner lives at module level so a request that takes minutes (many accounts,
// no server-side job id) keeps its "running" state and its result across page re-mounts.
// Every definition turns the server reply into { kind, toast, lines } and never treats ok:false as success.
import { add, t } from '../lib/i18n.js';
import { post } from '../lib/api.js';
import { toast, errText } from '../lib/ui.js';
import * as F from '../lib/fmt.js';
import { shortUid, num } from './tasks-model.js';

add({
  '每日活跃打卡': 'Daily activity check-in', '未活跃账号打卡': 'Check-in for pending accounts', '网页通道打卡': 'Web-channel check-in',
  'Buddy 加油站签到': 'Buddy gas-station check-in', '接取并领取已完成任务': 'Accept and claim tasks',
  '猫猫旅行': 'Cat travel', '连登管家与抽奖': 'Streak butler and lottery', '领取任务奖励': 'Claim task reward',
  '已跳过': 'Skipped', '今日活跃已完成，跳过本轮': 'Already active today, skipped', '签到成功': 'Checked in', '今日已签到': 'Already checked in', '待确认': 'Unconfirmed',
  '已领奖': 'Claimed', '已出发': 'Departed', '已执行': 'Done', '无需操作': 'Nothing to do',
  '{n} 个成功': '{n} succeeded', '{n} 个无需操作': '{n} need nothing', '{n} 个待确认': '{n} unconfirmed', '{n} 个失败': '{n} failed',
  '{label}（{name}）：{text}': '{label} ({name}): {text}', '{label}：{detail}': '{label}: {detail}',
  '{summary}；首个失败：{name}：{text}': '{summary}. First failure: {name}: {text}',
  '没有可打卡的国际版账号': 'No global-region accounts to check in', '没有可签到的国内版账号（企业版和 API Key 账号会被跳过）': 'No China-region accounts to check in (enterprise and API-key accounts are skipped)',
  '没有可操作的国内版账号': 'No China-region accounts to act on', '任务执行失败': 'Task run failed',
  '{n} 个账号已处理，新增 {c} 积分': 'Accounts processed: {n}, credits added: +{c}', '{n} 个账号已处理，新增 {c} 积分；部分步骤失败，详见下方结果': 'Accounts processed: {n}, credits added: +{c}. Some steps failed, see the result below',
  '领取失败：{msg}': 'Claim failed: {msg}', '领取成功': 'Claimed', '领取成功，+{n} 积分': 'Claimed, +{n} credits',
  '连登状态正常（无需兑换/无抽奖次数）': 'Streak is fine (nothing to redeem, no lottery chances)',
});

const LONG = { timeout: 30 * 60 * 1000 };
const FAIL_RE = /风控|没有可用凭证|凭证刷新失败|失败|错误|暂停|无法|超时/;

let running = null; // { label, realm, el }
let last = null;    // last result { kind, toast, lines, label, realm, at }
let sink = null;    // the mounted page: { onBusy(), afterAction(result) }

export const runState = () => running;
export const lastResult = () => last;
export function attach(page) { sink = page; return () => { if (sink === page) sink = null; }; }

/** Run one action exclusively. Resolves to the result, or null if another action is running or the session ended. */
export async function run(def, el) {
  if (running) return null;
  running = { label: def.label, realm: def.realm, el: el || null };
  if (sink) sink.onBusy();
  let res;
  let raw = null;
  try {
    raw = await def.request();
    res = def.interpret(raw || {});
  } catch (e) {
    if (e && e.unauthorized) { running = null; if (sink) sink.onBusy(); return null; }
    const msg = errText(e);
    res = { kind: 'bad', toast: msg, lines: [{ kind: 'bad', text: msg }] };
  }
  res.label = def.label;
  res.realm = def.realm;
  res.at = Date.now();
  res.accounts = raw && Array.isArray(raw.accounts) ? raw.accounts : null;
  last = res;
  running = null;
  toast(res.toast, res.kind, res.kind === 'bad' || res.kind === 'warn' ? 6500 : 4200);
  if (sink) { sink.afterAction(res); sink.onBusy(); }
  return res;
}

/* ---------------- reply -> report ---------------- */
const who = (r) => r.nickname || shortUid(r.uid);
const body = (uid, extra) => ({ ...(uid && uid !== 'all' ? { uid: String(uid) } : {}), ...extra });

function rowsReport(label, items, emptyText) {
  if (!items.length) return { kind: 'warn', toast: emptyText, lines: [] };
  const count = (k) => items.filter((x) => x.kind === k).length;
  const ok = count('ok'), info = count('info'), warn = count('warn'), bad = count('bad');
  const kind = bad ? (ok || info || warn ? 'warn' : 'bad') : warn ? 'warn' : ok ? 'ok' : 'info';
  let msg;
  if (items.length === 1) {
    const x = items[0];
    msg = t('{label}（{name}）：{text}', { label, name: x.name, text: x.text || x.tag });
  } else {
    const parts = [];
    if (ok) parts.push(t('{n} 个成功', { n: ok }));
    if (info) parts.push(t('{n} 个无需操作', { n: info }));
    if (warn) parts.push(t('{n} 个待确认', { n: warn }));
    if (bad) parts.push(t('{n} 个失败', { n: bad }));
    msg = t('{label}：{detail}', { label, detail: parts.join(' · ') });
    const first = items.find((x) => x.kind === 'bad');
    if (first) msg = t('{summary}；首个失败：{name}：{text}', { summary: msg, name: first.name, text: first.text || first.tag });
  }
  return { kind, toast: msg, lines: items };
}

const failed = (msg) => ({ kind: 'warn', toast: msg, lines: [{ kind: 'bad', text: msg }] });

function chatItem(r) {
  if (r.skipped) return { name: who(r), kind: 'info', tag: t('已跳过'), text: r.msg || t('今日活跃已完成，跳过本轮') };
  if (r.ok) return { name: who(r), kind: 'ok', tag: t('打卡成功'), text: r.msg || '' };
  return { name: who(r), kind: 'bad', tag: t('失败'), text: r.error || r.msg || t('未知错误') };
}

function checkinItem(r) {
  const name = who(r);
  if (r.state === 'already') return { name, kind: 'ok', tag: t('今日已签到'), text: r.msg || '' };
  if (r.ok) return { name, kind: 'ok', tag: t('签到成功'), text: r.msg || '' };
  const text = r.error || r.msg || t('未知错误');
  if (r.state === 'pending') return { name, kind: 'warn', tag: t('待确认'), text };
  return { name, kind: 'bad', tag: t('失败'), text };
}

/** Lines of `msg` ("<name>: <text>") for accounts that have no structured row. */
function msgLines(msg, known, kindOf) {
  const out = [];
  for (const line of String(msg || '').split('\n')) {
    const s = line.trim();
    if (!s) continue;
    const i = s.indexOf(': ');
    const name = i > 0 ? s.slice(0, i) : '';
    const text = i > 0 ? s.slice(i + 2) : s;
    if (known.has(name)) continue;
    const kind = kindOf(text);
    out.push({ name, kind, tag: kind === 'bad' ? t('失败') : kind === 'info' ? t('无需操作') : t('已执行'), text });
  }
  return out;
}

const TRAVEL = () => ({ claim: ['ok', t('已领奖')], depart: ['ok', t('已出发')], idle: ['info', t('今日已完成')], traveling: ['info', t('旅行中')], skip: ['info', t('已跳过')] });

function travelReport(r) {
  if (r.ok !== true) return failed(r.msg || t('未知错误'));
  const map = TRAVEL();
  const items = (r.results || []).map((x) => {
    const m = x.action ? map[x.action] : null;
    if (m) return { name: who(x), kind: m[0], tag: m[1], text: x.msg || '' };
    return { name: who(x), kind: x.action ? 'info' : 'bad', tag: x.action ? String(x.action) : t('失败'), text: x.msg || '' };
  });
  // accounts that failed to prepare (restricted, no credential) only appear in the text message
  const known = new Set(items.map((x) => x.name));
  items.push(...msgLines(r.msg, known, () => 'bad'));
  return rowsReport(t('猫猫旅行'), items, t('没有可操作的国内版账号'));
}

function streakReport(r) {
  if (r.ok !== true) return failed(r.msg || t('未知错误'));
  const quiet = t('连登状态正常（无需兑换/无抽奖次数）');
  const items = msgLines(r.msg, new Set(), (text) => (FAIL_RE.test(text) ? 'bad' : text === '连登状态正常（无需兑换/无抽奖次数）' ? 'info' : 'ok'));
  for (const x of items) if (x.kind === 'info') x.text = quiet;
  return rowsReport(t('连登管家与抽奖'), items, t('没有可操作的国内版账号'));
}

function logLine(raw) {
  const s = String(raw);
  const trimmed = s.trim();
  if (trimmed.startsWith('======')) return { kind: 'head', text: trimmed.replace(/^=+\s*|\s*=+$/g, '') };
  const kind = trimmed.startsWith('!') ? 'warn' : trimmed.startsWith('✓') ? 'ok' : 'info';
  return { kind, indent: /^\s/.test(s), text: trimmed.replace(/^[!✓⏸]\s*/, '') };
}

function growthReport(r) {
  if (r.ok !== true) return failed(r.msg || t('任务执行失败'));
  const lines = (Array.isArray(r.logs) ? r.logs : []).map(logLine);
  const credit = F.dec(num(r.credit_added) || 0, 2);
  const n = Number(r.accounts_count) || 0;
  const partial = lines.some((l) => l.kind === 'warn');
  return {
    kind: partial ? 'warn' : 'ok', lines,
    toast: partial ? t('{n} 个账号已处理，新增 {c} 积分；部分步骤失败，详见下方结果', { n, c: credit }) : t('{n} 个账号已处理，新增 {c} 积分', { n, c: credit }),
  };
}

function claimReport(r, name) {
  if (r.ok !== true) {
    const msg = r.msg || t('未知错误');
    return { kind: 'warn', toast: t('领取失败：{msg}', { msg }), lines: [{ name, kind: 'bad', tag: t('失败'), text: msg }] };
  }
  const credit = num(r.credit), energy = num(r.energy);
  const parts = [];
  if (credit > 0) parts.push(t('+{n} 积分', { n: F.dec(credit, 2) }));
  if (energy > 0) parts.push(t('+{n} 能量', { n: F.dec(energy, 2) }));
  return {
    kind: 'ok',
    toast: credit > 0 ? t('领取成功，+{n} 积分', { n: F.dec(credit, 2) }) : t('领取成功'),
    lines: [{ name, kind: 'ok', tag: t('领取成功'), text: parts.join(' · ') || r.msg || '' }],
  };
}

/* ---------------- definitions ---------------- */
export const defs = {
  dailyChat: (uid) => ({
    label: t('每日活跃打卡'), realm: 'intl',
    request: () => post('/accounts/daily-chat', body(uid), LONG),
    interpret: (r) => rowsReport(t('每日活跃打卡'), (r.results || []).map(chatItem), t('没有可打卡的国际版账号')),
  }),
  dailyChatPending: (uids) => ({
    label: t('未活跃账号打卡'), realm: 'intl',
    request: () => post('/accounts/daily-chat', { uids }, LONG),
    interpret: (r) => rowsReport(t('未活跃账号打卡'), (r.results || []).map(chatItem), t('没有可打卡的国际版账号')),
  }),
  dailyChatWeb: (uid) => ({
    label: t('网页通道打卡'), realm: 'intl',
    request: () => post('/accounts/daily-chat-web', body(uid), LONG),
    interpret: (r) => rowsReport(t('网页通道打卡'), (r.results || []).map(chatItem), t('没有可打卡的国际版账号')),
  }),
  checkin: (uid) => ({
    label: t('Buddy 加油站签到'), realm: 'cn',
    request: () => post('/accounts/checkin', body(uid), LONG),
    interpret: (r) => rowsReport(t('Buddy 加油站签到'), (r.results || []).map(checkinItem), t('没有可签到的国内版账号（企业版和 API Key 账号会被跳过）')),
  }),
  growthRun: (uid, claim) => ({
    label: t('接取并领取已完成任务'), realm: 'cn',
    request: () => post('/tasks/run', body(uid, { claim: claim !== false }), LONG),
    interpret: growthReport,
  }),
  travel: (uid) => ({
    label: t('猫猫旅行'), realm: 'cn',
    request: () => post('/tasks/travel', body(uid), LONG),
    interpret: travelReport,
  }),
  streak: (uid) => ({
    label: t('连登管家与抽奖'), realm: 'cn',
    request: () => post('/tasks/streak-bonus', body(uid), LONG),
    interpret: streakReport,
  }),
  claim: (uid, code, name) => ({
    label: t('领取任务奖励'), realm: 'cn',
    request: () => post('/tasks/claim', { uid: String(uid), code }),
    interpret: (r) => claimReport(r, name),
  }),
};
