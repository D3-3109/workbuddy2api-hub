// Accounts page: list header and keyed row cells. A row is created once; every cell is a
// "part" with its own signature, so a poll that changes one number touches one text node.
import { h, setText, setAttr, toggleClass } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { hashIndex } from '../lib/shared.js';
import { badge, btn, switchEl, menu, copyText } from '../lib/ui.js';
import { PRODUCTS, productOf, productShort, productLabel, accountName, initial, statusOf, creditsOf, expiringOf, tokenExpiry, errorOf } from './accounts-model.js';

add({
  '账号信息': 'Account', '状态': 'Status', '版本': 'Version', '积分': 'Credits', '今日用量': 'Today', '凭证到期': 'Token expiry', '权重': 'Weight', '任务': 'Tasks', '操作': 'Actions',
  '选择本页全部账号': 'Select all on this page', '选择账号 {name}': 'Select account {name}',
  '静态 API Key 账号（{mask}）：不参与刷新 / 签到 / 成长任务，额度走企业口径': 'Static API key account ({mask}): no refresh, check-in or growth tasks; quota follows the enterprise limit',
  '账号 ID：{uid}（点击复制）': 'Account ID: {uid} (click to copy)', '+ 备注': '+ Note', '编辑备注': 'Edit note', '账号备注': 'Account note',
  '留空清除；可用来标记「批量」/「自用」': 'Leave empty to clear; handy for tags like “bulk” or “personal”', '最多 200 个字符': 'At most 200 characters', '备注已保存': 'Note saved', '备注已清除': 'Note cleared',
  '处理中 {n}': 'In flight {n}', '{n} 个模型限流中': '{n} models cooling', '{m} · 恢复 {at}': '{m} · back at {at}', '{m} · 撞线 {cap} · 恢复 {at}': '{m} · capped {cap} · back at {at}',
  '禁用 {n} 个模型': '{n} models blocked', '已禁用：{list}': 'Blocked: {list}', '出口：{name}': 'Exit: {name}', '旧代理': 'Legacy proxy', '出站身份：{label}': 'Identity: {label}',
  '未查询': 'Not queried', '查询': 'Query', '查询积分': 'Fetch credits', '积分 {a} / {b}，更新于 {t}': 'Credits {a} / {b}, updated {t}', '积分 {a}，更新于 {t}': 'Credits {a}, updated {t}',
  '今日 Token': 'Tokens today', '今日积分': 'Credits today', '日 token 限额 {n}': 'Daily token cap {n}', '日积分限额 {n}': 'Daily credit cap {n}', '本次启动后尚无请求': 'No request since the gateway started',
  '权重 {n}': 'Weight {n}', '权重 >1 的账号优先接单，已开始的会话保持粘性不换号，权重对新会话生效。': 'Accounts with weight > 1 are preferred. Running sessions stay sticky; weight applies to new sessions.',
  '自定义…': 'Custom…', '权重 (1-100)': 'Weight (1-100)', '请输入 1 到 100 的整数': 'Enter an integer from 1 to 100', '权重已调整：该账号将优先接单': 'Weight updated: this account is preferred', '权重已恢复默认': 'Weight reset to default',
  '参与签到 / 成长任务 / 连登管家 / 猫猫旅行 / 国际版活跃；与 API 取号无关，点一下切换': 'Joins check-in, growth tasks, streak keeper, cat travel and global daily activity. Unrelated to API traffic; click to toggle.',
  '该账号已参与任务': 'Tasks enabled for this account', '该账号已停止任务': 'Tasks disabled for this account', 'API Key 账号不参与任务': 'API key accounts do not run tasks',
  '测试': 'Test', '刷新凭证': 'Refresh token', '重新登录': 'Re-login', '更多': 'More', '查看详情': 'Details', '复制账号 ID': 'Copy account ID', '启用账号': 'Enable', '停用账号': 'Disable', '出站身份': 'Outbound identity', '导出': 'Export', '删除': 'Delete',
  '没有 refresh token，只能重新登录': 'No refresh token; sign in again', '静态 API Key 不能刷新': 'Static API keys cannot be refreshed',
});

export const COLS = [
  { id: 'sel', cls: 'a-sel' },
  { id: 'main', cls: 'a-main', label: '账号信息', sort: 'name' },
  { id: 'status', cls: 'a-status', label: '状态', sort: 'status' },
  { id: 'plan', cls: 'a-plan', label: '版本' },
  { id: 'credit', cls: 'a-credit', label: '积分', sort: 'credits' },
  { id: 'usage', cls: 'a-usage', label: '今日用量', sort: 'usage' },
  { id: 'exp', cls: 'a-exp', label: '凭证到期', sort: 'tokenExp' },
  { id: 'weight', cls: 'a-weight-c', label: '权重', sort: 'weight' },
  { id: 'tasks', cls: 'a-tasks', label: '任务' },
  { id: 'act', cls: 'a-act', label: '操作' },
];

