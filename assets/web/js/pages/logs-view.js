// Terminal renderer for the Logs page. Append-only: a poll that brings N lines creates N
// nodes; nothing is re-rendered. The DOM never holds more than RENDER_CAP lines (the oldest
// are dropped in chunks), so thousands of log lines stay cheap. Lines are built with
// createElement + text nodes only, so log text can never become markup.
import { h, setText } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { t } from '../lib/i18n.js';
import { hashIndex } from '../lib/shared.js';

export const RENDER_CAP = 1000;
export const TRIM_CHUNK = 100;
const NEAR_PX = 40;       // closer than this to the bottom counts as "at the end"
const MAX_MARKS = 24;     // highlighted matches per line

function mk(tag, cls, text) {
  const el = document.createElement(tag);
  if (cls) el.className = cls;
  if (text != null) el.textContent = text;
  return el;
}

/** Append `text` to `parent`, wrapping occurrences of `q` (already lower-case) in <mark>. */
function fillInto(parent, text, q) {
  if (q) {
    const low = text.toLowerCase();
    let i = low.length === text.length ? low.indexOf(q) : -1;
    if (i >= 0) {
      let from = 0, n = 0;
      while (i >= 0 && n++ < MAX_MARKS) {
        if (i > from) parent.append(text.slice(from, i));
        parent.append(mk('mark', '', text.slice(i, i + q.length)));
        from = i + q.length;
        i = low.indexOf(q, from);
      }
      if (from < text.length) parent.append(text.slice(from));
      return;
    }
  }
  parent.append(text);
}

const tagClass = new Map();
function tagCls(tk) {
  let c = tagClass.get(tk);
  if (!c) { c = 'g tg' + hashIndex(tk, 8); tagClass.set(tk, c); }
  return c;
}

function lineEl(e, q) {
  const ln = mk('div', 'ln lv-' + e.level);
  const time = mk('span', 't');
  const msg = mk('span', 'm');
  const tag = mk('span', tagCls(e.tk));
  fillInto(time, e.time, q);
  fillInto(tag, '[' + e.tag + ']', q);
  msg.append(tag, ' ');
  fillInto(msg, e.msg, q);
  ln.append(time, mk('span', 'l', e.level), msg);
  return ln;
}

/**
 * createTerminal({ onState }) -> { el, render, append, setFollow, setWrap, resize, showOverlay, count, follow }
 * onState({ follow, near, unseen, count }) fires when any of those change.
 */
export function createTerminal({ onState, label }) {
  const lines = mk('div', 'lg-lines');
  const body = h('div.lg-body', { tabindex: '0', role: 'log', 'aria-live': 'off', 'aria-label': label }, lines);
  const overlay = mk('div', 'lg-overlay');
  overlay.hidden = true;
  const jumpCount = mk('span', 'lg-jump-n');
  const jump = h('button.lg-jump', { type: 'button', hidden: true }, icon('arrowDown', 14), h('span', t('跳到最新')), jumpCount);
  const el = h('div.lg-wrap', body, overlay, jump);

  let follow = true;   // keep the view pinned to the newest line
  let near = true;     // the view is at (or within NEAR_PX of) the bottom
  let unseen = 0;      // lines that arrived while the view was scrolled away
  let count = 0;       // lines currently in the DOM

  const distance = () => body.scrollHeight - body.scrollTop - body.clientHeight;

  function notify() {
    jump.hidden = near;
    setText(jumpCount, unseen > 0 ? t('{n} 条新日志', { n: unseen > 999 ? '999+' : unseen }) : '');
    onState({ follow, near, unseen, count });
  }

  function stick() {
    body.scrollTop = body.scrollHeight;
    near = true;
    unseen = 0;
  }

  // Scrolling away pauses following; coming back to the bottom resumes it.
  body.addEventListener('scroll', () => {
    const isNear = distance() <= NEAR_PX;
    if (isNear === near) return;
    near = isNear;
    follow = isNear;
    if (near) unseen = 0;
    notify();
  }, { passive: true });

  jump.addEventListener('click', () => api.setFollow(true));

  /** Remove the oldest `n` lines. Keeps what the user is reading in place when not following. */
  function trim(n) {
    n = Math.min(n, count);
    if (n <= 0) return;
    const reading = !follow;
    const top = body.scrollTop;
    const before = reading ? body.scrollHeight : 0;
    for (let i = 0; i < n; i++) lines.firstChild.remove();
    count -= n;
    if (reading) {
      const delta = before - body.scrollHeight;
      if (delta > 0) body.scrollTop = Math.max(0, top - delta);
    }
  }

  const api = {
    el, body, jump,
    get count() { return count; },
    get follow() { return follow; },

    /** Replace everything with `entries` (the caller already limited them to RENDER_CAP). */
    render(entries, q) {
      const frag = document.createDocumentFragment();
      for (const e of entries) frag.appendChild(lineEl(e, q));
      const top = body.scrollTop;
      lines.replaceChildren(frag);
      count = entries.length;
      if (follow) stick();
      else { body.scrollTop = top; near = distance() <= NEAR_PX; if (near) unseen = 0; }
      notify();
    },

    append(entries, q) {
      if (!entries.length) return;
      const list = entries.length > RENDER_CAP ? entries.slice(-RENDER_CAP) : entries;
      const frag = document.createDocumentFragment();
      for (const e of list) frag.appendChild(lineEl(e, q));
      lines.appendChild(frag);
      count += list.length;
      if (count > RENDER_CAP) trim(count - (RENDER_CAP - TRIM_CHUNK));
      if (follow) stick();
      else { near = distance() <= NEAR_PX; unseen = near ? 0 : unseen + entries.length; }
      notify();
    },

    setFollow(on) {
      follow = !!on;
      if (follow) stick();
      notify();
    },

    setWrap(on) {
      el.classList.toggle('nowrap', !on);
      if (follow) stick();
    },

    /** Window resized: stay pinned to the end while following. */
    resize() { if (follow) { stick(); notify(); } },

    /** Centered node over the (empty) body: loading, error or "no lines". */
    showOverlay(node) {
      if (!node) { if (!overlay.hidden) { overlay.hidden = true; overlay.replaceChildren(); } return; }
      if (overlay.firstChild !== node) overlay.replaceChildren(node);
      overlay.hidden = false;
    },
  };
  return api;
}
