// Boot, login gate, shell (sidebar + topbar) and hash router.
import { h, mount, setText, setAttr } from './lib/dom.js';
import { icon, injectSprite } from './lib/icons.js';
import { get, post, session, authEvents, ApiError } from './lib/api.js';
import { t, add, setLang, initialLang, onLang, getLang, languages } from './lib/i18n.js';
import * as theme from './lib/theme.js';
import { ctx, emit, on, loadRealm, setView, loadHealth, loadSettings, realmLabel } from './lib/ctx.js';
import { poller } from './lib/poll.js';
import { btn, menu, toast, fail, errText, confirmDialog, initTooltips, field, alertBox, empty, skeleton, seg, closeMenu } from './lib/ui.js';

add({
  '网关': 'Gateway', '账号': 'Accounts', '数据看板': 'Analytics', '任务与福利': 'Tasks', '智能体': 'Agents', '日志': 'Logs', '设置': 'Settings',
  '管理面板登录': 'Sign in to the admin panel', '面板密码': 'Panel password', '登录': 'Sign in', '正在登录…': 'Signing in…',
  '请输入面板密码': 'Enter the panel password',
  '当前仍在使用默认密码 admin，登录后请尽快在「设置」中修改。': 'The default password "admin" is still in use. Change it in Settings after signing in.',
  '退出登录': 'Sign out', '关闭网关服务': 'Shut down gateway', '外观': 'Appearance', '语言': 'Language', '主题模式': 'Mode', '配色': 'Palette',
  '跟随系统': 'System', '浅色': 'Light', '深色': 'Dark', '国际版': 'Global', '国内版': 'China',
  '在线': 'Online', '离线': 'Offline', '{a}/{b} 个账号可用': '{a}/{b} accounts ready', '无法连接到网关': 'Cannot reach the gateway',
  '未知错误': 'Unknown error', '请求超时': 'Request timed out', '当前 Rust 版本暂不支持此功能': 'Not supported by the Rust build yet', '登录已失效': 'Session expired',
  '页面加载失败': 'Failed to load the page', '重试': 'Retry', '已复制': 'Copied', '复制失败，请手动复制': 'Copy failed, copy it manually', '复制': 'Copy',
  '点击复制': 'Click to copy', '关闭': 'Close', '取消': 'Cancel', '确定': 'OK', '请确认': 'Please confirm', 'Rust 版暂不支持': 'Not supported in the Rust build',
  '刚刚': 'just now', '{n} 分钟前': '{n} min ago', '{n} 小时前': '{n} h ago', '{n} 天前': '{n} d ago',
  '确定关闭网关服务？进程会退出，需要重新启动才能再次使用。': 'Shut down the gateway? The process will exit and must be started again.',
  '网关服务已关闭': 'Gateway stopped', '停止请求已发送，但服务仍在运行：可能由容器或守护进程自动拉起': 'Stop request sent, but the service is still running (a supervisor may have restarted it)',
  '更多': 'More', '收起侧栏': 'Collapse sidebar', '展开侧栏': 'Expand sidebar', '查看区域': 'Viewing region', '版本': 'Version', '密码错误': 'Wrong password',
  '密码不能为空': 'Password required', '登录尝试过多，请稍后再试': 'Too many attempts, try again later',
});

const NAV = [
  { id: 'gateway', icon: 'gateway', label: '网关' },
  { id: 'accounts', icon: 'users', label: '账号' },
  { id: 'analytics', icon: 'chart', label: '数据看板' },
  { id: 'tasks', icon: 'gift', label: '任务与福利' },
  { id: 'agents', icon: 'bot', label: '智能体' },
  { id: 'logs', icon: 'terminal', label: '日志' },
  { id: 'settings', icon: 'sliders', label: '设置' },
];
const IDS = new Set(NAV.map((n) => n.id));

const root = document.getElementById('root');
let shell = null;       // { el, main, title, navLinks, ... }
let current = null;     // { id, cleanup, token }
let routeToken = 0;
const pageCache = new Map();

