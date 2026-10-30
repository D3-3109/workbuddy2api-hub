// Global-region view: today's daily-activity numbers and the check-in actions.
import { h, setText, setAttr } from '../lib/dom.js';
import { add, t } from '../lib/i18n.js';
import { kpi, btn, card, ok, confirmDialog } from '../lib/ui.js';
import { defs } from './tasks-actions.js';
import { can, gate, isDone, taskActive } from './tasks-model.js';

add({
  '今日活跃打卡': 'Daily activity today', '待打卡账号': 'Pending accounts', '累计活跃奖励积分': 'Total activity credits',
  '无国际账号': 'No global accounts', '今日已打卡': 'All checked in', '部分已打卡': 'Partly checked in', '今日未打卡': 'Not checked in yet',
  '国际版账号今天都已打卡': 'All global accounts are checked in today',
  '今日未打卡账号（{k}）：': 'Not checked in today ({k}):', '本网关版本不提供': 'Not provided by this gateway version',
  '每日活跃': 'Daily activity',
  '为国际版账号发起一次桌面端对话，并按设置补一次网页通道对话，用来领取每日活跃奖励。已完成的账号会被跳过，不会重复消耗额度。':
    'Starts one desktop-style chat per global-region account, plus a web-channel chat when enabled in settings, to earn the daily activity reward. Accounts that are already done are skipped, so no quota is wasted.',
  '未活跃账号一键打卡 ({n})': 'Check in pending accounts ({n})', '未活跃账号一键打卡': 'Check in pending accounts',
  '网页通道': 'Web channel', '仅网页通道打卡…': 'Web channel only…',
  '将为每个目标账号各新建一个网页端会话，只走网页通道，不会写入「已完成」标记。确定继续？': 'This opens one web session per target account and uses the web channel only; it does not set the "done today" marker. Continue?',
  '当前网关未启用每日活跃功能': 'Daily activity is not enabled on this gateway', '没有已开启任务的国际版账号': 'No global accounts with tasks enabled',
  '还没有国际版账号': 'No global-region accounts yet',
});

const TIP_LIMIT = 30;

export function intlSection(store) {
  const kDone = kpi({ label: t('今日活跃打卡'), icon: 'checkCircle' });
  const kPending = kpi({ label: t('待打卡账号'), icon: 'clock' });
  const kReward = kpi({ label: t('累计活跃奖励积分'), icon: 'coins' });
  const vDone = kDone.el.querySelector('.kpi-value');
  const kpis = h('div.grid.cols-3', kDone.el, kPending.el, kReward.el);

  /** Accounts the picker targets that can run and have not finished today. */
  function pendingOf() {
    return store.scope().target.filter((a) => can('dailyChat', a).ok && !isDone(store.statusOf(a)));
  }

  function verdictFor() {
    if (!store.caps.chat) return { ok: false, why: t('当前网关未启用每日活跃功能') };
    const { list, target } = store.scope();
    if (!list.length) return { ok: false, why: t('还没有国际版账号') };
    if (store.sel !== 'all') return can('dailyChat', target[0]);
    return target.some((a) => can('dailyChat', a).ok) ? { ok: true, why: '' } : { ok: false, why: t('没有已开启任务的国际版账号') };
  }

  const runBtn = btn(t('每日活跃打卡'), { variant: 'primary', icon: 'play', onClick: (e, el) => store.exec(defs.dailyChat(store.sel), el) });
  const pendingBtn = btn(t('未活跃账号一键打卡'), { icon: 'zap', onClick: (e, el) => {
    const uids = pendingOf().map((a) => a.uid);
    if (!uids.length) { ok(t('国际版账号今天都已打卡')); return null; }
    return store.exec(defs.dailyChatPending(uids), el);
  } });
  const pendingLabel = pendingBtn.querySelector('span');
  const webBtn = btn(t('网页通道'), { variant: 'ghost', onClick: async (e, el) => {
    const yes = await confirmDialog({ title: t('仅网页通道打卡…'), message: t('将为每个目标账号各新建一个网页端会话，只走网页通道，不会写入「已完成」标记。确定继续？') });
    if (yes) await store.exec(defs.dailyChatWeb(store.sel), el);
  } });
  const scope = h('div.tk-scope');
  const why = h('div.tk-why', { hidden: true });
  const actions = card(t('每日活跃'), { actions: [scope] },
    h('p.muted.tk-desc', t('为国际版账号发起一次桌面端对话，并按设置补一次网页通道对话，用来领取每日活跃奖励。已完成的账号会被跳过，不会重复消耗额度。')),
    h('div.row.wrap', { style: 'gap:10px;margin-top:14px' }, runBtn, pendingBtn, webBtn),
    why);

  function paint() {
    const { list } = store.scope();
    const active = list.filter((a) => can('dailyChat', a).ok && taskActive(a));
    const states = active.map((a) => store.statusOf(a));
    const n = active.length;
    const done = states.filter(isDone).length;
    const failedN = states.filter((s) => s.key === 'failed').length;
    const pending = active.filter((a, i) => !isDone(states[i]));

    if (!n) { kDone.set({ value: '—', foot: t('无国际账号') }); vDone.style.color = ''; }
    else {
      const label = done === n ? t('今日已打卡') : done ? t('部分已打卡') : t('今日未打卡');
      kDone.set({ value: `${done}/${n}`, foot: failedN ? `${label} · ${t('{n} 个失败', { n: failedN })}` : label });
      vDone.style.color = done === n ? 'var(--good)' : done ? 'var(--accent)' : 'var(--warn)';
    }

    const names = pending.map((a) => a.nickname || a.uid.slice(0, 8));
    kPending.set({ value: n ? String(pending.length) : '—', foot: !n ? ' ' : pending.length ? names.slice(0, 3).join(', ') + (names.length > 3 ? ' …' : '') : t('国际版账号今天都已打卡') });
    const tip = pending.length
      ? [t('今日未打卡账号（{k}）：', { k: pending.length }), ...names.slice(0, TIP_LIMIT).map((x) => '· ' + x), names.length > TIP_LIMIT ? '· …' : ''].filter(Boolean).join('\n')
      : '';
    setAttr(kPending.el, 'data-tip', tip || null);

    const g = store.grantTotal('intl');
    kReward.set({ value: g.value, foot: g.note });

    const verdict = verdictFor();
    const left = pendingOf().length;
    setText(pendingLabel, left ? t('未活跃账号一键打卡 ({n})', { n: left }) : t('未活跃账号一键打卡'));
    gate(runBtn, store, verdict);
    gate(webBtn, store, verdict);
    gate(pendingBtn, store, verdict.ok && !left ? { ok: false, why: t('国际版账号今天都已打卡') } : verdict);
    setText(scope, store.scopeText());
    setText(why, verdict.ok ? '' : verdict.why);
    why.hidden = verdict.ok;
  }

  return { top: [kpis, actions], paint };
}
