// Theme = palette style (claude | material | miuix) x mode (light | dark | system).
// The inline script in index.html applies the saved values before first paint;
// this module owns changes afterwards. Storage keys match the previous dashboard.

const MODE_KEY = 'wb-theme';
const STYLE_KEY = 'wb-theme-style';
export const STYLES = [
  { id: 'claude', label: 'Claude', swatch: ['#b04a28', '#f1ede3'] },
  { id: 'material', label: 'Material', swatch: ['#6750a4', '#f6f1fa'] },
  { id: 'miuix', label: 'MiuiX', swatch: ['#f0560a', '#f3f5f9'] },
];
const mq = window.matchMedia ? window.matchMedia('(prefers-color-scheme: dark)') : null;
const listeners = new Set();

function read(key, ok, dflt) {
  try { const v = localStorage.getItem(key); return ok.includes(v) ? v : dflt; } catch { return dflt; }
}

export const getMode = () => read(MODE_KEY, ['light', 'dark', 'system'], 'system');
export const getStyle = () => read(STYLE_KEY, STYLES.map((s) => s.id), 'claude');
export const resolved = () => { const m = getMode(); return m === 'system' ? (mq && mq.matches ? 'dark' : 'light') : m; };

export function apply() {
  const root = document.documentElement;
  const mode = resolved();
  root.setAttribute('data-theme', mode);
  root.setAttribute('data-theme-style', getStyle());
  const meta = document.querySelector('meta[name="theme-color"]');
  if (meta) meta.setAttribute('content', getComputedStyle(root).getPropertyValue('--bg').trim() || '#fff');
  for (const fn of listeners) fn(mode);
}

export function setMode(mode) { try { localStorage.setItem(MODE_KEY, mode); } catch { /* blocked */ } apply(); }
export function setStyle(style) { try { localStorage.setItem(STYLE_KEY, style); } catch { /* blocked */ } apply(); }
export function onTheme(fn) { listeners.add(fn); return () => listeners.delete(fn); }

export function init() {
  if (mq) mq.addEventListener('change', () => { if (getMode() === 'system') apply(); });
  apply();
}
