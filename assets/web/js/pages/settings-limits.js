// Settings: per-account guards (global / per realm), expiring-credit priority, concurrency.
import { h } from '../lib/dom.js';
import { t, add } from '../lib/i18n.js';
import { checkbox, confirmDialog, switchEl, fail } from '../lib/ui.js';
import { section, row, switchRow, numRow, numField, patchOf, errMsg } from './settings-kit.js';

add({
  '限额与调度': 'Limits and dispatch', '账号限额、临期优先与并发': 'Account guards, expiring-credit priority and concurrency',
  '账号限额': 'Account guards', '0 表示不启用该限额。国际版或国内版留空则沿用全局值。': '0 turns a guard off. Leave Global or China empty to inherit the global value.',
  '限额': 'Guard', '全局': 'All', '国际版': 'Global', '国内版': 'China', '继承': 'Inherit', '按地区分别设置': 'Set per region',
  '保底积分': 'Credit reserve', '账号积分低于该值时不再接收请求，避免余额归零触发上游短信提醒。': 'An account stops taking requests when its credits drop below this, which avoids the upstream SMS warning at zero.',
  '每日 Token 上限': 'Daily token limit', '账号当日 Token 用量达到后暂停，请求转移到其他账号；本地零点重置。': 'An account pauses after this many tokens today and requests move to other accounts. Resets at local midnight.',
  '每日积分上限': 'Daily credit limit', '当日积分消耗达到后，该账号只允许免费模型（目录倍率 ×0.00）。': 'After this many credits are spent today, the account only serves free models (catalogue multiplier ×0.00).',
  '单模型每日 Token 上限': 'Per-model daily token limit', '按「账号 × 模型」限制每日 Token，同账号的其他模型不受影响。': 'Daily token cap per account and model. Other models on the same account are unaffected.',
  '临期优先窗口（天）': 'Expiry window (days)', '账号最早到期的积分包在这么多天内到期时，调度优先使用它，越临近权重越高；0 为关闭。': 'When an account’s earliest-expiring credit pack is due within this many days, dispatch prefers it, more strongly the closer it gets. 0 is off.',
  '启用临期优先': 'Prefer expiring credits', '打开时使用 7 天窗口，也可以在上表调整天数；关闭会把全局窗口设为 0。': 'Turning it on uses a 7-day window (adjust it in the table above). Turning it off sets the global window to 0.',
  '取消按地区设置': 'Remove regional overrides', '关闭后会清除国际版和国内版的单独设置，全部沿用全局值。确定继续？': 'This clears the Global and China overrides so every region uses the global value. Continue?',
  '清除': 'Clear', '积分刷新间隔': 'Credit refresh interval', '多久重新拉取一次各账号的积分数据（小时），0 为关闭。范围 0 到 720。': 'How often to refetch each account’s credit data, in hours. 0 turns it off. Range 0 to 720.',
  '账号并发上限': 'Per-account concurrency', '每个上游账号同时处理的请求数，0 为不限制。超出的请求不排队，会收到「账号都在忙」，由客户端重试；设为 1 可确保开头相同的两个会话不会落到同一个账号。': 'Simultaneous requests per upstream account, 0 for unlimited. Extra requests are not queued: they get “every account busy” and the client retries. 1 guarantees two sessions with the same opening never share an account.',
  '稳定会话亲和': 'Stable session affinity', '会话粘性键同时包含第一条助手回复，开头完全相同的两个会话不再互相抢账号。已有对话会重新绑定一次（一次缓存未命中）。': 'The session stickiness key also hashes the first assistant reply, so two sessions with identical openings stop swapping accounts. Existing conversations rebind once (one cache miss).',
  '余量优先': 'Prefer low remaining quota', '优先使用预估剩余额度较少的「账号 + 模型」（剩余 30% 起加权，5% 时最强）。': 'Prefer account+model pairs whose estimated remaining quota is low (weighting starts at 30% left, strongest at 5%).',
  '小时': 'h',
});

const GUARDS = [
  { key: 'reserve_credits', title: () => t('保底积分'), help: () => t('账号积分低于该值时不再接收请求，避免余额归零触发上游短信提醒。') },
  { key: 'daily_token_limit', title: () => t('每日 Token 上限'), help: () => t('账号当日 Token 用量达到后暂停，请求转移到其他账号；本地零点重置。') },
  { key: 'daily_credit_limit', title: () => t('每日积分上限'), help: () => t('当日积分消耗达到后，该账号只允许免费模型（目录倍率 ×0.00）。') },
  { key: 'model_daily_token_limit', title: () => t('单模型每日 Token 上限'), help: () => t('按「账号 × 模型」限制每日 Token，同账号的其他模型不受影响。') },
  { key: 'expiring_window_days', title: () => t('临期优先窗口（天）'), help: () => t('账号最早到期的积分包在这么多天内到期时，调度优先使用它，越临近权重越高；0 为关闭。') },
];
const hasOverrides = (v) => GUARDS.some((g) => { const l = (v.limits || {})[g.key] || {}; return l.intl != null || l.cn != null; });

