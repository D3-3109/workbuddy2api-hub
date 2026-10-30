// Shared UI building blocks: buttons, toasts, dialogs, menus, tooltips, cards.
import { h, mount, setText } from './dom.js';
import { icon } from './icons.js';
import { t } from './i18n.js';

/* ---------------- toast ---------------- */
let toastBox = null;
const TOAST_ICON = { ok: 'checkCircle', bad: 'xCircle', warn: 'alert', info: 'info' };

export function toast(message, kind = 'info', ms = 4200) {
  if (!toastBox) { toastBox = h('div.toasts', { role: 'status', 'aria-live': 'polite' }); document.body.appendChild(toastBox); }
  const el = h('div.toast.' + kind, icon(TOAST_ICON[kind] || 'info', 18), h('div', String(message)));
  while (toastBox.children.length >= 4) toastBox.firstChild.remove();
  toastBox.appendChild(el);
  const close = () => { el.classList.add('out'); setTimeout(() => el.remove(), 170); };
  const timer = setTimeout(close, ms);
  el.addEventListener('click', () => { clearTimeout(timer); close(); });
  return close;
}
export const ok = (m) => toast(m, 'ok');
export const fail = (m) => toast(m, 'bad', 6500);

/** Human message for a caught error (ApiError or anything else). */
export function errText(e) {
  if (!e) return t('未知错误');
  if (e.status === 0) return e.message === 'timeout' ? t('请求超时') : t('无法连接到网关');
  if (e.status === 501) return t('当前 Rust 版本暂不支持此功能');
  if (e.status === 401 || e.status === 403) return t('登录已失效');
  return e.message || String(e);
}

/* ---------------- buttons ---------------- */
/**
 * btn('保存', { variant:'primary', icon:'check', size:'sm', onClick })
 * onClick may be async: the button shows a spinner and ignores clicks until it settles;
 * a thrown error becomes a toast.
 */
export function btn(label, o = {}) {
  const cls = ['btn'];
  if (o.variant) cls.push(o.variant);
  if (o.size) cls.push(o.size);
  if (o.iconOnly) cls.push('icon');
  if (o.block) cls.push('block');
  const el = h('button', { type: o.type || 'button', class: cls.join(' ') });
  if (o.icon) el.appendChild(icon(o.icon, o.size === 'xs' ? 13 : o.size === 'sm' ? 15 : 16));
  if (label && !o.iconOnly) el.appendChild(h('span', label));
  if (o.iconOnly && label) { el.setAttribute('aria-label', label); el.setAttribute('data-tip', label); }
  if (o.tip) el.setAttribute('data-tip', o.tip);
  if (o.disabled) el.disabled = true;
  if (o.onClick) {
    el.addEventListener('click', async (ev) => {
      if (el.classList.contains('loading')) return;
      let r;
      try { r = o.onClick(ev, el); } catch (e) { fail(errText(e)); return; }
      if (r && typeof r.then === 'function') {
        el.classList.add('loading');
        try { await r; } catch (e) { if (!(e && e.name === 'AbortError')) fail(errText(e)); }
        el.classList.remove('loading');
      }
    });
  }
  return el;
}

export function iconBtn(name, label, onClick, o = {}) {
  return btn(label, { iconOnly: true, icon: name, variant: 'ghost', size: 'sm', onClick, ...o });
}

/** Mark a control as not implemented in the Rust build. */
export function unsupported(el, why) {
  el.disabled = true;
  el.setAttribute('data-unsupported', '');
  el.setAttribute('data-tip', why || t('Rust 版暂不支持'));
  return el;
}

/* ---------------- small elements ---------------- */
export function badge(text, kind = '', o = {}) {
  const cls = 'badge' + (kind ? ' ' + kind : '') + (o.dot ? ' dot' : '') + (o.sm ? ' sm' : '') + (o.outline ? ' outline' : '');
  const el = h('span', { class: cls }, text);
  if (o.tip) el.setAttribute('data-tip', o.tip);
  return el;
}

