// Analytics (数据看板): cross-realm usage KPIs, trend, breakdown, performance, activity pattern.
//
// Request budget per 30 s tick, with unchanged data:  1 x /usage/recent?limit=12 (10 KB, usually a 304).
// That response doubles as the change detector: when the log has not grown (same total and newest
// row) the heavy endpoints (/usage/analytics, /usage/timeseries, /usage/perf) are not requested at all.
// When it did grow they are re-requested with byte-identical URLs, so the server answers 304 or a
// fresh body, and rendering is skipped when the response signature is unchanged.
import { h, setText, scope } from '../lib/dom.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { poller } from '../lib/poll.js';
import { loadSettings } from '../lib/ctx.js';
import { btn, seg, alertBox, badge, errText } from '../lib/ui.js';
import * as D from './analytics-data.js';
import { kpiRow } from './analytics-kpis.js';
import { seriesCard } from './analytics-chart.js';
import { breakdownCard } from './analytics-breakdown.js';
import { perfCard } from './analytics-perf.js';
import { heatCard } from './analytics-heat.js';
import { activityCard } from './analytics-recent.js';
import { notesCard } from './analytics-notes.js';

add({
  '数据看板': 'Analytics', '今日': 'Today', '本周': 'This week', '本月': 'This month', '近3天': '3 days', '近30天': '30 days', '自定义': 'Custom',
  '请求': 'Requests', '积分': 'Credits', '费用': 'Cost', '刷新': 'Refresh', '更新于 {t}': 'Updated {t}',
  '数据加载失败': 'Could not load analytics', '重试': 'Retry', '数据未能更新': 'Update failed', '开始日期': 'From', '结束日期': 'To',
  '日期范围无效（最长 400 天，开始不能晚于结束）': 'Invalid range (max 400 days, start must not be after end)',
  '区间': 'Range', '指标': 'Metric',
});

export const usesRealm = false;

const STORE = 'wb-analytics-v2';
const POLL_MS = 30000;
const RANGES = ['today', 'week', 'month', 'd3', 'd30', 'custom'];

function loadState() {
  let s = {};
  try { s = JSON.parse(localStorage.getItem(STORE) || '{}') || {}; } catch { /* corrupt or blocked */ }
  const pick = (v, ok, d) => (ok.includes(v) ? v : d);
  const today = D.ymd(Date.now() / 1000);
  const week = D.ymd(Date.now() / 1000 - 6 * 86400);
  const c = s.custom || {};
  return {
    range: pick(s.range, RANGES, 'today'),
    custom: { from: typeof c.from === 'string' ? c.from : week, to: typeof c.to === 'string' ? c.to : today },
    metric: pick(s.metric, D.METRICS, 'tokens'),
    chartType: pick(s.chartType, ['area', 'bar'], 'area'),
    stack: pick(s.stack, ['total', 'parts'], 'total'),
    dim: pick(s.dim, ['model', 'account', 'key'], 'model'),
    pricingOn: true,
  };
}

