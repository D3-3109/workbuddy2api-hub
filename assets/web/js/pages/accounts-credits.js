// Account detail dialog: credits and package breakdown (billing endpoints), lazily loaded.
import { h, setText } from '../lib/dom.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { badge, btn, seg, alertBox, spinner, empty, errText } from '../lib/ui.js';
import { api } from './accounts-store.js';

add({
  '正在向上游同步最新积分与权益包明细…': 'Syncing the latest credits and packages from upstream…', '登录凭证已失效': 'Login token is no longer valid', '拉取积分明细失败': 'Could not load credit details',
  '重新登录该账号': 'Sign in to this account again', '刷新明细': 'Refresh details', '剩余可用积分': 'Credits left', '已消耗积分': 'Credits used', '总额度容量': 'Total capacity', '最近将过期': 'Expiring soonest',
  '已有包过期': 'A package expired', '剩 {n} 天': '{n} d left', '剩 {n} 天 (即将到期)': '{n} d left (soon)', '{d} · {n} 积分即将到期': '{d} · {n} credits expiring', '{d} 到期 · {n} 积分': '{d} · {n} credits', '长期有效': 'No expiry', '暂无临期生效套餐包': 'No package expiring soon',
  '额度利用率: {p}': 'Utilisation: {p}', '更新时间: {t}': 'Updated: {t}', '每日签到: 今日已签到': 'Daily check-in: done today', '每日签到: 今日未签到': 'Daily check-in: not yet today', '连续 {n} 天': '{n}-day streak', '签到奖励: {n} 积分': 'Check-in reward: {n} credits', '今日领取: {n} 积分': 'Claimed today: {n} credits',
  '付费用户': 'Paid', '普通用户': 'Standard', '套餐权益包明细': 'Packages', '共 {n} 个': '{n} total', '全部': 'All', '有剩余': 'In stock', '即将到期': 'Expiring', '已用完/过期': 'Used up / expired',
  '搜索包名/代码/来源…': 'Search name / code / source…', '最快到期优先': 'Soonest expiry', '剩余额度最大': 'Most remaining', '已用额度最多': 'Most used', '总容量最大': 'Largest size',
  '套餐包 / 来源与属性': 'Package / source', '到期倒计时 / 有效期': 'Expiry', '总额度': 'Size', '已用': 'Used', '剩余可用': 'Remaining', '利用率': 'Usage', '生效中': 'In use', '自动续费': 'Auto-renew', '已过期': 'Expired',
  '无符合条件的套餐权益包': 'No matching packages', '计费周期结束（积分不过期）': 'Billing cycle end (credits do not expire)', '自 {d}': 'since {d}', '到期时间': 'Expires',
  '登录已过期，请重新登录': 'Login expired, please sign in again', '会员 {p}': 'Plan {p}',
});

const n0 = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : 0);
const isExpired = (p) => !!p.is_expired || (typeof p.days_left === 'number' && p.days_left < 0);
const GROUPS = {
  all: () => true,
  active: (p) => n0(p.remain) > 0 && !isExpired(p),
  expiring: (p) => n0(p.remain) > 0 && !isExpired(p) && !p.no_expiry && typeof p.days_left === 'number' && p.days_left <= 7,
  exhausted: (p) => n0(p.remain) <= 0 || isExpired(p),
};
const SORTS = {
  expiry: null,
  remain: (p) => n0(p.remain),
  used: (p) => n0(p.used),
  size: (p) => n0(p.size),
};

function expiryCell(p) {
  const d = p.days_left;
  const sub = p.no_expiry ? t('计费周期结束（积分不过期）') : (p.cycle_end_time ? String(p.cycle_end_time).slice(0, 19) : '');
  let b;
  if (isExpired(p)) b = badge(t('已过期'), 'bad', { sm: true });
  else if (p.no_expiry || typeof d !== 'number' || d >= 365) b = badge(t('长期有效'), 'good', { sm: true });
  else if (d <= 3) b = badge(t('剩 {n} 天 (即将到期)', { n: F.dec(d, 1) }), 'bad', { sm: true });
  else b = badge(t('剩 {n} 天', { n: Math.round(d) }), d <= 7 ? 'warn' : 'good', { sm: true });
  const since = p.cycle_start_time ? t('自 {d}', { d: String(p.cycle_start_time).slice(0, 10) }) : '';
  return h('td', b, h('div.cell-sub', { 'data-tip': t('到期时间') }, sub), since ? h('div.cell-sub', since) : null);
}

