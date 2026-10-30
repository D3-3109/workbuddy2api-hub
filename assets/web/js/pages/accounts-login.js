// Accounts page: OAuth login flow (start, poll, cancel) and the static API key dialog.
import { h, setText } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { get, post, qs } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import { poller } from '../lib/poll.js';
import { btn, modal, field, seg, ok, toast, errText, copyText } from '../lib/ui.js';
import { loose, bodyError } from './accounts-store.js';
import { accountName } from './accounts-model.js';

add({
  '添加 WorkBuddy 账号 (OAuth 授权)': 'Add a WorkBuddy account (OAuth)', '国内版 (copilot.tencent.com)': 'China (copilot.tencent.com)', '国际版 (www.workbuddy.ai)': 'Global (www.workbuddy.ai)',
  '正在向 {host} 申请授权...': 'Requesting authorization from {host}…', '在浏览器里打开下方链接并完成登录授权': 'Open the link below in a browser and finish signing in', '等待授权回调完成（自动检测）': 'Waiting for the authorization callback (detected automatically)',
  '打开登录页': 'Open sign-in page', '复制登录链接': 'Copy sign-in link', '重新生成链接': 'Regenerate link', '关闭': 'Close',
  '登录完成后本窗口会自动检测并完成入池。链接 10 分钟内有效；关闭窗口会取消这次登录。': 'This window detects the login and adds the account automatically. The link is valid for 10 minutes; closing the window cancels the login.',
  '发起失败: {msg}': 'Could not start: {msg}', '登录失败: {msg}': 'Login failed: {msg}', '登录成功：{name} 已加入账号池': 'Signed in: {name} was added to the pool',
  '已加入{r}，切换到该区域即可查看': 'Added to the {r} region; switch regions to see it', '等待授权…': 'Waiting for authorization…',
  '添加 API Key 账号': 'Add an API key account', '粘贴 CodeBuddy API 管理页签发的静态 Key（ck_ / pt_ 前缀）。它会先拿企业额度端点验一次，通过了才落盘——验不过就什么都不写。': 'Paste a static key issued by the CodeBuddy API page (ck_ / pt_ prefix). It is checked against the enterprise quota endpoint first and only saved if that passes.',
  '静态 API Key': 'Static API key', '备注名（可选）': 'Label (optional)', '例如：企微企业号': 'e.g. Company account', '验证并添加': 'Verify and add', '取消': 'Cancel', '验证中…': 'Verifying…',
  '请先填入 API Key': 'Enter an API key first', '前缀不对：应当以 ck_ 或 pt_ 开头': 'Wrong prefix: it should start with ck_ or pt_', '正在用企业额度端点验一次…': 'Checking against the enterprise quota endpoint…',
  '添加失败: {msg}': 'Could not add: {msg}', '已添加 {name}': 'Added {name}', '已更新 {name}': 'Updated {name}', ' · 额度 {n}': ' · quota {n}', '，本期已用 {n}': ', used {n} this cycle',
  '静态 Key 账号只属于国内版。': 'Static key accounts always belong to the China region.',
});

const HOST = { cn: 'copilot.tencent.com', intl: 'www.workbuddy.ai' };

