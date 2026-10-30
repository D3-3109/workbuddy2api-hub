// Account detail dialog: header, overview tab (status, guards, errors, usage) and tab switching.
import { h, setText } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { get } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { hashIndex } from '../lib/shared.js';
import { badge, modal, alertBox, copyText } from '../lib/ui.js';
import { accountName, initial, statusOf, creditsOf, tokenExpiry, productLabel, productOf, productShort } from './accounts-model.js';
import { creditsPanel } from './accounts-credits.js';
import { settingsPanel } from './accounts-settings.js';

add({
  '账号详情': 'Account details', '概览': 'Overview', '积分明细': 'Credits', '设置': 'Settings', '国内版': 'China', '国际版': 'Global',
  '区域': 'Region', '账号 ID': 'Account ID', '来源': 'Source', '出站身份': 'Outbound identity', '凭证到期': 'Token expiry', '并发请求': 'In flight', '连续失败': 'Recent failures', '企业账号': 'Enterprise', '个人账号': 'Personal',
  '加入时间': 'Added', '最近签到': 'Last check-in', '最近每日活跃': 'Last daily chat', '{n} 次': '{n}', '最近错误': 'Last error', '限额与保护': 'Limits and guards', '保护项': 'Guard', '限额值': 'Limit', '当前': 'Current', '状态': 'Status',
  '保留积分线': 'Reserve credits', '日 Token 限额': 'Daily token cap', '日积分限额': 'Daily credit cap', '单模型日 Token 限额': 'Per-model daily token cap', '未启用': 'Off', '已触发': 'Triggered', '正常': 'OK', '{n} 个模型已达限额': '{n} models capped',
  '限额在「设置」页调整；用量数据在网关处理请求后才会刷新。': 'Limits are set in Settings. Usage figures refresh once the gateway handles a request.', '模型限流': 'Model cooldowns',
  '{m} · 撞线 {cap} · 恢复 {at}': '{m} · capped {cap} · back at {at}', '{m} · 恢复 {at}': '{m} · back at {at}', '{m} · 今日已用 {n} / 上限 {l} token': '{m} · {n} / {l} tokens today',
  '累计请求': 'Requests', '总 Token': 'Total tokens', '缓存命中': 'Cache hit', '累计数据（全部时间）': 'All-time totals', '静态 API Key': 'Static API key',
  '出站身份：{label}': 'Identity: {label}', '会员 {p}': 'Plan {p}', '点击复制': 'Click to copy',
});

function Kv(label, value) { return [h('dt', label), h('dd', value)]; }

function headerBlock(host, uid) {
  const av = h('span.acct-av');
  const name = h('div.d-name');
  const badges = h('div.row.gap-1.wrap');
  const uidBtn = h('button.d-uid', { type: 'button', onClick: () => copyText(uid) }, h('span', uid), icon('copy', 11));
  const el = h('div.d-head', av, h('div.grow', name, badges, uidBtn));
  let sig = '';
  return {
    el,
    paint(a) {
      const s = statusOf(a);
      const k = [a.nickname, a.uid, s.label, s.tone, a.realm, a.product, a.apiKey, a.tier && a.tier.package].join('|');
      if (k === sig) return;
      sig = k;
      av.className = 'acct-av acct-av-' + hashIndex(a.uid, 8);
      setText(av, initial(a));
      setText(name, accountName(a));
      badges.replaceChildren(...[
        badge(s.label, 'dot' + (s.tone ? ' ' + s.tone : ''), { tip: s.tip }),
        badge(a.realm === 'cn' ? t('国内版') : t('国际版'), '', { outline: true }),
        badge(productShort(productOf(a)), '', { outline: true, tip: t('出站身份：{label}', { label: productLabel(productOf(a)) }) }),
        a.apiKey ? badge('API Key', 'info', { sm: true }) : null,
        a.tier && a.tier.package ? badge(a.tier.package, a.tier.paid === true ? 'good' : 'accent', { sm: true, tip: t('会员 {p}', { p: a.tier.package }) }) : null].filter(Boolean));
    },
  };
}

