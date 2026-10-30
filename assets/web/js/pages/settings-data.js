// Settings: pricing estimate, usage retention, logs.
import { h } from '../lib/dom.js';
import { getOr } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import { badge, btn, alertBox, unsupported } from '../lib/ui.js';
import { section, row, subhead, switchRow, numRow, unsupportedBadge } from './settings-kit.js';

add({
  '价格与数据': 'Pricing and data', '价格估算、统计数据保留与日志': 'Price estimate, usage retention and logs',
  '启用价格估算': 'Price estimate', '按 OpenRouter 价格把 token 折算成人民币，用于「OpenRouter 价估算」列和「API 等价花费」卡片。关闭后这些列和卡片都会隐藏；重新打开会补算关闭期间的请求。': 'Convert tokens to CNY with OpenRouter prices for the “OpenRouter price” columns and the “API-equivalent spend” card. Off hides them; turning it back on back-fills requests made while it was off.',
  '价格自动刷新间隔': 'Price refresh interval', '自动取价的间隔（分钟），0 为不自动取价。最大 43200。': 'How often prices are fetched automatically, in minutes. 0 disables it. Maximum 43200.', '分钟': 'min',
  '变体后缀继承': 'Inherit variant suffix prices', '带渠道后缀的模型名（如 -lkeap）在恰好匹配到一个基础模型时，沿用其价格。': 'Model names with a channel suffix (such as -lkeap) take the base model’s price when exactly one base matches.',
  '价格数据状态': 'Price data status', '价格快照时间': 'Price snapshot', '已收录模型': 'Models priced', '上次取价': 'Last fetch', '下次取价': 'Next fetch', '最近错误': 'Last error',
  '立即取价': 'Fetch prices now', '价格状态不可用': 'Price status is not available', '取价与手动映射在当前 Rust 版网关中未实现，现用的是内置价格。': 'Fetching prices and manual mappings are not implemented in the Rust gateway; the bundled prices are used.',
  '统计数据保留': 'Usage retention', '小时级预聚合保留': 'Hourly roll-up window', '用量看板的小时预聚合窗口，范围 1 到 336 小时。越大检查点越大、写入越多，宽时间范围查询越快。': 'Hourly pre-aggregation window for the usage dashboard, 1 to 336 hours. Larger means bigger checkpoints and more writes, but faster wide-range queries.', '小时': 'h',
  '日级保留': 'Daily bucket window', '日级聚合桶保留的天数，范围 1 到 366。': 'Days of daily buckets to keep, 1 to 366.', '天': 'days',
  '日志': 'Logs', '日志写入数据库': 'Persist logs to a database', '写入 usage/logs.sqlite3；关闭时只保存在内存。': 'Write logs to usage/logs.sqlite3. Off keeps them in memory only.',
  '日志仅保存在内存中（最近 2000 条），网关重启后丢失。': 'Logs are kept in memory only (the latest 2000) and are lost when the gateway restarts.',
  '日志行数上限': 'Log row cap', '留空为不限。最大 1 亿。': 'Empty means unlimited. Maximum 100 million.', '不限': 'No limit',
  '日志保留天数': 'Log age cap', '留空为不限。最大 36500。': 'Empty means unlimited. Maximum 36500.',
  '日志体积上限（MB）': 'Log size cap (MB)', '留空为不限。最大 102400（100 GB）。': 'Empty means unlimited. Maximum 102400 (100 GB).',
  '额度扫描': 'Quota scan', '每 6 小时主动探测各账号+模型的剩余额度。当前 Rust 版网关未实现。': 'Probe each account+model’s remaining quota every 6 hours. Not implemented in the Rust gateway.', '立即扫描': 'Scan now',
});

const MB = 1048576;

