// Settings: appearance (client-side theme/language) and interface preferences.
import { h, setAttr } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { t, add, getLang, languages, setLang } from '../lib/i18n.js';
import * as theme from '../lib/theme.js';
import { seg, checkbox, btn, toast, fail } from '../lib/ui.js';
import { section, row, switchRow, numRow, selectRow, errMsg } from './settings-kit.js';

add({
  '外观': 'Appearance', '主题模式': 'Mode', '配色': 'Palette', '语言': 'Language', '跟随系统': 'System', '浅色': 'Light', '深色': 'Dark',
  '本浏览器的外观与语言': 'Look and language for this browser',
  '深浅模式': 'Light / dark mode', '「跟随系统」会随操作系统的深浅色自动切换。': '“System” follows your operating system’s light or dark setting.',
  '配色方案': 'Palette', '三套配色都支持浅色和深色，预览随当前模式变化。': 'All three palettes support light and dark. The previews follow the current mode.',
  '界面语言': 'Language', '只影响当前浏览器，已保存在本地。': 'Applies to this browser only and is stored locally.',
  '界面与面板': 'Interface', '保存在网关上，所有浏览器共用': 'Stored on the gateway, shared by all browsers',
  '实例默认语言': 'Instance default language', '新访问者默认使用的语言。浏览器顶栏中单独选过语言的人以自己的选择为准。': 'Language for new visitors. People who picked a language in their own browser keep that choice.',
  '清除本浏览器的语言覆盖': 'Clear this browser’s language override', '已清除，正在重新加载…': 'Cleared, reloading…', '本浏览器没有单独设置语言': 'This browser has no language override',
  '导航栏显示的页面': 'Visible pages', '取消勾选即可在导航中隐藏该页面。设置页始终显示。': 'Untick a page to hide it from the navigation. Settings is always shown.',
  '网关': 'Gateway', '账号': 'Accounts', '数据看板': 'Analytics', '任务与福利': 'Tasks', '智能体': 'Agents', '日志': 'Logs', '设置': 'Settings',
});

const PAGES = [
  { id: 'gateway', label: '网关' }, { id: 'accounts', label: '账号' }, { id: 'analytics', label: '数据看板' },
  { id: 'tasks', label: '任务与福利' }, { id: 'agents', label: '智能体' }, { id: 'logs', label: '日志' },
];

/* Colours of the three palettes, copied from tokens.css so every card can show
   its own palette regardless of the one currently applied. */
const PV = {
  claude: { r: 6, light: ['#f1ede3', '#fffefb', '#e1dac8', '#1f1e1b', '#ece6d8', '#b04a28', '#fff'], dark: ['#1b1a17', '#262520', '#3b3830', '#efece3', '#3a372f', '#ec9170', '#2a1309'] },
  material: { r: 9, light: ['#f6f1fa', '#fffbff', '#e2dbe8', '#1d1b20', '#ebe4f0', '#6750a4', '#fff'], dark: ['#131116', '#1f1d24', '#38353f', '#e8e3ea', '#35323c', '#d0bcff', '#381e72'] },
  miuix: { r: 11, light: ['#f3f5f9', '#ffffff', '#e3e7ee', '#1c1e23', '#e6e9f0', '#f0560a', '#fff'], dark: ['#0d0e11', '#17191d', '#2a2e35', '#f0f1f4', '#292d34', '#ff7d38', '#1d0d02'] },
};
const PV_VARS = ['bg', 'surface', 'line', 'fg', 'mute', 'accent', 'on'];

function swatch(style, onPick) {
  const shot = h('div.pv-shot',
    h('div.pv-side', h('i'), h('i'), h('i.on')),
    h('div.pv-main', h('div.pv-card', h('b.w60'), h('b.w85'), h('div.pv-actions', h('span.pv-btn'), h('span.pv-sw')))));
  const check = h('span.pv-check', icon('check', 13));
  const el = h('button.pv', { type: 'button', role: 'radio', 'aria-checked': 'false', onClick: () => onPick(style.id) }, shot, h('div.pv-name', h('span', style.label), check));
  el.paint = (mode, active) => {
    const set = PV[style.id];
    PV_VARS.forEach((k, i) => shot.style.setProperty('--pv-' + k, set[mode][i]));
    shot.style.setProperty('--pv-r', set.r + 'px');
    setAttr(el, 'aria-checked', String(active));
  };
  return el;
}

