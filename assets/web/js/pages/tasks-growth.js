// China-region growth task list. GET /tasks shows ONE account (the selected one, or the first participating
// account when "all" is picked), so the reply's own `account` decides which uid a claim button uses.
import { h, reconcile, setText, toggleClass, once, mount } from '../lib/dom.js';
import { getOr, qs } from '../lib/api.js';
import { add, t } from '../lib/i18n.js';
import { card, btn, badge, alertBox, empty, skeleton, sorter, sortHeader, errText } from '../lib/ui.js';
import { defs } from './tasks-actions.js';
import { can, gate, taskView, rewardTexts, restrictionSeconds, restrictionText, shortUid } from './tasks-model.js';

add({
  '成长任务': 'Growth tasks', '显示账号：{name}': 'Showing: {name}', '任务': 'Tasks', '进度': 'Progress', '奖励': 'Reward', '状态': 'Status', '领取': 'Claim', '已达成': 'Reached',
  '暂无成长任务': 'No growth tasks', '本网关版本不提供成长任务': 'This gateway version does not provide growth tasks',
  '成长任务加载失败：{e}': 'Failed to load growth tasks: {e}', '重试': 'Retry', '刷新': 'Refresh',
  '没有已开启任务的国内版账号': 'No China accounts with tasks enabled', '还没有国内版账号': 'No China-region accounts yet',
  '在账号页添加国内版账号后，这里会显示它的成长任务。': 'Add a China-region account in the accounts page to see its growth tasks here.',
  '上游返回：{msg}': 'Upstream said: {msg}',
});

const PAGE_CAP = 150;

