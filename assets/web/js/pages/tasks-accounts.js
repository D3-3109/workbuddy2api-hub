// Per-account overview of the viewed realm: today's state, eligibility and a one-click action per row.
import { h, reconcile, setText, toggleClass, mount } from '../lib/dom.js';
import { add, t } from '../lib/i18n.js';
import { card, btn, empty } from '../lib/ui.js';
import { defs } from './tasks-actions.js';
import { can, flagBadges, eligibilityBadges, gate, realmOf, shortUid, statusBadge, stampShort, taskActive } from './tasks-model.js';

add({
  '国际版账号 · 今日活跃': 'Global accounts · today', '国内版账号 · 今日签到': 'China accounts · today',
  '{n} 个账号': 'Accounts: {n}', '名称': 'Name', '今日状态': 'Today', '任务资格': 'Eligibility',
  '还没有国际版账号': 'No global-region accounts yet', '还没有国内版账号': 'No China-region accounts yet',
  '在账号页添加账号后，这里会显示每个账号的任务状态。': 'Add accounts in the accounts page and their task state shows up here.',
  '选择此账号': 'Select this account',
});

export function accountsCard(store) {
  const { view } = store;
  const actionLabel = view === 'cn' ? t('Buddy 加油站签到') : t('每日活跃打卡');
  const verb = view === 'cn' ? 'checkin' : 'dailyChat';
  const tbody = h('tbody');
  const sub = h('span.sub');
  const th = (label, cls) => h('th' + (cls ? '.' + cls : ''), label);
  const table = h('table.table.compact.tk-accounts', h('thead', h('tr', th(t('名称')), th(t('今日状态')), th(t('任务资格'), 'hide-sm'), th(''))), tbody);
  const emptyBox = h('div', { hidden: true }, empty('users', view === 'cn' ? t('还没有国内版账号') : t('还没有国际版账号'), t('在账号页添加账号后，这里会显示每个账号的任务状态。')));
  const el = card(view === 'cn' ? t('国内版账号 · 今日签到') : t('国际版账号 · 今日活跃'), { sub, flush: true, cls: 'tk-accounts-card' },
    h('div.table-wrap', table), emptyBox);

  function create(a) {
    const rowBtn = btn(actionLabel, {
      iconOnly: true, icon: view === 'cn' ? 'check' : 'play', size: 'sm', variant: 'ghost',
      onClick: (ev, node) => store.exec(view === 'cn' ? defs.checkin(a.uid) : defs.dailyChat(a.uid), node),
    });
    const tr = h('tr', { dataset: { uid: a.uid } },
      h('td.tk-c-name'), h('td.tk-c-state'), h('td.hide-sm.tk-c-elig'), h('td.tk-c-act', rowBtn));
    tr.__btn = rowBtn;
    update(tr, a);
    return tr;
  }

  function fill(tr, a, st) {
    const name = a.nickname || shortUid(a.uid);
    const pick = h('button.tk-name', { type: 'button', 'data-tip': t('选择此账号'), onClick: () => store.setSel(a.uid) },
      h('span.cell-main.trunc', name),
      h('span.cell-sub', h('span.mono', shortUid(a.uid)), ...flagBadges(a)));
    tr.children[0].replaceChildren(pick);
    mount(tr.children[1], statusBadge(view, st), st.ts ? h('div.cell-sub.nowrap', stampShort(st.ts)) : null);
    tr.children[2].replaceChildren(h('div.row.wrap.gap-1', ...eligibilityBadges(view, a)));
  }

  function update(tr, a) {
    const st = store.statusOf(a);
    const sig = [a.nickname, a.enabled, a.tasksEnabled, a.canCheckin, a.canDailyChat, a.canReportActivity, a.needRelogin, a.tokenExpired,
      Math.ceil((a.restrictedFor || 0) / 60), st.key, st.ts, st.msg].join('|');
    if (tr.__sig !== sig) { tr.__sig = sig; fill(tr, a, st); }
    toggleClass(tr, 'selected', store.sel === a.uid);
    toggleClass(tr, 'dim', !taskActive(a));
    gate(tr.__btn, store, can(verb, a), actionLabel);
  }

  function paint() {
    const list = store.accounts.filter((a) => realmOf(a) === view);
    reconcile(tbody, list, (a) => a.uid, create, update);
    table.hidden = !list.length;
    emptyBox.hidden = !!list.length;
    setText(sub, list.length ? t('{n} 个账号', { n: list.length }) : '');
  }
  return { el, paint };
}
