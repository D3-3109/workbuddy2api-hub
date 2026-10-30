// Check-in / daily-activity history as a day-grouped timeline. Not polled (paging would jump):
// it loads on entry, on filter change, on refresh and after an action.
import { h, reconcile, setText } from '../lib/dom.js';
import { getOr, qs } from '../lib/api.js';
import { add, t } from '../lib/i18n.js';
import { card, btn, badge, seg, alertBox, empty, skeleton, errText } from '../lib/ui.js';
import { TRIGGER_LABEL } from './tasks-model.js';

add({
  '签到与活跃记录': 'Check-in and activity history', '今日': 'Today', '近 1 天': 'Last 1 day', '近 7 天': 'Last 7 days', '近 30 天': 'Last 30 days', '近 90 天': 'Last 90 days',
  '全部': 'All', '时间范围': 'Time range', '全部结果': 'All results', '仅成功': 'Success only', '仅失败': 'Failures only', '签到成功': 'Checked in', '打卡成功': 'Checked in', '这个区间里没有签到或活跃记录。': 'No check-in or activity records in this range.',
  '读取记录失败：{e}': 'Failed to load the history: {e}', '当前网关未提供签到与活跃记录': 'This gateway does not keep check-in history',
  '记录只包含真实的打卡尝试；被跳过的运行不会写入。': 'Only real attempts are recorded; skipped runs are not.',
  '已载入 {n} 条 / 共 {m} 条': 'Loaded {n} of {m}', '上一页': 'Prev', '下一页': 'Next', '第 {p} / {n} 页': 'Page {p} / {n}', '加载更多': 'Load more',
});

const RANGES = () => [['today', t('今日')], ['1d', t('近 1 天')], ['7d', t('近 7 天')], ['30d', t('近 30 天')], ['90d', t('近 90 天')], ['all', t('全部')]];
const RANGE_KEY = 'wb-tasks-history-range';
const PAGE = 30;
const STEP = 150;
const MAX = 1000;

function savedRange() {
  try { const v = localStorage.getItem(RANGE_KEY); if (RANGES().some(([k]) => k === v)) return v; } catch { /* blocked */ }
  return '7d';
}

export function historyCard(store) {
  const { view } = store;
  const triggers = TRIGGER_LABEL();
  const okLabel = view === 'cn' ? t('签到成功') : t('打卡成功');
  let range = savedRange();
  let result = '';
  let limit = STEP;
  let page = 1;
  let data = null;
  let error = '';
  let tok = 0;

  const sub = h('span.sub');
  const rangeSel = h('select.select.sm', { 'aria-label': t('时间范围'), onChange: () => {
    range = rangeSel.value;
    try { localStorage.setItem(RANGE_KEY, range); } catch { /* blocked */ }
    reload();
  } }, RANGES().map(([k, label]) => h('option', { value: k }, label)));
  rangeSel.value = range;
  const resultSeg = seg([{ value: '', label: t('全部结果') }, { value: 'ok', label: t('仅成功') }, { value: 'failed', label: t('仅失败') }], result, (v) => { result = v; reload(); }, { size: 'sm' });
  const refreshBtn = btn(t('刷新'), { iconOnly: true, icon: 'refresh', variant: 'ghost', size: 'sm', onClick: () => reload() });

  const stateBox = h('div');
  const list = h('ul.tk-tl');
  const pager = h('div.card-foot.row.between.wrap', { hidden: true });
  const el = card(t('签到与活跃记录'), { sub, flush: true, actions: [h('div.tk-filter.row.gap-1.wrap', rangeSel, resultSeg, refreshBtn)] },
    stateBox, list);
  el.appendChild(pager);

  async function load() {
    const my = ++tok;
    const params = { range, result, limit, task: view === 'cn' ? 'checkin' : 'daily_chat', uid: store.sel === 'all' ? '' : store.sel };
    try {
      const r = await getOr('/activity/history' + qs(params), { supported: false });
      if (my !== tok || store.disposed) return;
      data = r;
      error = '';
    } catch (e) {
      if (my !== tok || store.disposed) return;
      error = errText(e);
    }
    paint();
  }
  function reload() { limit = STEP; page = 1; return load(); }

  function itemEl(r) {
    return h('li.tk-ev.' + (r.ok ? 'ok' : 'bad'),
      h('span.tk-dot'),
      h('div.tk-ev-body',
        h('div.tk-ev-top',
          h('span.tk-time', String(r.ts).slice(11, 16)),
          h('b.trunc.tk-who', r.nickname || store.nameOf(r.uid)),
          badge(triggers[r.trigger] || triggers.unknown, '', { sm: true, outline: true }),
          badge(r.ok ? okLabel : t('失败'), r.ok ? 'good' : 'bad', { sm: true, dot: true })),
        r.message ? h('div.tk-ev-msg', { 'data-tip': r.message }, r.message) : null));
  }

  function items(rows) {
    const out = [];
    const seen = new Map();
    let day = '';
    for (const r of rows) {
      const d = String(r.ts).slice(0, 10);
      if (d !== day) { day = d; out.push({ key: 'd:' + d, day: d }); }
      const base = `${r.ts}|${r.uid}|${r.task}`;
      const n = seen.get(base) || 0;
      seen.set(base, n + 1);
      out.push({ key: 'e:' + base + '#' + n, row: r });
    }
    return out;
  }

  function showState(node) { stateBox.replaceChildren(...(node ? [node] : [])); }

  function paint() {
    setText(sub, store.sel === 'all' ? '' : store.nameOf(store.sel));
    const rows = data && data.rows ? data.rows : [];
    const pages = Math.max(1, Math.ceil(rows.length / PAGE));
    page = Math.min(page, pages);
    const slice = rows.slice((page - 1) * PAGE, page * PAGE);

    if (error) showState(h('div', { style: 'padding:16px 18px' }, alertBox('bad', h('div', t('读取记录失败：{e}', { e: error })), btn(t('重试'), { size: 'sm', onClick: () => load() }))));
    else if (!data) showState(h('div', { style: 'padding:16px 18px' }, skeleton(4)));
    else if (data.supported === false) showState(empty('info', t('当前网关未提供签到与活跃记录')));
    else if (!rows.length) showState(empty('inbox', t('这个区间里没有签到或活跃记录。'), t('记录只包含真实的打卡尝试；被跳过的运行不会写入。')));
    else showState(null);

    const showRows = !error && rows.length > 0;
    reconcile(list, showRows ? items(slice) : [], (it) => it.key,
      (it) => (it.day ? h('li.tk-day', it.day) : itemEl(it.row)));
    list.hidden = !showRows;

    pager.hidden = !showRows;
    if (!showRows) return;
    const total = data.total || rows.length;
    const more = rows.length < total && limit < MAX;
    pager.replaceChildren(
      h('span', t('已载入 {n} 条 / 共 {m} 条', { n: rows.length, m: total })),
      h('div.row.gap-1',
        btn(t('上一页'), { iconOnly: true, icon: 'chevronLeft', size: 'xs', variant: 'ghost', disabled: page <= 1, onClick: () => { page--; paint(); } }),
        h('span.nowrap', t('第 {p} / {n} 页', { p: page, n: pages })),
        btn(t('下一页'), { iconOnly: true, icon: 'chevronRight', size: 'xs', variant: 'ghost', disabled: page >= pages, onClick: () => { page++; paint(); } }),
        more ? btn(t('加载更多'), { size: 'xs', onClick: () => { limit = Math.min(MAX, limit + STEP); return load(); } }) : null));
  }

  paint();
  return { el, load, reload, paint };
}