/** Gate a cell's repaint on a cheap signature. */
function part(cell, sigOf, paint) {
  return (a) => {
    const s = sigOf(a);
    if (s === cell.__s) return;
    cell.__s = s;
    paint(a);
  };
}

export function createRows({ host, sel, sort }) {
  const { actions } = host;

  /* ---- header ---- */
  const headCb = h('input', { type: 'checkbox', 'aria-label': t('选择本页全部账号') });
  headCb.addEventListener('change', () => sel.togglePage(headCb.checked));
  const heads = new Map();
  const header = h('div.a-head', { role: 'row' }, COLS.map((c) => {
    let inner;
    if (c.id === 'sel') inner = h('label.check', headCb);
    else if (c.sort) {
      const arrow = h('span.arr');
      inner = h('button.a-hbtn', { type: 'button', onClick: () => sort.toggle(c.sort) }, t(c.label), arrow);
      heads.set(c.sort, { btn: inner, arrow });
    } else inner = t(c.label);
    return h('div.a-c.' + c.cls, { role: 'columnheader' }, inner);
  }));
  function paintHeader(state) {
    for (const [key, o] of heads) {
      const on = sort.key === key;
      setText(o.arrow, on ? (sort.dir === 'asc' ? '↑' : '↓') : '');
      setAttr(o.btn.parentNode, 'aria-sort', on ? (sort.dir === 'asc' ? 'ascending' : 'descending') : 'none');
    }
    if (state) { headCb.checked = state.all; headCb.indeterminate = !state.all && state.some; }
  }

  /* ---- row ---- */
  function create(a) {
    const el = h('div.a-row', { role: 'row' });
    el.__uid = a.uid;
    const cur = () => host.store.get(a.uid) || a;
    const cells = {};
    for (const c of COLS) {
      cells[c.id] = h('div.a-c.' + c.cls, { role: 'cell' });
      if (c.label && c.id !== 'main') cells[c.id].setAttribute('data-label', t(c.label));
    }
    const parts = [
      selPart(cells.sel, el, a, sel),
      mainPart(cells.main, cur, host),
      statusPart(cells.status, cur, host),
      planPart(cells.plan, cur),
      creditPart(cells.credit, cur, actions),
      usagePart(cells.usage),
      expPart(cells.exp),
      weightPart(cells.weight, cur, actions),
      tasksPart(cells.tasks, cur, actions),
      actPart(cells.act, cur, host),
    ];
    el.__parts = parts;
    el.__time = (acc) => { parts[1](acc, true); parts[6](acc); };
    el.append(...COLS.map((c) => cells[c.id]));
    for (const p of parts) p(a);
    el.__rowSig = host.store.sig(a);
    el.__sel = sel.has(a.uid);
    return el;
  }

  function update(el, a) {
    el.__time(a);             // relative times change without the payload changing
    const s = host.store.sig(a);
    if (el.__rowSig === s && el.__sel === sel.has(a.uid)) return;
    el.__rowSig = s;
    el.__sel = sel.has(a.uid);
    for (const p of el.__parts) p(a);
  }

  return { header, paintHeader, create, update };
}

/* ------------------------------------------------------------------ parts */
function selPart(cell, rowEl, a, sel) {
  const input = h('input', { type: 'checkbox' });
  input.addEventListener('click', (e) => sel.toggle(a.uid, input.checked, e.shiftKey));
  cell.appendChild(h('label.check', input));
  return (acc) => {
    const on = sel.has(acc.uid);
    if (input.checked !== on) input.checked = on;
    toggleClass(rowEl, 'sel', on);
    setAttr(input, 'aria-label', t('选择账号 {name}', { name: accountName(acc) }));
  };
}

