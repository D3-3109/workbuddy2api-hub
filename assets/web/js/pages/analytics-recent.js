// Activity card: live "recent requests" feed and the credit-deduction list of the selected range.
import { h, reconcile, setText } from '../lib/dom.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { btn, empty, seg, sorter, sortHeader } from '../lib/ui.js';
import { outcomeBadge, costTip, costText } from '../lib/shared.js';

add({
  '动态': 'Activity', '最近请求': 'Recent requests', '积分扣减': 'Credit deductions',
  '时间': 'Time', '模型': 'Model', '账号@@col': 'Account', '结果': 'Result', '耗时': 'Duration', '费用': 'Cost', '积分': 'Credits',
  '共 {n} 条记录（全部时间）': '{n} records in total (all time)', '暂无请求记录': 'No requests yet', '该区间没有积分扣减记录': 'No credit deductions in this range',
  '第 {p} / {n} 页': 'Page {p} / {n}', '上一页': 'Prev', '下一页': 'Next',
  '完成': 'Done', '客户端取消': 'Client abort', '上游中断': 'Upstream abort', '失败': 'Failed',
  '按 OpenRouter 公布的模型价折算的等价 token 花费': 'Equivalent token cost at OpenRouter list prices', '定价依据：出厂快照': 'Pricing: bundled snapshot',
  '定价策略：{s}': 'Pricing policy: {s}', '匹配：直接匹配 {id}': 'Match: direct {id}', '匹配：人工映射 {a} → {b}': 'Match: manual {a} → {b}', '匹配：变体继承 {a} → {b}': 'Match: variant {a} → {b}',
  '单价 (每百万)：缓存命中 {a} · 未命中 {b} · 输出 {c}': 'Per 1M: cache hit {a} · miss {b} · output {c}',
});

const PAGE = 10;
const stamp = (iso, at) => (iso ? iso.replace('T', ' ').slice(5) : F.mdHm(at));
const rowKey = (seen, r) => {
  const base = `${r.at}|${r.account}|${r.model}`;
  const n = seen.get(base) || 0;
  seen.set(base, n + 1);
  return n ? `${base}#${n}` : base;
};

