// Chinese-keyed i18n. Source strings are the Simplified Chinese text itself:
//   t('保存')                       -> '保存' | 'Save' | '儲存'
//   t('共 {n} 个账号', { n: 3 })     -> interpolation
// English lives in per-page dictionaries registered with add(); Traditional
// Chinese is derived from the source string by a lazily loaded converter.
// Nothing scans or rewrites the DOM: a language change simply re-renders.

const LANG_KEY = 'wb-lang';
const LANGS = ['zh', 'zh-Hant', 'en'];
const en = Object.create(null);
let lang = 'zh';
let toHant = null;
const hantCache = new Map();
const listeners = new Set();

export function add(dict) { Object.assign(en, dict); }

function interp(str, vars) {
  if (!vars) return str;
  return str.replace(/\{(\w+)\}/g, (m, k) => (k in vars ? String(vars[k]) : m));
}

// A source string may carry a disambiguating context after "@@" (for example '账号@@col'):
// the context picks a separate English entry but is never shown in Chinese.
const CONTEXT = '@@';

export function t(src, vars) {
  const at = src.indexOf(CONTEXT);
  const base = at < 0 ? src : src.slice(0, at);
  let out = base;
  if (lang === 'en') { const v = en[src]; out = v !== undefined ? v : base; }
  else if (lang === 'zh-Hant' && toHant) {
    out = hantCache.get(base);
    if (out === undefined) { out = toHant(base); hantCache.set(base, out); }
  }
  return interp(out, vars);
}

export function getLang() { return lang; }
export const languages = [
  { id: 'zh', label: '简体中文' },
  { id: 'zh-Hant', label: '正體中文' },
  { id: 'en', label: 'English' },
];
export const htmlLang = () => ({ zh: 'zh-CN', 'zh-Hant': 'zh-TW', en: 'en' }[lang]);
export const locale = () => ({ zh: 'zh-CN', 'zh-Hant': 'zh-TW', en: 'en-US' }[lang]);

export async function setLang(next, { persist = true } = {}) {
  if (!LANGS.includes(next)) next = 'zh';
  if (next === 'zh-Hant' && !toHant) {
    const mod = await import('/ui/js/i18n/hant.js');
    toHant = mod.convert;
  }
  lang = next;
  document.documentElement.lang = htmlLang();
  if (persist) { try { localStorage.setItem(LANG_KEY, next); } catch { /* storage blocked */ } }
  for (const fn of listeners) fn(lang);
}

export function onLang(fn) { listeners.add(fn); return () => listeners.delete(fn); }

export function initialLang() {
  let saved = null;
  try { saved = localStorage.getItem(LANG_KEY); } catch { /* storage blocked */ }
  if (LANGS.includes(saved)) return saved;
  const server = document.documentElement.getAttribute('data-ui-language');
  if (LANGS.includes(server)) return server;
  return /^zh-(TW|HK|MO)|Hant/i.test(navigator.language) ? 'zh-Hant' : /^zh/i.test(navigator.language) ? 'zh' : 'en';
}
