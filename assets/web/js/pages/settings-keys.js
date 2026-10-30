// Settings: API keys table (reveal, copy, enable, edit, delete) and deleted-key history.
import { h, setText, reconcile } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { get, qs } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import { badge, btn, switchEl, empty, confirmDialog, copyText, fail } from '../lib/ui.js';
import { section, errMsg } from './settings-kit.js';
import { editKey } from './settings-keys-edit.js';

add({
  'API Key@@section': 'API keys', '为每个 Key 限定出口、模型和账号，不同客户端可以同时走不同出口。': 'Give every key its own exit, models and accounts so different clients can use different exits at the same time.',
  '{n} 个生效': '{n} active', '当前不校验': 'Not checked', '添加 API Key': 'Add API key',
  '还没有 API Key': 'No API keys yet', '创建一个 Key 后，客户端就必须携带它才能调用接口。': 'Once you create a key, clients must send it to call the API.',
  '启用': 'Enabled', '创建时间': 'Created', '显示密钥': 'Reveal key', '隐藏密钥': 'Hide key', '复制密钥': 'Copy key', '密钥已复制': 'Secret copied',
  '编辑': 'Edit', '删除': 'Delete', '删除 API Key': 'Delete API key',
  '确定删除「{name}」？密钥会被抹掉且无法恢复，已有的用量记录仍会保留这个名称。': 'Delete “{name}”? The secret is erased and cannot be recovered. Existing usage records keep the name.',
  '已删除': 'Deleted', '已启用': 'Enabled', '已禁用': 'Disabled',
  '跟随面板': 'Follows panel', '跟随网关默认出口：{r}': 'Follows the gateway default exit: {r}', '固定国际版': 'Global exit', '固定国内版': 'China exit',
  '限 {n} 个模型': 'Model filter: {n}', '限 {n} 个账号': 'Account filter: {n}', '并发 {n}': 'Concurrency {n}',
  '已删除的 API Key（{n}）': 'Deleted API keys ({n})', '{a} 创建 · {b} 删除': 'Created {a} · deleted {b}',
  '国际版': 'Global', '国内版': 'China', '名称': 'Name', 'Key': 'Key',
});

const AUTO_HIDE_MS = 30000;