function overviewPanel(host, uid) {
  const banner = h('div');
  const kv = h('dl.kv.d-kv');
  const errBox = h('div.col', { style: 'gap:6px' });
  const gBody = h('tbody');
  const capped = h('div.row.gap-1.wrap');
  const pills = h('div.row.gap-1.wrap');
  const pillsWrap = h('div.col', { style: 'gap:6px' }, h('b', t('模型限流')), pills);
  const usage = h('div.row.gap-3.wrap.d-usage', { hidden: true });
  const el = h('div.col', { style: 'gap:16px' }, banner, kv, errBox, usage,
    h('div.col', { style: 'gap:8px' }, h('b', t('限额与保护')),
      h('div.card', h('div.table-wrap', h('table.table.compact', h('thead', h('tr', h('th', t('保护项')), h('th.num', t('限额值')), h('th.num', t('当前')), h('th', t('状态')))), gBody))),
      capped, h('div.faint', { style: 'font-size:12.5px' }, t('限额在「设置」页调整；用量数据在网关处理请求后才会刷新。'))),
    pillsWrap);

  const limit = (n) => (n > 0 ? F.int(n) : t('未启用'));
  const guard = (name, lim, now, hit) => h('tr', h('td', name), h('td.num', lim), h('td.num', now), h('td', hit === null ? '' : badge(hit ? t('已触发') : t('正常'), hit ? 'warn' : '', { sm: true })));

  let sig = '';
  function paint(a) {
    const s = statusOf(a);
    const k = host.store.sig(a) + '|' + JSON.stringify(a.modelDailyTokens || {}) + a.lastErrorDetail + a.fails + a.lastCheckin + a.lastDailyChat + a.machineId;
    if (k === sig) return;
    sig = k;
    banner.replaceChildren(s.key === 'ready' ? '' : alertBox(s.tone === 'bad' ? 'bad' : s.tone === 'warn' ? 'warn' : 'info', h('b', s.label), ' ', s.tip));
    const exp = tokenExpiry(a);
    kv.replaceChildren(...[
      Kv(t('区域'), a.realm === 'cn' ? t('国内版') : t('国际版')), Kv(t('来源'), a.source || '—'), Kv(t('出站身份'), productLabel(productOf(a))),
      Kv(t('凭证到期'), h('span', { class: exp.tone ? 'tx-' + exp.tone : '', 'data-tip': exp.tip }, exp.text)),
      Kv(t('企业账号'), a.enterpriseId ? a.enterpriseId : t('个人账号')), Kv(t('并发请求'), String(a.inFlight || 0)),
      Kv(t('连续失败'), t('{n} 次', { n: a.fails || 0 })), Kv(t('加入时间'), a.addedAt ? F.dateTime(a.addedAt) : '—'),
      a.lastCheckin ? Kv(t('最近签到'), a.lastCheckin) : [], a.lastDailyChat ? Kv(t('最近每日活跃'), a.lastDailyChat) : [],
    ].flat());
    const detail = a.lastErrorDetail || a.lastError;
    errBox.hidden = !detail;
    if (detail) errBox.replaceChildren(h('b', t('最近错误')), h('pre.d-pre', detail));

    const c = creditsOf(a);
    const mdl = a.modelDailyTokens || {};
    const over = a.modelDailyTokenLimit > 0 ? Object.entries(mdl).filter(([, n]) => n >= a.modelDailyTokenLimit) : [];
    gBody.replaceChildren(
      guard(t('保留积分线'), limit(a.reserveCredits), c ? F.fixed(c.remain, 2) : '—', a.reserveCredits > 0 ? !!a.reserveBlocked : null),
      guard(t('日 Token 限额'), limit(a.dailyTokenLimit), typeof a.dailyTokensToday === 'number' ? F.int(a.dailyTokensToday) : '—', a.dailyTokenLimit > 0 ? !!a.dailyLimitBlocked : null),
      guard(t('日积分限额'), limit(a.dailyCreditLimit), typeof a.dailyCreditsToday === 'number' ? F.fixed(a.dailyCreditsToday, 2) : '—', a.dailyCreditLimit > 0 ? !!a.creditLimitReached : null),
      guard(t('单模型日 Token 限额'), limit(a.modelDailyTokenLimit), String(Object.keys(mdl).length), a.modelDailyTokenLimit > 0 ? over.length > 0 : null));
    capped.replaceChildren(...over.map(([m, n]) => badge(t('{m} · 今日已用 {n} / 上限 {l} token', { m, n: F.int(n), l: F.int(a.modelDailyTokenLimit) }), 'warn', { sm: true })));
    const cools = (a.modelCooldowns || []).filter((m) => m && m.expiresAt > 0);
    pillsWrap.hidden = !cools.length;
    pills.replaceChildren(...cools.map((m) => badge(m.cappedAt
      ? t('{m} · 撞线 {cap} · 恢复 {at}', { m: m.model, cap: F.mdHm(m.cappedAt), at: F.mdHm(m.expiresAt) })
      : t('{m} · 恢复 {at}', { m: m.model, at: F.mdHm(m.expiresAt) }), 'warn', { sm: true })));
  }

  let usageLoaded = false;
  async function loadUsage() {
    if (usageLoaded) return;
    usageLoaded = true;
    try {
      const r = await get('/usage/by-account');
      const row = (r.accounts || []).find((x) => x.account === uid);
      if (!row) return;
      const hit = row.prompt_tokens > 0 ? F.pct((row.cached_tokens / row.prompt_tokens) * 100, 0) : '—';
      const stat = (label, value) => h('div.d-stat', h('div.faint', label), h('div.d-stat-v', value));
      usage.replaceChildren(stat(t('累计请求'), F.int(row.requests)), stat(t('总 Token'), F.compact(row.total_tokens)), stat(t('缓存命中'), hit),
        h('div.row.gap-1.wrap', ...(row.models || []).slice(0, 5).map(([m, n]) => badge(`${m} · ${F.int(n)}`, '', { sm: true, outline: true }))));
      usage.hidden = false;
    } catch { /* optional block */ }
  }
  return { el, paint, onShow: loadUsage };
}