export function switchEl({ checked = false, disabled = false, onChange, size = '', label } = {}) {
  const input = h('input', { type: 'checkbox', role: 'switch', checked, disabled });
  if (label) input.setAttribute('aria-label', label);
  const el = h('label.switch' + (size ? '.' + size : ''), input, h('i'));
  if (onChange) input.addEventListener('change', () => onChange(input.checked, input));
  el.input = input;
  return el;
}

export function checkbox({ checked = false, disabled = false, onChange, label } = {}) {
  const input = h('input', { type: 'checkbox', checked, disabled });
  if (onChange) input.addEventListener('change', () => onChange(input.checked, input));
  const el = h('label.check', input, label ? h('span', label) : null);
  el.input = input;
  return el;
}

/** Segmented control. Returns the element; el.set(value) updates the highlight. */
export function seg(options, value, onChange, { size = '' } = {}) {
  const el = h('div.seg' + (size ? '.' + size : ''), { role: 'group' });
  const buttons = new Map();
  for (const o of options) {
    const b = h('button', { type: 'button', 'aria-pressed': String(o.value === value) }, o.icon ? icon(o.icon, 14) : null, o.label);
    if (o.tip) b.setAttribute('data-tip', o.tip);
    b.addEventListener('click', () => { if (el.value !== o.value) { el.set(o.value); onChange(o.value); } });
    buttons.set(o.value, b);
    el.appendChild(b);
  }
  el.value = value;
  el.set = (v) => { el.value = v; for (const [k, b] of buttons) b.setAttribute('aria-pressed', String(k === v)); };
  return el;
}

export function field(label, control, hint, extra) {
  return h('div.field', label ? h('label', label) : null, control, hint ? h('div.hint', hint) : null, extra || null);
}

export function empty(iconName, title, text, action) {
  return h('div.empty', icon(iconName, 22, 'ic'), h('h3', title), text ? h('p', text) : null, action || null);
}

export function skeleton(lines = 3, widths = ['100%', '82%', '64%']) {
  return h('div.col', { style: 'gap:10px;padding:4px 0' }, Array.from({ length: lines }, (_, i) => h('span.skel', { style: { height: '14px', width: widths[i % widths.length] } })));
}

export function spinner() { return h('div.spinner', { role: 'progressbar' }); }

export function alertBox(kind, ...kids) {
  return h('div.alert.' + kind, icon(kind === 'bad' ? 'xCircle' : kind === 'warn' ? 'alert' : kind === 'good' ? 'checkCircle' : 'info', 18), h('div.grow', ...kids));
}

export function pageHead(title, desc, ...actions) {
  return h('div.page-head', h('div', h('h1', title), desc ? h('p', desc) : null), actions.length ? h('div.page-actions', ...actions) : null);
}

export function card(title, { sub, actions, flush = false, cls = '' } = {}, ...body) {
  const head = title || actions
    ? h('div.card-head', title ? h('h2', title) : null, sub ? h('span.sub', sub) : null, actions ? h('div.actions', ...[].concat(actions)) : null)
    : null;
  return h('section.card' + (cls ? '.' + cls.split(' ').join('.') : ''), head, h('div.card-body' + (flush ? '.flush' : ''), ...body));
}

/**
 * KPI tile with in-place updates:
 *   const k = kpi({ label, icon }); k.set({ value: '1.2M', unit: 'tok', foot: node|string })
 */
export function kpi({ label, icon: ic, cls = '' }) {
  const value = h('div.kpi-value');
  const foot = h('div.kpi-foot');
  const spark = h('div.spark');
  const el = h('div.card.kpi' + (cls ? '.' + cls : ''), h('div.kpi-label', ic ? icon(ic, 15, 'ic') : null, label), value, foot, spark);
  return {
    el, spark,
    set({ value: v, unit, foot: f } = {}) {
      if (v !== undefined) {
        const key = v + '|' + (unit || '');
        if (value.__k !== key) { value.__k = key; value.replaceChildren(String(v), unit ? h('small', unit) : ''); }
      }
      if (f !== undefined) {
        if (typeof f === 'string') { if (foot.__k !== f) { foot.__k = f; foot.textContent = f; } }
        else { foot.__k = null; foot.replaceChildren(f); }
      }
    },
  };
}