function mainPart(cell, cur, host) {
  const av = h('span.acct-av');
  const nick = h('button.a-nick', { type: 'button', onClick: () => host.openDetail(cur().uid) });
  const apiBadge = badge('API Key', 'info', { sm: true });
  const uidTxt = h('span');
  const uidBtn = h('button.a-uid', { type: 'button', onClick: () => copyText(cur().uid) }, uidTxt, icon('copy', 11));
  const noteBtn = h('button.a-note', { type: 'button', onClick: () => host.actions.editNote(cur()) });
  const expTxt = h('span.a-exp-inline');
  cell.append(av, h('div.a-id', h('div.a-name', nick, apiBadge), h('div.a-meta', uidBtn, expTxt, noteBtn)));
  const paint = part(cell, (a) => [a.nickname, a.uid, a.note, a.apiKey, a.credentialMask].join('|'), (a) => {
    av.className = 'acct-av acct-av-' + hashIndex(a.uid, 8);
    setText(av, initial(a));
    setText(nick, accountName(a));
    nick.setAttribute('data-tip', accountName(a));
    apiBadge.hidden = !a.apiKey;
    apiBadge.setAttribute('data-tip', t('静态 API Key 账号（{mask}）：不参与刷新 / 签到 / 成长任务，额度走企业口径', { mask: a.credentialMask || '***' }));
    setText(uidTxt, a.uid.slice(0, 8));
    uidBtn.setAttribute('data-tip', t('账号 ID：{uid}（点击复制）', { uid: a.uid }));
    setText(noteBtn, a.note || t('+ 备注'));
    noteBtn.classList.toggle('empty', !a.note);
    noteBtn.setAttribute('data-tip', a.note ? a.note : t('编辑备注'));
  });
  const inline = (a) => { const e = tokenExpiry(a); setText(expTxt, e.text === '—' ? '' : e.text); };
  return (a, timeOnly) => { if (timeOnly) inline(a); else { paint(a); inline(a); } };
}

function statusPart(cell, cur, host) {
  const st = badge('', 'dot');
  const flight = badge('', '', { sm: true, outline: true });
  const cools = badge('', 'warn', { sm: true });
  const blocked = badge('', '', { sm: true, outline: true });
  const err = h('button.a-err', { type: 'button', onClick: () => host.openDetail(cur().uid, 'overview') });
  cell.append(h('div.a-badges', st, flight, cools, blocked), err);
  const sigOf = (a) => {
    const s = statusOf(a);
    return [s.label, s.tone, s.tip, a.inFlight, JSON.stringify(a.modelCooldowns || []), (a.disabledModels || []).join(','), a.lastError, a.lastErrorDetail].join('|');
  };
  return part(cell, sigOf, (a) => {
    const s = statusOf(a);
    st.className = 'badge dot' + (s.tone ? ' ' + s.tone : '');
    setText(st, s.label);
    st.setAttribute('data-tip', s.tip);
    flight.hidden = !(a.inFlight > 0);
    setText(flight, t('处理中 {n}', { n: a.inFlight || 0 }));
    const mc = Array.isArray(a.modelCooldowns) ? a.modelCooldowns.filter((m) => m && m.expiresAt > 0) : [];
    cools.hidden = !mc.length;
    setText(cools, t('{n} 个模型限流中', { n: mc.length }));
    cools.setAttribute('data-tip', mc.map((m) => (m.cappedAt ? t('{m} · 撞线 {cap} · 恢复 {at}', { m: m.model, cap: F.mdHm(m.cappedAt), at: F.mdHm(m.expiresAt) }) : t('{m} · 恢复 {at}', { m: m.model, at: F.mdHm(m.expiresAt) }))).join('\n'));
    const dm = Array.isArray(a.disabledModels) ? a.disabledModels : [];
    blocked.hidden = !dm.length;
    setText(blocked, t('禁用 {n} 个模型', { n: dm.length }));
    blocked.setAttribute('data-tip', t('已禁用：{list}', { list: dm.join(', ') }));
    const e = errorOf(a);
    err.hidden = !e;
    if (e) { setText(err, e.label); err.setAttribute('data-tip', e.tip); }
  });
}

function planPart(cell, cur) {
  const prod = badge('', 'outline');
  const tier = badge('', 'accent', { sm: true });
  const proxy = h('span.a-proxy', icon('globe', 12));
  cell.append(h('div.a-badges', prod, tier, proxy));
  return part(cell, (a) => [a.product, a.tier && a.tier.package, a.tier && a.tier.paid, a.proxySlot, a.hasProxy].join('|'), (a) => {
    const id = productOf(a);
    setText(prod, productShort(id));
    prod.setAttribute('data-tip', t('出站身份：{label}', { label: productLabel(id) }));
    const pkg = a.tier && a.tier.package;
    tier.hidden = !pkg;
    if (pkg) {
      setText(tier, pkg);
      tier.className = 'badge sm ' + (a.tier.paid === true ? 'good' : 'accent');
      tier.setAttribute('data-tip', t('会员 {p}', { p: pkg }) + (a.tier.checked_at ? ' · ' + F.dateTime(a.tier.checked_at) : ''));
    }
    proxy.hidden = !(a.proxySlot || a.hasProxy);
    proxy.setAttribute('data-tip', t('出口：{name}', { name: a.proxySlot || t('旧代理') }));
  });
}