export function openDetail(host, uid, tab = 'overview') {
  const cur = () => host.store.get(uid);
  if (!cur()) return null;
  const head = headerBlock(host, uid);
  const views = { overview: overviewPanel(host, uid), credits: creditsPanel(host, uid), settings: settingsPanel(host, uid, () => m.close()) };
  const labels = { overview: t('概览'), credits: t('积分明细'), settings: t('设置') };
  const tabBtns = {};
  const tabsEl = h('div.tabs.d-tabs', { role: 'tablist' }, Object.keys(views).map((id) => {
    tabBtns[id] = h('button', { type: 'button', role: 'tab', onClick: () => show(id) }, labels[id]);
    return tabBtns[id];
  }));
  const panels = h('div.d-panels', ...Object.values(views).map((v) => v.el));

  function show(id) {
    for (const k of Object.keys(views)) { views[k].el.hidden = k !== id; tabBtns[k].setAttribute('aria-selected', String(k === id)); }
    views[id].onShow?.();
  }
  function paintAll() {
    const a = cur();
    if (!a) { m.close(); return; }
    head.paint(a); views.overview.paint(a); views.settings.paint(a); views.credits.paint();
  }
  const m = modal({ title: t('账号详情'), size: 'wide', body: [head.el, tabsEl, panels], onClose: () => { unsub(); host.dialogs.delete(m); } });
  const unsub = host.store.subscribe(paintAll);
  host.dialogs.add(m);
  paintAll();
  show(views[tab] ? tab : 'overview');
  host.loadSlots().then(() => { if (cur()) views.settings.paint(cur()); });
  return m;
}