export async function mount(root, app) {
  const sc = scope();
  const state = loadState();
  const save = () => {
    const { range, custom, metric, chartType, stack, dim } = state;
    try { localStorage.setItem(STORE, JSON.stringify({ range, custom, metric, chartType, stack, dim })); } catch { /* blocked */ }
  };
  const names = new Map();
  const pageCtl = new AbortController();
  const cache = { analytics: null, aSig: '', series: null, sSig: '', credits: [], perf: null, pSig: 'none', recent: null, prev: null, heat: null };
  let period = D.resolvePeriod(state.range, state.custom);
  let loadedKey = '', logSig = '', perfAt = 0, prevKey = '', heatAt = 0, heatBusy = false, heatDirty = true;
  let derivedSpeed = null, namesSig = '', updatedAt = 0;
  let seq = 0, ctl = null;

  /* ---------------------------------------------------------------- cards */
  const kpis = kpiRow();
  const series = seriesCard({ state, save });
  const breakdown = breakdownCard({ state, save });
  const perf = perfCard();
  const heat = heatCard({ state });
  const activity = activityCard({ names, state });
  const notes = notesCard();

  /* -------------------------------------------------------------- toolbar */
  const sub = h('p');
  const stale = badge(t('数据未能更新'), 'warn', { dot: true });
  stale.hidden = true;
  const errHost = h('div');
  const customHint = h('span.an-hint.tx-bad', { hidden: true }, t('日期范围无效（最长 400 天，开始不能晚于结束）'));
  const dateInput = (key, label) => h('input.input.sm', {
    type: 'date', value: state.custom[key], max: D.ymd(Date.now() / 1000), 'aria-label': label,
    onChange: (e) => { state.custom[key] = e.target.value; applyCustom(); },
  });
  const fromIn = dateInput('from', t('开始日期'));
  const toIn = dateInput('to', t('结束日期'));
  const customBox = h('div.an-custom', { hidden: state.range !== 'custom' }, fromIn, h('span.faint', '–'), toIn, customHint);

  function applyCustom() {
    const bad = D.customError(state.custom);
    customHint.hidden = !bad;
    fromIn.setAttribute('aria-invalid', String(!!bad));
    toIn.setAttribute('aria-invalid', String(!!bad));
    if (bad) return;
    save();
    reload();
  }

  const rangeSeg = seg([
    { value: 'today', label: t('今日') }, { value: 'week', label: t('本周') }, { value: 'month', label: t('本月') },
    { value: 'd3', label: t('近3天') }, { value: 'd30', label: t('近30天') }, { value: 'custom', label: t('自定义') },
  ], state.range, (v) => {
    state.range = v;
    customBox.hidden = v !== 'custom';
    save();
    if (v !== 'custom' || !D.customError(state.custom)) reload();
    else applyCustom();
  });
  rangeSeg.classList.add('an-range');

  const metricSeg = seg([
    { value: 'tokens', label: 'Token' }, { value: 'requests', label: t('请求') }, { value: 'credit', label: t('积分') }, { value: 'cost', label: t('费用') },
  ], state.metric, (v) => {
    state.metric = v;
    save();
    series.redraw();
    breakdown.setMetric();
    heat.redraw();
  });
  metricSeg.classList.add('an-metric');

  const refreshBtn = btn('', { iconOnly: true, icon: 'refresh', tip: t('刷新'), onClick: () => cycle({ force: true }) });
  refreshBtn.setAttribute('aria-label', t('刷新'));

  const head = h('div.page-head',
    h('div', h('h1', t('数据看板')), sub),
    h('div.page-actions.an-actions', rangeSeg, metricSeg, stale, refreshBtn));

  root.classList.add('p-analytics');
  root.append(h('div.stack',
    head, customBox, errHost, kpis.el,
    h('div.an-grid-main', series.el, perf.el),
    breakdown.el,
    heat.el, activity.el,
    notes.el));

  /* --------------------------------------------------------------- render */
  const nowSec = () => Math.floor(Date.now() / 1000);

  function paintHead() {
    const a = F.date(period.start), b = F.date(period.end - 1);
    const label = a === b ? a : a + ' – ' + b;
    setText(sub, updatedAt ? `${label} · ${t('更新于 {t}', { t: F.time(updatedAt) })}` : label);
  }

  function renderAll() {
    const a = cache.analytics;
    if (!a || !cache.series) return;
    const cur = (a.summary && a.summary.window) || {};
    const fill = D.fillSeries(cache.series, period, nowSec());
    const costSeries = D.allocCost(fill, cur.cost_cny);
    const fsig = `${cache.sSig}|${fill.n}|${cache.aSig}|${state.pricingOn}`;
    const prevSig = cache.prev ? D.sigOf(cache.prev) : 'none';

    if (namesSig !== cache.aSig) {
      namesSig = cache.aSig;
      names.clear();
      for (const acc of a.accounts || []) names.set(acc.uid, acc.nickname || String(acc.uid).slice(0, 8));
      activity.refresh();
    }
    kpis.update({ cur, prev: cache.prev, fill, costSeries, usd: a.usd_cny, pricingOn: state.pricingOn, sig: fsig + '|' + prevSig });
    series.update({ fill, costSeries, sig: fsig });
    breakdown.update({ a, sig: cache.aSig });
    const peakTps = D.peak(fill.completion.map((v) => v / fill.step)).value;
    perf.update({ perf: cache.perf, cur, derivedSpeed, peakTps, sig: `${cache.pSig}|${cache.aSig}|${derivedSpeed}|${Math.round(peakTps)}` });
    activity.updateCredits(cache.credits);
    notes.update({ pricingOn: state.pricingOn, rate: a.usd_cny, recentRows: cache.recent ? cache.recent.rows : [] });
    paintHead();
  }

  function ingestRecent(res) {
    cache.recent = res;
    let c = 0, g = 0;
    for (const r of res.rows || []) {
      if ((r.outcome || 'completed') === 'completed' && r.gen_ms > 0 && r.completion_tokens > 0) { c += r.completion_tokens; g += r.gen_ms; }
    }
    derivedSpeed = g > 0 ? c / (g / 1000) : null;
    activity.updateRecent(res);
  }

  /* ----------------------------------------------------------------- data */
  async function loadHeavy(p, signal, full) {
    const at = nowSec();
    const wantPerf = full || !cache.perf || at - perfAt >= 60;
    const quiet = (pr) => pr.catch((e) => { if (signal.aborted) throw e; return null; });
    const [a, s, pf] = await Promise.all([
      D.fetchAnalytics(p, signal),
      D.fetchSeries(p, D.stepFor(p), signal),
      wantPerf ? quiet(D.fetchPerf(p, signal)) : null,
    ]);
    return { a, s, pf, wantPerf, at };
  }

  function commit(r, key) {
    const periodChanged = key !== loadedKey;
    loadedKey = key;
    cache.analytics = r.a;
    cache.aSig = D.sigOf(r.a);
    cache.series = r.s;
    cache.sSig = D.sigOf({ s: r.s.series, c: r.s.credits, b: r.s.bucket_seconds });
    cache.credits = (r.s.credits || []);
    if (r.wantPerf) {
      cache.perf = r.pf;
      cache.pSig = r.pf ? D.sigOf({ ...r.pf, by_model: 0, by_model_realm: 0, by_model_acct: 0 }) : 'none';
      perfAt = r.at;
    }
    if (periodChanged) cache.prev = null;
  }

  function loadPrev() {
    const w = D.prevWindow(period, nowSec());
    const k = w ? w.start + ':' + w.end : 'none';
    if (k === prevKey) return;
    prevKey = k;
    if (!w) { cache.prev = null; renderAll(); return; }
    D.fetchAnalytics(w, pageCtl.signal).then((r) => {
      if (prevKey !== k) return;
      cache.prev = (r.summary && r.summary.window) || null;
      renderAll();
    }).catch(() => { if (prevKey === k) cache.prev = null; });
  }

  function loadHeat() {
    const at = nowSec();
    if (heatBusy || (cache.heat && !(heatDirty && at - heatAt >= 900))) return;
    heatBusy = true;
    D.fetchHeat(at, pageCtl.signal).then((r) => {
      r.__sig = D.sigOf(r.series);
      cache.heat = r; heatAt = at; heatDirty = false;
      heat.update(r);
    }).catch(() => { heatAt = at; }).finally(() => { heatBusy = false; });
  }

  async function run(id, signal, force) {
    const nowMs = Date.now();
    const p = D.resolvePeriod(state.range, state.custom, nowMs);
    const key = p.start + ':' + p.end;
    period = p;
    const wrap = (pr) => pr.then((v) => ({ v }), (e) => ({ e }));
    const recentP = D.fetchRecent(signal);
    let heavy = null;
    if (force || key !== loadedKey || !cache.analytics) heavy = wrap(loadHeavy(p, signal, true));
    const recent = await recentP;
    if (id !== seq) return;
    const sig = (recent.total || 0) + '|' + ((recent.rows && recent.rows[0] && recent.rows[0].at) || 0);
    const logChanged = sig !== logSig;
    if (!heavy && p.live && logChanged) heavy = wrap(loadHeavy(p, signal, false));
    ingestRecent(recent);
    if (heavy) {
      const r = await heavy;
      if (id !== seq) return;
      if (r.e) throw r.e;
      commit(r.v, key);
    }
    logSig = sig;
    if (logChanged) heatDirty = true;
    updatedAt = nowSec();
    errHost.replaceChildren();
    stale.hidden = true;
    renderAll();
    loadPrev();
    loadHeat();
  }

  function markFailed(e) {
    if (cache.analytics) { stale.hidden = false; return; }
    errHost.replaceChildren(alertBox('bad', h('b', t('数据加载失败')), ' ', errText(e), ' ',
      btn(t('重试'), { size: 'xs', onClick: () => cycle({ force: true }) })));
  }

  async function cycle({ force = false, signal } = {}) {
    if (ctl && !force) return;
    if (ctl) ctl.abort();
    const mine = (ctl = new AbortController());
    const id = ++seq;
    const onAbort = () => mine.abort();
    if (signal) { if (signal.aborted) mine.abort(); else signal.addEventListener('abort', onAbort, { once: true }); }
    if (force) root.classList.add('is-busy');
    try {
      await run(id, mine.signal, force);
    } catch (e) {
      if (e && e.name === 'AbortError') return;
      if (id === seq) markFailed(e);
      if (!force) throw e;
    } finally {
      if (ctl === mine) { ctl = null; root.classList.remove('is-busy'); }
      if (signal) signal.removeEventListener('abort', onAbort);
    }
  }

  function reload() {
    cache.prev = null;
    return cycle({ force: true });
  }

  const poll = poller((signal) => cycle({ signal }), POLL_MS);

  loadSettings().then((s) => {
    const on = !s || s.pricing_enabled !== false;
    if (on === state.pricingOn) return;
    state.pricingOn = on;
    activity.refresh();
    renderAll();
  }).catch(() => {});

  paintHead();
  poll.start();

  return () => {
    poll.stop();
    pageCtl.abort();
    if (ctl) ctl.abort();
    series.dispose();
    sc.dispose();
  };
}
