// "Usage breakdown" card: share bars + sortable table by model / account / API key,
// with an inline drill-down (the distribution of one row over the other dimension).
import { h, reconcile, setText } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { seriesColor } from '../lib/chart.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { costText } from '../lib/shared.js';
import { badge, btn, empty, seg, skeleton, sorter, sortHeader } from '../lib/ui.js';
import { metricOf, fmtMetric, errorRate, fullTip } from './analytics-data.js';

add({
  '用量明细': 'Usage breakdown', '按模型': 'By model', '按账号': 'By account', '按 Key': 'By key',
  '占比': 'Share', '名称': 'Name', '请求': 'Requests', '积分': 'Credits', '费用': 'Cost', '失败率': 'Error rate', '缓存命中': 'Cache hit',
  '其他 {n} 项': '{n} others', '显示更多（还有 {n} 项）': 'Show more ({n} left)', '显示 {a} / {b} 项': 'Showing {a} of {b}',
  '另有 {n} 项在该区间无用量，已隐藏': '{n} more had no usage in this range and are hidden',
  '该区间暂无数据': 'No data in this range', '各账号分布': 'Spread over accounts', '各模型分布': 'Spread over models',
  '仅统计成功请求': 'Completed requests only', '该区间暂无分布明细': 'No breakdown for this range',
  '面板创建': 'Created in panel', '启动参数': 'Launch argument', '系统归类': 'System bucket', '国际版': 'Global', '国内版': 'China',
  '展开': 'Expand', '该模型暂无定价数据': 'No pricing data yet', '{n} 次请求': '{n} requests', 'Token': 'Tokens', '用量': 'Usage', '输出': 'Output',
});

const PAGE = 15;
const METRIC_COL = { tokens: 'tokens', requests: 'requests', credit: 'credit', cost: 'cost' };

function rows(dim, a) {
  const src = (dim === 'model' ? a.models : dim === 'account' ? a.accounts : a.keys) || [];
  const out = [];
  let hidden = 0;
  for (const e of src) {
    const w = e.window || {};
    if (!((w.requests || 0) + (w.errors || 0) > 0 || (w.total_tokens || 0) > 0 || (w.credit || 0) > 0)) { hidden++; continue; }
    if (dim === 'model') out.push({ key: e.model, name: e.model, mono: true, w, raw: e });
    else if (dim === 'account') out.push({ key: e.uid, name: e.nickname || String(e.uid).slice(0, 8), sub: String(e.uid).slice(0, 13), realm: e.realm, w, raw: e });
    else {
      const src2 = e.source === 'panel' ? t('面板创建') : e.source === 'launcher' ? t('启动参数') : t('系统归类');
      out.push({ key: e.key, name: e.name || e.key, sub: src2, realm: e.realm, w, raw: e });
    }
  }
  return { list: out, hidden };
}

/** The open row's spread over the other dimension, from the per-account / per-key model maps. */
function spread(dim, key, a) {
  const items = [];
  const push = (name, o) => { if (o && (o.tokens || o.requests)) items.push({ name, tokens: o.tokens || 0, requests: o.requests || 0, cost: o.cost_cny }); };
  if (dim === 'model') for (const acc of a.accounts || []) push(acc.nickname || String(acc.uid).slice(0, 8), (acc.window_models || {})[key]);
  else if (dim === 'account') {
    const acc = (a.accounts || []).find((x) => x.uid === key);
    if (acc) for (const [m, o] of Object.entries(acc.window_models || {})) push(m, o);
  } else {
    const k = (a.keys || []).find((x) => x.key === key);
    if (k) { for (const m of k.models || []) push(m.model, m); if (k.models_other) push(t('其他 {n} 项', { n: k.models_other.models || 0 }), k.models_other); }
  }
  return items.sort((x, y) => y.tokens - x.tokens || y.requests - x.requests);
}