/* ---------------- clipboard ---------------- */
export async function copyText(text, msg) {
  const legacy = () => {
    const ta = h('textarea', { style: 'position:fixed;opacity:0;top:0;left:0', value: text });
    document.body.appendChild(ta);
    ta.select();
    try { return document.execCommand('copy'); } finally { ta.remove(); }
  };
  let done = false;
  try {
    if (navigator.clipboard && window.isSecureContext) { await navigator.clipboard.writeText(text); done = true; }
  } catch { /* permission denied or unfocused document: fall back below */ }
  if (!done) { try { done = legacy(); } catch { done = false; } }
  if (done) ok(msg || t('已复制')); else fail(t('复制失败，请手动复制'));
  return done;
}

export function codeBlock(text, { copy = true } = {}) {
  const pre = h('pre', h('code', text));
  const el = h('div.codeblock', pre);
  if (copy) el.appendChild(h('div.copy', btn(t('复制'), { size: 'xs', icon: 'copy', variant: 'ghost', onClick: () => copyText(pre.textContent) })));
  el.setText = (v) => { pre.firstChild.textContent = v; };
  return el;
}

/** A value with a copy affordance: click to copy. */
export function copyable(text, { cls = 'mono', max } = {}) {
  const el = h('button.btn.ghost.xs', { type: 'button', style: { fontFamily: 'var(--mono)', maxWidth: max || '100%', padding: '0 6px' }, 'data-tip': t('点击复制') }, h('span.trunc', text), icon('copy', 12));
  el.addEventListener('click', () => copyText(text));
  return el;
}

/* ---------------- modal ---------------- */
let openModals = [];

/**
 * modal({ title, body: Node|Node[], actions: [Node], size: 'narrow'|'wide', persistent })
 * Returns { el, close(result) , closed: Promise }
 */
export function modal({ title, body, actions, size = '', persistent = false, onClose, left } = {}) {
  const previous = document.activeElement;
  const closeBtn = h('button.btn.ghost.icon.sm.x', { type: 'button', 'aria-label': t('关闭') }, icon('x', 16));
  const box = h('div.modal' + (size ? '.' + size : ''), { role: 'dialog', 'aria-modal': 'true' },
    h('div.modal-head', h('h2', title || ''), closeBtn),
    h('div.modal-body', ...[].concat(body || [])),
    actions ? h('div.modal-foot', left ? h('div.left', left) : null, ...[].concat(actions)) : null);
  const scrim = h('div.scrim', box);
  let done;
  const closed = new Promise((r) => { done = r; });
  const api = {
    el: box, closed,
    close(result) {
      if (!scrim.isConnected) return;
      scrim.remove();
      openModals = openModals.filter((m) => m !== api);
      document.removeEventListener('keydown', onKey, true);
      if (previous && previous.focus && previous.isConnected) previous.focus({ preventScroll: true });
      if (onClose) onClose(result);
      done(result);
    },
  };
  function onKey(e) {
    if (openModals[openModals.length - 1] !== api) return;
    if (e.key === 'Escape' && !persistent) { e.stopPropagation(); api.close(); }
    else if (e.key === 'Tab') {
      const f = box.querySelectorAll('button:not(:disabled), [href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])');
      if (!f.length) return;
      const first = f[0], last = f[f.length - 1];
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
    }
  }
  closeBtn.addEventListener('click', () => api.close());
  let downOnScrim = false;
  scrim.addEventListener('pointerdown', (e) => { downOnScrim = e.target === scrim; });
  scrim.addEventListener('click', (e) => { if (!persistent && downOnScrim && e.target === scrim) api.close(); });
  document.addEventListener('keydown', onKey, true);
  document.body.appendChild(scrim);
  openModals.push(api);
  const first = box.querySelector('input:not([type=hidden]):not(:disabled), textarea, select');
  (first || box.querySelector('.modal-foot .btn.primary') || closeBtn).focus({ preventScroll: true });
  return api;
}