export function keysCard(S, env) {
  const timers = new Set();
  const realmName = (r) => (r === 'cn' ? t('国内版') : t('国际版'));

  const countBadge = h('span');
  const addBtn = btn(t('添加 API Key'), { icon: 'plus', variant: 'primary', size: 'sm', onClick: () => { editKey(S, env, null); } });
  const tbody = h('tbody');
  const wrap = h('div.table-wrap', h('table.table.k-table',
    h('thead', h('tr', h('th', t('名称')), h('th', t('Key')), h('th.hide-sm', t('创建时间')), h('th', t('启用')), h('th'))),
    tbody));
  const none = empty('key', t('还没有 API Key'), t('创建一个 Key 后，客户端就必须携带它才能调用接口。'), btn(t('添加 API Key'), { icon: 'plus', variant: 'primary', onClick: () => { editKey(S, env, null); } }));
  const delSummary = h('summary');
  const delList = h('div.k-del-list');
  const deleted = h('details.k-deleted', delSummary, delList);

  /* ---- reveal / copy ---- */
  async function fetchSecret(k) {
    const r = await get('/settings/reveal' + qs({ id: k.id }));
    return r.key || '';
  }
  function hide(tr) {
    clearTimeout(tr.__timer); timers.delete(tr.__timer);
    tr.__plain = null;
    setText(tr.__masked, tr.__k.masked || '');
    tr.__eye.replaceChildren(icon('eye', 15));
    tr.__eye.setAttribute('aria-label', t('显示密钥'));
    tr.__eye.setAttribute('data-tip', t('显示密钥'));
  }
  async function toggleReveal(tr) {
    if (tr.__plain) return hide(tr);
    try { tr.__plain = await fetchSecret(tr.__k); } catch (e) { fail(errMsg(e)); return; }
    setText(tr.__masked, tr.__plain);
    tr.__eye.replaceChildren(icon('eyeOff', 15));
    tr.__eye.setAttribute('aria-label', t('隐藏密钥'));
    tr.__eye.setAttribute('data-tip', t('隐藏密钥'));
    tr.__timer = setTimeout(() => hide(tr), AUTO_HIDE_MS);
    timers.add(tr.__timer);
  }
  async function copyKey(tr) {
    let secret = tr.__plain;
    if (!secret) { try { secret = await fetchSecret(tr.__k); } catch (e) { fail(errMsg(e)); return; } }
    await copyText(secret, t('密钥已复制'));
  }

  async function remove(k) {
    const yes = await confirmDialog({
      title: t('删除 API Key'), message: t('确定删除「{name}」？密钥会被抹掉且无法恢复，已有的用量记录仍会保留这个名称。', { name: k.name }),
      confirmText: t('删除'), danger: true,
    });
    if (!yes) return;
    try { await S.save({ api_keys: [], deleted_api_key_ids: [k.id] }); } catch (e) { fail(errMsg(e)); }
  }

  /* ---- rows ---- */
  function createRow(k) {
    const name = h('div.cell-main.trunc');
    const meta = h('div.row.wrap.gap-1.k-meta');
    const masked = h('span.mono.k-masked');
    const sw = switchEl({ size: 'sm', label: t('启用') });
    const tr = h('tr',
      h('td.k-name', name, meta),
      h('td.k-key', h('div.row.gap-1', masked)),
      h('td.hide-sm.faint.nowrap.k-created'),
      h('td', sw),
      h('td.k-actions'));
    const eye = btn(t('显示密钥'), { iconOnly: true, icon: 'eye', variant: 'ghost', size: 'sm', onClick: () => toggleReveal(tr) });
    const copy = btn(t('复制密钥'), { iconOnly: true, icon: 'copy', variant: 'ghost', size: 'sm', onClick: () => copyKey(tr) });
    const edit = btn(t('编辑'), { iconOnly: true, icon: 'edit', variant: 'ghost', size: 'sm', onClick: () => { editKey(S, env, tr.__k); } });
    const del = btn(t('删除'), { iconOnly: true, icon: 'trash', variant: 'ghost', size: 'sm', onClick: () => remove(tr.__k) });
    tr.querySelector('.k-key .row').append(eye, copy);
    tr.querySelector('.k-actions').append(h('div.row.gap-1', edit, del));
    sw.input.addEventListener('change', async () => {
      const on = sw.input.checked;
      sw.input.disabled = true;
      try { await S.save({ api_keys: [{ id: tr.__k.id, enabled: on }], deleted_api_key_ids: [] }); }
      catch (e) { sw.input.checked = !on; fail(errMsg(e)); }
      finally { sw.input.disabled = false; }
    });
    Object.assign(tr, { __name: name, __meta: meta, __masked: masked, __eye: eye, __sw: sw, __created: tr.querySelector('.k-created'), __plain: null });
    updateRow(tr, k);
    return tr;
  }

  function updateRow(tr, k) {
    tr.__k = k;
    const sig = JSON.stringify([k.name, k.realm, k.models, k.accounts, k.concurrency_limit, k.enabled, k.masked, k.created_at, env.app.realm]);
    if (tr.__sig === sig) return;
    tr.__sig = sig;
    if (tr.__plain) hide(tr);
    setText(tr.__name, k.name);
    setText(tr.__created, k.created_at || '');
    setText(tr.__masked, k.masked || '');
    tr.__sw.input.checked = !!k.enabled;
    tr.classList.toggle('dim', !k.enabled);
    const realm = k.realm
      ? badge(k.realm === 'cn' ? t('固定国内版') : t('固定国际版'), k.realm === 'cn' ? 'info' : 'accent', { sm: true })
      : badge(t('跟随面板'), '', { sm: true, outline: true, tip: t('跟随网关默认出口：{r}', { r: realmName(env.app.realm) }) });
    tr.__meta.replaceChildren(...[
      realm,
      k.models.length ? badge(t('限 {n} 个模型', { n: k.models.length }), 'violet', { sm: true, tip: k.models.join('\n') }) : null,
      k.accounts.length ? badge(t('限 {n} 个账号', { n: k.accounts.length }), 'violet', { sm: true }) : null,
      k.concurrency_limit > 0 ? badge(t('并发 {n}', { n: k.concurrency_limit }), 'warn', { sm: true }) : null,
    ].filter(Boolean));
  }

  function paintDeleted(list) {
    deleted.hidden = !list.length;
    const sig = JSON.stringify(list.map((d) => [d.id, d.name, d.deleted_at]));
    if (deleted.__sig === sig) return;
    deleted.__sig = sig;
    setText(delSummary, t('已删除的 API Key（{n}）', { n: list.length }));
    delList.replaceChildren(...list.slice(0, 200).map((d) => h('div.k-del-row',
      h('span.trunc', d.name),
      h('span.faint.nowrap', t('{a} 创建 · {b} 删除', { a: d.created_at || '—', b: d.deleted_at || '—' })))));
  }

  S.bind((v) => {
    const keys = v.api_keys || [];
    const active = keys.filter((k) => k.enabled).length;
    countBadge.replaceChildren(badge(active ? t('{n} 个生效', { n: active }) : t('当前不校验'), active ? 'good' : '', { dot: true, sm: true }));
    wrap.hidden = !keys.length;
    none.hidden = !!keys.length;
    reconcile(tbody, keys, (k) => k.id, createRow, updateRow);
    paintDeleted(v.deleted_api_keys || []);
  });

  const el = section('keys', t('API Key@@section'), { actions: [countBadge, addBtn] },
    h('div.s-block.k-intro', t('为每个 Key 限定出口、模型和账号，不同客户端可以同时走不同出口。')),
    wrap, none, deleted);
  el.destroy = () => { for (const id of timers) clearTimeout(id); timers.clear(); };
  return el;
}