/* ------------------------------------------------------------------ login */
async function showLogin(reason) {
  if (current) { try { await current.cleanup?.(); } catch { /* ignore */ } current = null; }
  closeMenu();
  shell = null;
  let status = {};
  try { status = await get('/panel/status'); } catch { /* shown below */ }

  const pw = h('input.input', { type: 'password', autocomplete: 'current-password', placeholder: t('面板密码'), style: 'height:40px', autofocus: true });
  const err = h('div.err', { role: 'alert', style: 'min-height:18px;color:var(--bad);font-size:12.5px' });
  const eye = btn('', { iconOnly: true, icon: 'eye', variant: 'ghost', size: 'sm', onClick: () => {
    const show = pw.type === 'password';
    pw.type = show ? 'text' : 'password';
    eye.replaceChildren(icon(show ? 'eyeOff' : 'eye', 15));
  } });
  eye.setAttribute('aria-label', 'toggle');
  eye.style.cssText = 'position:absolute;right:4px;top:6px';
  pw.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.isComposing) { e.preventDefault(); form.requestSubmit(); }
  });
  const submit = btn(t('登录'), { variant: 'primary', block: true, type: 'submit' });
  submit.style.height = '40px';

  const form = h('form', { style: 'display:flex;flex-direction:column;gap:14px', onSubmit: async (e) => {
    e.preventDefault();
    if (!pw.value) { setText(err, t('密码不能为空')); return; }
    submit.classList.add('loading'); setText(err, '');
    try {
      const r = await post('/panel/login', { password: pw.value });
      session.set(r.token);
      ctx.panel = { ...(status || {}), panel_password_is_default: !!r.using_default_password };
      await start();
    } catch (ex) {
      setText(err, ex.status === 401 ? t('密码错误') : ex.status === 429 ? t('登录尝试过多，请稍后再试') : errText(ex));
      pw.select();
    } finally { submit.classList.remove('loading'); }
  } },
  h('div.field', h('label', t('面板密码')), h('div', { style: 'position:relative' }, pw, eye)),
  err, submit);

  mount(root, h('div', { style: 'height:100%;display:grid;place-items:center;padding:20px;overflow:auto;background:radial-gradient(900px 420px at 50% -120px,var(--glow),transparent 70%),var(--bg)' },
    h('div.card', { style: 'width:min(400px,100%);padding:32px 28px;box-shadow:var(--shadow-3);animation:pop-in 220ms var(--ease) both' },
      h('div', { style: 'display:flex;flex-direction:column;align-items:center;gap:12px;margin-bottom:24px;text-align:center' },
        h('div.brand-logo', { style: 'width:48px;height:48px;border-radius:14px' }, icon('zap', 24)),
        h('div', h('h1', { style: 'font-size:20px' }, 'WorkBuddy Hub'), h('p.muted', { style: 'margin-top:2px' }, t('管理面板登录')))),
      reason ? h('div', { style: 'margin-bottom:14px' }, alertBox('warn', reason)) : null,
      status && status.panel_password_is_default ? h('div', { style: 'margin-bottom:14px' }, alertBox('info', t('当前仍在使用默认密码 admin，登录后请尽快在「设置」中修改。'))) : null,
      form),
    h('div', { style: 'position:fixed;top:14px;right:14px;display:flex;gap:6px' }, appearanceButton(), languageButton())));
  pw.focus();
}

authEvents.addEventListener('unauthorized', () => {
  if (!session.token && !current) return;
  session.clear();
  showLogin(t('登录已失效'));
});

/* ----------------------------------------------------------------- topbar */
function appearanceButton() {
  const glyph = () => icon(theme.resolved() === 'dark' ? 'moon' : 'sun', 16);
  const b = btn(t('外观'), { iconOnly: true, icon: theme.resolved() === 'dark' ? 'moon' : 'sun', variant: 'ghost', onClick: (e, el) => {
    const mode = theme.getMode(), style = theme.getStyle();
    const apply = (fn) => () => { fn(); el.replaceChildren(glyph()); };
    menu(el, [
      { header: t('主题模式') },
      { label: t('跟随系统'), icon: 'monitor', checked: mode === 'system', onClick: apply(() => theme.setMode('system')) },
      { label: t('浅色'), icon: 'sun', checked: mode === 'light', onClick: apply(() => theme.setMode('light')) },
      { label: t('深色'), icon: 'moon', checked: mode === 'dark', onClick: apply(() => theme.setMode('dark')) },
      { sep: true },
      { header: t('配色') },
      ...theme.STYLES.map((s) => ({ label: s.label, icon: 'palette', checked: style === s.id, onClick: apply(() => theme.setStyle(s.id)) })),
    ], { align: 'end' });
  } });
  return b;
}

function languageButton() {
  return btn(t('语言'), { iconOnly: true, icon: 'languages', variant: 'ghost', onClick: (e, el) => {
    menu(el, languages.map((l) => ({ label: l.label, checked: getLang() === l.id, onClick: () => setLang(l.id) })), { align: 'end' });
  } });
}

