// API key create/edit dialog with model and account allow-list pickers.
import { h, setText } from '../lib/dom.js';
import { get, qs } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import { btn, field, alertBox, codeBlock, spinner, copyText, ok } from '../lib/ui.js';
import { parseNum, errMsg, dialog } from './settings-kit.js';

add({
  '添加 API Key': 'Add API key', '编辑 API Key': 'Edit API key', '名称': 'Name', '如：Cursor / 本地测试': 'e.g. Cursor / local testing',
  '密钥': 'Secret', '留空表示不修改': 'Leave blank to keep the current secret', '重新生成': 'Generate a new secret',
  '至少 4 个字符。客户端用它作为 Bearer Token。': 'At least 4 characters. Clients send it as the Bearer token.',
  '密钥至少需要 4 个字符': 'The secret needs at least 4 characters',
  '出口': 'Exit', '跟随面板切换': 'Follow the panel’s default exit', '固定国际版出口': 'Always the Global exit', '固定国内版出口': 'Always the China exit',
  '并发上限': 'Concurrency limit', '0 代表不限制。超过上限的请求会收到限流错误。': '0 means unlimited. Requests above the limit get a rate-limit error.',
  '模型白名单': 'Allowed models', '不选表示允许全部模型。': 'Select none to allow every model.',
  '自定义规则': 'Custom patterns', '支持通配符 * ? [abc]，用逗号或换行分隔；不区分大小写。': 'Wildcards * ? [abc] are supported. Separate with commas or new lines; case-insensitive.',
  '账号白名单': 'Allowed accounts', '不选表示允许全部账号。': 'Select none to allow every account.',
  '搜索': 'Search', '全选': 'Select all', '清空': 'Clear', '已选 {n}': '{n} selected', '加载中…': 'Loading…',
  '没有可选项': 'Nothing to choose from', '没有匹配的项目': 'No matches', '仅显示前 {n} 项，请用搜索缩小范围': 'Showing the first {n}; narrow it down with search',
  '（不在列表中）': '(not in the list)', '最多选择 200 个账号': 'Select at most 200 accounts',
  '保存': 'Save', '添加': 'Add', '取消': 'Cancel', '完成': 'Done',
  'API Key 已创建': 'API key created', '请立即复制并保存。之后仍可在列表里点击眼睛图标查看。': 'Copy it now. You can still reveal it later with the eye icon in the list.',
  '复制密钥': 'Copy key', '密钥已复制': 'Secret copied', 'API Key 已更新': 'API key updated',
});

const MAX_RENDER = 500;

export function genKey() {
  const b = new Uint8Array(18);
  crypto.getRandomValues(b);
  return Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
}

const splitPatterns = (s) => [...new Set(String(s).split(/[\s,，;；]+/).map((x) => x.trim().toLowerCase()).filter(Boolean))];

/** Searchable checklist. items: [{ value, label, sub }] */
export function picker() {
  const search = h('input.input.sm', { type: 'search', placeholder: t('搜索'), 'aria-label': t('搜索'), autocomplete: 'off' });
  const count = h('span.faint.pk-count');
  const list = h('div.pk-list', { role: 'group' }, h('div.pk-note', spinner()));
  const nodes = [];
  const selected = new Set();
  const refresh = () => setText(count, t('已选 {n}', { n: selected.size }));
  const visible = () => nodes.filter((n) => !n.hidden);

  function setItems(items) {
    nodes.length = 0;
    const known = new Set(items.map((i) => i.value));
    const all = [...items, ...[...selected].filter((v) => !known.has(v)).map((v) => ({ value: v, label: v, sub: t('（不在列表中）') }))];
    for (const it of all.slice(0, MAX_RENDER)) {
      const input = h('input', { type: 'checkbox', checked: selected.has(it.value) });
      input.addEventListener('change', () => { if (input.checked) selected.add(it.value); else selected.delete(it.value); refresh(); });
      const el = h('label.pk-item', h('span.check', input), h('span.pk-main.trunc', it.label), it.sub ? h('span.pk-sub.faint', it.sub) : null);
      el.__text = (it.label + ' ' + (it.sub || '')).toLowerCase();
      el.__input = input;
      el.__value = it.value;
      nodes.push(el);
    }
    if (!nodes.length) list.replaceChildren(h('div.pk-note.faint', t('没有可选项')));
    else list.replaceChildren(...nodes, ...(all.length > MAX_RENDER ? [h('div.pk-note.faint', t('仅显示前 {n} 项，请用搜索缩小范围', { n: MAX_RENDER }))] : []));
    refresh();
  }
  search.addEventListener('input', () => {
    const q = search.value.trim().toLowerCase();
    for (const n of nodes) n.hidden = q !== '' && !n.__text.includes(q);
  });
  const all = btn(t('全选'), { size: 'xs', variant: 'ghost', onClick: () => { for (const n of visible()) { n.__input.checked = true; selected.add(n.__value); } refresh(); } });
  const none = btn(t('清空'), { size: 'xs', variant: 'ghost', onClick: () => { for (const n of nodes) n.__input.checked = false; selected.clear(); refresh(); } });
  const el = h('div.pk', h('div.pk-bar', search, all, none), list, h('div.pk-foot', count));
  refresh();
  return {
    el, setItems, selected,
    preselect(values) { selected.clear(); for (const v of values) selected.add(v); refresh(); },
    values: () => [...selected],
  };
}

const accountLabel = (a) => (a.nickname || a.uid.slice(0, 8)) + ' · ' + a.uid.slice(0, 8) + ' · ' + (a.realm || 'intl');

