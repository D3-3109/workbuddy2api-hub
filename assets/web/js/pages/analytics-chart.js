// Main time-series card of the Analytics page.
// The API has no per-model / per-account series, so "stacked" means the composition of each bucket
// (input / cached / output / reasoning tokens, or ok / failed requests).
import { h } from '../lib/dom.js';
import { timeChart } from '../lib/chart.js';
import { seg, empty } from '../lib/ui.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { DAY, sum, peak, fmtMetric } from './analytics-data.js';

add({
  '用量趋势': 'Usage trend', '面积': 'Area', '柱状': 'Bars', '合计': 'Total', '构成': 'Breakdown',
  '{n} 分钟粒度': '{n}-min buckets', '{n} 小时粒度': '{n}-hour buckets', '{n} 天粒度': '{n}-day buckets',
  '总计': 'Total', '峰值': 'Peak', '日均': 'Daily avg', '时均': 'Hourly avg',
  '输入（未缓存）': 'Input (uncached)', '输入（缓存命中）': 'Input (cache hit)', '思考': 'Reasoning', '成功': 'Succeeded',
  '该区间暂无用量': 'No usage in this range', '换个时间范围看看，或等待新的请求。': 'Try another range, or wait for new requests.',
  '分时费用按积分占比估算，区间总额与上方一致。': 'Per-bucket cost is estimated from each bucket’s share of credits; the range total matches the cards above.',
  '趋势图': 'Usage chart', '趋势图只统计成功的请求，因此 Token 合计会略低于上方卡片。': 'The chart counts completed requests only, so the token total can be slightly below the card above.',
});

const NICE = [900, 1800, 3600, 7200, 10800, 21600, 43200, DAY, 2 * DAY, 7 * DAY, 14 * DAY, 28 * DAY];

/**
 * timeChart labels every ceil(len / ticks)-th point. Pick a tick count (and, if needed, a few
 * trailing empty slots) so the spacing lands on round clock intervals: 1 h, 3 h, 1 day, 7 days ...
 */
function layoutTicks(n, step, width) {
  const pw = Math.max(120, (width || 640) - 66);
  const max = Math.max(2, Math.min(8, Math.floor(pw / 90)));
  let best = null;
  for (const d of NICE) {
    if (d % step) continue;
    const e = d / step;
    for (let want = 2; want <= max; want++) {
      if (e * want < n) continue;
      const len = Math.max(n, (e - 1) * want + 1);
      const pad = len - n;
      if (!best || pad < best.pad || (pad === best.pad && want > best.ticks)) best = { len, ticks: want, pad };
    }
  }
  if (!best || best.pad > Math.max(3, Math.ceil(n * 0.12))) return { len: n, ticks: max };
  return best;
}

const padTo = (a, len) => (a.length >= len ? a : a.concat(new Array(len - a.length).fill(0)));

function stepLabel(step) {
  if (step >= DAY) return t('{n} 天粒度', { n: Math.round(step / DAY) });
  if (step >= 3600) return t('{n} 小时粒度', { n: Math.round(step / 3600) });
  return t('{n} 分钟粒度', { n: Math.round(step / 60) });
}

