// Settings page toolkit: the settings store (every save returns the full view
// and is re-applied to all bound controls), row builders and validators.
import { h, setText } from '../lib/dom.js';
import { ApiError, post, session, authEvents } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import { badge, switchEl, seg, unsupported, card, modal, ok, fail, errText } from '../lib/ui.js';

add({
  '已保存': 'Saved', '当前 Rust 版网关未实现此功能': 'Not available in the Rust gateway', '未实现': 'Not available', '仅存储': 'Stored only',
  '已保存，但当前版本的网关暂未使用该设置': 'Saved, but this gateway build does not use it yet',
  '不能为空': 'Required', '请输入整数': 'Enter a whole number', '请输入数字': 'Enter a number',
  '请输入 {min} 到 {max} 之间的值': 'Enter a value between {min} and {max}', '不能小于 {min}': 'Must be at least {min}',
  '设置值无效，未保存任何内容': 'Invalid value; nothing was saved', '无法写入设置文件': 'Could not write the settings file',
  '当前密码错误': 'Current password is wrong', '面板密码错误': 'Wrong panel password', '新密码至少需要 4 个字符': 'The new password needs at least 4 characters',
  '网络监听模式下不能使用默认密码 admin': 'The default password "admin" is not allowed on a network listener',
  '代理地址无效': 'Invalid proxy URL', '密钥不存在或已删除': 'The key does not exist or was deleted',
});

/* ---------------- paths and patches ---------------- */
export function getPath(obj, path) {
  let cur = obj;
  for (const k of path.split('.')) { if (cur == null) return undefined; cur = cur[k]; }
  return cur;
}

/** patchOf('prompt.mode', 'append') -> { prompt: { mode: 'append' } } */
export function patchOf(path, value) {
  const keys = path.split('.');
  const out = {};
  let cur = out;
  keys.forEach((k, i) => { if (i === keys.length - 1) cur[k] = value; else cur = cur[k] = {}; });
  return out;
}

export const byteLen = (s) => new TextEncoder().encode(s).length;

/* ---------------- errors ---------------- */
const KNOWN = {
  'invalid settings value; no settings were saved': () => t('设置值无效，未保存任何内容'),
  'could not persist settings': () => t('无法写入设置文件'),
  'current password is wrong': () => t('当前密码错误'),
  'current panel password is required for identity_versions': () => t('面板密码错误'),
  'new password must contain at least 4 characters': () => t('新密码至少需要 4 个字符'),
  'a network listener requires a non-default panel password': () => t('网络监听模式下不能使用默认密码 admin'),
  'invalid proxy slot URL': () => t('代理地址无效'),
  'no such key': () => t('密钥不存在或已删除'),
};

/** errText() plus translations of the few fixed server messages. */
export function errMsg(e) {
  const f = e && KNOWN[e.message];
  return f ? f() : errText(e);
}

/**
 * POST that never raises the app-wide "session lost" event for the two 401s the
 * server uses for a wrong password (panel/password, identity_versions). A 401
 * that really means "no session" still logs out.
 */
export async function postKeep(path, body) {
  const headers = { 'Content-Type': 'application/json', Accept: 'application/json' };
  if (session.token) headers['X-Panel-Token'] = session.token;
  let res;
  try {
    res = await fetch(path, { method: 'POST', headers, body: JSON.stringify(body), cache: 'no-store', signal: AbortSignal.timeout(60000) });
  } catch (e) {
    throw new ApiError(0, e && e.name === 'TimeoutError' ? 'timeout' : 'network');
  }
  const text = await res.text();
  let data = null;
  try { data = text ? JSON.parse(text) : {}; } catch { data = null; }
  if (!res.ok) {
    const msg = (data && data.error && data.error.message) || (data && data.message) || 'HTTP ' + res.status;
    const err = new ApiError(res.status, msg, data);
    if (res.status === 401 && /^panel (session|password) required/i.test(msg)) authEvents.dispatchEvent(new Event('unauthorized'));
    throw err;
  }
  return data || {};
}

/* ---------------- store ---------------- */
/**
 * One store per page mount. `save()` calls are serialised so replies cannot be
 * applied out of order; each reply (the full settings view) replaces the state
 * and re-syncs every bound control.
 */