/** Show the secret of a freshly created key. */
function createdDialog(secret) {
  const m = dialog({
    title: t('API Key 已创建'), size: 'narrow',
    body: [alertBox('info', t('请立即复制并保存。之后仍可在列表里点击眼睛图标查看。')), codeBlock(secret, { copy: false })],
    actions: [btn(t('复制密钥'), { icon: 'copy', onClick: () => copyText(secret, t('密钥已复制')) }), btn(t('完成'), { variant: 'primary', onClick: () => m.close() })],
  });
}

/** Create (key == null) or edit an API key. */
export function editKey(S, env, key) {
  const isNew = !key;
  const name = h('input.input', { value: key ? key.name : '', placeholder: t('如：Cursor / 本地测试'), maxlength: '80', autocomplete: 'off' });
  const secret = h('input.input.mono', { value: isNew ? genKey() : '', placeholder: isNew ? '' : t('留空表示不修改'), autocomplete: 'off', spellcheck: 'false' });
  const regen = btn(t('重新生成'), { iconOnly: true, icon: 'refresh', onClick: () => { secret.value = genKey(); eSecret.textContent = ''; } });
  const realm = h('select.select', ...[['', t('跟随面板切换')], ['intl', t('固定国际版出口')], ['cn', t('固定国内版出口')]].map(([v, l]) => h('option', { value: v }, l)));
  realm.value = key ? key.realm || '' : '';
  const conc = h('input.input', { type: 'text', inputmode: 'numeric', value: key ? String(key.concurrency_limit || 0) : '0', autocomplete: 'off' });
  const eName = h('div.err'), eSecret = h('div.err'), eConc = h('div.err'), eMisc = h('div');

  const modelPick = picker();
  const accPick = picker();
  const custom = h('textarea.textarea.mono', { rows: '2', spellcheck: 'false', placeholder: 'gpt-*, deepseek-v4*' });
  let modelsLoaded = false;
  const initialModels = key ? key.models || [] : [];
  const initialAccounts = key ? key.accounts || [] : [];
  accPick.preselect(initialAccounts);

  // Catalogues load in the background; saving before they arrive keeps the old lists.
  Promise.allSettled([env.catalog(''), env.catalog('intl'), env.catalog('cn')]).then((res) => {
    const byId = new Map();
    for (const r of res) if (r.status === 'fulfilled') for (const m of r.value) if (m.id && !byId.has(m.id.toLowerCase())) byId.set(m.id.toLowerCase(), m);
    const items = [...byId.entries()].map(([id, m]) => ({ value: id, label: m.id, sub: m.credits || '' })).sort((a, b) => a.value.localeCompare(b.value));
    const ids = new Set(items.map((i) => i.value));
    modelPick.preselect(initialModels.filter((p) => ids.has(p)));
    custom.value = initialModels.filter((p) => !ids.has(p)).join(', ');
    modelPick.setItems(items);
    modelsLoaded = true;
  });
  get('/accounts' + qs({ realm: 'all' })).then((r) => {
    accPick.setItems((r.accounts || []).map((a) => ({ value: a.uid, label: accountLabel(a), sub: '' })));
  }).catch(() => accPick.setItems([]));

  async function submit() {
    for (const e of [eName, eSecret, eConc]) setText(e, '');
    eMisc.replaceChildren();
    let bad = null;
    const sec = secret.value.trim();
    if ((isNew || sec) && [...sec].length < 4) { setText(eSecret, t('密钥至少需要 4 个字符')); bad = bad || secret; }
    const c = parseNum(conc.value, { min: 0, max: 1000, blank: 'null' });
    if (c.error) { setText(eConc, c.error); bad = bad || conc; }
    const accounts = accPick.values();
    if (accounts.length > 200) { eMisc.replaceChildren(alertBox('warn', t('最多选择 200 个账号'))); bad = bad || accPick.el; }
    if (bad) { if (bad.focus) bad.focus(); return; }
    const models = modelsLoaded ? [...new Set([...modelPick.values(), ...splitPatterns(custom.value)])] : initialModels;
    const row = { name: name.value.trim(), realm: realm.value, models, accounts, concurrency_limit: c.value || 0 };
    if (!isNew) row.id = key.id; else row.enabled = true;
    if (sec) row.key = sec;
    try {
      await S.save({ api_keys: [row], deleted_api_key_ids: [] }, { quiet: !isNew });
      m.close(true);
      if (isNew) createdDialog(sec);
      else ok(t('API Key 已更新'));
    } catch (e) {
      eMisc.replaceChildren(alertBox('bad', errMsg(e)));
    }
  }

  const modelBlock = h('div.kd-col', h('div.kd-label', t('模型白名单')), h('div.hint', t('不选表示允许全部模型。')), modelPick.el,
    field(t('自定义规则'), custom, t('支持通配符 * ? [abc]，用逗号或换行分隔；不区分大小写。')));
  const accBlock = h('div.kd-col', h('div.kd-label', t('账号白名单')), h('div.hint', t('不选表示允许全部账号。')), accPick.el);
  const body = [
    h('div.kd-grid', field(t('名称'), name, null, eName), field(t('出口'), realm)),
    field(t('密钥'), h('div.input-affix', secret, regen), isNew ? t('至少 4 个字符。客户端用它作为 Bearer Token。') : null, eSecret),
    field(t('并发上限'), conc, t('0 代表不限制。超过上限的请求会收到限流错误。'), eConc),
    h('div.kd-grid.lists', modelBlock, accBlock),
    eMisc,
  ];
  const m = dialog({
    title: isNew ? t('添加 API Key') : t('编辑 API Key'), size: 'wide', body,
    actions: [btn(t('取消'), { onClick: () => m.close(false) }), btn(t('保存'), { variant: 'primary', onClick: submit })],
  });
  for (const i of [name, secret, conc]) i.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); submit(); } });
  return m;
}