function creditPart(cell, cur, actions) {
  const none = h('div.a-cr-none', h('span.faint', t('未查询')),
    btn(t('查询'), { size: 'xs', tip: t('查询积分'), onClick: () => actions.refreshCredits(cur()) }));
  const nums = h('div.a-cr-nums');
  const exp = badge('', 'warn', { sm: true });
  const fill = h('i');
  const bar = h('div.bar', fill);
  const full = h('div.a-cr', h('div.a-cr-top', nums, exp), bar);
  cell.append(none, full);
  return part(cell, (a) => { const c = creditsOf(a); const e = expiringOf(a); return c ? [c.remain, c.size, c.updatedAt, e && e.label].join('|') : 'none'; }, (a) => {
    const c = creditsOf(a);
    none.hidden = !!c;
    full.hidden = !c;
    if (!c) return;
    nums.replaceChildren(h('b', crText(c.remain)), c.size != null ? h('span.faint', ' / ' + crText(c.size)) : '');
    nums.setAttribute('data-tip', c.size != null
      ? t('积分 {a} / {b}，更新于 {t}', { a: F.fixed(c.remain, 2), b: F.fixed(c.size, 2), t: c.updatedAt ? F.dateTime(c.updatedAt) : '—' })
      : t('积分 {a}，更新于 {t}', { a: F.fixed(c.remain, 2), t: c.updatedAt ? F.dateTime(c.updatedAt) : '—' }));
    bar.hidden = c.ratio == null;
    if (c.ratio != null) {
      bar.className = 'bar ' + (c.ratio < 0.1 ? 'bad' : c.ratio < 0.3 ? 'warn' : 'good');
      fill.style.width = (c.ratio * 100).toFixed(1) + '%';
    }
    const e = expiringOf(a);
    exp.hidden = !e;
    if (e) { exp.className = 'badge sm ' + e.tone; setText(exp, e.label); exp.setAttribute('data-tip', e.tip); }
  });
}

const crText = (n) => (n >= 1000 ? F.int(n) : F.dec(n, 1));

function usagePart(cell) {
  const tok = h('div.a-use'), cr = h('div.a-use');
  cell.append(tok, cr);
  const line = (box, label, now, limit, fmt) => {
    const has = typeof now === 'number';
    const ratio = limit > 0 && has ? Math.min(1, now / limit) : null;
    const kids = [h('span.faint', label), h('span.num', has ? fmt(now) : '—', limit > 0 ? h('span.faint', ' / ' + fmt(limit)) : '')];
    if (ratio != null) kids.push(h('div.bar.thin' + (ratio >= 0.9 ? '.bad' : ratio >= 0.7 ? '.warn' : ''), h('i', { style: { width: (ratio * 100).toFixed(0) + '%' } })));
    box.replaceChildren(...kids);
    box.setAttribute('data-tip', limit > 0 ? (label === t('今日 Token') ? t('日 token 限额 {n}', { n: F.int(limit) }) : t('日积分限额 {n}', { n: F.int(limit) })) : (has ? '' : t('本次启动后尚无请求')));
  };
  return part(cell, (a) => [a.dailyTokensToday, a.dailyTokenLimit, a.dailyCreditsToday, a.dailyCreditLimit].join('|'), (a) => {
    line(tok, t('今日 Token'), a.dailyTokensToday, a.dailyTokenLimit, F.compact);
    line(cr, t('今日积分'), a.dailyCreditsToday, a.dailyCreditLimit, (v) => F.fixed(v, v >= 100 ? 0 : 2));
    cell.classList.toggle('is-idle', typeof a.dailyTokensToday !== 'number' && typeof a.dailyCreditsToday !== 'number' && !(a.dailyTokenLimit > 0) && !(a.dailyCreditLimit > 0));
  });
}

function expPart(cell) {
  const txt = h('span.a-exp-txt');
  cell.appendChild(txt);
  return (a) => {
    const e = tokenExpiry(a);
    const s = e.text + e.tone;
    if (cell.__t === s) return;
    cell.__t = s;
    setText(txt, e.text);
    txt.className = 'a-exp-txt' + (e.tone ? ' tx-' + e.tone : '');
    txt.setAttribute('data-tip', e.tip);
  };
}