function guardsTable(S) {
  const cellErr = (m) => { if (m) fail(m); };
  const table = h('table.table.lim-table');
  const rows = GUARDS.map((g) => {
    const cell = (scope, placeholder) => numField(S, {
      key: `limits.${g.key}.${scope}`, label: g.title() + ' · ' + (scope === 'global' ? t('全局') : scope === 'intl' ? t('国际版') : t('国内版')),
      blank: scope === 'global' ? 'reject' : 'null', placeholder, cls: 'sm', onError: cellErr,
      patch: (v) => ({ limits: { [g.key]: { [scope]: v } } }),
    });
    return h('tr', h('td.lim-name', h('div.cell-main', g.title()), h('div.cell-sub', g.help())),
      h('td.lim-cell', cell('global', '0')), h('td.lim-cell.c-realm', cell('intl', t('继承'))), h('td.lim-cell.c-realm', cell('cn', t('继承'))));
  });
  table.append(h('thead', h('tr', h('th', t('限额')), h('th', t('全局')), h('th.c-realm', t('国际版')), h('th.c-realm', t('国内版')))), h('tbody', rows));

  let per = hasOverrides(S.view);
  const apply = () => table.classList.toggle('no-realm', !per);
  const box = checkbox({ label: t('按地区分别设置'), checked: per, onChange: async (on, input) => {
    if (on) { per = true; apply(); return; }
    if (hasOverrides(S.view)) {
      const yes = await confirmDialog({ title: t('取消按地区设置'), message: t('关闭后会清除国际版和国内版的单独设置，全部沿用全局值。确定继续？'), confirmText: t('清除'), danger: true });
      if (!yes) { input.checked = true; return; }
      const limits = {};
      for (const g of GUARDS) limits[g.key] = { intl: null, cn: null };
      try { await S.save({ limits }); } catch (e) { input.checked = true; fail(errMsg(e)); return; }
    }
    per = false; apply();
  } });
  S.bind((v) => { if (hasOverrides(v) && !per) { per = true; box.input.checked = true; } apply(); });
  apply();
  return { table, box };
}

export function limitsCard(S) {
  const { table, box } = guardsTable(S);

  // Expiring-credit priority: on = global window 7 days, off = 0.
  const sw = switchEl({ checked: (S.val('limits.expiring_window_days.global') || 0) > 0, label: t('启用临期优先') });
  sw.input.addEventListener('change', async () => {
    const on = sw.input.checked;
    sw.input.disabled = true;
    try { await S.save(patchOf('limits.expiring_window_days.global', on ? 7 : 0)); }
    catch (e) { sw.input.checked = !on; fail(errMsg(e)); }
    finally { sw.input.disabled = false; }
  });
  S.bind((v) => { sw.input.checked = ((v.limits || {}).expiring_window_days || {}).global > 0; });
  const expiring = row({ title: t('启用临期优先'), help: t('打开时使用 7 天窗口，也可以在上表调整天数；关闭会把全局窗口设为 0。'), ctl: sw });

  const cap = S.view.account_max_concurrent_max || 10;
  return section('limits', t('限额与调度'), { sub: t('账号限额、临期优先与并发') },
    h('div.s-block.lim-head', h('div.px-title', h('span', t('账号限额'))), box),
    h('div.s-block.lim-note', t('0 表示不启用该限额。国际版或国内版留空则沿用全局值。')),
    h('div.table-wrap', table),
    expiring.el,
    numRow(S, { key: 'credits_refresh_hours', title: t('积分刷新间隔'), help: t('多久重新拉取一次各账号的积分数据（小时），0 为关闭。范围 0 到 720。'), min: 0, max: 720, int: false, unit: t('小时') }).el,
    numRow(S, { key: 'account_max_concurrent', title: t('账号并发上限'), help: t('每个上游账号同时处理的请求数，0 为不限制。超出的请求不排队，会收到「账号都在忙」，由客户端重试；设为 1 可确保开头相同的两个会话不会落到同一个账号。'), min: 0, max: cap }).el,
    switchRow(S, { key: 'affinity_stable_key_enabled', title: t('稳定会话亲和'), help: t('会话粘性键同时包含第一条助手回复，开头完全相同的两个会话不再互相抢账号。已有对话会重新绑定一次（一次缓存未命中）。') }).el,
    switchRow(S, { key: 'remaining_priority_enabled', title: t('余量优先'), help: t('优先使用预估剩余额度较少的「账号 + 模型」（剩余 30% 起加权，5% 时最强）。') }).el);
}