export function confirmDialog({ title, message, confirmText, cancelText, danger = false, extra } = {}) {
  return new Promise((resolve) => {
    let m;
    const okBtn = btn(confirmText || t('确定'), { variant: danger ? 'danger solid' : 'primary', onClick: () => m.close(true) });
    const noBtn = btn(cancelText || t('取消'), { onClick: () => m.close(false) });
    m = modal({
      title: title || t('请确认'), size: 'narrow',
      body: [h('p', { style: 'color:var(--fg-2);white-space:pre-line' }, message), extra || null],
      actions: [noBtn, okBtn],
      onClose: (r) => resolve(!!r),
    });
    okBtn.focus();
  });
}

export function promptDialog({ title, label, value = '', placeholder = '', confirmText, type = 'text', hint, validate } = {}) {
  return new Promise((resolve) => {
    const input = h('input.input', { type, value, placeholder, autocomplete: 'off', spellcheck: 'false' });
    const err = h('div.err');
    let m;
    const submit = () => {
      const v = input.value.trim();
      const msg = validate ? validate(v) : '';
      if (msg) { setText(err, msg); return; }
      m.close(v);
    };
    input.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); submit(); } });
    m = modal({
      title, size: 'narrow',
      body: [field(label, input, hint, err)],
      actions: [btn(t('取消'), { onClick: () => m.close(null) }), btn(confirmText || t('确定'), { variant: 'primary', onClick: submit })],
      onClose: (r) => resolve(r ?? null),
    });
  });
}

/* ---------------- menu ---------------- */
let openMenu = null;

export function closeMenu() { if (openMenu) openMenu.close(); }

/**
 * menu(anchorEl, [{ label, icon, onClick, danger, disabled, checked } | { sep:true } | { header:'text' }], { align:'start'|'end' })
 */
export function menu(anchor, items, { align = 'start', minWidth } = {}) {
  closeMenu();
  const el = h('div.menu', { role: 'menu' });
  if (minWidth) el.style.minWidth = minWidth + 'px';
  for (const it of items) {
    if (!it) continue;
    if (it.sep) { el.appendChild(h('div.menu-sep')); continue; }
    if (it.header) { el.appendChild(h('div.menu-label', it.header)); continue; }
    const b = h('button.menu-item' + (it.danger ? '.danger' : ''), { type: 'button', role: it.checked != null ? 'menuitemradio' : 'menuitem', disabled: !!it.disabled },
      it.icon ? icon(it.icon, 16) : null, h('span.grow.trunc', it.label), it.hint ? h('span.kbd', it.hint) : null);
    if (it.checked != null) b.setAttribute('aria-checked', String(!!it.checked));
    if (it.tip) b.setAttribute('data-tip', it.tip);
    b.addEventListener('click', async () => {
      close();
      try { await it.onClick?.(); } catch (e) { fail(errText(e)); }
    });
    el.appendChild(b);
  }
  document.body.appendChild(el);
  const r = anchor.getBoundingClientRect();
  const mw = el.offsetWidth, mh = el.offsetHeight;
  let left = align === 'end' ? r.right - mw : r.left;
  left = Math.max(8, Math.min(left, innerWidth - mw - 8));
  let top = r.bottom + 6;
  if (top + mh > innerHeight - 8 && r.top - mh - 6 > 8) { top = r.top - mh - 6; el.style.transformOrigin = 'bottom left'; }
  el.style.left = left + 'px';
  el.style.top = Math.max(8, top) + 'px';
  const onDown = (e) => { if (!el.contains(e.target) && !anchor.contains(e.target)) close(); };
  const onKey = (e) => {
    if (e.key === 'Escape') { close(); anchor.focus?.(); }
    else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const list = [...el.querySelectorAll('.menu-item:not(:disabled)')];
      const i = list.indexOf(document.activeElement);
      list[(i + (e.key === 'ArrowDown' ? 1 : -1) + list.length) % list.length]?.focus();
    }
  };
  const onMove = () => close();
  document.addEventListener('pointerdown', onDown, true);
  document.addEventListener('keydown', onKey, true);
  window.addEventListener('resize', onMove);
  document.addEventListener('scroll', onMove, true);
  function close() {
    el.remove();
    document.removeEventListener('pointerdown', onDown, true);
    document.removeEventListener('keydown', onKey, true);
    window.removeEventListener('resize', onMove);
    document.removeEventListener('scroll', onMove, true);
    if (openMenu === api) openMenu = null;
  }
  const api = { close, el };
  openMenu = api;
  return api;
}

