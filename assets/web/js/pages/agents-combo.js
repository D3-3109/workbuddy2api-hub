// Searchable model picker: a text box over a listbox. Options keep the input focused
// (mousedown is cancelled) and blur closes the list, so no document-level listener is needed.
import { h } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { btn } from '../lib/ui.js';

add({
  '(不指定模型)': '(no model)', '搜索或输入模型 ID': 'Search or type a model ID', '清除': 'Clear', '使用 “{id}”': 'Use “{id}”',
  '{n} 上下文': '{n} ctx', '还有 {n} 项，继续输入以筛选': '{n} more, keep typing to narrow down', '没有匹配的模型': 'No matching models',
});

const MAX_ROWS = 200;
let seq = 0;

/** modelCombo({ onChange(id) }) -> { el, input, setModels([{id, context_window}]), setValue(id) } */
export function modelCombo({ onChange }) {
  const uid = 'agmc' + ++seq;
  let models = [];
  let value = '';
  let open = false;
  let typing = false;   // filter only after the user typed; opening shows the full list
  let active = -1;
  let rows = [];

  const input = h('input.input.mono', {
    type: 'text', role: 'combobox', 'aria-autocomplete': 'list', 'aria-expanded': 'false', 'aria-controls': uid,
    autocomplete: 'off', spellcheck: 'false', placeholder: t('搜索或输入模型 ID'), 'aria-label': t('搜索或输入模型 ID'),
  });
  const clear = btn(t('清除'), { iconOnly: true, icon: 'x', size: 'xs', variant: 'ghost', onClick: () => choose('') });
  clear.classList.add('clear');
  clear.hidden = true;
  const list = h('div.ag-pop', { id: uid, role: 'listbox', hidden: true });
  const el = h('div.ag-combo', h('div.input-wrap', icon('search', 15, 'ic'), input, clear), list);

  function optionEl(row, i) {
    const o = h('div.ag-opt', { id: uid + '-' + i, role: 'option', 'aria-selected': String(row.id === value && !row.custom) },
      row.custom ? h('span.ag-opt-id', t('使用 “{id}”', { id: row.id })) : row.id ? h('span.ag-opt-id.mono', row.id) : h('span.ag-opt-id.faint', t('(不指定模型)')),
      row.ctx ? h('span.ag-opt-ctx', t('{n} 上下文', { n: F.compact(row.ctx) })) : null);
    o.addEventListener('click', () => choose(row.id));
    return o;
  }

  function build() {
    const q = typing ? input.value.trim().toLowerCase() : '';
    const typed = input.value.trim();
    const found = q ? models.filter((m) => m.id.toLowerCase().includes(q)) : models;
    rows = q ? [] : [{ id: '' }];
    for (const m of found.slice(0, MAX_ROWS)) rows.push({ id: m.id, ctx: m.context_window });
    if (typing && typed && !models.some((m) => m.id === typed)) rows.push({ id: typed, custom: true });
    const more = found.length - MAX_ROWS;
    list.replaceChildren(...rows.map(optionEl),
      ...(more > 0 ? [h('div.ag-pop-note', t('还有 {n} 项，继续输入以筛选', { n: F.int(more) }))] : []),
      ...(!rows.length ? [h('div.ag-pop-note', t('没有匹配的模型'))] : []));
    setActive(Math.max(0, rows.findIndex((r) => r.id === value && !r.custom)));
  }

  function setActive(i) {
    const prev = list.children[active];
    if (prev) prev.classList.remove('active');
    active = rows.length ? Math.max(0, Math.min(i, rows.length - 1)) : -1;
    const cur = list.children[active];
    if (cur) {
      cur.classList.add('active');
      cur.scrollIntoView({ block: 'nearest' });
      input.setAttribute('aria-activedescendant', cur.id);
    } else input.removeAttribute('aria-activedescendant');
  }

  function show() {
    open = true;
    list.hidden = false;
    input.setAttribute('aria-expanded', 'true');
    build();
  }

  function close() {
    open = false;
    typing = false;
    list.hidden = true;
    input.setAttribute('aria-expanded', 'false');
    input.removeAttribute('aria-activedescendant');
    input.value = value;
  }

  function choose(id) {
    value = id;
    clear.hidden = !id;
    close();
    onChange(id);
  }

  input.addEventListener('focus', () => { typing = false; input.select(); show(); });
  input.addEventListener('pointerdown', () => { if (!open && document.activeElement === input) show(); });
  input.addEventListener('blur', close);
  input.addEventListener('input', () => { typing = true; clear.hidden = !input.value && !value; show(); setActive(0); });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (!open) show(); else setActive(active + (e.key === 'ArrowDown' ? 1 : -1));
    } else if (e.key === 'Enter') {
      if (open && rows[active]) { e.preventDefault(); choose(rows[active].id); }
    } else if (e.key === 'Escape' && open) {
      e.preventDefault();
      e.stopPropagation();
      close();
    }
  });
  list.addEventListener('mousedown', (e) => e.preventDefault());

  return {
    el, input,
    get value() { return value; },
    setModels(next) { models = next; if (open) build(); },
    setValue(id) { value = id; clear.hidden = !id; if (!open) input.value = id; },
  };
}
