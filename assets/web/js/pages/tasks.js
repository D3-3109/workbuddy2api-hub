// Tasks & benefits: account picker, per-realm task cards, last result, account overview and history.
// Polling: accounts every 15 s, today's events + scheduler summary every 30 s. /tasks and the history are
// loaded on entry, on selection change and after actions only (spec: the server caches /tasks for 20 s).
import { h, setText, once, scope, mount as fill } from '../lib/dom.js';
import { get, getOr, qs } from '../lib/api.js';
import { add, t } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { loadSettings } from '../lib/ctx.js';
import { poller } from '../lib/poll.js';
import { icon } from '../lib/icons.js';
import { btn, badge, pageHead, alertBox, spinner, errText } from '../lib/ui.js';
import { run, runState, attach } from './tasks-actions.js';
import { can, taskActive, scopeOf, todayIndex, todayStatus, localDay, eligibilityBadges, flagBadges, num } from './tasks-model.js';
import { intlSection } from './tasks-intl.js';
import { cnSection } from './tasks-cn.js';
import { growthCard } from './tasks-growth.js';
import { accountsCard } from './tasks-accounts.js';
import { resultCard } from './tasks-result.js';
import { historyCard } from './tasks-history.js';

add({
  '任务与福利': 'Tasks', '每日活跃打卡、Buddy 加油站签到、成长任务与福利积分总览。': 'Daily activity check-in, Buddy gas-station check-in, growth tasks and reward credits.',
  '为国际版账号领取每日活跃奖励，并查看打卡记录。': 'Earn the daily activity reward for global-region accounts and review the check-in history.',
  'Buddy 加油站签到、成长任务领奖、猫猫旅行与连登管家。': 'Buddy gas-station check-in, growth task rewards, cat travel and the streak butler.',
  '目标账号': 'Target account', '选择账号': 'Choose account', '全部国内版账号': 'All China accounts', '全部国际版账号': 'All global accounts',
  '全部国际版账号（{n} 个已开任务）': 'All global accounts ({n} with tasks on)', '全部国内版账号（{n} 个已开任务）': 'All China accounts ({n} with tasks on)',
  '{a}/{b} 个账号可操作': '{a}/{b} accounts can run this', '账号加载失败：{e}': 'Failed to load accounts: {e}', '重试': 'Retry', '刷新': 'Refresh',
  '正在执行「{label}」…请求会保持到所有账号处理完毕，期间可以切换页面。': 'Running "{label}"... the request stays open until every account is done. You can switch pages meanwhile.',
  '后台调度运行中': 'Scheduler running', '后台调度已暂停': 'Scheduler paused', '下次 {t}': 'Next {t}', '每日 {times}': 'Daily {times}', '尚未运行': 'Not run yet', '待调度': 'Pending',
  '离线模式': 'Offline mode', '网关处于离线或测试上游模式：调度器在运行，但不会真正访问上游。': 'The gateway is offline or uses a test upstream: the scheduler runs but does not reach the real upstream.',
  '在网关页管理': 'Manage on the Gateway page',
});

export const usesRealm = true;

const sel_key = (view) => 'wb-tasks-account-' + view;
function savedSel(view) { try { return localStorage.getItem(sel_key(view)) || 'all'; } catch { return 'all'; } }