/** Button that opens a menu below it; `items` may be a function evaluated on open. */
export function menuButton(label, items, o = {}) {
  const b = btn(label, { ...o, onClick: (e, el) => { menu(el, typeof items === 'function' ? items() : items, { align: o.align || 'start' }); } });
  return b;
}

/* ---------------- tooltip (one delegated listener for the whole page) ---------------- */
let tipEl = null, tipTimer = 0, tipTarget = null;
function hideTip() { clearTimeout(tipTimer); tipTarget = null; if (tipEl) { tipEl.remove(); tipEl = null; } }
export function initTooltips() {
  document.addEventListener('pointerover', (e) => {
    if (e.pointerType === 'touch') return;
    const el = e.target.closest ? e.target.closest('[data-tip]') : null;
    if (el === tipTarget) return;
    hideTip();
    if (!el) return;
    tipTarget = el;
    tipTimer = setTimeout(() => {
      const text = el.getAttribute('data-tip');
      if (!text || !el.isConnected) return;
      tipEl = h('div.tip', text);
      document.body.appendChild(tipEl);
      const r = el.getBoundingClientRect();
      const w = tipEl.offsetWidth, hh = tipEl.offsetHeight;
      let left = Math.max(8, Math.min(r.left + r.width / 2 - w / 2, innerWidth - w - 8));
      let top = r.top - hh - 8;
      if (top < 8) top = r.bottom + 8;
      tipEl.style.left = left + 'px';
      tipEl.style.top = top + 'px';
    }, 260);
  }, { passive: true });
  document.addEventListener('pointerdown', hideTip, { passive: true, capture: true });
  document.addEventListener('scroll', hideTip, { passive: true, capture: true });
  document.addEventListener('keydown', (e) => { if (e.key === 'Escape') hideTip(); });
}

/* ---------------- sorting helper ---------------- */
/** Click-to-sort header state: returns { key, dir, toggle(key), apply(rows, getters) } */
export function sorter(initialKey, initialDir = 'desc', onChange) {
  const s = {
    key: initialKey, dir: initialDir,
    toggle(key) {
      if (s.key === key) s.dir = s.dir === 'asc' ? 'desc' : 'asc'; else { s.key = key; s.dir = 'desc'; }
      onChange?.();
    },
    apply(rows, getters) {
      const g = getters[s.key];
      if (!g) return rows;
      const dir = s.dir === 'asc' ? 1 : -1;
      return rows.slice().sort((a, b) => {
        const x = g(a), y = g(b);
        if (x == null && y == null) return 0;
        if (x == null) return 1;
        if (y == null) return -1;
        if (typeof x === 'string' || typeof y === 'string') return String(x).localeCompare(String(y), undefined, { numeric: true }) * dir;
        return (x - y) * dir;
      });
    },
  };
  return s;
}

export function sortHeader(label, key, state, o = {}) {
  const arrow = h('span.sort');
  const th = h('th.sortable' + (o.num ? '.num' : ''), { onClick: () => state.toggle(key) }, label, arrow);
  th.refresh = () => { setText(arrow, state.key === key ? (state.dir === 'asc' ? '↑' : '↓') : ''); th.setAttribute('aria-sort', state.key === key ? (state.dir === 'asc' ? 'ascending' : 'descending') : 'none'); };
  th.refresh();
  return th;
}

export { mount };