export function createStore(initial, { onApply } = {}) {
  let cur = initial;
  let chain = Promise.resolve();
  const binds = [];
  const S = {
    get view() { return cur; },
    val: (key) => getPath(cur, key),
    unsupported: (key) => (cur.unsupported_settings || []).includes(key),
    cap: (name) => !!(cur.management_capabilities && cur.management_capabilities[name]),
    bind(fn) { binds.push(fn); fn(cur); return fn; },
    apply(next) {
      cur = next;
      if (onApply) onApply(next);
      for (const fn of binds) { try { fn(cur); } catch (e) { console.error(e); } }
    },
    /** POST /settings/save with a partial patch. Resolves with the new view. */
    save(patch, { quiet = false, keepSession = false } = {}) {
      const run = async () => {
        const r = keepSession ? await postKeep('/settings/save', patch) : await post('/settings/save', patch);
        S.apply(r);
        if (!quiet) ok(t('已保存'));
        return r;
      };
      const p = chain.then(run, run);
      chain = p.catch(() => {});
      return p;
    },
  };
  return S;
}

/* ---------------- validation ---------------- */
/** Parse a numeric text field. Returns { value } or { error }. */
export function parseNum(raw, { min = 0, max = Infinity, int = true, blank = 'reject' } = {}) {
  const s = String(raw).trim();
  if (s === '') return blank === 'null' ? { value: null } : { error: t('不能为空') };
  if (!/^-?\d+(\.\d+)?$/.test(s)) return { error: int ? t('请输入整数') : t('请输入数字') };
  const n = Number(s);
  if (int && !Number.isInteger(n)) return { error: t('请输入整数') };
  if (n < min || n > max) return { error: Number.isFinite(max) ? t('请输入 {min} 到 {max} 之间的值', { min, max }) : t('不能小于 {min}', { min }) };
  return { value: n };
}

/* ---------------- rows ---------------- */
export function unsupportedBadge() { return badge(t('未实现'), '', { sm: true, outline: true, tip: t('当前 Rust 版网关未实现此功能') }); }
export function storedBadge() { return badge(t('仅存储'), 'info', { sm: true, tip: t('已保存，但当前版本的网关暂未使用该设置') }); }

/** A `.setting` row: label + help on the left, control(s) on the right. */
export function row({ title, help, ctl, badges, cls = '' }) {
  const titleEl = h('div.s-title', h('span', title), ...(badges || []).filter(Boolean));
  const helpEl = help != null ? h('div.s-help', help) : null;
  const errEl = h('div.s-err', { role: 'alert', hidden: true });
  const ctlEl = h('div.s-ctl', ...[].concat(ctl || []));
  const el = h('div.setting' + (cls ? '.' + cls.split(' ').join('.') : ''), h('div.s-text', titleEl, helpEl, errEl), ctlEl);
  return {
    el, titleEl, helpEl, ctlEl,
    error(msg) { errEl.hidden = !msg; setText(errEl, msg || ''); },
    setHelp(text) { if (helpEl) setText(helpEl, text); },
  };
}

const whyOff = () => t('当前 Rust 版网关未实现此功能');

/** Boolean setting that saves immediately. Unsupported keys render disabled and off. */
export function switchRow(S, { key, title, help, stored = false }) {
  const off = S.unsupported(key);
  const sw = switchEl({ checked: !!S.val(key), label: title });
  const r = row({ title, help, ctl: sw, badges: [off ? unsupportedBadge() : null, stored ? storedBadge() : null], cls: off ? 'is-off' : '' });
  if (off) { unsupported(sw.input, whyOff()); sw.setAttribute('data-tip', whyOff()); }
  sw.input.addEventListener('change', async () => {
    const on = sw.input.checked;
    sw.input.disabled = true;
    try { await S.save(patchOf(key, on)); }
    catch (e) { sw.input.checked = !on; fail(errMsg(e)); }
    finally { if (!off) sw.input.disabled = false; }
  });
  S.bind((v) => { sw.input.checked = off ? false : !!getPath(v, key); });
  r.sw = sw;
  return r;
}

/**
 * Bare number input wired to a setting: validated client-side, saved on
 * blur/Enter, reverted on failure. `onError(msg)` is called with '' to clear.
 */