export function creditsPanel(host, uid) {
  const cur = () => host.store.get(uid);
  let filter = 'active', query = '', sortKey = 'expiry', state = 'idle', err = null, started = false;

  const box = h('div.col', { style: 'gap:14px' });
  const loading = h('div.row', { style: 'padding:24px 0;justify-content:center;gap:10px' }, spinner(), h('span.muted', t('正在向上游同步最新积分与权益包明细…')));
  const errBox = h('div');
  const content = h('div.col', { style: 'gap:14px' });
  const metrics = h('div.cr-metrics');
  const barFill = h('i');
  const barText = h('div.row.between.faint', { style: 'font-size:12.5px' });
  const strip = h('div.row.gap-1.wrap');
  const count = h('span');
  const tbody = h('tbody');
  const emptyBox = h('div', { hidden: true }, empty('inbox', t('无符合条件的套餐权益包')));
  const filters = seg([['all', '全部'], ['active', '有剩余'], ['expiring', '即将到期'], ['exhausted', '已用完/过期']].map(([v, l]) => ({ value: v, label: t(l) })), filter, (v) => { filter = v; paintTable(); }, { size: 'sm' });
  const search = h('input.input.sm', { type: 'search', placeholder: t('搜索包名/代码/来源…'), 'aria-label': t('搜索包名/代码/来源…'), style: 'max-width:240px' });
  search.addEventListener('input', () => { query = search.value.trim().toLowerCase(); paintTable(); });
  const sortSel = h('select.select.sm', { 'aria-label': t('最快到期优先'), style: 'width:auto' }, ['expiry', 'remain', 'used', 'size'].map((k) => h('option', { value: k }, t({ expiry: '最快到期优先', remain: '剩余额度最大', used: '已用额度最多', size: '总容量最大' }[k]))));
  sortSel.addEventListener('change', () => { sortKey = sortSel.value; paintTable(); });
  const refreshBtn = btn(t('刷新明细'), { size: 'sm', icon: 'refresh', onClick: () => load(true) });

  const table = h('table.table.compact', h('thead', h('tr', h('th', t('套餐包 / 来源与属性')), h('th', t('到期倒计时 / 有效期')),
    h('th.num.hide-sm', t('总额度')), h('th.num.hide-sm', t('已用')), h('th.num', t('剩余可用')), h('th.num.hide-sm', t('利用率')))), tbody);
  content.append(metrics, h('div', h('div.bar.lg', barFill), barText), strip,
    h('div.row.gap-3.wrap', h('b', t('套餐权益包明细')), count, h('span.grow'), filters, search, sortSel),
    h('div.table-wrap.cr-table', table), emptyBox);
  box.append(loading, errBox, content, h('div.row', { style: 'justify-content:flex-end' }, refreshBtn));

  const metric = (label, value, sub, tone) => h('div.cr-metric', h('div.faint', label), h('div.cr-val' + (tone ? '.tx-' + tone : ''), value), h('div.faint', sub || ' '));

  function expiring(c) {
    const ee = c.earliest_expiring;
    let days = ee && typeof ee.days_left === 'number' ? ee.days_left : null, amount = ee ? n0(ee.remain) : 0, end = ee ? ee.cycle_end_time : '';
    if (days == null) {
      const live = (c.packages || []).filter((p) => !p.no_expiry && !isExpired(p) && n0(p.remain) > 0 && typeof p.days_left === 'number').sort((a, b) => a.days_left - b.days_left)[0];
      if (live) { days = live.days_left; amount = n0(live.remain); end = live.cycle_end_time; }
    }
    const pk = (c.packages || []).some((p) => isExpired(p) && n0(p.remain) > 0);
    const md = end ? String(end).slice(5, 16) : '';
    if (pk && days == null) return metric(t('最近将过期'), t('已有包过期'), '', 'bad');
    if (days == null) return metric(t('最近将过期'), t('长期有效'), t('暂无临期生效套餐包'), 'good');
    if (days <= 3) return metric(t('最近将过期'), t('剩 {n} 天', { n: F.dec(days, 1) }), t('{d} · {n} 积分即将到期', { d: md, n: F.fixed(amount, 2) }), 'bad');
    return metric(t('最近将过期'), t('剩 {n} 天', { n: Math.round(days) }), t('{d} 到期 · {n} 积分', { d: md.slice(0, 5), n: F.fixed(amount, 2) }), days <= 7 ? 'warn' : '');
  }

  function paintContent(c) {
    const used = c.used != null ? n0(c.used) : null, remain = n0(c.remain), size = c.size != null ? n0(c.size) : (c.total != null ? n0(c.total) : remain + n0(used));
    const usedV = used != null ? used : Math.max(0, size - remain);
    metrics.replaceChildren(metric(t('剩余可用积分'), F.fixed(remain, 2), '', 'good'), metric(t('已消耗积分'), F.fixed(usedV, 2)), metric(t('总额度容量'), F.fixed(size, 2)), expiring(c));
    const pct = size > 0 ? (usedV / size) * 100 : 0;
    barFill.style.width = Math.min(100, pct).toFixed(1) + '%';
    barFill.parentNode.className = 'bar lg' + (pct >= 90 ? ' bad' : pct >= 70 ? ' warn' : '');
    barText.replaceChildren(h('span', t('额度利用率: {p}', { p: c.used_percent || F.pct(pct, 1) })), h('span', t('更新时间: {t}', { t: c.updated_iso || (c.updated_at || c.updatedAt ? F.dateTime(c.updated_at || c.updatedAt) : '—') })));
    const a = cur(), ck = c.checkin, kids = [];
    if (c.is_paid_user != null) kids.push(badge(c.is_paid_user ? t('付费用户') : t('普通用户'), c.is_paid_user ? 'good' : ''));
    if (a && a.tier && a.tier.package) kids.push(badge(t('会员 {p}', { p: a.tier.package }), 'accent'));
    if (ck) {
      kids.push(badge(ck.today_checked_in ? t('每日签到: 今日已签到') : t('每日签到: 今日未签到'), ck.today_checked_in ? 'good' : '', { outline: true }));
      if (ck.streak_days != null) kids.push(badge(t('连续 {n} 天', { n: ck.streak_days }), '', { outline: true }));
      if (ck.daily_credit != null) kids.push(badge(t('签到奖励: {n} 积分', { n: ck.daily_credit }), '', { outline: true }));
      if (ck.today_credit != null) kids.push(badge(t('今日领取: {n} 积分', { n: ck.today_credit }), '', { outline: true }));
    }
    strip.replaceChildren(...kids);
    strip.hidden = !kids.length;
    paintTable();
  }

  function paintTable() {
    const c = cur() && cur().credits;
    const pkgs = (c && c.packages) || [];
    setText(count, t('共 {n} 个', { n: pkgs.length }));
    const q = query;
    let rows = pkgs.filter((p) => GROUPS[filter](p) && (!q || [p.name, p.package_code, p.grant_reason, p.sub_product_name, p.resource_id].some((x) => String(x || '').toLowerCase().includes(q))));
    if (SORTS[sortKey]) rows = rows.slice().sort((x, y) => SORTS[sortKey](y) - SORTS[sortKey](x));
    tbody.replaceChildren(...rows.map((p) => {
      const size = n0(p.size), used = n0(p.used), pct = size > 0 ? Math.min(100, (used / size) * 100) : 0;
      return h('tr', isExpired(p) ? { class: 'dim' } : null,
        h('td', h('div.cell-main', p.name || '—', ' ', p.in_usage ? badge(t('生效中'), 'accent', { sm: true }) : null, p.auto_renew ? badge(t('自动续费'), '', { sm: true }) : null),
          h('div.cell-sub', [p.grant_reason, p.sub_product_name].filter(Boolean).join(' · ')), h('div.cell-sub.mono', [p.package_code, p.resource_id].filter(Boolean).join(' · '))),
        expiryCell(p), h('td.num.hide-sm', F.fixed(size, 2)), h('td.num.hide-sm', F.fixed(used, 2)), h('td.num.tx-good', { style: 'font-weight:650' }, F.fixed(n0(p.remain), 2)),
        h('td.num.hide-sm', F.pct(pct, 0), h('div.bar' + (pct >= 90 ? '.bad' : pct >= 70 ? '.warn' : ''), { style: 'width:46px;height:4px;margin:3px 0 0 auto' }, h('i', { style: { width: pct.toFixed(0) + '%' } }))));
    }));
    table.hidden = !rows.length;
    emptyBox.hidden = !!rows.length;
  }

  function paint() {
    const c = cur() && cur().credits;
    loading.hidden = state !== 'loading';
    errBox.hidden = state !== 'error';
    content.hidden = !c || state === 'loading';
    refreshBtn.disabled = state === 'loading';
    if (state === 'error') {
      errBox.replaceChildren(alertBox('bad', h('b', err.relogin ? t('登录凭证已失效') : t('拉取积分明细失败')), h('div', err.msg),
        err.relogin ? h('div', { style: 'margin-top:8px' }, btn(t('重新登录该账号'), { size: 'sm', variant: 'primary', icon: 'key', onClick: () => host.actions.relogin(cur()) })) : null));
    }
    if (c && !content.hidden) paintContent(c);
  }

  async function load(refresh) {
    state = 'loading'; paint();
    try {
      const r = await api.creditsDetail(uid, refresh);
      if (r.ok && r.credits) {
        host.store.merge(uid, { credits: r.credits, ...(r.tier !== undefined ? { tier: r.tier } : {}), tokenExpired: false });
        state = 'ready';
      } else {
        err = { msg: r.error || t('未知错误'), relogin: !!r.need_relogin || r.status === 401 };
        if (r.credits) host.store.merge(uid, { credits: r.credits });
        if (err.relogin) host.store.merge(uid, { tokenExpired: true });
        state = 'error';
      }
    } catch (e) { err = { msg: errText(e), relogin: false }; state = 'error'; }
    paint();
  }

  return {
    el: box,
    paint() { if (started && state !== 'loading') paint(); },
    onShow() {
      if (started) return;
      started = true;
      const c = cur() && cur().credits;
      const cached = c && Array.isArray(c.packages) && c.packages.some((p) => p.cycle_end_time || p.expire_time);
      if (cached) { state = 'ready'; paint(); } else load(true);
    },
  };
}
