// Activity pattern: weekday x hour heat-map over the last 28 days (hourly timeseries buckets).
import { h, setText } from '../lib/dom.js';
import { heatmap } from '../lib/chart.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { skeleton } from '../lib/ui.js';
import { fmtMetric } from './analytics-data.js';

add({
  '活跃时段': 'Activity pattern', '最近 28 天': 'Last 28 days', '少': 'Less', '多': 'More',
  '最忙时段：{w} {h}': 'Busiest: {w} {h}', '最近 28 天没有请求': 'No requests in the last 28 days',
  '费用无分时数据，按积分显示': 'No hourly cost data; showing credits',
});

const SRC = { tokens: 'total_tokens', requests: 'requests', credit: 'credit', cost: 'credit' };
const pad = (n) => String(n).padStart(2, '0');

export function heatCard({ state }) {
  const host = h('div.an-heat');
  const sub = h('span.sub');
  const peakLine = h('div.an-heat-peak');
  const legend = h('div.an-heat-legend', h('span.faint', t('少')), ...[0, 1, 2, 3, 4].map((i) => h('i', { style: { background: `var(--hm${i})` } })), h('span.faint', t('多')));
  const skel = h('div', skeleton(5));
  const el = h('section.card.an-card.defer',
    h('div.card-head', h('h2', t('活跃时段')), sub, h('div.actions', legend)),
    h('div.card-body', skel, host, peakLine));
  host.hidden = true;

  const metricNow = () => (state.metric === 'cost' ? 'credit' : state.metric);
  const grid = heatmap(host, {
    rowLabels: Array.from({ length: 7 }, (_, i) => F.weekday(i)),
    tipFn: (r, c, v) => `${F.weekday(r)} ${pad(c)}:00–${pad((c + 1) % 24)}:00\n${fmtMetric(metricNow(), v, true)}`,
  });
  let res = null, lastSig = '';

  function draw(force) {
    if (!res) return;
    const sig = res.__sig + state.metric;
    if (!force && sig === lastSig) return;
    lastSig = sig;
    const key = SRC[state.metric];
    const m = Array.from({ length: 7 }, () => new Array(24).fill(0));
    for (const b of res.series || []) {
      const d = new Date(b.at * 1000);
      m[(d.getDay() + 6) % 7][d.getHours()] += b[key] || 0;
    }
    skel.hidden = true;
    host.hidden = false;
    grid.update(m);
    let best = 0, br = -1, bc = -1;
    m.forEach((row, r) => row.forEach((v, c) => { if (v > best) { best = v; br = r; bc = c; } }));
    const label = state.metric === 'cost' ? t('费用无分时数据，按积分显示') : '';
    setText(sub, t('最近 28 天') + (label ? ' · ' + label : ''));
    setText(peakLine, br < 0 ? t('最近 28 天没有请求') : t('最忙时段：{w} {h}', { w: F.weekday(br), h: `${pad(bc)}:00–${pad((bc + 1) % 24)}:00` }));
  }

  return {
    el,
    /** r = raw /usage/timeseries response; r.__sig is stamped by the caller */
    update(r) { res = r; draw(false); },
    redraw() { draw(true); },
  };
}
