// Tiny DOM toolkit. No virtual DOM: pages build real nodes once and patch only
// what changed, so a poll that returns identical data touches nothing.

const SVG_NS = 'http://www.w3.org/2000/svg';
const TAG_RE = /^([a-z][a-z0-9-]*)?((?:[.#][\w-]+)*)$/i;

function applyProps(el, props) {
  for (const key in props) {
    const v = props[key];
    if (v == null || v === false) continue;
    if (key === 'class' || key === 'className') el.setAttribute('class', Array.isArray(v) ? v.filter(Boolean).join(' ') : v);
    else if (key === 'style') {
      if (typeof v === 'string') el.style.cssText = v;
      else for (const p in v) { if (p.startsWith('--')) el.style.setProperty(p, v[p]); else el.style[p] = v[p]; }
    }
    else if (key === 'dataset') Object.assign(el.dataset, v);
    else if (key === 'ref') v(el);
    else if (key.length > 2 && key.startsWith('on') && typeof v === 'function') el.addEventListener(key.slice(2).toLowerCase(), v);
    else if (key === 'value' || key === 'checked' || key === 'disabled' || key === 'selected' || key === 'indeterminate') el[key] = v;
    else if (v === true) el.setAttribute(key, '');
    else el.setAttribute(key, v);
  }
}

function appendKids(el, kids) {
  for (const k of kids) {
    if (k == null || k === false || k === true) continue;
    if (Array.isArray(k)) appendKids(el, k);
    else if (k instanceof Node) el.appendChild(k);
    else el.appendChild(document.createTextNode(String(k)));
  }
}

/** h('div.card.pad#id', {onClick}, child, ...) */
export function h(tag, props, ...kids) {
  const m = TAG_RE.exec(tag);
  if (!m) throw new Error('bad tag ' + tag);
  const el = document.createElement(m[1] || 'div');
  if (m[2]) {
    for (const part of m[2].match(/[.#][\w-]+/g)) {
      if (part[0] === '#') el.id = part.slice(1); else el.classList.add(part.slice(1));
    }
  }
  if (props && (typeof props !== 'object' || props instanceof Node || Array.isArray(props))) { kids.unshift(props); props = null; }
  if (props) applyProps(el, props);
  appendKids(el, kids);
  return el;
}

export function s(tag, props, ...kids) {
  const el = document.createElementNS(SVG_NS, tag);
  if (props && (typeof props !== 'object' || props instanceof Node || Array.isArray(props))) { kids.unshift(props); props = null; }
  if (props) for (const k in props) {
    const v = props[k];
    if (v == null || v === false) continue;
    if (k === 'class') el.setAttribute('class', v);
    else if (k === 'style' && typeof v === 'object') for (const p in v) el.style.setProperty(p, v[p]);
    else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2).toLowerCase(), v);
    else el.setAttribute(k, v);
  }
  appendKids(el, kids);
  return el;
}

export const $ = (sel, root = document) => root.querySelector(sel);
export const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

export function mount(parent, ...nodes) { parent.replaceChildren(...nodes.flat().filter(Boolean)); return parent; }

/** Set text only when it differs; avoids needless style/layout invalidation. */
export function setText(el, value) {
  const v = value == null ? '' : String(value);
  if (el.textContent !== v) el.textContent = v;
}

export function setAttr(el, name, value) {
  if (value == null || value === false) { if (el.hasAttribute(name)) el.removeAttribute(name); }
  else { const v = value === true ? '' : String(value); if (el.getAttribute(name) !== v) el.setAttribute(name, v); }
}

export function toggleClass(el, name, on) { if (el.classList.contains(name) !== !!on) el.classList.toggle(name, !!on); }

/**
 * Keyed list reconcile. `create(item)` builds a node once per key; `update(node, item)`
 * runs on every pass but the callers keep it cheap by comparing against node.__sig.
 * Nodes are moved only when their position actually changed.
 */
export function reconcile(parent, items, getKey, create, update) {
  const cache = parent.__rc || (parent.__rc = new Map());
  const seen = new Set();
  let ref = parent.firstChild;
  for (let i = 0; i < items.length; i++) {
    const item = items[i];
    const key = getKey(item, i);
    seen.add(key);
    let el = cache.get(key);
    if (!el) { el = create(item, i); cache.set(key, el); }
    else if (update) update(el, item, i);
    if (el === ref) ref = ref.nextSibling;
    else parent.insertBefore(el, ref);
  }
  if (cache.size !== seen.size) {
    for (const [key, el] of cache) if (!seen.has(key)) { el.remove(); cache.delete(key); }
  }
}

/** Signature gate: run `fn` only if `sig` differs from the last one stored on `el`. */
export function once(el, sig, fn) {
  if (el.__sig === sig) return false;
  el.__sig = sig;
  fn();
  return true;
}

export function debounce(fn, ms) {
  let t = 0;
  const d = (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); };
  d.cancel = () => clearTimeout(t);
  return d;
}

export function rafThrottle(fn) {
  let queued = false, last;
  return (...a) => { last = a; if (queued) return; queued = true; requestAnimationFrame(() => { queued = false; fn(...last); }); };
}

/** Collects cleanups so a page can tear everything down in one call. */
export function scope() {
  const fns = [];
  return {
    add(fn) { fns.push(fn); return fn; },
    on(target, type, handler, opts) { target.addEventListener(type, handler, opts); fns.push(() => target.removeEventListener(type, handler, opts)); },
    dispose() { while (fns.length) { try { fns.pop()(); } catch (e) { console.error(e); } } },
  };
}

export function uid(prefix = 'id') { return prefix + Math.random().toString(36).slice(2, 9); }
