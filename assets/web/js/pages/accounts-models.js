// Accounts page: pick which models an account may serve. Unchecked = disabled for this account.
import { h, setText, debounce } from '../lib/dom.js';
import { get, qs } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import { btn, modal, fail, toast, errText } from '../lib/ui.js';
import { accountName } from './accounts-model.js';

add({
  '启用模型 · {name}': 'Allowed models · {name}', '取消勾选的模型不再派给这个账号，请求会自动落到池子里其他账号；其余模型不受影响。默认全部勾选 = 不限制。区域 {realm}，共 {n} 个模型。': 'Unchecked models are no longer sent to this account; requests fall back to other accounts. All checked = no restriction. Region {realm}, {n} models.',
  '全选': 'Select all', '全不选': 'Select none', '保存': 'Save', '取消': 'Cancel', '将禁用 {n} 个': '{n} will be blocked', '不限制': 'No restriction', '搜索模型': 'Search models',
  '取模型清单失败: {msg}': 'Could not load the model list: {msg}', '该区域没有可用模型': 'No models available in this region', '不在当前清单中': 'not in the current list',
});

export async function pickModels(host, a) {
  let ids;
  try {
    const r = await get('/v1/models' + qs({ realm: a.realm || host.view }));
    ids = [...new Set((r.data || []).map((m) => m.id).filter(Boolean))];
  } catch (e) { fail(t('取模型清单失败: {msg}', { msg: errText(e) })); return; }
  if (!ids.length) { toast(t('该区域没有可用模型'), 'warn'); return; }

  const lower = (s) => String(s).toLowerCase();
  const blocked = new Set((a.disabledModels || []).map(lower));
  const known = new Set(ids.map(lower));
  // previously blocked ids that the current list no longer shows must not be dropped on save
  const extra = [...blocked].filter((x) => !known.has(x));
  const all = [...ids.sort((x, y) => x.localeCompare(y)), ...extra.sort()];
  const rows = new Map();
  const counter = h('span');
  const list = h('div.mp-list');

  for (const id of all) {
    const input = h('input', { type: 'checkbox', checked: !blocked.has(lower(id)) });
    input.addEventListener('change', paintCount);
    const row = h('label.mp-row', input, h('span.mono.trunc', id), extra.includes(id) ? h('span.faint', t('不在当前清单中')) : null);
    rows.set(id, { input, row });
    list.appendChild(row);
  }
  function paintCount() {
    const n = all.filter((id) => !rows.get(id).input.checked).length;
    setText(counter, n ? t('将禁用 {n} 个', { n }) : t('不限制'));
    counter.className = n ? 'tx-warn' : 'faint';
  }
  const setAll = (on) => { for (const { input, row } of rows.values()) if (!row.hidden) input.checked = on; paintCount(); };
  const search = h('input.input.sm', { type: 'search', placeholder: t('搜索模型'), 'aria-label': t('搜索模型'), autocomplete: 'off' });
  search.addEventListener('input', debounce(() => {
    const q = search.value.trim().toLowerCase();
    for (const [id, { row }] of rows) row.hidden = !!q && !id.toLowerCase().includes(q);
  }, 120));
  paintCount();

  const m = modal({
    title: t('启用模型 · {name}', { name: accountName(a) }), persistent: true,
    body: [h('p.muted', { style: 'font-size:13px' }, t('取消勾选的模型不再派给这个账号，请求会自动落到池子里其他账号；其余模型不受影响。默认全部勾选 = 不限制。区域 {realm}，共 {n} 个模型。', { realm: a.realm || host.view, n: ids.length })),
      h('div.row.gap-3.wrap', search, btn(t('全选'), { size: 'sm', onClick: () => setAll(true) }), btn(t('全不选'), { size: 'sm', onClick: () => setAll(false) }), h('span.grow'), counter), list],
    actions: [btn(t('取消'), { onClick: () => m.close() }), btn(t('保存'), { variant: 'primary', onClick: async () => {
      const off = all.filter((id) => !rows.get(id).input.checked).map(lower);
      await host.actions.setDisabledModels(a, off);
      m.close();
    } })],
    onClose: () => host.dialogs.delete(m),
  });
  host.dialogs.add(m);
}
