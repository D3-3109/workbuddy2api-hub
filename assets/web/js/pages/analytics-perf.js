// Performance panel: success rate and latency / speed percentiles from /usage/perf.
// The Rust gateway stores no tokens_per_sec; the speed row falls back to the analytics average
// (when rows carry a speed) or to a figure derived from the recent rows' completion/gen time.
import { h, setText } from '../lib/dom.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { skeleton } from '../lib/ui.js';

add({
  '性能': 'Performance', '{n} 个样本': '{n} samples', '成功率': 'Success rate', '峰值吞吐': 'Peak throughput',
  '首字延迟': 'Time to first token', '生成时长': 'Generation time', '全程耗时': 'Total time', '输出速度': 'Output speed',
  '平均': 'Avg', '成功 {a} · 失败 {b} · 取消 {c}': '{a} ok · {b} failed · {c} aborted',
  '{v} tok/s': '{v} tok/s', '网关未记录生成速度，按最近请求的输出量与生成耗时估算': 'The gateway does not store generation speed; estimated from recent requests',
  '输出 tokens ÷ 时间桶，不是单请求速度': 'Output tokens ÷ bucket length; not per-request speed',
  '样本只取区间内最近的记录，较长区间的分位数仅供参考。': 'Only the most recent rows are sampled, so percentiles for long ranges are indicative.',
  '暂无性能数据': 'No performance data yet', '时间桶内的平均值': 'Averaged over a time bucket',
});

const ms = (v) => F.ms(v);
const tps = (v) => (v == null ? '—' : t('{v} tok/s', { v: F.dec(v, 1) }));

export function perfCard() {
  const sub = h('span.sub');
  const body = h('div.an-perf-body', skeleton(4));
  const el = h('section.card.an-card', h('div.card-head', h('h2', t('性能')), sub), h('div.card-body', body));
  let lastSig = '';

  function pm(label, m, fmt, fallback) {
    const bar = h('div.an-pbar');
    if (m && m.p99 > 0) {
      const top = Math.max(m.p99, m.p90, m.p50, 1);
      bar.append(h('i.p99', { style: { width: '100%' } }), h('i.p90', { style: { width: Math.max(2, (m.p90 / top) * 100) + '%' } }), h('i.p50', { style: { width: Math.max(1, (m.p50 / top) * 100) + '%' } }));
    }
    const avg = m ? m.avg : fallback ? fallback.value : null;
    const tipText = !m && fallback ? fallback.tip : (m ? `max ${fmt(m.max)} · n=${F.int(m.samples)}` : '');
    return h('div.an-pm',
      h('div.an-pm-top', h('span.l', label), h('span.v', { 'data-tip': tipText }, h('small', t('平均')), ' ', h('b', avg == null ? '—' : fmt(avg)))),
      bar,
      h('div.an-pm-vals', h('span', 'P50 ', h('b', m ? fmt(m.p50) : '—')), h('span', 'P90 ', h('b', m ? fmt(m.p90) : '—')), h('span', 'P99 ', h('b', m ? fmt(m.p99) : '—'))));
  }

  /** d = { perf, cur, derivedSpeed, peakTps, sig } */
  function update(d) {
    if (d.sig === lastSig) return;
    lastSig = d.sig;
    const p = d.perf;
    if (!p) { body.replaceChildren(h('div.faint', t('暂无性能数据'))); setText(sub, ''); return; }
    setText(sub, t('{n} 个样本', { n: F.int(p.sampled) }));
    const rate = p.success_rate_pct;
    const speedFallback = d.cur.speed_n > 0 ? { value: d.cur.speed_avg, tip: '' }
      : d.derivedSpeed != null ? { value: d.derivedSpeed, tip: t('网关未记录生成速度，按最近请求的输出量与生成耗时估算') } : null;
    body.replaceChildren(...[
      h('div.an-ptiles',
        h('div.an-ptile',
          h('span.l', t('成功率')),
          h('b', { style: { color: rate == null ? '' : rate >= 99 ? 'var(--good)' : rate >= 95 ? 'var(--warn)' : 'var(--bad)' } }, rate == null ? '—' : F.pct(rate, 1)),
          h('span.s', t('成功 {a} · 失败 {b} · 取消 {c}', { a: F.int(p.success), b: F.int(p.errors), c: F.int(p.client_aborted) }))),
        h('div.an-ptile',
          h('span.l', t('峰值吞吐')),
          h('b', { 'data-tip': t('输出 tokens ÷ 时间桶，不是单请求速度') }, d.peakTps > 0 ? tps(d.peakTps) : '—'),
          h('span.s', t('时间桶内的平均值')))),
      pm(t('首字延迟'), p.ttft_ms, ms),
      pm(t('生成时长'), p.generation_ms, ms),
      pm(t('全程耗时'), p.wall_ms, ms),
      pm(t('输出速度'), p.tokens_per_sec, (v) => F.dec(v, 1) + ' tok/s', speedFallback && { value: speedFallback.value, tip: speedFallback.tip }),
      p.sample_capped ? h('div.an-note', t('样本只取区间内最近的记录，较长区间的分位数仅供参考。')) : null,
    ].filter(Boolean));
  }

  return { el, update };
}
