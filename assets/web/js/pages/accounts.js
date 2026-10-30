// Accounts: pool overview with search, status filters, keyed rows, bulk actions and a detail dialog.
import { h, reconcile, rafThrottle, scope } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { loadSettings, realmLabel } from '../lib/ctx.js';
import { poller } from '../lib/poll.js';
import { kpi, btn, menuButton, pageHead, empty, skeleton, alertBox, closeMenu, errText, toast } from '../lib/ui.js';
import { createStore, api } from './accounts-store.js';
import { productOf, statusOf, matchQuery, countGroups, sortAccounts, summarize } from './accounts-model.js';
import { createRows } from './accounts-rows.js';
import { createActions } from './accounts-actions.js';
import { createBulk } from './accounts-batch.js';
import { createToolbar, createSort, createPager, createBulkBar } from './accounts-toolbar.js';

add({
  '管理账号池：查看状态、积分与限额，添加、导入、测试并批量维护账号。': 'Manage the account pool: status, credits and limits, plus adding, importing, testing and bulk maintenance.',
  '添加账号': 'Add account', '通过 OAuth 登录': 'Sign in with OAuth', '添加 API Key': 'Add API key', '导入账号（JSON）': 'Import accounts (JSON)', '扫描桌面客户端': 'Scan desktop client',
  '当前 Rust 版网关未实现此功能': 'Not available in the Rust gateway', '批量操作': 'Bulk actions', '刷新': 'Refresh', '刷新列表': 'Refresh list',
  '刷新全部凭证': 'Refresh all tokens', '刷新全部积分': 'Refresh all credits', '补全缺失积分': 'Fill missing credits', '测试全部可用账号': 'Test all ready accounts',
  '导出当前区域': 'Export this region', '导出全部区域': 'Export all regions',
  '账号总数': 'Accounts', '启用 {n} 个': '{n} enabled', '可用账号': 'Ready', '占启用账号 {p}': '{p} of enabled', '剩余积分': 'Credits left', '总额度 {a} · {n} 个未查询': 'Total {a} · {n} not queried',
  '需重新登录': 'Need re-login', '点击查看': 'Click to filter', '全部凭证正常': 'All tokens valid',
  '当前{r}还没有账号': 'No {r} accounts yet', '请先登录或导入一个 {r} 账号，之后相关请求都会走该账号': 'Sign in or import a {r} account first; matching requests will then use it.', '登录新账号 (OAuth)': 'Sign in (OAuth)',
  '没有匹配的账号': 'No matching accounts', '调整搜索词或筛选条件。': 'Adjust the search or filters.', '清除筛选': 'Clear filters',
  '账号加载失败': 'Failed to load accounts', '账号刷新失败：{msg}（当前显示的是上次的数据）': 'Refresh failed: {msg} (showing the last data)', '重试': 'Retry',
  '为未绑定账号分配代理出口': 'Assign exits to unbound accounts', '把尚未绑定出口的已启用账号依次分配到已启用的代理槽；已有绑定的账号不受影响': 'Spreads enabled accounts without an exit over the enabled proxy slots; accounts that already have one are untouched',
  '没有启用的代理槽，请先在「设置」添加并保存': 'No enabled proxy slot. Add and save one in Settings first.', '所有已启用账号都已绑定代理出口': 'Every enabled account already has a proxy exit',
});

export const usesRealm = true;

const PREF_KEY = 'wb-accounts-view';
function readPrefs() {
  try { return JSON.parse(localStorage.getItem(PREF_KEY) || '{}') || {}; } catch { return {}; }
}