export function breakdownCard({ state, save }) {
  let data = null, openKey = null, limit = PAGE, lastSig = '';
  let current = { list: [], hidden: 0 };
  const sort = sorter(METRIC_COL[state.metric] || 'tokens', 'desc', () => { heads.forEach((th) => th.refresh()); render(true); });

  const COLS = [
    { id: 'name', label: t('名称'), get: (e) => e.name },
    { id: 'requests', label: t('请求'), num: true, get: (e) => e.w.requests },
    { id: 'tokens', label: t('Token'), num: true, get: (e) => e.w.total_tokens },
    { id: 'credit', label: t('积分'), num: true, cls: 'hide-sm', get: (e) => e.w.credit },
    { id: 'cost', label: t('费用'), num: true, get: (e) => e.w.cost_cny },
    { id: 'err', label: t('失败率'), num: true, cls: 'hide-sm', get: (e) => errorRate(e.w) },
    { id: 'cache', label: t('缓存命中'), num: true, cls: 'hide-sm', get: (e) => e.w.cache_hit_pct },
  ];
  const getters = Object.fromEntries(COLS.map((c) => [c.id, c.get]));
  const heads = COLS.map((c) => { const th = sortHeader(c.label, c.id, sort, { num: c.num }); if (c.cls) th.classList.add(c.cls); return th; });

  const dimSeg = seg([{ value: 'model', label: t('按模型') }, { value: 'account', label: t('按账号') }, { value: 'key', label: t('按 Key') }], state.dim, (v) => {
    state.dim = v; save(); openKey = null; limit = PAGE; render(true);
  }, { size: 'sm' });

  const sub = h('span.sub');
  const barsTitle = h('div.an-bd-title');
  const bars = h('div.an-hbars');
  const tbody = h('tbody');
  const table = h('table.table.compact.an-table', h('thead', h('tr', ...heads)), tbody);
  const emptyBox = h('div', { hidden: true }, empty('inbox', t('该区间暂无数据')));
  const skel = h('div', skeleton(6));
  table.hidden = true;
  const moreLabel = h('span');
  const moreBtn = btn('', { size: 'xs', onClick: () => { limit += PAGE; render(true); } });
  moreBtn.append(moreLabel);
  const foot = h('div.card-foot.row.between.wrap', { hidden: true }, h('span.an-foot-note'), moreBtn);

  const el = h('section.card.an-card',
    h('div.card-head', h('h2', t('用量明细')), sub, h('div.actions', dimSeg)),
    h('div.card-body.an-bd', h('div.an-bd-left', barsTitle, bars), h('div.table-wrap.an-bd-right', skel, table, emptyBox)),
    foot);

  function toggle(key) { openKey = openKey === key ? null : key; render(true); }

  /* ---- share bars ---- */
  function renderBars(list) {
    const m = state.metric;
    const ranked = list.map((e) => ({ key: e.key, name: e.name, v: metricOf(e.w, m) })).filter((e) => e.v > 0).sort((a, b) => b.v - a.v);
    const total = ranked.reduce((s, e) => s + e.v, 0);
    const top = ranked.slice(0, 8);
    const rest = ranked.slice(8);
    if (rest.length) top.push({ key: '\u0000other', name: t('其他 {n} 项', { n: rest.length }), v: rest.reduce((s, e) => s + e.v, 0), other: true });
    const max = top.length ? top[0].v : 1;
    top.forEach((e, i) => { e.i = i; e.pct = total ? (e.v / total) * 100 : 0; e.w = Math.max(1.5, (e.v / max) * 100); });
    setText(barsTitle, t('占比') + ' · ' + { tokens: 'Token', requests: t('请求'), credit: t('积分'), cost: t('费用') }[m]);
    const updateBar = (row, e) => {
      const sig = e.i + '|' + e.v + '|' + m + '|' + e.name + '|' + (openKey === e.key);
      if (row.__sig === sig) return;
      row.__sig = sig;
      const r = row.__r;
      r.fill.style.width = e.w + '%';
      r.fill.style.background = e.other ? 'var(--c8)' : seriesColor(e.i);
      setText(r.nm, e.name);
      row.setAttribute('data-tip', e.name);
      setText(r.val, fmtMetric(m, e.v));
      setText(r.pct, ' ' + F.pct(e.pct, e.pct < 10 ? 1 : 0));
      row.classList.toggle('on', openKey === e.key);
    };
    reconcile(bars, top, (e) => e.key, (e) => {
      const fill = h('i'), val = h('b'), pct = h('span.faint'), nm = h('span.trunc.an-hb-nm');
      const row = h(e.other ? 'div.hbar.an-hb' : 'button.hbar.an-hb', { type: e.other ? undefined : 'button', onClick: e.other ? undefined : () => { openKey = e.key; const idx = current.list.findIndex((x) => x.key === e.key); if (idx >= limit) limit = idx + 1; render(true); } },
        nm, h('div.track', fill), h('span.an-hb-v', val, pct));
      row.__r = { fill, val, pct, nm };
      updateBar(row, e);
      return row;
    }, updateBar);
  }

  /* ---- table ---- */
  function nameCell(it) {
    const nm = h('b.nm' + (it.mono ? '.mono' : '')), badgeBox = h('span'), s = h('span.s');
    const btnEl = h('button.an-exp', { type: 'button', 'aria-expanded': 'false', 'aria-label': t('展开') }, icon('chevronRight', 14), h('span.an-nm', h('span.an-nm-top', nm, badgeBox), s));
    return { td: h('td', btnEl), nm, badgeBox, s, btnEl };
  }
  function makeRow(it) {
    const name = nameCell(it);
    const cells = COLS.slice(1).map((c) => h('td' + (c.num ? '.num' : '') + (c.cls ? '.' + c.cls : '')));
    const tr = h('tr.an-row', { onClick: () => toggle(it.key) }, name.td, ...cells);
    tr.__r = { name, cells };
    return tr;
  }
  function updateRow(tr, it, pricingOn) {
    const sig = [it.name, it.sub, it.realm, JSON.stringify(it.w), pricingOn, openKey === it.key].join('|');
    if (tr.__sig === sig) return;
    tr.__sig = sig;
    const { name, cells } = tr.__r, w = it.w;
    setText(name.nm, it.name);
    name.nm.setAttribute('data-tip', it.name);
    name.badgeBox.replaceChildren(it.realm ? badge(it.realm === 'cn' ? t('国内版') : t('国际版'), it.realm === 'cn' ? 'info' : '', { sm: true }) : '');
    setText(name.s, it.sub || t('{n} 次请求', { n: F.int(w.requests) }));
    const open = openKey === it.key;
    name.btnEl.setAttribute('aria-expanded', String(open));
    tr.classList.toggle('open', open);
    const [req, tok, credit, cost, err, cache] = cells;
    setText(req, F.int(w.requests));
    setText(tok, F.compact(w.total_tokens));
    tok.setAttribute('data-tip', fullTip(w.total_tokens));
    setText(credit, F.dec(w.credit, 1));
    const unpriced = pricingOn && !w.cost_cny && w.total_tokens > 0;
    setText(cost, pricingOn && !unpriced ? costText(w.cost_cny) : '—');
    if (unpriced) cost.setAttribute('data-tip', t('该模型暂无定价数据')); else cost.removeAttribute('data-tip');
    const er = errorRate(w);
    setText(err, er == null ? '—' : F.pct(er, 1));
    err.className = 'num hide-sm' + (er >= 10 ? ' tx-bad' : er >= 3 ? ' tx-warn' : '');
    setText(cache, w.cache_hit_pct ? F.pct(w.cache_hit_pct, 1) : '—');
  }

  function makeDetail() {
    const body = h('div.an-dbody');
    return Object.assign(h('tr.an-detail', h('td', { colspan: COLS.length }, body)), { __body: body });
  }
  function updateDetail(tr, it) {
    const items = spread(state.dim, it.key, data.a);
    const m = state.metric === 'requests' ? 'requests' : state.metric === 'cost' ? 'cost' : 'tokens';
    const sig = JSON.stringify(items) + state.dim + m;
    if (tr.__sig === sig) return;
    tr.__sig = sig;
    const val = (x) => (m === 'requests' ? x.requests : m === 'cost' ? x.cost || 0 : x.tokens);
    items.sort((a, b) => val(b) - val(a));
    let show = items;
    if (items.length > 9) {
      const rest = items.slice(7);
      const other = { name: t('其他 {n} 项', { n: rest.length }), other: true, tokens: 0, requests: 0, cost: 0 };
      for (const x of rest) { other.tokens += x.tokens; other.requests += x.requests; other.cost += x.cost || 0; }
      show = items.slice(0, 7).concat([other]);
    }
    const max = Math.max(1e-9, ...show.map(val));
    const lines = show.map((x, i) => h('div.an-drow',
      h('span.trunc', { 'data-tip': x.name }, x.name),
      h('div.track', h('i', { style: { width: Math.max(1.5, (val(x) / max) * 100) + '%', background: x.other ? 'var(--c8)' : seriesColor(i) } })),
      h('span.num', F.compact(x.tokens)), h('span.num.hide-sm', F.int(x.requests)),
      h('span.num.hide-sm', state.pricingOn && x.cost != null ? costText(x.cost) : '—')));
    tr.__body.replaceChildren(
      h('div.an-dhead', h('b', state.dim === 'model' ? t('各账号分布') : t('各模型分布')), h('span.faint', t('仅统计成功请求'))),
      ...(lines.length ? lines : [h('div.faint', t('该区间暂无分布明细'))]));
  }

  function render(force) {
    if (!data) return;
    const sig = data.sig + '|' + state.dim + state.metric + openKey + limit + sort.key + sort.dir + state.pricingOn;
    if (!force && sig === lastSig) return;
    lastSig = sig;
    current = rows(state.dim, data.a);
    const { list, hidden } = current;
    if (openKey != null && !list.some((e) => e.key === openKey)) openKey = null;
    renderBars(list);
    const sorted = sort.apply(list, getters);
    const shown = sorted.slice(0, limit);
    const items = [];
    for (const it of shown) { items.push({ k: 'r:' + it.key, it }); if (it.key === openKey) items.push({ k: 'd:' + it.key, it, detail: true }); }
    const upd = (tr, x) => (x.detail ? updateDetail(tr, x.it) : updateRow(tr, x.it, state.pricingOn));
    reconcile(tbody, items, (x) => x.k, (x) => { const tr = x.detail ? makeDetail() : makeRow(x.it); upd(tr, x); return tr; }, upd);
    skel.hidden = true;
    table.hidden = !list.length;
    emptyBox.hidden = !!list.length;
    setText(sub, t('显示 {a} / {b} 项', { a: shown.length, b: list.length }));
    const more = list.length - shown.length;
    foot.hidden = !(more > 0 || hidden > 0);
    setText(foot.firstChild, hidden > 0 ? t('另有 {n} 项在该区间无用量，已隐藏', { n: hidden }) : '');
    moreBtn.hidden = more <= 0;
    setText(moreLabel, t('显示更多（还有 {n} 项）', { n: more }));
  }

  return {
    el,
    /** d = { a: analytics response, sig } */
    update(d) { data = d; render(false); },
    /** Metric changed: re-sort to the new metric and redraw. */
    setMetric() { sort.key = METRIC_COL[state.metric] || 'tokens'; sort.dir = 'desc'; heads.forEach((th) => th.refresh()); render(true); },
  };
}
