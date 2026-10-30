// Settings: grouped preference cards with a sticky section index (scroll-spy).
// Every control saves a partial patch through the store in settings-kit.js; the
// reply (the full settings view) is re-applied to all controls.
import { h, scope, rafThrottle } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { get, getOr, qs } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import { btn, pageHead, empty, skeleton, toast, errText } from '../lib/ui.js';
import { createStore } from './settings-kit.js';
import { appearanceCard, interfaceCard } from './settings-ui.js';
import { securityCard } from './settings-security.js';
import { keysCard } from './settings-keys.js';
import { egressCard } from './settings-proxy.js';
import { limitsCard } from './settings-limits.js';
import { modelsCard, identityCard } from './settings-models.js';
import { dailyCard } from './settings-daily.js';
import { dataCard } from './settings-data.js';
import { aboutCard } from './settings-about.js';

add({
  '设置': 'Settings', '刷新': 'Refresh', '重试': 'Retry',
  '管理网关的访问、出口、限额、模型行为和每日任务。大多数更改会立即保存。': 'Manage access, exits, limits, model behaviour and daily tasks. Most changes are saved as soon as you make them.',
  '设置分区': 'Settings sections', '设置加载失败': 'Failed to load settings', '已刷新': 'Refreshed',
  '外观': 'Appearance', '界面与面板': 'Interface', '访问与安全': 'Access and security', 'API Key@@section': 'API keys', '出口与代理': 'Egress and proxies', '限额与调度': 'Limits and dispatch', '模型与接口': 'Models and API', '出站身份': 'Outbound identity', '每日活跃': 'Daily activity', '价格与数据': 'Pricing and data', '运行信息': 'About and runtime',
});

export const usesRealm = false;

// Order = order on the page and in the index.
const SECTIONS = [
  { id: 'appearance', icon: 'palette', label: () => t('外观'), build: appearanceCard },
  { id: 'interface', icon: 'sliders', label: () => t('界面与面板'), build: interfaceCard },
  { id: 'security', icon: 'shield', label: () => t('访问与安全'), build: securityCard },
  { id: 'keys', icon: 'key', label: () => t('API Key@@section'), build: keysCard },
  { id: 'egress', icon: 'globe', label: () => t('出口与代理'), build: egressCard },
  { id: 'limits', icon: 'coins', label: () => t('限额与调度'), build: limitsCard },
  { id: 'models', icon: 'layers', label: () => t('模型与接口'), build: modelsCard },
  { id: 'identity', icon: 'tag', label: () => t('出站身份'), build: identityCard },
  { id: 'daily', icon: 'calendar', label: () => t('每日活跃'), build: dailyCard },
  { id: 'data', icon: 'database', label: () => t('价格与数据'), build: dataCard },
  { id: 'about', icon: 'info', label: () => t('运行信息'), build: aboutCard },
];

function makeEnv(sc, data, app) {
  const cache = new Map();
  return {
    sc, app, ...data,
    /** Model catalogue of a realm ('' = default), fetched once. */
    catalog(realm = '') {
      let p = cache.get(realm);
      if (!p) {
        p = getOr('/v1/models' + qs({ realm }), { data: [] }).then((r) => r.data || []);
        p.catch(() => cache.delete(realm));
        cache.set(realm, p);
      }
      return p;
    },
  };
}

export async function mount(root, app) {
  root.classList.add('p-settings');
  const sc = scope();
  let alive = true;
  let built = [];
  let S = null;
  let lockUntil = 0;

  const body = h('div.s-body');
  const refresh = btn(t('刷新'), { iconOnly: true, icon: 'refresh', onClick: () => reload() });
  root.append(pageHead(t('设置'), t('管理网关的访问、出口、限额、模型行为和每日任务。大多数更改会立即保存。'), refresh), body);

  async function load() {
    const [view, slots, updates, pricing] = await Promise.all([
      get('/settings'),
      getOr('/proxy/slots', { slots: [] }),
      getOr('/updates', null).catch(() => null),
      getOr('/pricing', null).catch(() => null),
    ]);
    return { view, slots: slots.slots || [], updates, pricing };
  }

  async function init() {
    body.replaceChildren(skeleton(5));
    let data;
    try { data = await load(); }
    catch (e) {
      if (alive) body.replaceChildren(empty('alert', t('设置加载失败'), errText(e), btn(t('重试'), { variant: 'primary', onClick: init })));
      return;
    }
    if (alive) render(data);
  }

  async function reload() {
    if (!S) return init();
    const view = await get('/settings');
    if (!alive) return;
    S.apply(view);
    await Promise.all(built.map((b) => (b.el.reload ? b.el.reload() : null)));
    toast(t('已刷新'), 'ok', 1800);
  }

  function render(data) {
    built.forEach((b) => b.el.destroy && b.el.destroy());
    S = createStore(data.view, { onApply: (v) => { app.settings = v; } });
    app.settings = data.view;
    const env = makeEnv(sc, data, app);
    built = SECTIONS.map((s) => ({ ...s, el: s.build(S, env) }));

    const index = h('nav.s-index', { 'aria-label': t('设置分区') });
    const items = new Map();
    for (const b of built) {
      const item = h('button.s-index-item', { type: 'button', onClick: () => jump(b.id) }, icon(b.icon, 16), h('span', b.label()));
      items.set(b.id, item);
      index.appendChild(item);
    }
    body.replaceChildren(h('div.s-layout', index, h('div.s-main', built.map((b) => b.el))));

    let activeId = null;
    function setActive(id) {
      if (id === activeId) return;
      activeId = id;
      for (const [k, el] of items) {
        if (k === id) el.setAttribute('aria-current', 'true'); else el.removeAttribute('aria-current');
      }
      const el = items.get(id);
      if (el && index.scrollWidth > index.clientWidth) {
        index.scrollTo({ left: Math.max(0, el.offsetLeft - (index.clientWidth - el.offsetWidth) / 2), behavior: 'smooth' });
      }
    }

    function jump(id) {
      const target = built.find((b) => b.id === id);
      if (!target) return;
      lockUntil = performance.now() + 800;
      setActive(id);
      target.el.scrollIntoView({ behavior: 'smooth', block: 'start' });
    }

    const scroller = root.closest('.main') || root.parentElement;
    const spy = rafThrottle(() => {
      if (performance.now() < lockUntil) return;
      const line = scroller.getBoundingClientRect().top + 120;
      let current = built[0];
      for (const b of built) { if (b.el.getBoundingClientRect().top <= line) current = b; else break; }
      if (scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 6) current = built[built.length - 1];
      setActive(current.id);
    });
    sc.on(scroller, 'scroll', spy, { passive: true });
    setActive(built[0].id);
    spy();

    const want = app.route && app.route.query && app.route.query.get('sec');
    if (want) requestAnimationFrame(() => jump(want));
  }

  init();

  return () => {
    alive = false;
    sc.dispose();
    built.forEach((b) => b.el.destroy && b.el.destroy());
  };
}
