// "Recent requests" card of the Gateway page: paginated, live while on page 1.
import { h, reconcile, setText } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { get, qs } from '../lib/api.js';
import { t } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { badge, btn, seg, empty } from '../lib/ui.js';
import { outcomeBadge, tokensPerSec, cachePct, modelCell, costTip, costText, pageList } from '../lib/shared.js';
import { poller } from '../lib/poll.js';

const LIMITS = [10, 20, 50, 100];
const LIMIT_KEY = 'WB_RECENT_LIMIT';

function savedLimit() {
  try { const v = Number(localStorage.getItem(LIMIT_KEY)); if (LIMITS.includes(v)) return v; } catch { /* blocked */ }
  return 20;
}

export function recentCard({ view, names }) {
  let limit = savedLimit();
  let page = 1;
  let data = null;
  let loading = false;

  const tbody = h('tbody');
  const emptyBox = h('div', { hidden: true }, empty('inbox', t('还没有请求记录'), t('网关处理的请求会实时出现在这里。')));
  const pager = h('div.card-foot.row.between.wrap');
  const sub = h('span.sub');

  const th = (label, cls = '') => h('th' + (cls ? '.' + cls.split(' ').join('.') : ''), label);
  const thKey = th(t('Key'), 'hide-sm');
  const table = h('table.table.compact.recent',
    h('thead', h('tr',
      th(t('时间')), th(t('模型')), th(t('账号@@col'), 'hide-sm'), thKey, th(t('结果')),
      th(t('耗时'), 'num'), th(t('首字'), 'num hide-sm'), th(t('速度'), 'num hide-sm'), th(t('输入'), 'num'), th(t('输出'), 'num'),
      th(t('缓存'), 'num hide-sm'), th(t('积分'), 'num'), th(t('估算价格'), 'num'))),
    tbody);
  const limitSeg = seg(LIMITS.map((n) => ({ value: n, label: String(n) })), limit, (n) => {
    limit = n; page = 1;
    try { localStorage.setItem(LIMIT_KEY, String(n)); } catch { /* blocked */ }
    load();
  }, { size: 'sm' });

  const el = h('section.card',
    h('div.card-head', h('h2', t('最近请求')), sub, h('div.actions', h('span.muted.hide-sm', { style: 'font-size:12.5px' }, t('显示条数')), limitSeg)),
    h('div.table-wrap', table),
    emptyBox, pager);

  function makeRow(r) {
    const acct = names.get(r.account) || (r.account ? r.account.slice(0, 8) : '—');
    const tps = tokensPerSec(r), cache = cachePct(r);
    const price = h('td.num.nowrap', costText(r.cost_cny), r.cost_backfilled ? h('span.faint', '*') : null);
    const tip = costTip(r);
    if (tip) price.setAttribute('data-tip', tip);
    const acctCell = h('td.hide-sm', h('span.trunc', { style: 'display:block;max-width:120px' }, acct));
    if (r.account) acctCell.setAttribute('data-tip', r.account);
    return h('tr',
      h('td.nowrap.muted', { style: 'font-variant-numeric:tabular-nums' }, r.iso ? r.iso.replace('T', ' ').slice(5) : F.mdHm(r.at)),
      h('td', modelCell(r.model, r.reasoning_effort, r.stream ? badge(t('流式'), 'info', { sm: true }) : null)),
      acctCell,
      h('td.hide-sm.muted.key-col', r.key_name || '—'),
      h('td', outcomeBadge(r)),
      h('td.num.nowrap', F.ms(r.elapsed_ms)),
      h('td.num.nowrap.hide-sm', F.ms(r.ttft_ms)),
      h('td.num.hide-sm', tps ? F.dec(tps, 1) : '—'),
      h('td.num', F.int(r.prompt_tokens)),
      h('td.num', F.int(r.completion_tokens)),
      h('td.num.hide-sm', cache ? F.pct(cache, 0) : '—'),
      h('td.num.tx-good', r.credit ? F.fixed(r.credit, 2) : '—'),
      price);
  }

  function renderPager() {
    const total = data.total || 0, pages = data.total_pages || 1;
    const nums = pageList(page, pages).map((p) => p === '…'
      ? h('span.faint', { style: 'padding:0 4px' }, '…')
      : h('button.btn.xs' + (p === page ? '.primary' : '.ghost'), { type: 'button', onClick: () => { page = p; load(); } }, String(p)));
    pager.replaceChildren(
      h('span', t('第 {p} / {n} 页 · 共 {c} 条记录', { p: page, n: pages, c: F.int(total) })),
      h('div.row.gap-1',
        btn(t('上一页'), { size: 'xs', disabled: page <= 1, onClick: () => { page--; load(); } }),
        ...nums,
        btn(t('下一页'), { size: 'xs', disabled: page >= pages, onClick: () => { page++; load(); } })));
  }

  function render() {
    const rows = data.rows || [];
    const seen = new Map();
    reconcile(tbody, rows, (r) => {
      const base = `${r.at}|${r.account}|${r.model}`;
      const n = seen.get(base) || 0;
      seen.set(base, n + 1);
      return n ? `${base}#${n}` : base;
    }, makeRow);
    table.hidden = !rows.length;
    table.classList.toggle('no-keys', !rows.some((r) => r.key_name));
    emptyBox.hidden = !!rows.length;
    pager.hidden = !rows.length;
    setText(sub, view() === 'cn' ? t('（国内版）') : t('（国际版）'));
    if (rows.length) renderPager();
  }

  async function load(signal) {
    if (loading && !signal) return;
    loading = true;
    try {
      data = await get('/usage/recent' + qs({ limit, page, realm: view() }), { signal });
      page = data.page || page;
      render();
    } finally { loading = false; }
  }

  // Auto-refresh only while looking at the newest page.
  const live = poller(async (signal) => { if (page === 1) await load(signal); }, 15000, { immediate: false });

  return {
    el,
    start() { load().catch(() => {}); live.start(); },
    stop() { live.stop(); },
    reload() { page = 1; return load(); },
  };
}