export async function mount(root, app) {
  const sc = scope();
  const view = app.view;
  root.classList.add('p-accounts');
  const saved = readPrefs();
  const prefs = {
    status: ['all', 'ready', 'limited', 'expired', 'disabled'].includes(saved.status) ? saved.status : 'all',
    product: ['all', 'workbuddy', 'vscode', 'cli'].includes(saved.product) ? saved.product : 'all',
    pageSize: [25, 50, 100, 200].includes(saved.pageSize) ? saved.pageSize : 50,
  };
  const state = { q: '', status: prefs.status, product: prefs.product };
  const save = () => {
    try { localStorage.setItem(PREF_KEY, JSON.stringify({ status: state.status, product: state.product, sort: sort.key, dir: sort.dir, pageSize: prefs.pageSize })); } catch { /* blocked */ }
  };

  const store = createStore();
  const selected = new Set();
  let slots = [];
  let page = 1;
  let pageUids = [];
  let loaded = false;
  let loadError = null;
  let lastClick = null;
  let poll = null;

  const host = {
    app, view, store, caps: {}, dialogs: new Set(),
    slots: () => slots,
    loadSlots: () => api.slots().then((r) => { slots = r.slots || []; }).catch(() => {}),
    reload: () => poll && poll.trigger(),
    openDetail: async (uid, tab) => (await import('./accounts-detail.js')).openDetail(host, uid, tab),
    openLogin: async (realm) => (await import('./accounts-login.js')).openLogin(host, realm),
    openApiKey: async () => (await import('./accounts-login.js')).openApiKey(host),
    openImport: async () => (await import('./accounts-import.js')).openImport(host),
  };
  host.actions = createActions(host);
  const ops = createBulk(host, host.actions);

  /* ---- sort / toolbar ---- */
  const sort = createSort(saved, () => { save(); page = 1; toolbar.paintSort(); render(); });
  const toolbar = createToolbar({ state, sort, onChange: (o = {}) => { if (o.page) page = o.page; if (o.save) save(); render(); } });
  toolbar.paintSort();

  /* ---- selection ---- */
  const sel = {
    has: (uid) => selected.has(uid),
    toggle(uid, on, shift) {
      const ids = shift && lastClick && pageUids.includes(lastClick) ? range(lastClick, uid) : [uid];
      for (const id of ids) { if (on) selected.add(id); else selected.delete(id); }
      lastClick = uid;
      afterSelect();
    },
    togglePage(on) { for (const id of pageUids) { if (on) selected.add(id); else selected.delete(id); } afterSelect(); },
  };
  const range = (a, b) => { const i = pageUids.indexOf(a), j = pageUids.indexOf(b); return pageUids.slice(Math.min(i, j), Math.max(i, j) + 1); };
  const selectedList = () => store.list.filter((a) => selected.has(a.uid));
  const bulkBar = createBulkBar({
    host, ops, selected: selectedList,
    clear: () => { selected.clear(); afterSelect(); },
    selectAllFiltered: () => { for (const a of derive().rows) selected.add(a.uid); afterSelect(); },
  });
  function afterSelect() { renderRows(); paintSelection(); }

  /* ---- rows ---- */
  const body = h('div.a-body', { role: 'rowgroup' });
  const rows = createRows({ host, sel, sort });
  const stateBox = h('div.acct-state');
  const pager = createPager({ prefs, onChange: (o) => { if (o.delta) page += o.delta; if (o.page) page = o.page; if (o.save) save(); render(); } });
  const notice = h('div.acct-notice', { hidden: true });
  const list = h('div.acct-list', { role: 'table' }, rows.header, body, stateBox);

  /* ---- derived data (memoised per store version + filters) ---- */
  let dKey = '', dRes = null;
  function derive() {
    const key = [store.version, state.q, state.status, state.product, sort.key, sort.dir].join('|');
    if (key === dKey) return dRes;
    const base = store.list.filter((a) => (state.product === 'all' || productOf(a) === state.product) && (!state.q || matchQuery(a, state.q)));
    const filtered = state.status === 'all' ? base : base.filter((a) => statusOf(a).group === state.status);
    dRes = { counts: countGroups(base), rows: sortAccounts(filtered, sort.key, sort.dir) };
    dKey = key;
    return dRes;
  }

  /* ---- KPI strip ---- */
  const kTotal = kpi({ label: t('账号总数'), icon: 'users' });
  const kReady = kpi({ label: t('可用账号'), icon: 'checkCircle' });
  const kCredits = kpi({ label: t('剩余积分'), icon: 'coins' });
  const kRelogin = kpi({ label: t('需重新登录'), icon: 'key' });
  kRelogin.el.addEventListener('click', () => { if (summary && summary.expired) { state.status = 'expired'; page = 1; save(); render(); } });
  let summary = null;
  function paintKpis() {
    const s = summary = summarize(store.list);
    kTotal.set({ value: F.int(s.total), foot: t('启用 {n} 个', { n: s.enabled }) });
    kReady.set({ value: F.int(s.ready), foot: t('占启用账号 {p}', { p: s.enabled ? F.pct((s.ready / s.enabled) * 100, 0) : '—' }) });
    kCredits.set({ value: s.withCredits ? F.int(s.remain) : '—', foot: t('总额度 {a} · {n} 个未查询', { a: s.size ? F.compact(s.size) : '—', n: s.missing }) });
    kRelogin.set({ value: F.int(s.expired), foot: s.expired ? t('点击查看') : t('全部凭证正常') });
    kRelogin.el.classList.toggle('alarm', s.expired > 0);
    kRelogin.el.style.cursor = s.expired ? 'pointer' : '';
  }

  /* ---- render ---- */
  function paintSelection() {
    const d = derive();
    bulkBar.paint(selected.size, d.rows.length);
    const onPage = pageUids.filter((id) => selected.has(id)).length;
    rows.paintHeader({ all: pageUids.length > 0 && onPage === pageUids.length, some: onPage > 0 });
  }

  function renderRows() {
    const d = derive();
    const size = prefs.pageSize;
    const pages = Math.max(1, Math.ceil(d.rows.length / size));
    if (page > pages) page = pages;
    if (page < 1) page = 1;
    const slice = d.rows.slice((page - 1) * size, page * size);
    pageUids = slice.map((a) => a.uid);
    reconcile(body, slice, (a) => a.uid, rows.create, rows.update);
    return { d, pages, slice };
  }

  function paintState(d) {
    const total = store.list.length;
    list.classList.toggle('is-empty', !d.rows.length);
    if (!loaded) {
      if (loadError) stateBox.replaceChildren(empty('alert', t('账号加载失败'), errText(loadError), btn(t('重试'), { variant: 'primary', onClick: () => poll.trigger() })));
      else if (!stateBox.firstChild || stateBox.firstChild.className !== 'acct-skel') stateBox.replaceChildren(h('div.acct-skel', skeleton(5)));
      stateBox.hidden = false;
      return;
    }
    if (d.rows.length) { stateBox.hidden = true; return; }
    stateBox.hidden = false;
    if (!total) {
      const r = t(realmLabel(view));
      stateBox.replaceChildren(empty('users', t('当前{r}还没有账号', { r }), t('请先登录或导入一个 {r} 账号，之后相关请求都会走该账号', { r }),
        btn(t('登录新账号 (OAuth)'), { variant: 'primary', icon: 'plus', onClick: () => host.openLogin(view) })));
    } else {
      stateBox.replaceChildren(empty('search', t('没有匹配的账号'), t('调整搜索词或筛选条件。'), btn(t('清除筛选'), { onClick: clearFilters })));
    }
  }

  function clearFilters() {
    state.q = ''; state.status = 'all'; state.product = 'all'; page = 1;
    toolbar.el.querySelector('input').value = '';
    toolbar.el.querySelector('select').value = 'all';
    save(); render();
  }

  function render() {
    for (const uid of selected) if (!store.get(uid)) selected.delete(uid);
    const { d, pages, slice } = renderRows();
    toolbar.paintCounts(d.counts);
    toolbar.paintSort();
    rows.paintHeader();
    paintState(d);
    paintSelection();
    const from = d.rows.length ? (page - 1) * prefs.pageSize + 1 : 0;
    pager.paint({ total: d.rows.length, page, pages, from, to: from + slice.length - (d.rows.length ? 1 : 0) });
    paintKpis();
    notice.hidden = !(loadError && loaded);
    if (!notice.hidden) notice.replaceChildren(alertBox('warn', t('账号刷新失败：{msg}（当前显示的是上次的数据）', { msg: errText(loadError) }), ' ', btn(t('重试'), { size: 'xs', onClick: () => poll.trigger() })));
  }
  const scheduleRender = rafThrottle(render);
  sc.add(store.subscribe(scheduleRender));

  /* ---- loading ---- */
  let inflight = null;
  function loadOnce(signal) {
    if (inflight) return inflight;
    const startedAt = Date.now();
    inflight = api.list(view, signal).then((r) => {
      loaded = true; loadError = null;
      store.replaceAll(r.accounts || [], startedAt);
    }).catch((e) => {
      if (e && e.name === 'AbortError') return;
      loadError = e;
      render();
      throw e;
    }).finally(() => { inflight = null; });
    return inflight;
  }
  poll = poller(loadOnce, 15000);

  /* ---- header actions ---- */
  const addMenu = menuButton(t('添加账号'), () => [
    { label: t('通过 OAuth 登录'), icon: 'link', onClick: () => host.openLogin(view) },
    { label: t('添加 API Key'), icon: 'key', onClick: () => host.openApiKey() },
    { label: t('导入账号（JSON）'), icon: 'upload', onClick: () => host.openImport() },
    { sep: true },
    { label: t('扫描桌面客户端'), icon: 'monitor', disabled: !host.caps.desktop_import, tip: t('当前 Rust 版网关未实现此功能'), onClick: () => {} },
  ], { variant: 'primary', icon: 'plus', align: 'end' });
  const inView = () => store.list;
  const batchMenu = menuButton(t('批量操作'), () => [
    { label: t('刷新全部凭证'), icon: 'refresh', onClick: () => ops.refreshTokens(inView().filter((a) => a.enabled)) },
    { label: t('刷新全部积分'), icon: 'coins', onClick: () => ops.refreshCredits(inView()) },
    { label: t('补全缺失积分'), icon: 'coins', onClick: () => ops.refreshCredits(inView().filter((a) => !a.credits), { missingOnly: true }) },
    { label: t('测试全部可用账号'), icon: 'zap', onClick: () => ops.test(inView()) },
    { label: t('为未绑定账号分配代理出口'), icon: 'globe', tip: t('把尚未绑定出口的已启用账号依次分配到已启用的代理槽；已有绑定的账号不受影响'), onClick: assignProxies },
    { sep: true },
    { label: t('导出当前区域'), icon: 'download', onClick: () => host.actions.exportAccounts({ realm: view, count: store.list.length }) },
    { label: t('导出全部区域'), icon: 'download', onClick: () => host.actions.exportAccounts({ realm: 'all', count: null }) },
  ], { icon: 'sliders', align: 'end' });
  async function assignProxies() {
    await host.loadSlots();
    if (!slots.some((s) => s.enabled !== false)) { toast(t('没有启用的代理槽，请先在「设置」添加并保存'), 'warn'); return; }
    const todo = inView().filter((a) => a.enabled && !a.proxySlot && !a.hasProxy);
    if (!todo.length) { toast(t('所有已启用账号都已绑定代理出口'), 'ok'); return; }
    await ops.setProxy(todo, null);
  }
  const refreshBtn = btn(t('刷新列表'), { iconOnly: true, icon: 'refresh', onClick: async () => { await loadOnce(); } });

  root.append(
    pageHead(t('账号'), t('管理账号池：查看状态、积分与限额，添加、导入、测试并批量维护账号。'), refreshBtn, batchMenu, addMenu),
    h('div.stack',
      h('div.acct-kpis', kTotal.el, kReady.el, kCredits.el, kRelogin.el),
      h('section.card.acct-card', toolbar.el, notice, list, pager.el, bulkBar.el)));

  render();
  loadSettings().then((s) => { host.caps = (s && s.management_capabilities) || {}; }).catch(() => {});
  host.loadSlots();
  poll.start();

  return () => {
    poll.stop();
    closeMenu();
    for (const m of [...host.dialogs]) { try { m.close(); } catch { /* gone */ } }
    sc.dispose();
  };
}
