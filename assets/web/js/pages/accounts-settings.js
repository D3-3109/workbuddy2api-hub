// Account detail dialog: the "settings" tab. Every control applies at once through host.actions.
import { h, setText } from '../lib/dom.js';
import { t, add } from '../lib/i18n.js';
import { btn, switchEl, seg, badge, fail } from '../lib/ui.js';
import { PRODUCTS, productOf } from './accounts-model.js';
import { needsLogin } from './accounts-rows.js';
import { pickModels } from './accounts-models.js';

add({
  '调度与任务': 'Scheduling and tasks', '参与 API 调度': 'Serve API traffic', '关闭后该账号不再接收请求，但仍可参与任务。': 'When off the account takes no requests but can still run tasks.',
  '参与任务': 'Run tasks', '签到 / 成长任务 / 连登管家 / 猫猫旅行 / 国际版活跃。新登录或新添加的账号默认关闭。': 'Check-in, growth tasks, streak keeper, cat travel and global daily activity. Off by default for newly added accounts.',
  'API Key 账号不参与任务': 'API key accounts do not run tasks',
  '权重': 'Weight', '权重 >1 的账号优先接单，多个高权重账号按权重比分流量；它们都接不了单时其他账号照常顶上。已开始的会话保持粘性不换号，权重对新会话生效。1 = 不干预。': 'Accounts with weight > 1 are preferred and share traffic by weight ratio; others still fill in when they cannot serve. Running sessions stay sticky; weight applies to new sessions. 1 = no preference.',
  '保存': 'Save', '请输入 1 到 100 的整数': 'Enter an integer from 1 to 100',
  '备注': 'Note', '最多 200 个字符，可用来标记「批量」/「自用」。': 'Up to 200 characters, handy for tags like “bulk” or “personal”.', '账号备注': 'Account note',
  '出站身份': 'Outbound identity', '出站身份：WB = WorkBuddy 独立桌面客户端（默认），VSC = 官方 VSCode 插件，CLI = 官方 CodeBuddy CLI；三者对应不同的出站指纹与配额通道。': 'WB = WorkBuddy desktop client (default), VSC = official VSCode extension, CLI = official CodeBuddy CLI. Each has its own fingerprint and quota channel.',
  '出口代理': 'Proxy exit', '直连': 'Direct', '（停用）': ' (disabled)', '旧代理（仅旧版配置，选择其他项后无法恢复）': 'Legacy proxy (old config, cannot be restored once changed)', '代理槽在「设置」页维护。': 'Proxy slots are managed in Settings.',
  '禁用的模型': 'Blocked models', '无': 'None', '编辑…': 'Edit…', '被禁用的模型不会派给这个账号。': 'Blocked models are never sent to this account.',
  '凭证': 'Credentials', '刷新凭证': 'Refresh token', '重新登录': 'Re-login', '测试': 'Test', '用 refresh token 换新的 access token。': 'Exchange the refresh token for a new access token.', '需要重新登录': 'Needs re-login',
  '危险操作': 'Danger zone', '导出': 'Export', '删除': 'Delete', '导出该账号（含明文凭证）': 'Export this account (includes plain-text tokens)', '删除账号及其凭证文件，无法恢复。': 'Deletes the account and its credential file. This cannot be undone.',
});

function row(title, help, ...ctl) {
  return h('div.setting', h('div.s-text', h('div.s-title', title), help ? h('div.s-help', help) : null), h('div.s-ctl', ...ctl));
}