async function shutdown() {
  if (!(await confirmDialog({ title: t('关闭网关服务'), message: t('确定关闭网关服务？进程会退出，需要重新启动才能再次使用。'), danger: true }))) return;
  try { await post('/shutdown', {}); } catch (e) { if (e.unauthorized) return; }
  for (let i = 0; i < 10; i++) {
    await new Promise((r) => setTimeout(r, 600));
    try { const r = await fetch('/health', { cache: 'no-store' }); if (!r.ok) break; } catch { break; }
    if (i === 9) { toast(t('停止请求已发送，但服务仍在运行：可能由容器或守护进程自动拉起'), 'warn'); return; }
  }
  toast(t('网关服务已关闭'), 'warn');
  healthPoll?.stop();
  if (shell) { shell.pill.className = 'badge bad dot'; setText(shell.pillText, t('离线')); }
}

let healthPoll = null;
let listening = false;
function buildShell() {
  const collapsed = (() => { try { return localStorage.getItem('wb-sidebar') === '1'; } catch { return false; } })();
  const app = h('div#app', { dataset: { collapsed: collapsed ? '1' : '0' } });

  const links = new Map();
  const nav = h('nav.nav', { 'aria-label': 'main' });
  for (const n of NAV) {
    const a = h('a.nav-item', { href: '#/' + n.id, 'data-tip': '' }, icon(n.icon, 19), h('span.nav-label', t(n.label)));
    a.__label = n.label;
    // Warm the page's modules when the pointer or keyboard focus reaches the link.
    const warm = () => { loadModule(n.id).catch(() => {}); };
    a.addEventListener('pointerenter', warm, { once: true, passive: true });
    a.addEventListener('focus', warm, { once: true, passive: true });
    links.set(n.id, a);
    nav.appendChild(a);
  }
  const ver = h('div.ver');
  const collapseBtn = h('button.nav-item.hide-sm', { type: 'button' }, icon('sidebar', 19), h('span.nav-label', t('收起侧栏')));
  collapseBtn.addEventListener('click', () => {
    const next = app.dataset.collapsed === '1' ? '0' : '1';
    app.dataset.collapsed = next;
    try { localStorage.setItem('wb-sidebar', next); } catch { /* blocked */ }
    syncCollapse();
  });
  function syncCollapse() {
    const c = app.dataset.collapsed === '1';
    setText(collapseBtn.querySelector('.nav-label'), t(c ? '展开侧栏' : '收起侧栏'));
    for (const a of links.values()) setAttr(a, 'data-tip', c ? t(a.__label) : null);
  }

  const sidebar = h('aside.sidebar',
    h('div.brand', h('div.brand-logo', icon('zap', 19)), h('div.brand-text', h('div.brand-name', 'WorkBuddy Hub'), h('div.brand-sub', 'Rust gateway'))),
    nav,
    h('div.sidebar-foot', ver, collapseBtn));

  const title = h('div.topbar-title');
  const pillText = h('span.hide-xs');
  const pill = h('span.badge.dot', pillText);
  const realmSeg = seg([{ value: 'intl', label: t('国际版') }, { value: 'cn', label: t('国内版') }], ctx.view, (v) => setView(v), { size: 'sm' });
  realmSeg.setAttribute('data-tip', t('查看区域'));

  const more = btn(t('更多'), { iconOnly: true, icon: 'user', variant: 'ghost', onClick: (e, el) => menu(el, [
    { label: t('退出登录'), icon: 'logout', onClick: async () => { try { await post('/panel/logout', {}); } catch { /* ignore */ } session.clear(); await showLogin(); } },
    { sep: true },
    { label: t('关闭网关服务'), icon: 'power', danger: true, onClick: shutdown },
  ], { align: 'end' }) });

  const topbar = h('header.topbar', title, h('div.topbar-actions', pill, realmSeg, appearanceButton(), languageButton(), more));
  const main = h('main.main', { id: 'main', tabindex: '-1' });
  app.append(sidebar, h('div.main-col', topbar, main));

  syncCollapse();
  return { el: app, main, title, links, pill, pillText, ver, syncCollapse, realmSeg };
}

function renderHealth(hl) {
  if (!shell) return;
  if (!hl) { shell.pill.className = 'badge bad dot'; setText(shell.pillText, t('离线')); return; }
  shell.pill.className = 'badge dot ' + (hl.accounts_ready > 0 ? 'good' : 'warn');
  setText(shell.pillText, t('{a}/{b} 个账号可用', { a: hl.accounts_ready ?? 0, b: hl.accounts ?? 0 }));
}

