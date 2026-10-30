// "Last action" card: the server's real per-account outcome, kept until dismissed or replaced.
import { h, setText } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { add, t } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { badge, btn, alertBox } from '../lib/ui.js';
import { lastResult } from './tasks-actions.js';

add({
  '最近一次操作': 'Last action', '收起结果': 'Dismiss result', '仅显示前 {n} 行，完整内容请查看「日志」页。': 'Showing the first {n} lines only; see the Logs page for the rest.',
});

const MAX_LINES = 200;
const ICON = { ok: 'checkCircle', warn: 'alert', bad: 'xCircle', info: 'info' };
const ALERT = { ok: 'good', warn: 'warn', bad: 'bad', info: 'info' };
const TAG_KIND = { ok: 'good', warn: 'warn', bad: 'bad', info: '' };

function lineEl(l) {
  if (l.kind === 'head') return h('div.tk-line.head', l.text);
  return h('div.tk-line.' + l.kind + (l.indent ? '.indent' : ''),
    icon(ICON[l.kind] || 'info', 15, 'ic'),
    h('div.tk-line-body',
      l.name || l.tag ? h('div.tk-line-top', l.name ? h('b.trunc', l.name) : null, l.tag ? badge(l.tag, TAG_KIND[l.kind] || '', { sm: true }) : null) : null,
      l.text ? h('div.tk-line-text', l.text) : null));
}

export function resultCard(store) {
  let shown = 0;       // `at` of the result currently rendered
  let dismissed = 0;   // `at` of the result the user closed
  const sub = h('span.sub');
  const list = h('div.tk-lines');
  const sum = h('div');
  const close = btn(t('收起结果'), { iconOnly: true, icon: 'x', variant: 'ghost', size: 'sm', onClick: () => { dismissed = shown; paint(); } });
  const el = h('section.card.tk-result', { hidden: true },
    h('div.card-head', h('h2', t('最近一次操作')), sub, h('div.actions', close)),
    h('div.card-body', sum, list));

  function paint() {
    const r = lastResult();
    const visible = !!r && r.realm === store.view && r.at !== dismissed;
    el.hidden = !visible;
    if (!visible || r.at === shown) return;
    shown = r.at;
    setText(sub, `${r.label} · ${F.time(r.at)}`);
    sum.replaceChildren(alertBox(ALERT[r.kind] || 'info', r.toast));
    const lines = r.lines.slice(0, MAX_LINES).map(lineEl);
    if (r.lines.length > MAX_LINES) lines.push(h('div.faint', { style: 'font-size:12.5px;padding:4px 0' }, t('仅显示前 {n} 行，完整内容请查看「日志」页。', { n: MAX_LINES })));
    list.replaceChildren(...lines);
    // a single row is already spelled out in the summary above
    list.hidden = r.lines.length < 2;
  }
  return { el, paint };
}
