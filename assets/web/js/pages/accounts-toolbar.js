// Accounts page: search / filter / sort toolbar, pager and the bulk-action bar.
import { h, setText, debounce } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { btn, menu, promptDialog } from '../lib/ui.js';
import { FILTERS, PRODUCTS, productLabel, SORT_KEYS, SORT_DIR } from './accounts-model.js';

add({
  '搜索昵称 / UID / 备注': 'Search nickname / UID / note', '搜索账号': 'Search accounts',
  '全部': 'All', '可用': 'Ready', '冷却 / 受限': 'Cooling / limited', '凭证失效': 'Re-login', '已停用': 'Disabled',
  '全部出站身份': 'All identities', '出站身份': 'Outbound identity', '排序': 'Sort', '切换排序方向': 'Toggle sort direction',
  '默认排序': 'Default order', '昵称': 'Nickname', '状态': 'Status', '剩余积分': 'Credits left', '积分到期': 'Credits expiry',
  '凭证到期': 'Token expiry', '权重': 'Weight', '今日 Token 用量': 'Tokens today', '加入时间': 'Added',
  '显示 {a}–{b} / 共 {n} 个': 'Showing {a}–{b} of {n}', '没有账号': 'No accounts', '每页': 'Per page', '每页 {n} 条': '{n} per page', '上一页': 'Prev', '下一页': 'Next', '第 {p} / {n} 页': 'Page {p} / {n}',
  '已选 {n} 个': '{n} selected', '选择全部 {n} 个筛选结果': 'Select all {n} filtered', '取消选择': 'Clear selection', '启用账号': 'Enable', '停用账号': 'Disable', '测试': 'Test',
  '刷新凭证': 'Refresh token', '刷新积分': 'Refresh credits', '更多': 'More', '设置权重…': 'Set weight…', '批量设置权重': 'Set weight', '权重 (1-100)': 'Weight (1-100)', '保存': 'Save',
  '请输入 1 到 100 的整数': 'Enter an integer from 1 to 100', '绑定出口': 'Assign exit', '直连': 'Direct', '自动均衡分配（仅未绑定）': 'Spread over slots (unbound only)', '代理槽': 'Proxy slots',
  '导出选中': 'Export selected', '删除': 'Delete', '批量操作': 'Bulk actions',
});

const SORT_LABEL = {
  default: '默认排序', name: '昵称', status: '状态', credits: '剩余积分', creditExp: '积分到期',
  tokenExp: '凭证到期', weight: '权重', usage: '今日 Token 用量', added: '加入时间',
};
const FILTER_LABEL = { all: '全部', ready: '可用', limited: '冷却 / 受限', expired: '凭证失效', disabled: '已停用' };
const PAGE_SIZES = [25, 50, 100, 200];

export function createToolbar({ state, sort, onChange }) {
  const input = h('input.input.sm', { type: 'search', placeholder: t('搜索昵称 / UID / 备注'), 'aria-label': t('搜索账号'), value: state.q, autocomplete: 'off', spellcheck: 'false' });
  input.addEventListener('input', debounce(() => { state.q = input.value.trim().toLowerCase(); onChange({ page: 1 }); }, 140));

  const chips = new Map();
  const chipRow = h('div.acct-chips', { role: 'group' }, FILTERS.map((k) => {
    const count = h('span.count');
    const b = h('button.chip', { type: 'button', 'aria-pressed': String(state.status === k), onClick: () => { state.status = k; onChange({ page: 1, save: true }); } }, t(FILTER_LABEL[k]), count);
    chips.set(k, { b, count });
    return b;
  }));

  const product = h('select.select.sm', { 'aria-label': t('出站身份'), onChange: () => { state.product = product.value; onChange({ page: 1, save: true }); } },
    h('option', { value: 'all' }, t('全部出站身份')), PRODUCTS.map((p) => h('option', { value: p.id }, p.short)));
  product.value = state.product;

  const sortSel = h('select.select.sm', { 'aria-label': t('排序'), onChange: () => { sort.set(sortSel.value); } },
    SORT_KEYS.map((k) => h('option', { value: k }, t(SORT_LABEL[k]))));
  const dirBtn = btn(t('切换排序方向'), { iconOnly: true, icon: 'arrowDown', size: 'sm', onClick: () => sort.flip() });

  const el = h('div.acct-toolbar',
    h('div.input-wrap.acct-search', icon('search', 15), input),
    chipRow,
    h('div.acct-selects', product, sortSel, dirBtn));

  return {
    el,
    paintSort() {
      sortSel.value = sort.key;
      dirBtn.hidden = sort.key === 'default';
      dirBtn.replaceChildren(icon(sort.dir === 'asc' ? 'arrowUp' : 'arrowDown', 15));
    },
    paintCounts(counts) {
      for (const [k, o] of chips) {
        o.b.setAttribute('aria-pressed', String(state.status === k));
        setText(o.count, counts[k]);
      }
    },
  };
}