/* ---------------------------------------------------------------- OAuth */
export function openLogin(host, realm) {
  let realmSel = realm === 'cn' ? 'cn' : 'intl';
  let state = '', url = '', stage = 'starting', msg = '', seq = 0, closed = false, pl = null, timer = 0;

  const realmSeg = seg([{ value: 'cn', label: t('国内版 (copilot.tencent.com)') }, { value: 'intl', label: t('国际版 (www.workbuddy.ai)') }], realmSel, (v) => { realmSel = v; start(); }, { size: 'sm' });
  const stepEls = [0, 1, 2].map(() => h('span.ls-dot'));
  const stepTxt = [h('span'), h('span'), h('span')];
  const steps = h('ol.login-steps', stepEls.map((d, i) => h('li', d, stepTxt[i])));
  const link = h('a.login-link.mono', { target: '_blank', rel: 'noopener noreferrer' });
  const linkBox = h('div.login-linkbox', link,
    h('div.row.gap-1', btn(t('复制登录链接'), { size: 'sm', icon: 'copy', onClick: () => copyText(url) }),
      btn(t('打开登录页'), { size: 'sm', icon: 'external', variant: 'primary', onClick: () => window.open(url, '_blank', 'noopener,noreferrer') })));
  const status = h('div.login-status', { role: 'status' });
  const regen = btn(t('重新生成链接'), { icon: 'refresh', onClick: () => start() });
  const close = btn(t('关闭'), { onClick: () => m.close() });

  const m = modal({
    title: t('添加 WorkBuddy 账号 (OAuth 授权)'), persistent: true,
    body: [realmSeg, steps, linkBox, status, h('p.faint', { style: 'font-size:12.5px' }, t('登录完成后本窗口会自动检测并完成入池。链接 10 分钟内有效；关闭窗口会取消这次登录。'))],
    actions: [regen, close],
    onClose: () => { closed = true; clearTimeout(timer); stopPoll(); if (state && stage !== 'done') cancel(state); host.dialogs.delete(m); },
  });
  host.dialogs.add(m);

  const cancel = (s) => { post('/accounts/login/cancel', { state: s }).catch(() => {}); };
  function stopPoll() { if (pl) { pl.stop(); pl = null; } }

  function paint() {
    const phase = stage === 'starting' ? 0 : stage === 'waiting' ? 2 : stage === 'done' ? 3 : (url ? 2 : 0);
    setText(stepTxt[0], t('正在向 {host} 申请授权...', { host: HOST[realmSel] }));
    setText(stepTxt[1], t('在浏览器里打开下方链接并完成登录授权'));
    setText(stepTxt[2], t('等待授权回调完成（自动检测）'));
    stepEls.forEach((d, i) => {
      d.className = 'ls-dot' + (i < phase ? ' done' : i === phase ? (stage === 'error' ? ' bad' : ' active') : '');
    });
    linkBox.hidden = !url || stage === 'error' && !state;
    link.href = url || '#';
    setText(link, url);
    status.className = 'login-status' + (stage === 'error' ? ' bad' : stage === 'done' ? ' good' : '');
    setText(status, stage === 'waiting' ? (msg || t('等待授权…')) : msg);
    realmSeg.set(realmSel);
  }

  async function start() {
    const mine = ++seq;
    stopPoll();
    if (state && stage !== 'done') cancel(state);
    state = ''; url = ''; msg = ''; stage = 'starting';
    paint();
    try {
      const r = await post('/accounts/login/start', { platform: 'CLI', realm: realmSel });
      if (mine !== seq || closed) { cancel(r.state); return; }
      state = r.state; url = r.authUrl; stage = 'waiting'; paint();
      pl = poller(() => pollOnce(mine), 2500);
      pl.start();
    } catch (e) {
      if (mine !== seq) return;
      stage = 'error'; msg = t('发起失败: {msg}', { msg: errText(e) }); paint();
    }
  }

  async function pollOnce(mine) {
    let r;
    try { r = await get('/accounts/login/poll' + qs({ state })); } catch (e) {
      if (mine !== seq || closed) return;
      if (e && e.status === 0) return;                 // transient network error: keep trying
      stopPoll(); stage = 'error'; msg = t('登录失败: {msg}', { msg: errText(e) }); paint(); return;
    }
    if (mine !== seq || closed) return;
    if (r.status === 'ok' && r.account) {
      stopPoll(); stage = 'done';
      msg = t('登录成功：{name} 已加入账号池', { name: accountName(r.account) });
      paint();
      host.store.put(r.account);
      host.reload();
      if (r.account.realm && r.account.realm !== host.view) toast(t('已加入{r}，切换到该区域即可查看', { r: t(r.account.realm === 'cn' ? '国内版' : '国际版') }), 'info');
      timer = setTimeout(() => m.close(), 1200);
    } else if (r.status === 'pending') {
      msg = r.message || ''; paint();
    } else {
      stopPoll(); stage = 'error'; msg = r.message || r.status || ''; paint();
    }
  }
  start();
  return m;
}

/* -------------------------------------------------------------- API key */
export function openApiKey(host) {
  const key = h('input.input.mono', { type: 'password', autocomplete: 'off', spellcheck: 'false', placeholder: 'ck_xxxxxxxx.yyyyyyyy', 'aria-label': t('静态 API Key') });
  const nick = h('input.input', { type: 'text', maxlength: '64', placeholder: t('例如：企微企业号'), autocomplete: 'off' });
  const msg = h('div.login-status', { role: 'status' });
  let busy = false;
  const submit = btn(t('验证并添加'), { variant: 'primary', icon: 'check', onClick: go });
  const m = modal({
    title: t('添加 API Key 账号'), size: 'narrow',
    body: [h('p.muted', { style: 'font-size:13px' }, t('粘贴 CodeBuddy API 管理页签发的静态 Key（ck_ / pt_ 前缀）。它会先拿企业额度端点验一次，通过了才落盘——验不过就什么都不写。')),
      field(t('静态 API Key'), key, t('静态 Key 账号只属于国内版。')), field(t('备注名（可选）'), nick), msg],
    actions: [btn(t('取消'), { onClick: () => m.close() }), submit],
    onClose: () => { key.value = ''; host.dialogs.delete(m); },
  });
  host.dialogs.add(m);
  key.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); go(); } });

  const say = (text, kind) => { msg.className = 'login-status' + (kind ? ' ' + kind : ''); setText(msg, text); };
  async function go() {
    if (busy) return;
    const k = key.value.trim();
    if (!k) { say(t('请先填入 API Key'), 'bad'); return; }
    if (!/^(ck_|pt_)/.test(k)) { say(t('前缀不对：应当以 ck_ 或 pt_ 开头'), 'bad'); return; }
    busy = true; submit.classList.add('loading'); say(t('正在用企业额度端点验一次…'));
    try {
      const r = await loose('POST', '/accounts/add-api-key', { key: k, nickname: nick.value.trim() });
      if (!r.ok || r.data.ok === false) { say(t('添加失败: {msg}', { msg: bodyError(r.data, r.status) }), 'bad'); return; }
      const d = r.data, p = d.probe || {};
      if (d.account) host.store.put(d.account);
      ok(t(d.updated ? '已更新 {name}' : '已添加 {name}', { name: d.nickname || d.uid })
        + (p.limit != null ? t(' · 额度 {n}', { n: p.limit }) : '') + (p.used != null ? t('，本期已用 {n}', { n: p.used }) : ''));
      if (host.view !== 'cn') toast(t('已加入{r}，切换到该区域即可查看', { r: t('国内版') }), 'info');
      host.reload();
      m.close();
    } catch (e) { say(t('添加失败: {msg}', { msg: errText(e) }), 'bad'); }
    finally { busy = false; submit.classList.remove('loading'); }
  }
  return m;
}