export function numField(S, o) {
  const { key, label, min = 0, max = Infinity, int = true, blank = 'reject', placeholder = '', patch, show, onError = () => {}, cls = 'narrow' } = o;
  const fmt = show || ((v) => (v == null ? '' : String(v)));
  const input = h('input.input' + (cls ? '.' + cls.split(' ').join('.') : ''), { type: 'text', inputmode: int ? 'numeric' : 'decimal', autocomplete: 'off', spellcheck: 'false', 'aria-label': label, placeholder });
  let dirty = false, busy = false;
  const reset = () => { input.value = fmt(S.val(key)); dirty = false; };
  const invalid = (msg) => { onError(msg); input.setAttribute('aria-invalid', 'true'); };
  async function commit() {
    if (busy || (!dirty && input.value === fmt(S.val(key)))) return;
    const res = parseNum(input.value, { min, max, int, blank });
    if (res.error) { invalid(res.error); reset(); return; }
    const prev = S.val(key);
    dirty = false;
    if (res.value === (prev ?? null)) { input.value = fmt(prev); return; }
    busy = true;
    try { await S.save(patch ? patch(res.value) : patchOf(key, res.value)); }
    catch (e) { reset(); invalid(errMsg(e)); }
    finally { busy = false; }
  }
  input.addEventListener('input', () => { dirty = true; onError(''); input.removeAttribute('aria-invalid'); });
  input.addEventListener('change', commit);
  input.addEventListener('blur', commit);
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') { e.preventDefault(); commit(); }
    else if (e.key === 'Escape') { reset(); onError(''); input.removeAttribute('aria-invalid'); }
  });
  S.bind((v) => { if (!dirty) { const text = fmt(getPath(v, key)); if (input.value !== text) input.value = text; } });
  return input;
}

/** Number setting row (label, help and a numField on the right). */
export function numRow(S, o) {
  const { title, help, unit, stored = false, disabled } = o;
  let r;
  const input = numField(S, { ...o, label: title, onError: (m) => r.error(m) });
  r = row({ title, help, ctl: [input, unit ? h('span.faint.s-unit', unit) : null], badges: [stored ? storedBadge() : null] });
  if (disabled) input.disabled = true;
  r.input = input;
  r.setDisabled = (on) => { input.disabled = !!on; };
  return r;
}

/** Dropdown that saves on change. `options`: [{ value, label }]. */
export function selectRow(S, { key, title, help, options, stored = false, patch, width, keepUnknown }) {
  const select = h('select.select', { 'aria-label': title });
  if (width) select.style.width = width;
  const r = row({ title, help, ctl: select, badges: [stored ? storedBadge() : null] });
  r.keepUnknown = keepUnknown;
  r.setOptions = (list) => {
    select.replaceChildren(...list.map((o) => h('option', { value: o.value }, o.label)));
    sync();
  };
  function sync() {
    const v = String(S.val(key) ?? '');
    if (r.keepUnknown && v && ![...select.options].some((o) => o.value === v)) {
      select.appendChild(h('option', { value: v }, r.keepUnknown(v)));
    }
    select.value = v;
  }
  select.addEventListener('change', async () => {
    const value = select.value;
    select.disabled = true;
    try { await S.save(patch ? patch(value) : patchOf(key, value)); }
    catch (e) { sync(); fail(errMsg(e)); }
    finally { select.disabled = false; }
  });
  r.setOptions(options || []);
  S.bind(sync);
  r.select = select;
  return r;
}

/** Few-choice enum rendered as a segmented control. */
export function segRow(S, { key, title, help, options, stored = false }) {
  const s = seg(options, String(S.val(key) ?? options[0].value), async (value) => {
    const prev = String(S.val(key) ?? '');
    try { await S.save(patchOf(key, value)); }
    catch (e) { s.set(prev); fail(errMsg(e)); }
  });
  s.setAttribute('aria-label', title);
  const r = row({ title, help, ctl: s, badges: [stored ? storedBadge() : null] });
  S.bind((v) => s.set(String(getPath(v, key) ?? options[0].value)));
  r.seg = s;
  return r;
}

/** Row with a button (or any node) on the right and no setting of its own. */
export function actionRow({ title, help, ctl, badges, cls }) { return row({ title, help, ctl, badges, cls }); }

export function subhead(text, ...extra) { return h('div.s-subhead', h('span', text), ...extra); }

/** A settings group card; body rows sit flush against the card edges. */
export function section(id, title, { sub, actions } = {}, ...body) {
  const el = card(title, { sub, actions, flush: true, cls: 's-sec' }, ...body);
  el.dataset.sec = id;
  return el;
}

/** Padded free-form block inside a flush card. */
export function block(...kids) { return h('div.s-block', ...kids); }

/** modal() with a class so dialog-only styles can be scoped (dialogs live outside .p-settings). */
export function dialog(opts) {
  const m = modal(opts);
  m.el.classList.add('s-dlg');
  return m;
}