export function growthCard(store) {
  let tasks = { status: 'idle', data: null, error: '' }; // idle | loading | ready | error
  let tok = 0;

  const sort = sorter('status', 'asc', () => paint());
  const heads = [sortHeader(t('任务'), 'name', sort), sortHeader(t('进度'), 'progress', sort), sortHeader(t('状态'), 'status', sort)];
  const rewardHead = h('th.hide-sm', t('奖励'));
  const tbody = h('tbody');
  const table = h('table.table.compact.tk-tasks', h('thead', h('tr', heads[0], heads[1], rewardHead, heads[2])), tbody);
  const note = h('div', { style: 'padding:14px 18px 0' });
  const stateBox = h('div');
  const sub = h('span.sub');
  const reloadBtn = btn(t('刷新'), { iconOnly: true, icon: 'refresh', variant: 'ghost', size: 'sm', onClick: () => load() });
  const el = card(t('成长任务'), { sub, flush: true, actions: [reloadBtn] }, note, stateBox, h('div.table-wrap', table));
  const wrap = table.parentNode;

  /** Verdict for the current selection (does a growth list apply at all?). */
  function verdict() {
    const { list, target } = store.scope();
    if (!list.length) return { ok: false, why: t('还没有国内版账号'), none: true };
    if (store.sel !== 'all') return can('growth', target[0]);
    return target.some((a) => can('growth', a).ok) ? { ok: true, why: '' } : { ok: false, why: t('没有已开启任务的国内版账号') };
  }

  async function load() {
    if (store.disposed) return;
    if (!store.loaded || !verdict().ok) { tasks = { status: 'idle', data: null, error: '' }; store.paint(); return; }
    const my = ++tok;
    tasks = { ...tasks, status: 'loading' };
    try {
      const r = await getOr('/tasks' + qs({ uid: store.sel === 'all' ? '' : store.sel }), { supported: false });
      if (my !== tok || store.disposed) return;
      const secs = restrictionSeconds(r.msg);
      if (secs != null) r.__until = Date.now() + secs * 1000;
      tasks = { status: 'ready', data: r, error: '' };
    } catch (e) {
      if (my !== tok || store.disposed) return;
      tasks = { status: 'error', data: tasks.data, error: errText(e) };
    }
    store.paint();
  }
  /** Selection changed: drop the rows of the previous account at once. */
  function reset() { tok++; tasks = { status: 'idle', data: null, error: '' }; }

  function shownAccount(d) {
    if (d && d.account) return d.account;
    return store.sel === 'all' ? null : store.byUid.get(store.sel) || null;
  }

  function friendly(msg) {
    return restrictionSeconds(msg) != null && tasks.data && tasks.data.__until
      ? restrictionText(Math.max(0, Math.round((tasks.data.__until - Date.now()) / 1000)))
      : msg;
  }

  function setNote(kind, text, withRetry) {
    once(note, kind + '|' + text + '|' + withRetry, () => {
      note.replaceChildren(...(kind ? [alertBox(kind, h('div', text), withRetry ? btn(t('重试'), { size: 'sm', onClick: () => load() }) : null)] : []));
    });
    note.hidden = !kind;
  }
  function setState(key, build) {
    once(stateBox, key, () => stateBox.replaceChildren(...(build ? [build()] : [])));
    stateBox.hidden = !build;
  }

  function rewardNodes(tk) {
    const r = rewardTexts(tk);
    return r.length ? r.map((x) => h('div.tk-rw.' + x.kind, x.text)) : [h('span.faint', '—')];
  }

  function fillRow(tr, tk, v, uid) {
    const name = tk.name || tk.task_code;
    mount(tr.children[0],
      h('div.cell-main', name),
      tk.task_code && tk.task_code !== name ? h('div.cell-sub.mono', tk.task_code) : null,
      tk.description ? h('div.cell-sub.tk-desc', tk.description) : null,
      h('div.tk-reward-sm', ...rewardNodes(tk)));
    tr.children[1].replaceChildren(h('div.tk-prog',
      h('span.num', v.done ? t('已达成') : `${v.cur} / ${v.target}`),
      h('div.bar' + (v.key === 'claimed' || v.claimable ? '.good' : ''), h('i', { style: { width: v.pct + '%' } }))));
    tr.children[2].replaceChildren(...rewardNodes(tk));
    tr.__claim = v.claimable
      ? btn(t('领取'), { size: 'xs', variant: 'primary', onClick: (e, node) => store.exec(defs.claim(uid, tk.task_code, name), node) })
      : null;
    mount(tr.children[3], h('div.tk-status', badge(v.label, v.kind === 'outline' ? '' : v.kind, { dot: true, outline: v.kind === 'outline' }), tr.__claim));
  }

  function rowUpdate(tr, tk) {
    const uid = tasks.data && tasks.data.account ? tasks.data.account.uid : '';
    const v = taskView(tk);
    const sig = [tk.name, tk.description, tk.status, tk.current, tk.target, tk.reward_credit, tk.reward_energy, uid].join('|');
    if (tr.__sig !== sig) { tr.__sig = sig; fillRow(tr, tk, v, uid); }
    toggleClass(tr, 'dim', v.key === 'claimed');
    if (tr.__claim) gate(tr.__claim, store, can('growth', store.byUid.get(uid)));
  }

  const GET = {
    name: (x) => x.tk.name || x.tk.task_code || '',
    progress: (x) => x.v.pct,
    status: (x) => x.v.rank,
  };

  function paint() {
    for (const th of heads) th.refresh();
    const d = tasks.data;
    const acct = shownAccount(d);
    setText(sub, acct ? t('显示账号：{name}', { name: acct.nickname || shortUid(acct.uid) }) : '');
    const v = verdict();
    let rows = [];
    let showTable = false;

    if (!store.loaded || (!d && (tasks.status === 'idle' || tasks.status === 'loading') && v.ok)) {
      setNote('', '', false);
      setState('skel', () => h('div', { style: 'padding:16px 18px' }, skeleton(4)));
    } else if (!v.ok) {
      setNote('', '', false);
      setState('no:' + v.why, () => (v.none
        ? empty('users', t('还没有国内版账号'), t('在账号页添加国内版账号后，这里会显示它的成长任务。'))
        : h('div', { style: 'padding:16px 18px' }, alertBox('info', v.why))));
    } else if (tasks.status === 'error' && !d) {
      setNote('', '', false);
      setState('err:' + tasks.error, () => h('div', { style: 'padding:16px 18px' }, alertBox('bad', h('div', t('成长任务加载失败：{e}', { e: tasks.error })), btn(t('重试'), { size: 'sm', onClick: () => load() }))));
    } else if (d && d.supported === false) {
      setNote('', '', false);
      setState('unsup', () => empty('info', t('本网关版本不提供成长任务')));
    } else if (d) {
      const list = Array.isArray(d.tasks) ? d.tasks : [];
      setNote(d.msg ? 'warn' : tasks.status === 'error' ? 'bad' : '', d.msg ? friendly(d.msg) : tasks.error, true);
      if (list.length) setState('', null);
      else setState('none', () => empty('inbox', t('暂无成长任务')));
      rows = sort.apply(list.slice(0, PAGE_CAP).map((tk) => ({ tk, v: taskView(tk) })), GET).map((x) => x.tk);
      showTable = rows.length > 0;
    }
    reconcile(tbody, showTable ? rows : [], (tk, i) => tk.task_code || 'i' + i, (tk) => { const tr = h('tr', h('td'), h('td'), h('td.hide-sm'), h('td')); rowUpdate(tr, tk); return tr; }, rowUpdate);
    wrap.hidden = !showTable;
  }

  return { el, paint, load, reset, state: () => tasks };
}
