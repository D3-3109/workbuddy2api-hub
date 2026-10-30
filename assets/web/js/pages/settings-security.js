// Settings: panel password and API access status.
import { h, setText } from '../lib/dom.js';
import { session } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import { badge, btn, alertBox, field, checkbox, ok } from '../lib/ui.js';
import { section, row, postKeep, errMsg, dialog } from './settings-kit.js';

add({
  '访问与安全': 'Access and security', '面板密码与接口访问校验': 'Panel password and API access check',
  '面板访问密码': 'Panel password', '默认密码': 'Default password', '已自定义': 'Customised',
  '登录管理面板所需的密码。修改后其他已登录的会话会被撤销，当前会话保持登录。': 'The password for signing in to this panel. Changing it revokes other signed-in sessions; this one stays signed in.',
  '修改密码': 'Change password', '当前仍在使用默认密码 admin，任何知道地址的人都能登录面板，建议立即修改。': 'The default password "admin" is still in use. Anyone who can reach this address can sign in; change it now.',
  '接口访问校验': 'API access check', '需要 API Key': 'API key required', '不校验': 'No check',
  '客户端必须携带有效的 API Key。当前有 {n} 个已启用的 Key。': 'Clients must send a valid API key. {n} keys are enabled.',
  '没有可用的 API Key，任何能访问网关的客户端都可以调用接口。请在下方创建 Key。': 'No API key is enabled, so any client that can reach the gateway may call the API. Create a key below.',
  '「auth_disabled」已打开：接口不校验 Key。': '“auth_disabled” is on: the API does not check keys.',
  '当前密码': 'Current password', '新密码': 'New password', '确认新密码': 'Confirm new password', '显示密码': 'Show passwords',
  '至少 4 个字符。': 'At least 4 characters.', '请输入当前密码': 'Enter the current password', '新密码至少需要 4 个字符': 'The new password needs at least 4 characters',
  '两次输入的新密码不一致': 'The two new passwords do not match', '新密码不能与当前密码相同': 'The new password must differ from the current one',
  '密码已修改': 'Password changed',
});

const chars = (s) => [...s].length;

function passwordDialog(S) {
  const cur = h('input.input', { type: 'password', autocomplete: 'current-password', spellcheck: 'false' });
  const nw = h('input.input', { type: 'password', autocomplete: 'new-password', spellcheck: 'false' });
  const again = h('input.input', { type: 'password', autocomplete: 'new-password', spellcheck: 'false' });
  const eCur = h('div.err'), eNew = h('div.err'), eAgain = h('div.err'), eMisc = h('div');
  const inputs = [cur, nw, again];
  let m;

  async function submit() {
    [eCur, eNew, eAgain].forEach((e) => setText(e, ''));
    eMisc.replaceChildren();
    for (const i of inputs) i.removeAttribute('aria-invalid');
    let bad = null;
    const fail = (input, el, msg) => { setText(el, msg); input.setAttribute('aria-invalid', 'true'); bad = bad || input; };
    if (!cur.value) fail(cur, eCur, t('请输入当前密码'));
    if (chars(nw.value) < 4) fail(nw, eNew, t('新密码至少需要 4 个字符'));
    else if (nw.value === cur.value) fail(nw, eNew, t('新密码不能与当前密码相同'));
    else if (again.value !== nw.value) fail(again, eAgain, t('两次输入的新密码不一致'));
    if (bad) { bad.focus(); return; }
    try {
      const r = await postKeep('/panel/password', { current: cur.value, new: nw.value });
      if (r.token) session.set(r.token);
      S.apply({ ...S.view, panel_password_is_default: !!r.using_default_password });
      m.close(true);
      ok(t('密码已修改'));
    } catch (e) {
      if (e.status === 401) fail(cur, eCur, errMsg(e));
      else if (e.status === 400) fail(nw, eNew, errMsg(e));
      else eMisc.replaceChildren(alertBox('bad', errMsg(e)));
      if (bad) bad.focus();
    }
  }
  for (const i of inputs) i.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); submit(); } });

  const show = checkbox({ label: t('显示密码'), onChange: (on) => { for (const i of inputs) i.type = on ? 'text' : 'password'; } });
  m = dialog({
    title: t('修改密码'), size: 'narrow',
    body: [field(t('当前密码'), cur, null, eCur), field(t('新密码'), nw, t('至少 4 个字符。'), eNew), field(t('确认新密码'), again, null, eAgain), show, eMisc],
    actions: [btn(t('取消'), { onClick: () => m.close(false) }), btn(t('修改密码'), { variant: 'primary', onClick: submit })],
  });
  return m;
}

export function securityCard(S) {
  const stateBadge = h('span');
  const passRow = row({
    title: t('面板访问密码'), help: t('登录管理面板所需的密码。修改后其他已登录的会话会被撤销，当前会话保持登录。'),
    ctl: btn(t('修改密码'), { icon: 'lock', onClick: () => { passwordDialog(S); } }),
  });
  passRow.titleEl.appendChild(stateBadge);
  const warn = h('div.s-block', alertBox('warn', t('当前仍在使用默认密码 admin，任何知道地址的人都能登录面板，建议立即修改。')));

  const authBadge = h('span');
  const authRow = row({ title: t('接口访问校验'), help: '' });
  authRow.titleEl.appendChild(authBadge);

  S.bind((v) => {
    const dflt = !!v.panel_password_is_default;
    stateBadge.replaceChildren(badge(dflt ? t('默认密码') : t('已自定义'), dflt ? 'warn' : 'good', { dot: true, sm: true }));
    warn.hidden = !dflt;
    const need = v.auth_required !== false;
    authBadge.replaceChildren(badge(need ? t('需要 API Key') : t('不校验'), need ? 'good' : 'warn', { dot: true, sm: true }));
    const n = (v.api_keys || []).filter((k) => k.enabled).length;
    authRow.setHelp(v.auth_disabled ? t('「auth_disabled」已打开：接口不校验 Key。') : need ? t('客户端必须携带有效的 API Key。当前有 {n} 个已启用的 Key。', { n }) : t('没有可用的 API Key，任何能访问网关的客户端都可以调用接口。请在下方创建 Key。'));
  });
  return section('security', t('访问与安全'), { sub: t('面板密码与接口访问校验') }, passRow.el, warn, authRow.el);
}