export function settingsPanel(host, uid, onGone) {
  const { actions } = host;
  const cur = () => host.store.get(uid);

  const enabled = switchEl({ label: t('参与 API 调度'), onChange: async (on, input) => {
    await actions.setEnabled(cur(), on);
    input.checked = !!cur().enabled;
  } });
  const tasks = switchEl({ label: t('参与任务'), onChange: async (on, input) => {
    try { await actions.setTasks(cur(), on); } catch { input.checked = !on; }
  } });
  const tasksRow = row(t('参与任务'), t('签到 / 成长任务 / 连登管家 / 猫猫旅行 / 国际版活跃。新登录或新添加的账号默认关闭。'), tasks);
  const tasksNa = row(t('参与任务'), t('API Key 账号不参与任务'), badge('—', ''));

  const weight = h('input.input.narrow', { type: 'number', min: '1', max: '100', step: '1', 'aria-label': t('权重') });
  const saveWeight = btn(t('保存'), { size: 'sm', onClick: async () => {
    const v = weight.value.trim();
    if (!/^d{1,3}$/.test(v) || +v < 1 || +v > 100) { fail(t('请输入 1 到 100 的整数')); return; }
    await actions.setWeight(cur(), Number(v));
  } });

  const note = h('input.input', { type: 'text', maxlength: '200', autocomplete: 'off', 'aria-label': t('账号备注') });
  const saveNote = btn(t('保存'), { size: 'sm', onClick: () => actions.saveNote(cur(), note.value.trim()) });
  note.addEventListener('keydown', (e) => { if (e.key === 'Enter') saveNote.click(); });

  const product = seg(PRODUCTS.map((p) => ({ value: p.id, label: p.short })), 'workbuddy', async (v) => {
    await actions.setProduct(cur(), v);
    product.set(productOf(cur()));
  }, { size: 'sm' });

  const proxy = h('select.select', { 'aria-label': t('出口代理') });
  proxy.addEventListener('change', () => actions.setProxy(cur(), proxy.value));

  const blockedTxt = h('span.muted.trunc', { style: 'max-width:260px' });
  const edit = btn(t('编辑…'), { size: 'sm', onClick: async () => { await host.loadSlots(); pickModels(host, cur()); } });

  const refresh = btn(t('刷新凭证'), { size: 'sm', icon: 'refresh', onClick: () => actions.refreshToken(cur()) });
  const relogin = btn(t('重新登录'), { size: 'sm', icon: 'key', onClick: () => actions.relogin(cur()) });
  const test = btn(t('测试'), { size: 'sm', icon: 'zap', onClick: () => actions.test(cur()) });

  const del = btn(t('删除'), { size: 'sm', icon: 'trash', variant: 'danger', onClick: async () => { if (await actions.remove(cur())) onGone(); } });
  const exp = btn(t('导出'), { size: 'sm', icon: 'download', onClick: () => actions.exportUids([uid]) });

  const el = h('div.col', { style: 'gap:16px' },
    h('div.card', row(t('参与 API 调度'), t('关闭后该账号不再接收请求，但仍可参与任务。'), enabled), tasksRow, tasksNa,
      row(t('权重'), t('权重 >1 的账号优先接单，多个高权重账号按权重比分流量；它们都接不了单时其他账号照常顶上。已开始的会话保持粘性不换号，权重对新会话生效。1 = 不干预。'), weight, saveWeight),
      row(t('备注'), t('最多 200 个字符，可用来标记「批量」/「自用」。'), note, saveNote),
      row(t('出站身份'), t('出站身份：WB = WorkBuddy 独立桌面客户端（默认），VSC = 官方 VSCode 插件，CLI = 官方 CodeBuddy CLI；三者对应不同的出站指纹与配额通道。'), product),
      row(t('出口代理'), t('代理槽在「设置」页维护。'), proxy),
      row(t('禁用的模型'), t('被禁用的模型不会派给这个账号。'), blockedTxt, edit),
      row(t('凭证'), t('用 refresh token 换新的 access token。'), test, refresh, relogin)),
    h('div.card', { style: 'border-color:color-mix(in srgb, var(--bad) 35%, var(--line))' },
      row(t('导出'), t('导出该账号（含明文凭证）'), exp),
      row(t('删除'), t('删除账号及其凭证文件，无法恢复。'), del)));

  let sig = '';
  function paint(a) {
    const slots = host.slots();
    const s = [a.enabled, a.tasksEnabled, a.apiKey, a.weight, a.note, a.product, a.proxySlot, a.hasProxy, a.hasRefreshToken, a.tokenExpired, (a.disabledModels || []).join(','), slots.map((x) => x.id + x.enabled).join(',')].join('|');
    if (s === sig) return;
    sig = s;
    enabled.input.checked = !!a.enabled;
    tasks.input.checked = !!a.tasksEnabled;
    tasksRow.hidden = !!a.apiKey;
    tasksNa.hidden = !a.apiKey;
    if (document.activeElement !== weight) weight.value = String(a.weight || 1);
    if (document.activeElement !== note) note.value = a.note || '';
    product.set(productOf(a));
    const opts = [h('option', { value: '' }, t('直连'))];
    if (a.hasProxy && !a.proxySlot) opts.push(h('option', { value: '__legacy', disabled: true }, t('旧代理（仅旧版配置，选择其他项后无法恢复）')));
    for (const sl of slots) opts.push(h('option', { value: sl.id }, (sl.label || sl.name || sl.id) + (sl.enabled === false ? t('（停用）') : '')));
    if (a.proxySlot && !slots.some((x) => x.id === a.proxySlot)) opts.push(h('option', { value: a.proxySlot }, a.proxySlot));
    proxy.replaceChildren(...opts);
    proxy.value = a.proxySlot || (a.hasProxy ? '__legacy' : '');
    const dm = a.disabledModels || [];
    setText(blockedTxt, dm.length ? dm.join(', ') : t('无'));
    blockedTxt.setAttribute('data-tip', dm.join('\n'));
    const login = needsLogin(a);
    refresh.hidden = login || !!a.apiKey;
    relogin.hidden = !login;
    relogin.classList.toggle('danger', !!a.tokenExpired);
  }
  return { el, paint };
}
