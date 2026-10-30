// KPI row of the Analytics page: six tiles with period-over-period deltas and sparklines.
import { h } from '../lib/dom.js';
import { kpi } from '../lib/ui.js';
import { sparkline } from '../lib/chart.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { costText } from '../lib/shared.js';
import { fmtMetric, fullTip, rateSpark } from './analytics-data.js';

add({
  '请求数': 'Requests', 'Token 用量': 'Token usage', '积分消耗': 'Credits used', '估算费用': 'Est. cost', '成功率': 'Success rate', '缓存命中': 'Cache hit',
  '平均耗时 {v}': 'Avg time {v}', '输入 {a} · 输出 {b}': 'In {a} · Out {b}', '每次请求 {v}': '{v} per request', '≈ ${v}': '≈ ${v}',
  '失败 {n} 次': '{n} failed', '复用 {n} tokens': '{n} tokens reused', '价格估算已关闭': 'Cost estimation is off',
  '上一周期同期：{v}': 'Same point last period: {v}', '{v} 个百分点': '{v} pt',
  '按 OpenRouter 公布的价格折算，不是账单': 'Converted at OpenRouter list prices; not a bill',
});

const tone = (kind, d) => {
  if (Math.abs(d) < 0.05) return 'muted';
  if (kind === 'cost') return d > 0 ? 'bad' : 'good';
  if (kind === 'quality') return d > 0 ? 'good' : 'bad';
  return 'muted';
};

/** Relative change in %, or percentage points for rates. Null when there is nothing to compare. */
function deltaChip(cur, prev, { kind = 'volume', pp = false, prevText = '' } = {}) {
  if (prev == null || cur == null || !Number.isFinite(prev) || !Number.isFinite(cur)) return null;
  let d;
  if (pp) d = cur - prev;
  else { if (!prev) return null; d = ((cur - prev) / Math.abs(prev)) * 100; }
  const flat = Math.abs(d) < 0.05;
  const text = (flat ? '– ' : d > 0 ? '▲ ' : '▼ ') + Math.abs(d).toFixed(1) + (pp ? ' pt' : '%');
  const el = h('span.an-delta', { 'data-tone': tone(kind, d) }, text);
  if (prevText) el.setAttribute('data-tip', t('上一周期同期：{v}', { v: prevText }));
  return el;
}

const rateOf = (s) => { const a = (s.requests || 0) + (s.errors || 0); return a > 0 ? ((s.requests || 0) / a) * 100 : null; };

function sparkOf(values, color) {
  return values.length > 1 && values.some((v) => v) ? sparkline(values, { color }) : '';
}

