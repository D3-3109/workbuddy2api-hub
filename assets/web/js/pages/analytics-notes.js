// Pricing / cost notes and the features this Rust build does not provide (shown calmly, never requested).
import { h } from '../lib/dom.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { btn, unsupported } from '../lib/ui.js';

add({
  '说明': 'Notes', '估算费用按 OpenRouter 公布的模型价折算（1 USD = {rate} CNY），是等价 token 花费，不是实际账单。': 'Estimated cost is converted at OpenRouter list prices (1 USD = {rate} CNY). It is the equivalent token spend, not a bill.',
  '没有定价数据的模型按 0 计入，因此区间总额可能偏低。': 'Models without pricing data count as 0, so range totals can be slightly low.',
  '价格估算已关闭，费用相关数字显示为 —。可在「设置」中重新开启。': 'Cost estimation is off, so cost figures show —. You can turn it back on in Settings.',
  '最近 {n} 条请求里有 {m} 条没有定价数据。': '{m} of the last {n} requests have no pricing data.',
  '带 * 的价格由之后取到的价格补算。': 'A * marks a price back-filled from a later price list.',
  'Rust 版暂未提供': 'Not available in the Rust build', '剩余用量估算': 'Remaining-usage estimate', '积分获取历史': 'Credit grant history', '在线取价与模型映射': 'Online pricing refresh and mapping',
  '当前 Rust 版网关未实现此功能': 'Not available in the Rust gateway',
});

export function notesCard() {
  const lines = h('div.an-notes');
  const el = h('section.card.an-card.defer',
    h('div.card-head', h('h2', t('说明'))),
    h('div.card-body.col', { style: 'gap:12px' },
      lines,
      h('div.an-unsup',
        h('span.faint', t('Rust 版暂未提供')),
        ...[t('剩余用量估算'), t('积分获取历史'), t('在线取价与模型映射')].map((l) => unsupported(btn(l, { size: 'xs' }), t('当前 Rust 版网关未实现此功能'))))));
  let lastSig = '';
  return {
    el,
    /** d = { pricingOn, rate, recentRows } */
    update(d) {
      const unpriced = (d.recentRows || []).filter((r) => r.cost_cny == null).length;
      const backfilled = (d.recentRows || []).some((r) => r.cost_backfilled);
      const sig = [d.pricingOn, d.rate, unpriced, backfilled, (d.recentRows || []).length].join('|');
      if (sig === lastSig) return;
      lastSig = sig;
      const out = [];
      if (!d.pricingOn) out.push(h('p', t('价格估算已关闭，费用相关数字显示为 —。可在「设置」中重新开启。')));
      else {
        out.push(h('p', t('估算费用按 OpenRouter 公布的模型价折算（1 USD = {rate} CNY），是等价 token 花费，不是实际账单。', { rate: d.rate ? F.dec(d.rate, 2) : '—' })));
        out.push(h('p', t('没有定价数据的模型按 0 计入，因此区间总额可能偏低。')));
        if (unpriced) out.push(h('p.tx-warn', t('最近 {n} 条请求里有 {m} 条没有定价数据。', { n: d.recentRows.length, m: unpriced })));
        if (backfilled) out.push(h('p', t('带 * 的价格由之后取到的价格补算。')));
      }
      lines.replaceChildren(...out);
    },
  };
}
