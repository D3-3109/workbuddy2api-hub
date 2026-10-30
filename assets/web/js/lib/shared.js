// Helpers used by more than one page.
import { h } from './dom.js';
import { t } from './i18n.js';
import { badge } from './ui.js';
import * as F from './fmt.js';

/** Result badge for a usage row (`outcome`, falling back to `error`). */
export function outcomeBadge(row) {
  const o = row.outcome || (row.error ? 'failed' : 'completed');
  if (o === 'completed') return badge(t('完成'), 'good', { dot: true });
  if (o === 'client_aborted' || o === 'client_abort') return badge(t('客户端取消'), '', { dot: true });
  if (o === 'upstream_aborted') return badge(t('上游中断'), 'warn', { dot: true });
  const b = badge(t('失败'), 'bad', { dot: true });
  if (row.message) b.setAttribute('data-tip', String(row.message));
  return b;
}

/** Tokens per second: use the stored value, else derive from generation time. */
export function tokensPerSec(r) {
  if (r.tokens_per_sec) return r.tokens_per_sec;
  if (r.gen_ms > 0 && r.completion_tokens > 0) return r.completion_tokens / (r.gen_ms / 1000);
  return null;
}

export function cachePct(r) {
  if (r.cache_hit_pct) return r.cache_hit_pct;
  if (r.prompt_tokens > 0 && r.cached_tokens > 0) return (r.cached_tokens / r.prompt_tokens) * 100;
  return null;
}

/** "GPT-5.6-Terra" style names are shown as-is; ids are monospace. */
export function modelCell(id, effort, extra) {
  return h('div.row.gap-1', { style: 'min-width:0' }, h('span.mono.trunc', { style: 'font-size:12.5px' }, id || '—'), effort ? badge(effort, 'violet', { sm: true }) : null, extra || null);
}

/** Multi-line explanation of how a row's estimated price was derived. */
export function costTip(r) {
  if (r.cost_cny == null) return '';
  const lines = [t('按 OpenRouter 公布的模型价折算的等价 token 花费')];
  if (r.cost_source === 'builtin') lines.push(t('定价依据：出厂快照'));
  else if (r.cost_source) lines.push(t('定价策略：{s}', { s: r.cost_source }));
  if (r.cost_via === 'direct') lines.push(t('匹配：直接匹配 {id}', { id: r.cost_or_id }));
  else if (r.cost_override_from || r.cost_via === 'override') lines.push(t('匹配：人工映射 {a} → {b}', { a: r.cost_override_from || r.model, b: r.cost_or_id }));
  else if (r.cost_inherited_from) lines.push(t('匹配：变体继承 {a} → {b}', { a: r.cost_inherited_from, b: r.cost_or_id }));
  const rt = r.cost_rates;
  if (rt) lines.push(t('单价 (每百万)：缓存命中 {a} · 未命中 {b} · 输出 {c}', { a: rt.input_cache_hit, b: rt.input_cache_miss, c: rt.output }));
  return lines.join('\n');
}

export function costText(v) {
  if (v == null) return '—';
  const a = Math.abs(v);
  return '¥' + (a > 0 && a < 0.01 ? v.toFixed(4) : a < 1 ? v.toFixed(3) : F.fixed(v, 2));
}

/** Page numbers with ellipses: [1, '…', 4, 5, 6, '…', 20]. */
export function pageList(cur, total) {
  if (total <= 7) return Array.from({ length: total }, (_, i) => i + 1);
  let a = Math.max(2, cur - 2), b = Math.min(total - 1, cur + 2);
  if (cur <= 4) { a = 2; b = 5; }
  if (cur >= total - 3) { a = total - 4; b = total - 1; }
  const out = [1];
  if (a > 2) out.push('…');
  for (let i = a; i <= b; i++) out.push(i);
  if (b < total - 1) out.push('…');
  out.push(total);
  return out;
}

/** Stable accent colour index for a string (account / model chips). */
export function hashIndex(str, mod = 8) {
  let x = 5381;
  for (let i = 0; i < str.length; i++) x = ((x << 5) + x + str.charCodeAt(i)) | 0;
  return Math.abs(x) % mod;
}
