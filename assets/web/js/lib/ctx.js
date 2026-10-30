// App-wide state shared by the shell and pages. Kept deliberately small: pages
// own their own data; only things that cross pages live here.
import { get, post, getOr } from './api.js';

export const bus = new EventTarget();

export const ctx = {
  realm: 'intl',         // the gateway's default exit (server state, POST /realm)
  view: 'intl',          // which realm pages display (client only)
  realms: ['intl', 'cn'],
  health: null,          // last /health payload
  panel: null,           // last /panel/status payload
  settings: null,        // last /settings payload (loaded lazily)
  route: { id: 'gateway', query: new URLSearchParams() },
};

export const emit = (type, detail) => bus.dispatchEvent(new CustomEvent(type, { detail }));
export const on = (type, fn) => { const h = (e) => fn(e.detail); bus.addEventListener(type, h); return () => bus.removeEventListener(type, h); };

export async function loadRealm() {
  const r = await get('/realm');
  ctx.realm = r.current || 'intl';
  ctx.realms = r.options || ctx.realms;
  if (!ctx.viewChosen) {
    const q = new URLSearchParams(location.search).get('view');
    ctx.view = q === 'cn' || q === 'intl' ? q : ctx.realm;
  }
  return ctx.realm;
}

/** Change which realm the pages show. Client only; nothing is sent to the server. */
export function setView(view) {
  if (view === ctx.view) return;
  ctx.view = view;
  ctx.viewChosen = true;
  emit('view', view);
}

/** Change the gateway's default exit (persisted by the server). */
export async function setActiveRealm(realm) {
  const r = await post('/realm', { realm });
  ctx.realm = r.current || realm;
  emit('realm', ctx.realm);
  return ctx.realm;
}

export async function loadHealth() {
  ctx.health = await get('/health');
  emit('health', ctx.health);
  return ctx.health;
}

export async function loadSettings(force = false) {
  if (!ctx.settings || force) ctx.settings = await get('/settings');
  return ctx.settings;
}

export const realmLabel = (r) => (r === 'cn' ? '国内版' : '国际版');

export { get, post, getOr };

/** Navigate to a page id, optionally with query params for the target page. */
export function go(id, query) {
  const q = query ? '?' + new URLSearchParams(query).toString() : '';
  location.hash = '#/' + id + q;
}