export function kpiRow() {
  const defs = [
    { id: 'req', label: t('请求数'), icon: 'activity', color: 'var(--c2)' },
    { id: 'tok', label: t('Token 用量'), icon: 'layers', color: 'var(--c1)' },
    { id: 'credit', label: t('积分消耗'), icon: 'coins', color: 'var(--c4)' },
    { id: 'cost', label: t('估算费用'), icon: 'tag', color: 'var(--c5)' },
    { id: 'ok', label: t('成功率'), icon: 'shield', color: 'var(--good)' },
    { id: 'cache', label: t('缓存命中'), icon: 'zap', color: 'var(--c7)' },
  ];
  const cards = {};
  for (const d of defs) {
    const k = kpi({ label: d.label, icon: d.icon, cls: 'an-kpi' });
    k.set({ value: '—', foot: ' ' });
    cards[d.id] = { k, color: d.color, valueEl: k.el.querySelector('.kpi-value') };
  }
  const el = h('div.an-kpis', ...defs.map((d) => cards[d.id].k.el));
  let lastSig = '';

  function put(id, { value, foot, delta, spark, tip, valueColor }) {
    const c = cards[id];
    c.k.set({ value, foot: h('span.an-foot', delta || null, foot ? h('span.trunc', foot) : null) });
    c.k.spark.replaceChildren(spark || '');
    c.valueEl.style.color = valueColor || '';
    if (tip) c.valueEl.setAttribute('data-tip', tip); else c.valueEl.removeAttribute('data-tip');
  }

  /** m = { cur, prev, fill, usd, pricingOn, sig } */
  function update(m) {
    if (m.sig === lastSig) return;
    lastSig = m.sig;
    const { cur, prev, fill: f } = m;
    const rate = rateOf(cur);
    const prevRate = prev ? rateOf(prev) : null;
    const cacheNow = cur.prompt_tokens > 0 ? (cur.cached_tokens / cur.prompt_tokens) * 100 : null;
    const cachePrev = prev && prev.prompt_tokens > 0 ? (prev.cached_tokens / prev.prompt_tokens) * 100 : null;
    const usd = m.usd > 0 ? cur.cost_cny / m.usd : null;
    const chip = (key, mode, o = {}) => (prev ? deltaChip(cur[key], prev[key], { ...o, prevText: fmtMetric(mode, prev[key]) }) : null);

    put('req', {
      value: fmtMetric('requests', cur.requests),
      tip: fullTip(cur.requests),
      foot: cur.elapsed_ms_avg ? t('平均耗时 {v}', { v: F.ms(cur.elapsed_ms_avg) }) : '',
      delta: chip('requests', 'requests'),
      spark: sparkOf(f.req, cards.req.color),
    });
    put('tok', {
      value: F.compact(cur.total_tokens),
      tip: fullTip(cur.total_tokens),
      foot: t('输入 {a} · 输出 {b}', { a: F.compact(cur.prompt_tokens), b: F.compact(cur.completion_tokens) }),
      delta: chip('total_tokens', 'tokens'),
      spark: sparkOf(f.total, cards.tok.color),
    });
    put('credit', {
      value: fmtMetric('credit', cur.credit),
      tip: fullTip(cur.credit, 2),
      foot: cur.requests > 0 ? t('每次请求 {v}', { v: F.dec(cur.credit / cur.requests, 2) }) : '',
      delta: chip('credit', 'credit'),
      spark: sparkOf(f.credit, cards.credit.color),
    });
    if (m.pricingOn) {
      put('cost', {
        value: costText(cur.cost_cny),
        tip: t('按 OpenRouter 公布的价格折算，不是账单'),
        foot: usd == null ? '' : t('≈ ${v}', { v: F.fixed(usd, 2) }),
        delta: chip('cost_cny', 'cost', { kind: 'cost' }),
        spark: sparkOf(m.costSeries, cards.cost.color),
      });
    } else {
      put('cost', { value: '—', foot: t('价格估算已关闭') });
    }
    put('ok', {
      value: rate == null ? '—' : F.pct(rate, 1),
      valueColor: rate == null ? '' : rate >= 99 ? 'var(--good)' : rate >= 95 ? 'var(--warn)' : 'var(--bad)',
      foot: t('失败 {n} 次', { n: F.int(cur.errors || 0) }),
      delta: rate != null && prevRate != null ? deltaChip(rate, prevRate, { kind: 'quality', pp: true, prevText: F.pct(prevRate, 1) }) : null,
      spark: sparkOf(rateSpark(f.req.map((v, i) => { const a = v + f.err[i]; return a > 0 ? (v / a) * 100 : null; })), cards.ok.color),
    });
    put('cache', {
      value: F.pct(cur.cache_hit_pct ?? cacheNow, 1),
      foot: cur.cached_tokens > 0 ? t('复用 {n} tokens', { n: F.compact(cur.cached_tokens) }) : '',
      delta: cacheNow != null && cachePrev != null ? deltaChip(cacheNow, cachePrev, { kind: 'quality', pp: true, prevText: F.pct(cachePrev, 1) }) : null,
      spark: sparkOf(rateSpark(f.prompt.map((v, i) => (v > 0 ? (f.cached[i] / v) * 100 : null))), cards.cache.color),
    });
    el.setAttribute('aria-busy', 'false');
  }

  return { el, update };
}