function weightPart(cell, cur, actions) {
  const b = h('button.a-weight', { type: 'button', 'data-tip': t('权重 >1 的账号优先接单，已开始的会话保持粘性不换号，权重对新会话生效。') });
  b.addEventListener('click', () => {
    const a = cur();
    const opts = [1, 2, 3, 5, 8, 10];
    if (!opts.includes(a.weight)) opts.push(a.weight);
    opts.sort((x, y) => x - y);
    menu(b, [
      { header: t('权重') },
      ...opts.map((n) => ({ label: '×' + n, checked: (a.weight || 1) === n, onClick: () => actions.setWeight(cur(), n) })),
      { sep: true },
      { label: t('自定义…'), onClick: () => actions.customWeight(cur()) },
    ], { align: 'start' });
  });
  cell.appendChild(b);
  return part(cell, (a) => a.weight, (a) => { setText(b, '×' + (a.weight || 1)); b.classList.toggle('hot', (a.weight || 1) > 1); });
}

function tasksPart(cell, cur, actions) {
  const sw = switchEl({ size: 'sm', label: t('任务'), onChange: async (on, input) => {
    try { await actions.setTasks(cur(), on); } catch { input.checked = !on; }
  } });
  sw.setAttribute('data-tip', t('参与签到 / 成长任务 / 连登管家 / 猫猫旅行 / 国际版活跃；与 API 取号无关，点一下切换'));
  const na = h('span.faint', { 'data-tip': t('API Key 账号不参与任务') }, '—');
  cell.append(sw, na);
  return part(cell, (a) => [a.tasksEnabled, a.apiKey].join('|'), (a) => {
    sw.hidden = !!a.apiKey;
    na.hidden = !a.apiKey;
    sw.input.checked = !!a.tasksEnabled;
  });
}

function actPart(cell, cur, host) {
  const { actions } = host;
  const test = btn(t('测试'), { iconOnly: true, icon: 'zap', variant: 'ghost', size: 'sm', onClick: () => actions.test(cur()) });
  const refresh = btn(t('刷新凭证'), { iconOnly: true, icon: 'refresh', variant: 'ghost', size: 'sm', onClick: () => {
    const a = cur();
    return needsLogin(a) ? actions.relogin(a) : actions.refreshToken(a);
  } });
  const more = btn(t('更多'), { iconOnly: true, icon: 'more', variant: 'ghost', size: 'sm', onClick: (e, el) => openRowMenu(el, cur(), host) });
  cell.append(test, refresh, more);
  return part(cell, (a) => [needsLogin(a), a.apiKey, a.hasRefreshToken, a.enabled].join('|'), (a) => {
    const login = needsLogin(a);
    const label = login ? t('重新登录') : t('刷新凭证');
    refresh.replaceChildren(icon(login ? 'key' : 'refresh', 15));
    refresh.setAttribute('aria-label', label);
    refresh.setAttribute('data-tip', a.apiKey ? t('静态 API Key 不能刷新') : (!a.hasRefreshToken && !a.tokenExpired ? t('没有 refresh token，只能重新登录') : label));
    refresh.classList.toggle('danger', login && !!a.tokenExpired);
    refresh.disabled = !!a.apiKey;
  });
}

/** Expired token or no refresh token: the only way back is the OAuth flow. */
export function needsLogin(a) { return !a.apiKey && (a.tokenExpired || !a.hasRefreshToken); }

export function openRowMenu(anchor, a, host) {
  const { actions } = host;
  const cur = () => host.store.get(a.uid) || a;
  menu(anchor, [
    { label: t('查看详情'), icon: 'panel', onClick: () => host.openDetail(a.uid) },
    { label: t('复制账号 ID'), icon: 'copy', onClick: () => copyText(a.uid) },
    { sep: true },
    { label: a.enabled ? t('停用账号') : t('启用账号'), icon: a.enabled ? 'pause' : 'play', onClick: () => actions.setEnabled(cur(), !a.enabled) },
    { header: t('出站身份') },
    ...PRODUCTS.map((p) => ({ label: productLabel(p.id), checked: productOf(a) === p.id, onClick: () => actions.setProduct(cur(), p.id) })),
    { sep: true },
    { label: t('导出'), icon: 'download', onClick: () => actions.exportUids([a.uid]) },
    { label: t('删除'), icon: 'trash', danger: true, onClick: () => actions.remove(cur()) },
  ], { align: 'end', minWidth: 230 });
}