/** Sort state shared by the toolbar select and the clickable column headers. */
export function createSort(initial, onChange) {
  const s = {
    key: SORT_KEYS.includes(initial.key) ? initial.key : 'default',
    dir: initial.dir === 'asc' ? 'asc' : 'desc',
    toggle(key) {
      if (s.key === key) s.dir = s.dir === 'asc' ? 'desc' : 'asc'; else { s.key = key; s.dir = SORT_DIR[key] || 'asc'; }
      onChange();
    },
    set(key) { s.key = key; s.dir = SORT_DIR[key] || 'asc'; onChange(); },
    flip() { s.dir = s.dir === 'asc' ? 'desc' : 'asc'; onChange(); },
  };
  return s;
}

export function createPager({ prefs, onChange }) {
  const info = h('span.muted');
  const size = h('select.select.sm', { 'aria-label': t('每页'), onChange: () => { prefs.pageSize = Number(size.value); onChange({ page: 1, save: true }); } },
    PAGE_SIZES.map((n) => h('option', { value: n }, t('每页 {n} 条', { n }))));
  size.value = String(prefs.pageSize);
  const prev = btn(t('上一页'), { size: 'xs', onClick: () => onChange({ delta: -1 }) });
  const next = btn(t('下一页'), { size: 'xs', onClick: () => onChange({ delta: 1 }) });
  const pageTxt = h('span.num.muted');
  const nav = h('div.row.gap-1', prev, pageTxt, next);
  const el = h('div.acct-foot', info, h('div.row.gap-3', nav, size));
  return {
    el,
    paint({ total, page, pages, from, to }) {
      el.hidden = total === 0;
      setText(info, t('显示 {a}–{b} / 共 {n} 个', { a: F.int(from), b: F.int(to), n: F.int(total) }));
      nav.hidden = pages <= 1;
      setText(pageTxt, t('第 {p} / {n} 页', { p: page, n: pages }));
      prev.disabled = page <= 1;
      next.disabled = page >= pages;
      size.value = String(prefs.pageSize);
    },
  };
}

/** Bulk bar: acts on the selected accounts. `ops` come from createBulk(). */
export function createBulkBar({ host, ops, selected, clear, selectAllFiltered }) {
  const count = h('b');
  const selAll = h('button.linkish', { type: 'button', onClick: selectAllFiltered });
  const list = () => selected();
  const slotItems = () => [
    { header: t('绑定出口') },
    { label: t('直连'), onClick: () => ops.setProxy(list(), '') },
    { label: t('自动均衡分配（仅未绑定）'), onClick: () => ops.setProxy(list(), null) },
    ...(host.slots().length ? [{ header: t('代理槽') }] : []),
    ...host.slots().map((s) => ({ label: s.label || s.name || s.id, hint: s.enabled === false ? '×' : '', onClick: () => ops.setProxy(list(), s.id) })),
  ];
  const moreItems = () => [
    { label: t('设置权重…'), icon: 'sliders', onClick: async () => {
      const v = await promptDialog({ title: t('批量设置权重'), label: t('权重 (1-100)'), value: '1', type: 'number', confirmText: t('保存'),
        validate: (s) => (/^\d{1,3}$/.test(s) && +s >= 1 && +s <= 100 ? '' : t('请输入 1 到 100 的整数')) });
      if (v != null) ops.setWeight(list(), Number(v));
    } },
    { header: t('出站身份') },
    ...PRODUCTS.map((p) => ({ label: productLabel(p.id), onClick: () => ops.setProduct(list(), p.id) })),
    { sep: true },
    ...slotItems(),
    { sep: true },
    { label: t('导出选中'), icon: 'download', onClick: () => host.actions.exportUids(list().map((a) => a.uid)) },
    { label: t('删除'), icon: 'trash', danger: true, onClick: () => ops.remove(list()) },
  ];
  const el = h('div.acct-bulk', { role: 'toolbar', hidden: true },
    h('div.row.gap-3.wrap', btn(t('取消选择'), { iconOnly: true, icon: 'x', size: 'sm', variant: 'ghost', onClick: clear }), h('span', count), selAll),
    h('div.row.gap-1.wrap.grow', { style: 'justify-content:flex-end' },
      btn(t('启用账号'), { size: 'sm', icon: 'play', onClick: () => ops.setEnabled(list(), true) }),
      btn(t('停用账号'), { size: 'sm', icon: 'pause', onClick: () => ops.setEnabled(list(), false) }),
      btn(t('测试'), { size: 'sm', icon: 'zap', onClick: () => ops.test(list()) }),
      btn(t('刷新凭证'), { size: 'sm', icon: 'refresh', onClick: () => ops.refreshTokens(list()) }),
      btn(t('刷新积分'), { size: 'sm', icon: 'coins', onClick: () => ops.refreshCredits(list()) }),
      btn(t('更多'), { size: 'sm', icon: 'more', onClick: (e, b) => { host.loadSlots(); menu(b, moreItems(), { align: 'end', minWidth: 240 }); } })));
  return {
    el,
    paint(n, filteredTotal) {
      el.hidden = n === 0;
      setText(count, t('已选 {n} 个', { n }));
      selAll.hidden = n >= filteredTotal;
      setText(selAll, t('选择全部 {n} 个筛选结果', { n: filteredTotal }));
    },
  };
}