export async function mount(root, app) {
  const view = app.view === 'cn' ? 'cn' : 'intl';
  const cn = view === 'cn';
  const sc = scope();
  let scoped = null;
  let first = true;

  const store = {
    view, disposed: false, loaded: false,
    accounts: [], byUid: new Map(), todayIdx: new Map(), day: localDay(),
    sel: savedSel(view), caps: { chat: true }, grants: null,
    busy: runState,
    exec: (def, el) => run(def, el),
    scope() {
      if (!scoped || scoped.acc !== store.accounts || scoped.sel !== store.sel) scoped = { acc: store.accounts, sel: store.sel, val: scopeOf(view, store.accounts, store.sel) };
      return scoped.val;
    },
    statusOf: (a) => todayStatus(view, a, store.todayIdx, store.day),
    nameOf: (uid) => { const a = store.byUid.get(uid); return (a && a.nickname) || String(uid || '').slice(0, 8); },
    scopeText() {
      if (store.sel !== 'all') return store.nameOf(store.sel);
      const n = store.scope().list.filter(taskActive).length;
      return cn ? t('全部国内版账号（{n} 个已开任务）', { n }) : t('全部国际版账号（{n} 个已开任务）', { n });
    },
    grantTotal(kind) {
      const g = store.grants;
      if (!g) return { value: '—', note: ' ' };
      if (g.supported === false) return { value: '—', note: t('本网关版本不提供') };
      const keep = kind === 'cn'
        ? (r) => r.realm === 'cn' && (/签到|成长|活动|裂变/.test(r.grant_reason || '') || String(r.name || '').includes('裂变'))
        : (r) => r.realm === 'intl' && (r.name === 'Bonus Pack' || String(r.source_inferred || '').includes('活跃'));
      let sum = 0;
      for (const r of Array.isArray(g.rows) ? g.rows : []) if (keep(r) && (store.sel === 'all' || r.uid === store.sel)) sum += num(r.size ?? r.amount) || 0;
      return { value: t('+{n} 积分', { n: F.int(sum) }), note: ' ' };
    },
    paint: () => paint(),
    setSel(v) {
      if (v === store.sel) return;
      store.sel = v;
      try { localStorage.setItem(sel_key(view), v); } catch { /* blocked */ }
      if (growth) growth.reset();
      paint();
      history.reload();
      if (growth) growth.load();
    },
  };

  const growth = cn ? growthCard(store) : null;
  const section = cn ? cnSection(store, growth) : intlSection(store);
  const table = accountsCard(store);
  const result = resultCard(store);
  const history = historyCard(store);

  /* ---- toolbar: picker, eligibility, running hint, scheduler summary ---- */
  const picker = h('select.select.tk-picker#tk-picker', { 'aria-label': t('选择账号'), onChange: () => store.setSel(picker.value) });
  const chips = h('div.row.wrap.gap-1');
  const busyBox = h('div.tk-busy', { hidden: true }, spinner(), h('span'));
  const schedBox = h('a.tk-sched', { href: '#/gateway', hidden: true });
  const loadErr = h('div', { hidden: true });
  const toolbar = h('section.card.tk-bar',
    h('div.tk-bar-main',
      h('div.field', h('label', { for: 'tk-picker' }, t('目标账号')), picker),
      h('div.tk-chips', chips)),
    h('div.tk-bar-side', busyBox, schedBox));

  function syncPicker() {
    const { list } = scopeOf(view, store.accounts, 'all');
    once(picker, list.map((a) => `${a.uid}|${a.nickname}|${taskActive(a)}`).join(',') + '|' + view, () => {
      picker.replaceChildren(
        h('option', { value: 'all' }, cn ? t('全部国内版账号') : t('全部国际版账号')),
        ...list.map((a) => h('option', { value: a.uid }, (a.nickname || a.uid.slice(0, 8)) + (taskActive(a) ? '' : ' · ' + t('任务未开启')))));
    });
    if (store.sel !== 'all' && !list.some((a) => a.uid === store.sel)) store.setSel('all');
    picker.value = store.sel;
  }

  function syncChips() {
    const { list, target } = store.scope();
    const verb = cn ? 'growth' : 'dailyChat';
    if (store.sel === 'all' || !target[0]) {
      const ok = list.filter((a) => can(verb, a).ok).length;
      once(chips, 'all|' + ok + '|' + list.length, () => chips.replaceChildren(badge(t('{a}/{b} 个账号可操作', { a: ok, b: list.length }), ok ? 'good' : '', { dot: true })));
      return;
    }
    const a = target[0];
    const sig = [a.uid, a.enabled, a.tasksEnabled, a.canCheckin, a.canDailyChat, a.canReportActivity, a.needRelogin, a.tokenExpired, Math.ceil((a.restrictedFor || 0) / 60)].join('|');
    once(chips, sig, () => chips.replaceChildren(...eligibilityBadges(view, a), ...flagBadges(a)));
  }

  function syncBusy() {
    const r = runState();
    busyBox.hidden = !r;
    if (r) setText(busyBox.lastChild, t('正在执行「{label}」…请求会保持到所有账号处理完毕，期间可以切换页面。', { label: r.label }));
  }

  let sched = null;
  function syncSched() {
    const s = sched;
    schedBox.hidden = !(s && s.supported !== false);
    if (schedBox.hidden) return;
    const parts = [s.enabled ? t('后台调度运行中') : t('后台调度已暂停')];
    if (!cn && Array.isArray(s.daily_chat_times) && s.daily_chat_times.length) parts.push(t('每日 {times}', { times: s.daily_chat_times.join(' · ') }));
    parts.push(t('下次 {t}', { t: s.next_run_time === '待调度' ? t('待调度') : s.next_run_time || '—' }));
    once(schedBox, parts.join(' · ') + '|' + s.enabled + '|' + s.active, () => {
      fill(schedBox, h('span.pill-dot' + (s.enabled ? '.good' : '')), h('span.trunc', parts.join(' · ')),
        s.active === false ? badge(t('离线模式'), 'warn', { sm: true, tip: t('网关处于离线或测试上游模式：调度器在运行，但不会真正访问上游。') }) : null, icon('chevronRight', 14));
      schedBox.setAttribute('data-tip', (cn ? s.mode_cn : s.mode_intl) || s.mode || t('在网关页管理'));
    });
  }

  function paint() {
    if (store.disposed || !store.loaded) return;
    syncPicker();
    syncChips();
    syncBusy();
    syncSched();
    section.paint();
    if (growth) growth.paint();
    table.paint();
    result.paint();
  }

  /* ---- data ---- */
  function applyAccounts(list) {
    store.accounts = list;
    store.byUid = new Map(list.map((a) => [a.uid, a]));
    store.loaded = true;
    paint();
  }

  const accountsPoll = poller(async (signal) => {
    try {
      const r = await get('/accounts' + qs({ realm: 'all' }), { signal });
      loadErr.hidden = true;
      applyAccounts(r.accounts || []);
    } catch (e) {
      if (e && e.name !== 'AbortError' && !store.disposed) {
        loadErr.hidden = false;
        loadErr.replaceChildren(alertBox('bad', h('div', t('账号加载失败：{e}', { e: errText(e) })), btn(t('重试'), { size: 'sm', onClick: () => accountsPoll.trigger() })));
      }
      throw e;
    }
    if (first) { first = false; history.load(); if (growth) growth.load(); }
  }, 15000);

  const eventsPoll = poller(async (signal) => {
    const [ev, s] = await Promise.all([
      getOr('/activity/history' + qs({ range: 'today', limit: 200, task: cn ? 'checkin' : 'daily_chat' }), { rows: [] }, { signal }).catch(() => null),
      getOr('/scheduler', null, { signal }).catch(() => null),
    ]);
    if (store.disposed) return;
    if (ev) { store.todayIdx = todayIndex(ev.rows || []); store.day = localDay(); }
    sched = s;
    paint();
  }, 30000);

  /** Called by the module-level runner. */
  const page = {
    onBusy: () => paint(),
    afterAction(res) {
      if (store.disposed) return;
      if (res.accounts) applyAccounts(res.accounts); else accountsPoll.trigger();
      eventsPoll.trigger();
      if (res.realm === view) { history.reload(); if (growth) growth.load(); }
      paint();
    },
  };
  sc.add(attach(page));

  /* ---- layout ---- */
  const refreshBtn = btn(t('刷新'), { iconOnly: true, icon: 'refresh', variant: '', onClick: () => {
    accountsPoll.trigger(); eventsPoll.trigger();
    return Promise.allSettled([history.reload(), growth ? growth.load() : null]);
  } });
  root.append(
    pageHead(t('任务与福利'), cn ? t('Buddy 加油站签到、成长任务领奖、猫猫旅行与连登管家。') : t('为国际版账号领取每日活跃奖励，并查看打卡记录。'), refreshBtn),
    h('div.p-tasks.stack', loadErr, toolbar, ...section.top, result.el, growth ? growth.el : null, table.el, history.el));

  syncBusy();
  accountsPoll.start();
  eventsPoll.start();
  getOr('/accounts/credits/grants', { supported: false }).then((g) => { store.grants = g; paint(); }).catch(() => { store.grants = { supported: false }; paint(); });
  loadSettings().then((s) => {
    const mc = (s && s.management_capabilities) || {};
    store.caps.chat = mc.daily_tasks !== false && !(s && s.daily_chat_supported === false);
    paint();
  }).catch(() => {});

  return () => {
    store.disposed = true;
    accountsPoll.stop();
    eventsPoll.stop();
    sc.dispose();
  };
}