export function activityCard({ names, state }) {
  let tab = 'recent', cpage = 1;
  let recent = null, credits = [];
  const acctName = (uid) => names.get(uid) || (uid ? String(uid).slice(0, 8) : '—');

  /* ---- recent ---- */
  const rBody = h('tbody');
  const rTable = h('table.table.compact.an-rtable',
    h('thead', h('tr', h('th', t('时间')), h('th', t('模型')), h('th.hide-sm', t('账号@@col')), h('th', t('结果')), h('th.num', 'Token'), h('th.num.hide-sm', t('耗时')), h('th.num', t('费用')))),
    rBody);
  const rEmpty = h('div', { hidden: true }, empty('inbox', t('暂无请求记录')));
  const rFoot = h('div.card-foot');
  const rWrap = h('div', h('div.table-wrap', rTable), rEmpty, rFoot);

  function makeRecent(r) {
    const acct = h('span.trunc', { style: 'display:block;max-width:130px' });
    const price = h('td.num.nowrap');
    const tr = h('tr',
      h('td.nowrap.muted', { style: 'font-variant-numeric:tabular-nums' }, stamp(r.iso, r.at)),
      h('td', h('span.mono.trunc', { style: 'display:block;max-width:190px;font-size:12.5px' }, r.model || '—')),
      h('td.hide-sm', acct),
      h('td', outcomeBadge(r)),
      h('td.num', F.compact(r.total_tokens || 0)),
      h('td.num.nowrap.hide-sm', F.ms(r.elapsed_ms)),
      price);
    tr.__r = { acct, price, row: r };
    return tr;
  }
  function updateRecent(tr) {
    const { acct, price, row } = tr.__r;
    const name = acctName(row.account);
    const sig = name + '|' + state.pricingOn;
    if (tr.__sig === sig) return;
    tr.__sig = sig;
    setText(acct, name);
    if (row.account) acct.setAttribute('data-tip', row.account);
    const on = state.pricingOn && row.cost_cny != null;
    price.replaceChildren(state.pricingOn ? costText(row.cost_cny) : '—', on && row.cost_backfilled ? h('span.faint', '*') : '');
    const tip = on ? costTip(row) : '';
    if (tip) price.setAttribute('data-tip', tip); else price.removeAttribute('data-tip');
  }
  function renderRecent() {
    if (!recent) return;
    const rows = recent.rows || [];
    const seen = new Map();
    reconcile(rBody, rows, (r) => rowKey(seen, r), (r) => { const tr = makeRecent(r); updateRecent(tr); return tr; }, updateRecent);
    rTable.hidden = !rows.length;
    rEmpty.hidden = !!rows.length;
    rFoot.hidden = !rows.length;
    setText(rFoot, t('共 {n} 条记录（全部时间）', { n: F.int(recent.total || 0) }));
  }

  /* ---- credits ---- */
  const sort = sorter('at', 'desc', () => { cHeads.forEach((th) => th.refresh()); cpage = 1; renderCredits(); });
  const cCols = [
    { id: 'at', label: t('时间'), get: (r) => r.at },
    { id: 'model', label: t('模型'), get: (r) => r.model },
    { id: 'account', label: t('账号@@col'), cls: 'hide-sm', get: (r) => acctName(r.account) },
    { id: 'credit', label: t('积分'), num: true, get: (r) => r.credit },
    { id: 'tokens', label: 'Token', num: true, cls: 'hide-sm', get: (r) => r.total_tokens },
  ];
  const getters = Object.fromEntries(cCols.map((c) => [c.id, c.get]));
  const cHeads = cCols.map((c) => { const th = sortHeader(c.label, c.id, sort, { num: c.num }); if (c.cls) th.classList.add(c.cls); return th; });
  const cBody = h('tbody');
  const cTable = h('table.table.compact.an-ctable', h('thead', h('tr', ...cHeads)), cBody);
  const cEmpty = h('div', { hidden: true }, empty('coins', t('该区间没有积分扣减记录')));
  const pageInfo = h('span');
  const prev = btn(t('上一页'), { size: 'xs', onClick: () => { cpage--; renderCredits(); } });
  const next = btn(t('下一页'), { size: 'xs', onClick: () => { cpage++; renderCredits(); } });
  const cFoot = h('div.card-foot.row.between.wrap', pageInfo, h('div.row.gap-1', prev, next));
  const cWrap = h('div', { hidden: true }, h('div.table-wrap', cTable), cEmpty, cFoot);

  function renderCredits() {
    const sorted = sort.apply(credits, getters);
    const pages = Math.max(1, Math.ceil(sorted.length / PAGE));
    cpage = Math.min(Math.max(1, cpage), pages);
    const slice = sorted.slice((cpage - 1) * PAGE, cpage * PAGE);
    const seen = new Map();
    const patch = (tr, r) => setText(tr.__acct, acctName(r.account));
    reconcile(cBody, slice, (r) => rowKey(seen, r), (r) => {
      const acct = h('span.trunc', { style: 'display:block;max-width:130px', 'data-tip': r.account || '' });
      const tr = h('tr',
        h('td.nowrap.muted', { style: 'font-variant-numeric:tabular-nums' }, stamp(r.iso, r.at)),
        h('td', h('span.mono.trunc', { style: 'display:block;max-width:200px;font-size:12.5px' }, r.model || '—')),
        h('td.hide-sm', acct),
        h('td.num.tx-good', F.dec(r.credit, 2)),
        h('td.num.hide-sm', F.int(r.total_tokens || 0)));
      tr.__acct = acct;
      patch(tr, r);
      return tr;
    }, patch);
    cTable.hidden = !credits.length;
    cEmpty.hidden = !!credits.length;
    cFoot.hidden = sorted.length <= PAGE;
    setText(pageInfo, t('第 {p} / {n} 页', { p: cpage, n: pages }));
    prev.disabled = cpage <= 1;
    next.disabled = cpage >= pages;
  }

  const tabSeg = seg([{ value: 'recent', label: t('最近请求') }, { value: 'credits', label: t('积分扣减') }], tab, (v) => {
    tab = v; rWrap.hidden = v !== 'recent'; cWrap.hidden = v !== 'credits';
  }, { size: 'sm' });
  const el = h('section.card.an-card.defer',
    h('div.card-head', h('h2', t('动态')), h('div.actions', tabSeg)),
    h('div.card-body.flush', rWrap, cWrap));

  return {
    el,
    updateRecent(res) { recent = res; renderRecent(); },
    updateCredits(list) {
      if (list === credits) return;
      credits = list; cpage = 1;
      renderCredits();
    },
    /** Re-run row patches after names / pricing state changed. */
    refresh() { renderRecent(); renderCredits(); },
  };
}