export function appearanceCard(S, env) {
  const modeSeg = seg([
    { value: 'system', label: t('跟随系统'), icon: 'monitor' },
    { value: 'light', label: t('浅色'), icon: 'sun' },
    { value: 'dark', label: t('深色'), icon: 'moon' },
  ], theme.getMode(), (v) => theme.setMode(v));
  modeSeg.setAttribute('aria-label', t('深浅模式'));

  const cards = theme.STYLES.map((s) => swatch(s, (id) => theme.setStyle(id)));
  const grid = h('div.pv-grid', { role: 'radiogroup', 'aria-label': t('配色方案') }, cards);
  const paint = () => {
    const mode = theme.resolved(), style = theme.getStyle();
    modeSeg.set(theme.getMode());
    cards.forEach((c, i) => c.paint(mode, theme.STYLES[i].id === style));
  };
  paint();
  env.sc.add(theme.onTheme(paint));

  const langSeg = seg(languages.map((l) => ({ value: l.id, label: l.label })), getLang(), (v) => setLang(v));
  langSeg.setAttribute('aria-label', t('界面语言'));

  return section('appearance', t('外观'), { sub: t('本浏览器的外观与语言') },
    row({ title: t('深浅模式'), help: t('「跟随系统」会随操作系统的深浅色自动切换。'), ctl: modeSeg }).el,
    h('div.setting.stack-ctl', h('div.s-text', h('div.s-title', t('配色方案')), h('div.s-help', t('三套配色都支持浅色和深色，预览随当前模式变化。'))), grid),
    row({ title: t('界面语言'), help: t('只影响当前浏览器，已保存在本地。'), ctl: langSeg }).el);
}

export function interfaceCard(S, env) {
  const langRow = selectRow(S, {
    key: 'ui_language', title: t('实例默认语言'), help: t('新访问者默认使用的语言。浏览器顶栏中单独选过语言的人以自己的选择为准。'),
    options: languages.map((l) => ({ value: l.id, label: l.label })),
  });
  langRow.ctlEl.appendChild(btn(t('清除本浏览器的语言覆盖'), { size: 'sm', onClick: () => {
    let had = false;
    try { had = localStorage.getItem('wb-lang') != null; localStorage.removeItem('wb-lang'); } catch { /* storage blocked */ }
    if (!had) { toast(t('本浏览器没有单独设置语言'), 'info'); return; }
    toast(t('已清除，正在重新加载…'), 'ok');
    setTimeout(() => location.reload(), 500);
  } }));

  // Page visibility: one checkbox per page; checked = visible, saved immediately.
  const boxes = new Map();
  const pageList = h('div.pg-list');
  for (const p of PAGES) {
    const c = checkbox({ checked: true, label: t(p.label), onChange: async (on, input) => {
      const hidden = new Set(S.val('hidden_pages') || []);
      if (on) hidden.delete(p.id); else hidden.add(p.id);
      input.disabled = true;
      try { await S.save({ hidden_pages: [...hidden] }); }
      catch (e) { input.checked = !on; fail(errMsg(e)); }
      finally { input.disabled = false; }
    } });
    boxes.set(p.id, c.input);
    pageList.appendChild(c);
  }
  pageList.appendChild(checkbox({ checked: true, disabled: true, label: t('设置') }));
  S.bind((v) => { const hidden = v.hidden_pages || []; for (const [id, input] of boxes) input.checked = !hidden.includes(id); });
  const pagesRow = row({ title: t('导航栏显示的页面'), help: t('取消勾选即可在导航中隐藏该页面。设置页始终显示。'), ctl: pageList, cls: 'stack-ctl' });

  return section('interface', t('界面与面板'), { sub: t('保存在网关上，所有浏览器共用') },
    langRow.el,
    pagesRow.el);
}