function pricingPanel(S, env) {
  const kv = h('dl.kv.pr-kv');
  const note = h('div.pr-note');
  const refresh = btn(t('立即取价'), { size: 'sm', icon: 'refresh' });
  if (!S.cap('pricing_refresh')) unsupported(refresh, t('当前 Rust 版网关未实现此功能'));
  const box = h('div.s-block.pr-panel', h('div.row.between', h('div.px-title', h('span', t('价格数据状态'))), refresh), kv, note);
  let info = env.pricing;
  function paint() {
    if (!info) { kv.replaceChildren(); note.replaceChildren(alertBox('info', t('价格状态不可用'))); return; }
    const rows = [[t('价格快照时间'), info.current_at_label || '—'], [t('已收录模型'), String(info.models ?? 0)], [t('上次取价'), info.last_run_label || '—'], [t('下次取价'), info.next_run_label || '—']];
    if (info.last_error) rows.push([t('最近错误'), info.last_error]);
    kv.replaceChildren(...rows.flatMap(([k, v]) => [h('dt', k), h('dd', v)]));
    note.replaceChildren(S.cap('pricing_refresh') ? '' : h('div.faint', t('取价与手动映射在当前 Rust 版网关中未实现，现用的是内置价格。')));
  }
  paint();
  box.reload = async () => { info = await getOr('/pricing', null).catch(() => info); paint(); };
  S.bind((v) => box.classList.toggle('is-off', v.pricing_enabled === false));
  return box;
}

export function dataCard(S, env) {
  const interval = numRow(S, { key: 'pricing_refresh_minutes', title: t('价格自动刷新间隔'), help: t('自动取价的间隔（分钟），0 为不自动取价。最大 43200。'), min: 0, max: 43200, int: false, unit: t('分钟'), stored: true });
  const inherit = switchRow(S, { key: 'pricing_variant_inherit', title: t('变体后缀继承'), help: t('带渠道后缀的模型名（如 -lkeap）在恰好匹配到一个基础模型时，沿用其价格。'), stored: true });
  S.bind((v) => { interval.setDisabled(v.pricing_enabled === false); inherit.sw.input.disabled = v.pricing_enabled === false; });

  const panel = pricingPanel(S, env);
  const mem = (S.view.management_capabilities || {}).log_mode;
  const logNote = h('div.s-block', alertBox('info', mem === 'memory' ? t('日志仅保存在内存中（最近 2000 条），网关重启后丢失。') : String(mem || '')));

  const scanBtn = btn(t('立即扫描'), { size: 'sm', icon: 'play' });
  unsupported(scanBtn, t('当前 Rust 版网关未实现此功能'));
  const scan = row({ title: t('额度扫描'), help: t('每 6 小时主动探测各账号+模型的剩余额度。当前 Rust 版网关未实现。'), ctl: scanBtn, badges: [unsupportedBadge()], cls: 'is-off' });

  const el = section('data', t('价格与数据'), { sub: t('价格估算、统计数据保留与日志') },
    switchRow(S, { key: 'pricing_enabled', title: t('启用价格估算'), help: t('按 OpenRouter 价格把 token 折算成人民币，用于「OpenRouter 价估算」列和「API 等价花费」卡片。关闭后这些列和卡片都会隐藏；重新打开会补算关闭期间的请求。') }).el,
    interval.el, inherit.el, panel,
    subhead(t('统计数据保留')),
    numRow(S, { key: 'usage.hour_keep_hours', title: t('小时级预聚合保留'), help: t('用量看板的小时预聚合窗口，范围 1 到 336 小时。越大检查点越大、写入越多，宽时间范围查询越快。'), min: 1, max: 336, unit: t('小时'), stored: true }).el,
    numRow(S, { key: 'usage.day_keep_days', title: t('日级保留'), help: t('日级聚合桶保留的天数，范围 1 到 366。'), min: 1, max: 366, unit: t('天'), stored: true }).el,
    subhead(t('日志')),
    switchRow(S, { key: 'log_db_enabled', title: t('日志写入数据库'), help: t('写入 usage/logs.sqlite3；关闭时只保存在内存。') }).el,
    logNote,
    numRow(S, { key: 'log_db_max_rows', title: t('日志行数上限'), help: t('留空为不限。最大 1 亿。'), min: 0, max: 100000000, blank: 'null', placeholder: t('不限'), stored: true }).el,
    numRow(S, { key: 'log_db_max_days', title: t('日志保留天数'), help: t('留空为不限。最大 36500。'), min: 0, max: 36500, blank: 'null', placeholder: t('不限'), unit: t('天'), stored: true }).el,
    numRow(S, {
      key: 'log_db_max_bytes', title: t('日志体积上限（MB）'), help: t('留空为不限。最大 102400（100 GB）。'), min: 0, max: 102400, blank: 'null', placeholder: t('不限'), unit: 'MB', stored: true,
      show: (v) => (v == null ? '' : String(Math.max(1, Math.round(v / MB)))), patch: (v) => ({ log_db_max_bytes: v ? v * MB : null }),
    }).el,
    scan.el);
  el.reload = () => panel.reload();
  return el;
}
