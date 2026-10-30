// "Models" card of the Gateway page: what the gateway currently serves for the viewed realm.
import { h, reconcile, setText, debounce } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { get, post, qs } from '../lib/api.js';
import { t } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { badge, btn, toast, ok, empty, copyText, sorter, sortHeader, errText } from '../lib/ui.js';

function abilities(m) {
  const out = [];
  if (m.supports_vision || m.vision) out.push(badge(t('视觉'), 'info', { sm: true }));
  if (m.supports_tool_calls) out.push(badge(t('工具'), 'good', { sm: true }));
  if (m.supports_reasoning) out.push(badge(t('推理'), 'violet', { sm: true }));
  return out;
}

const mult = (m) => {
  const x = parseFloat(String(m.credits || '').replace(/[^0-9.]/g, ''));
  return Number.isFinite(x) ? x : null;
};

export function modelsCard({ view }) {
  let models = [];
  let catalog = null;
  let query = '';
  const tbody = h('tbody');
  const sourceLine = h('div.muted', { style: 'font-size:12.5px;padding:10px 18px 0' });
  const count = h('span.sub');
  const sort = sorter('credits', 'asc', render);
  const heads = [
    sortHeader(t('模型'), 'name', sort),
    sortHeader(t('消费倍率'), 'credits', sort, { num: true }),
    h('th', t('能力')),
    sortHeader(t('上下文'), 'ctx', sort, { num: true }),
    sortHeader(t('最大输出'), 'out', sort, { num: true }),
    h('th.hide-sm', t('思考档位')),
  ];
  const table = h('table.table.compact', h('thead', h('tr', ...heads)), tbody);
  const emptyBox = h('div', { hidden: true }, empty('layers', t('没有匹配的模型')));
  const search = h('input.input.sm', { type: 'search', placeholder: t('搜索模型'), style: 'width:180px', 'aria-label': t('搜索模型'), onInput: debounce((e) => { query = e.target.value.trim().toLowerCase(); render(); }, 120) });
  const refreshBtn = btn(t('刷新模型清单'), { size: 'sm', icon: 'refresh', onClick: async () => {
    try {
      await post('/catalog/refresh', { realm: view() });
      ok(t('模型清单已更新'));
      await load();
    } catch (e) {
      toast(e.status === 429 || /cooldown/i.test(e.message) ? t('同一版本 30 秒内只能刷新一次') : errText(e), 'warn');
    }
  } });

  const el = h('section.card',
    h('div.card-head', h('h2', t('网关模型清单')), count, h('div.actions', search, refreshBtn)),
    sourceLine,
    h('div.table-wrap', table),
    emptyBox);

  const getters = {
    name: (m) => (m.name || m.id || '').toLowerCase(),
    credits: mult,
    ctx: (m) => m.context_length ?? null,
    out: (m) => m.max_output_tokens ?? null,
  };

  function row(m) {
    const copy = h('button.btn.ghost.xs', { type: 'button', 'data-tip': t('复制模型 ID'), style: 'padding:0 5px', onClick: () => copyText(m.id) }, icon('copy', 12));
    const efforts = (m.reasoning_efforts || []);
    const tr = h('tr',
      h('td', h('div.cell-main', m.name || m.id), h('div.cell-sub.row.gap-1', h('span.mono.trunc', { style: 'max-width:240px' }, m.id), copy)),
      h('td.num', m.credits ? h('span.mono', m.credits) : '—'),
      h('td', h('div.row.gap-1.wrap', ...abilities(m))),
      h('td.num', m.context_length ? F.compact(m.context_length) : '—'),
      h('td.num', m.max_output_tokens ? F.compact(m.max_output_tokens) : '—'),
      h('td.hide-sm', efforts.length
        ? h('div.row.gap-1.wrap', ...efforts.map((e) => badge(e, e === m.reasoning_default_effort ? 'accent' : '', { sm: true })))
        : m.always_reasoning ? badge(t('始终思考'), 'violet', { sm: true }) : h('span.faint', '—')));
    if (m.description) tr.firstChild.setAttribute('data-tip', m.description);
    return tr;
  }

  function render() {
    const q = query;
    const rows = sort.apply(models.filter((m) => !q || (m.id + ' ' + (m.name || '')).toLowerCase().includes(q)), getters);
    reconcile(tbody, rows, (m) => m.id, row);
    for (const th of heads) th.refresh?.();
    table.hidden = !rows.length;
    emptyBox.hidden = !!rows.length;
    setText(count, t('{n} 个', { n: rows.length }));
    if (catalog) {
      const when = catalog.fetched_at ? F.mdHm(catalog.fetched_at) : '';
      setText(sourceLine, catalog.source === 'bundled'
        ? t('内置清单（尚未从上游拉取），模型与倍率可能与实际不一致。')
        : t('来自上游 API · 更新于 {t}', { t: when }));
      sourceLine.style.color = catalog.source === 'bundled' ? 'var(--warn)' : '';
    }
  }

  async function load() {
    const r = await get('/v1/models' + qs({ realm: view() }));
    models = r.data || [];
    catalog = r.catalog || null;
    render();
  }

  return { el, load, reload: load };
}