export function seriesCard({ state, save }) {
  const view = { step: 3600, metric: 'tokens' };
  const host = h('div.an-chart-host');
  const emptyBox = h('div.an-chart-empty', { hidden: true }, empty('chart', t('该区间暂无用量'), t('换个时间范围看看，或等待新的请求。')));
  const skel = h('span.skel.an-chart-skel');
  const legend = h('div.legend.an-legend');
  const note = h('div.an-note', { hidden: true }, t('分时费用按积分占比估算，区间总额与上方一致。'));
  const statsEl = h('div.an-stats');
  const sub = h('span.sub');

  const typeSeg = seg([{ value: 'area', label: t('面积') }, { value: 'bar', label: t('柱状') }], state.chartType, (v) => {
    state.chartType = v; save(); mk(); draw(true);
  }, { size: 'sm' });
  const stackSeg = seg([{ value: 'total', label: t('合计') }, { value: 'parts', label: t('构成') }], state.stack, (v) => {
    state.stack = v; save(); draw(true);
  }, { size: 'sm' });

  const el = h('section.card.an-card',
    h('div.card-head', h('h2', t('用量趋势')), sub, h('div.actions', stackSeg, typeSeg)),
    h('div.card-body', statsEl, h('div.an-chart-wrap', { 'aria-label': t('趋势图') }, skel, host, emptyBox), legend, note));

  const opts = {
    height: 280,
    maxTicksX: 7,
    fmtX: (x) => {
      if (view.step >= DAY) return F.md(x + 12 * 3600);
      const d = new Date(x * 1000);
      if (d.getHours() === 0 && d.getMinutes() === 0) return F.md(x);
      return view.multiDay ? F.md(x) + ' ' + F.hm(x) : F.hm(x);
    },
    fmtTipX: (x) => (view.step >= DAY ? F.date(x + 12 * 3600) : F.mdHm(x) + ' – ' + F.hm(x + view.step)),
    fmtY: (v) => (view.metric === 'cost' ? '¥' + F.compact(v) : F.compact(v)),
    fmtTip: (v) => (view.metric === 'cost' ? '≈ ' + fmtMetric('cost', v) : fmtMetric(view.metric, v, true)),
  };
  let chart = null;
  // timeChart reads stacked / maxTicksX lazily, so keep a live reference to the options object it got.
  let live = null;
  function mk() {
    if (chart) chart.dispose();
    host.replaceChildren();
    live = { ...opts, type: state.chartType, stacked: false };
    chart = timeChart(host, live);
  }
  mk();

  let data = null, lastSig = '', lastW = 0, raf = 0;

  function parts(d) {
    const f = d.fill, metric = state.metric, split = state.stack === 'parts';
    if (metric === 'tokens') {
      if (!split) return [{ name: 'Token', values: f.total, color: 'var(--c1)' }];
      return [
        { name: t('输入（未缓存）'), values: f.prompt.map((v, i) => Math.max(0, v - f.cached[i])), color: 'var(--c2)' },
        { name: t('输入（缓存命中）'), values: f.cached, color: 'var(--c3)' },
        { name: t('输出'), values: f.completion.map((v, i) => Math.max(0, v - f.reasoning[i])), color: 'var(--c1)' },
        { name: t('思考'), values: f.reasoning, color: 'var(--c4)' },
      ];
    }
    if (metric === 'requests') {
      if (!split) return [{ name: t('请求数'), values: f.req, color: 'var(--c2)' }];
      return [{ name: t('成功'), values: f.req, color: 'var(--c3)' }, { name: t('失败'), values: f.err, color: 'var(--bad)' }];
    }
    if (metric === 'credit') return [{ name: t('积分'), values: f.credit, color: 'var(--c4)' }];
    return [{ name: t('估算费用'), values: d.costSeries, color: 'var(--c5)' }];
  }

  function paintStats(d, main) {
    const f = d.fill, total = sum(main), pk = peak(main);
    const perDay = f.n * f.step >= 3 * DAY;
    const units = Math.max(1, (f.n * f.step) / (perDay ? DAY : 3600));
    const when = pk.index >= 0 ? (f.step >= DAY ? F.md(f.at[pk.index] + 12 * 3600) : F.mdHm(f.at[pk.index])) : '';
    const tile = (label, value, subText, tip) => {
      const el = h('div.an-stat', h('span.l', label), h('b', value), subText ? h('span.s', subText) : null);
      if (tip) el.setAttribute('data-tip', tip);
      return el;
    };
    const completedOnly = state.metric === 'tokens' ? t('趋势图只统计成功的请求，因此 Token 合计会略低于上方卡片。') : '';
    statsEl.replaceChildren(
      tile(t('总计'), fmtMetric(state.metric, total), '', completedOnly),
      tile(t('峰值'), fmtMetric(state.metric, pk.value), when),
      tile(perDay ? t('日均') : t('时均'), fmtMetric(state.metric, total / units), ''));
  }

  function draw(force) {
    if (!data) return;
    const d = data, f = d.fill;
    const width = host.clientWidth;
    const sig = [d.sig, state.metric, state.stack, state.chartType, Math.round(width / 40)].join('|');
    if (!force && sig === lastSig) return;
    lastSig = sig;
    skel.hidden = true;
    sub.textContent = stepLabel(f.step);
    view.step = f.step; view.metric = state.metric; view.multiDay = f.n * f.step > 1.2 * DAY;
    const split = state.stack === 'parts' && (state.metric === 'tokens' || state.metric === 'requests');
    stackSeg.hidden = !(state.metric === 'tokens' || state.metric === 'requests');
    note.hidden = state.metric !== 'cost';
    const series = parts(d);
    const main = state.metric === 'tokens' ? f.total : state.metric === 'requests' ? f.req : state.metric === 'credit' ? f.credit : d.costSeries;
    const hasData = sum(f.total) + sum(f.req) + sum(f.err) + sum(f.credit) > 0;
    emptyBox.hidden = hasData;
    host.hidden = !hasData;
    paintStats(d, main);
    legend.replaceChildren(...series.map((s) => h('span', h('i', { style: { background: s.color } }), s.name)));
    if (!hasData) return;
    live.stacked = split;
    const lay = layoutTicks(f.n, f.step, width);
    live.maxTicksX = lay.ticks;
    const x = new Array(lay.len);
    for (let i = 0; i < lay.len; i++) x[i] = f.s0 + i * f.step;
    chart.update({ x, series: series.map((s) => ({ ...s, values: padTo(s.values, lay.len) })) });
  }

  // Re-run the tick layout when the card width changes (the chart redraws itself first).
  const ro = new ResizeObserver(() => {
    const w = host.clientWidth;
    if (w === lastW || !w) return;
    lastW = w;
    cancelAnimationFrame(raf);
    raf = requestAnimationFrame(() => draw(true));
  });
  ro.observe(host);

  return {
    el,
    /** d = { fill, costSeries, sig } */
    update(d) { data = d; draw(false); },
    redraw() { draw(true); },
    dispose() { cancelAnimationFrame(raf); ro.disconnect(); if (chart) chart.dispose(); },
  };
}