/* ----------------------------------------------------------------- router */
function parseRoute() {
  const legacy = new URLSearchParams(location.search).get('tab');
  let raw = location.hash.replace(/^#\/?/, '');
  if (!raw && legacy) raw = legacy;
  const [path, qs] = raw.split('?');
  const id = IDS.has(path) ? path : 'gateway';
  return { id, query: new URLSearchParams(qs || '') };
}

async function loadModule(id) {
  let p = pageCache.get(id);
  if (!p) { p = import(`/ui/js/pages/${id}.js`); pageCache.set(id, p); p.catch(() => pageCache.delete(id)); }
  return p;
}

/** Nav entries the operator hid in Settings. The Settings page itself is always reachable. */
function hiddenPages() {
  const list = (ctx.settings && ctx.settings.hidden_pages) || [];
  return new Set(list.filter((id) => id !== 'settings'));
}

function applyHiddenPages() {
  if (!shell) return;
  const hidden = hiddenPages();
  for (const [id, a] of shell.links) a.hidden = hidden.has(id);
}

async function navigate() {
  if (!shell) return;
  const route = parseRoute();
  const leaving = current && current.id;
  if (leaving === 'settings' && route.id !== 'settings') { await loadSettings(true).catch(() => {}); applyHiddenPages(); }
  const hidden = hiddenPages();
  if (hidden.has(route.id)) {
    const first = NAV.find((n) => !hidden.has(n.id));
    location.replace('#/' + (first ? first.id : 'settings'));
    return;
  }
  const myToken = ++routeToken;
  ctx.route = route;
  closeMenu();
  const prev = current;
  current = null;
  if (prev) { try { await prev.cleanup?.(); } catch (e) { console.error(e); } }
  if (myToken !== routeToken) return;

  for (const [id, a] of shell.links) { if (id === route.id) a.setAttribute('aria-current', 'page'); else a.removeAttribute('aria-current'); }
  const nav = NAV.find((n) => n.id === route.id);
  mount(shell.title, h('span', t(nav.label)));
  document.title = t(nav.label) + ' · WorkBuddy Hub';

  const page = h('div.page');
  page.appendChild(skeleton(4));
  mount(shell.main, page);
  shell.main.scrollTop = 0;
  const entry = { id: route.id, cleanup: null };
  current = entry;
  try {
    const mod = await loadModule(route.id);
    if (myToken !== routeToken) return;
    shell.realmSeg.hidden = !mod.usesRealm;
    const fresh = h('div.page');
    shell.main.replaceChildren(fresh);
    entry.cleanup = await mod.mount(fresh, ctx);
    if (myToken !== routeToken) { await entry.cleanup?.(); }
  } catch (e) {
    if (myToken !== routeToken) return;
    console.error(e);
    mount(page, empty('alert', t('页面加载失败'), errText(e), btn(t('重试'), { variant: 'primary', onClick: () => navigate() })));
  }
}

/* ------------------------------------------------------------------ start */
async function start() {
  injectSprite();
  try {
    await loadRealm();
  } catch (e) {
    if (e.unauthorized) { session.clear(); return showLogin(); }
  }
  await loadSettings().catch(() => {});
  shell = buildShell();
  mount(root, shell.el);
  applyHiddenPages();
  shell.realmSeg.set(ctx.view);

  healthPoll?.stop();
  healthPoll = poller(async () => {
    try { renderHealth(await loadHealth()); } catch (e) { if (!(e instanceof ApiError) || e.status === 0) renderHealth(null); }
  }, 20000).start();

  get('/updates').then((u) => { if (shell) setText(shell.ver, 'v' + (u.current_version || '')); }).catch(() => {});

  if (!listening) {
    listening = true;
    on('view', (r) => { shell?.realmSeg.set(r); loadHealth().then(renderHealth).catch(() => {}); navigate(); });
    on('realm', () => { loadHealth().then(renderHealth).catch(() => {}); });
  }
  await navigate();

}

onLang(() => {
  if (!shell) { if (!session.token && root.querySelector('form')) showLogin(); return; }
  // Rebuild the chrome and current page in the new language.
  const wasCollapsed = shell.el.dataset.collapsed;
  closeMenu();
  const next = buildShell();
  next.el.dataset.collapsed = wasCollapsed;
  next.syncCollapse();
  shell = next;
  mount(root, shell.el);
  applyHiddenPages();
  shell.realmSeg.set(ctx.view);
  renderHealth(ctx.health);
  get('/updates').then((u) => { if (shell) setText(shell.ver, 'v' + (u.current_version || '')); }).catch(() => {});
  current = null;
  navigate();
});

window.addEventListener('hashchange', navigate);

(async function boot() {
  theme.init();
  initTooltips();
  injectSprite();
  await setLang(initialLang(), { persist: false });
  let status = null;
  try { status = await get('/panel/status'); ctx.panel = status; } catch { /* handled by login */ }
  if (status && status.authenticated && session.token) await start();
  else { session.clear(); await showLogin(); }
})();
