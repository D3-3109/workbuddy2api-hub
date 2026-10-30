// Gateway: region cards, today's numbers, scheduler, connection info, recent requests, models.
import { h, setText, scope, toggleClass } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { get, post, qs } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { ctx, on, setView, setActiveRealm, loadSettings, realmLabel } from '../lib/ctx.js';
import { poller } from '../lib/poll.js';
import { sparkline } from '../lib/chart.js';
import { kpi, btn, badge, card, pageHead, toast, ok, fail, errText, confirmDialog, copyText, codeBlock, seg } from '../lib/ui.js';
import { costText } from '../lib/shared.js';
import { recentCard } from './gateway-recent.js';
import { modelsCard } from './gateway-models.js';

add({
  '刷新': 'Refresh', '网关': 'Gateway', '管理账号池出口、查看今日调用量、成功率、最近请求与网关提供的模型。': 'Watch today’s traffic, success rate, recent requests and the models the gateway serves.',
  '国际版': 'Global', '国内版': 'China', '默认出口': 'Default exit', '查看中': 'Viewing', '{n} 个 Key': '{n} keys', '可用积分': 'Available credits',
  '/ {t} 总积分 · {a}/{b} 账号': '/ {t} total · {a}/{b} accounts', '网关默认出口': 'Default exit', '{n} 个 Key 生效': '{n} keys apply',
  '切换默认出口为【{r}】': 'Switch default exit to {r}', '切换默认出口': 'Switch default exit',
  '没有携带地区的请求会使用默认出口，该设置会保存到网关。': 'Requests without a region use the default exit. This is saved on the gateway.',
  '网关默认上游已切换并永久保存为：{r}': 'Default upstream switched and saved: {r}',
  '今日调用量': 'Requests today', '今日积分消耗': 'Credits today', '请求成功率': 'Success rate', '今日 Token': 'Tokens today',
  '失败 {n}': '{n} failed', '估算价值 {v}': 'Est. value {v}', '样本 {n}': 'Sample {n}', '输入 {a} · 输出 {b}': 'In {a} · Out {b}', '缓存 {p}': 'Cached {p}',
  '后台定时调度器': 'Scheduler', '运行中': 'Running', '已暂停': 'Paused', '上次': 'Last', '下次': 'Next', '尚未运行': 'Not run yet', '待调度': 'Pending',
  '立即巡检保活': 'Run now', '暂停调度': 'Pause', '启用调度': 'Resume', '后台调度器已启用': 'Scheduler resumed', '后台调度器已暂停': 'Scheduler paused',
  '已触发后台调度执行': 'Run triggered', '已有巡检正在执行，请稍候再试': 'A run is already in progress',
  '接入信息': 'Connect', 'Base URL': 'Base URL', '需要 API Key': 'API key required', '无需 API Key': 'No API key needed',
  '把 YOUR_API_KEY 换成「设置」里创建的 Key。': 'Replace YOUR_API_KEY with a key created in Settings.',
  '复制 Base URL': 'Copy base URL', '请求示例': 'Examples', '模型已加载': 'Models loaded',
  '账号加载失败：{e}': 'Failed to load accounts: {e}', '(国内版)': '(China)', '（国内版）': '(China)', '（国际版）': '(Global)',
  '还没有请求记录': 'No requests yet', '网关处理的请求会实时出现在这里。': 'Requests handled by the gateway show up here live.',
  '最近请求': 'Recent requests', '显示条数': 'Rows', '时间': 'Time', '模型': 'Model', '账号@@col': 'Account', 'Key': 'Key', '模式': 'Mode', '结果': 'Result',
  '耗时': 'Duration', '首字': 'TTFT', '速度': 'Speed', '输入': 'Input', '输出': 'Output', '缓存': 'Cache', '积分': 'Credits', '估算价格': 'Est. cost',
  '流式': 'Stream', '非流式': 'Non-stream', '完成': 'Done', '客户端取消': 'Client abort', '上游中断': 'Upstream abort', '失败': 'Failed',
  '第 {p} / {n} 页 · 共 {c} 条记录': 'Page {p} / {n} · {c} records', '上一页': 'Prev', '下一页': 'Next',
  '按 OpenRouter 公布的模型价折算的等价 token 花费': 'Equivalent token cost at OpenRouter list prices', '定价依据：出厂快照': 'Pricing: bundled snapshot',
  '定价策略：{s}': 'Pricing policy: {s}', '匹配：直接匹配 {id}': 'Match: direct {id}', '匹配：人工映射 {a} → {b}': 'Match: manual {a} → {b}', '匹配：变体继承 {a} → {b}': 'Match: variant {a} → {b}',
  '单价 (每百万)：缓存命中 {a} · 未命中 {b} · 输出 {c}': 'Per 1M: cache hit {a} · miss {b} · output {c}',
  '网关模型清单': 'Gateway models', '消费倍率': 'Cost ×', '能力': 'Abilities', '上下文': 'Context', '最大输出': 'Max output', '思考档位': 'Reasoning',
  '视觉': 'Vision', '工具': 'Tools', '推理': 'Reasoning', '始终思考': 'Always on', '没有匹配的模型': 'No matching models', '搜索模型': 'Search models',
  '刷新模型清单': 'Refresh models', '模型清单已更新': 'Model list updated', '同一版本 30 秒内只能刷新一次': 'Refresh is limited to once per 30 s per realm',
  '复制模型 ID': 'Copy model ID', '{n} 个': '{n}', '内置清单（尚未从上游拉取），模型与倍率可能与实际不一致。': 'Bundled list (not fetched from upstream yet); models and multipliers may differ.',
  '来自上游 API · 更新于 {t}': 'From the upstream API · updated {t}',
});

export const usesRealm = true;

const HOST = { intl: 'www.workbuddy.ai', cn: 'copilot.tencent.com' };

function accountActive(a) { return a.enabled && !a.reserveBlocked && !a.dailyLimitBlocked && !a.creditLimitReached; }

function regionStats(accounts, realm) {
  const rows = accounts.filter((a) => (a.realm || 'intl') === realm);
  const active = rows.filter(accountActive);
  const remain = active.map((a) => a.credits && a.credits.remain).filter((v) => typeof v === 'number');
  const total = rows.map((a) => a.credits && a.credits.total).filter((v) => typeof v === 'number');
  return {
    active: active.length, count: rows.length,
    remain: remain.length ? remain.reduce((x, y) => x + y, 0) : null,
    total: total.length ? total.reduce((x, y) => x + y, 0) : null,
  };
}

function keyCount(keys, realm, includeUnbound) {
  return (keys || []).filter((k) => k.enabled !== false && !k.deleted_at && (k.realm === realm || (includeUnbound && !k.realm))).length;
}

export async function mount(root, app) {
  const sc = scope();
  const names = new Map();
  let accounts = [];
  let keys = [];
  let pricingOn = true;
  const view = () => app.view;

  /* ---- region cards ---- */
  const cards = {};
  function regionCard(realm) {
    const big = h('div.kpi-value'), sub = h('div.kpi-foot'), badges = h('div.row.gap-1.wrap');
    const el = h('button.card.kpi.region', { type: 'button', style: 'text-align:left;cursor:pointer', onClick: () => setView(realm) },
      h('div.row.between', h('div.kpi-label', icon('globe', 15, 'ic'), realm === 'intl' ? t('国际版') : t('国内版')), h('span.faint.mono', { style: 'font-size:11.5px' }, HOST[realm])),
      h('div', { style: 'margin-top:4px' }, h('div.faint', { style: 'font-size:12px' }, t('可用积分')), big),
      sub, badges);
    return { el, big, sub, badges, realm };
  }
  cards.intl = regionCard('intl');
  cards.cn = regionCard('cn');

  const exitName = h('b'), exitBadge = h('span');
  const exitBtn = btn(t('切换默认出口'), { size: 'sm', icon: 'refresh', block: true, onClick: async () => {
    const next = app.realm === 'cn' ? 'intl' : 'cn';
    if (!(await confirmDialog({ title: t('切换默认出口'), message: t('没有携带地区的请求会使用默认出口，该设置会保存到网关。') + '\n\n' + realmLabelT(app.realm) + ' → ' + realmLabelT(next) }))) return;
    await setActiveRealm(next);
    setView(next);
    ok(t('网关默认上游已切换并永久保存为：{r}', { r: realmLabelT(next) }));
  } });
  const exitCard = h('div.card.kpi', { style: 'justify-content:space-between' },
    h('div.row.between', h('div.kpi-label', icon('power', 15, 'ic'), t('网关默认出口')), exitBadge),
    h('div.row', { style: 'font-size:20px;font-weight:700;margin:4px 0' }, h('span.pill-dot'), exitName),
    exitBtn);
  const realmLabelT = (r) => (r === 'cn' ? t('国内版') : t('国际版'));

  function paintRegions() {
    for (const realm of ['intl', 'cn']) {
      const c = cards[realm], s = regionStats(accounts, realm);
      setText(c.big, s.remain == null ? '—' : F.compact(s.remain));
      setText(c.sub, t('/ {t} 总积分 · {a}/{b} 账号', { t: s.total == null ? '—' : F.compact(s.total), a: s.active, b: s.count }));
      toggleClass(c.el, 'selected', view() === realm);
      c.el.style.borderColor = view() === realm ? 'color-mix(in srgb, var(--accent) 55%, var(--line))' : '';
      c.el.style.boxShadow = view() === realm ? '0 0 0 3px var(--accent-soft)' : '';
      c.badges.replaceChildren(...[
        app.realm === realm ? badge(t('默认出口'), 'good', { dot: true }) : null,
        view() === realm ? badge(t('查看中'), 'accent') : null,
        badge(t('{n} 个 Key', { n: keyCount(keys, realm, app.realm === realm) }), '', { outline: true }),
      ].filter(Boolean));
    }
    setText(exitName, realmLabelT(app.realm));
    exitCard.querySelector('.pill-dot').style.background = app.realm === 'cn' ? 'var(--bad)' : 'var(--accent)';
    exitBadge.replaceChildren(badge(t('{n} 个 Key 生效', { n: keyCount(keys, app.realm, true) }), '', { outline: true }));
    exitBtn.lastChild.textContent = t('切换默认出口为【{r}】', { r: realmLabelT(app.realm === 'cn' ? 'intl' : 'cn') });
  }

  /* ---- KPIs ---- */
  const kReq = kpi({ label: t('今日调用量'), icon: 'activity' });
  const kCredit = kpi({ label: t('今日积分消耗'), icon: 'coins' });
  const kRate = kpi({ label: t('请求成功率'), icon: 'shield' });
  const kTok = kpi({ label: t('今日 Token'), icon: 'layers' });

  async function loadUsage(signal) {
    const v = view();
    const [u, ts] = await Promise.all([
      get('/usage' + qs({ realm: v, range: 'today' }), { signal }),
      get('/usage/timeseries' + qs({ realm: v, range: 'today', bucket_seconds: 3600 }), { signal }),
    ]);
    const series = ts.series || [];
    kReq.set({ value: F.int(u.requests), unit: '', foot: u.errors ? t('失败 {n}', { n: F.int(u.errors) }) : ' ' });
    kCredit.set({ value: F.fixed(u.credit || 0, 2), foot: pricingOn && u.cost_cny != null ? t('估算价值 {v}', { v: costText(u.cost_cny) }) : ' ' });
    // `requests` counts completed calls only; failures are in `errors` (client aborts are in neither).
    const settled = (u.requests || 0) + (u.errors || 0);
    kRate.set({ value: settled ? F.pct(((u.requests || 0) / settled) * 100, 1) : '—', foot: t('样本 {n}', { n: F.int(settled) }) });
    kRate.el.querySelector('.kpi-value').style.color = u.errors ? 'var(--warn)' : 'var(--good)';
    const cacheRatio = u.prompt_tokens > 0 ? F.pct((u.cached_tokens / u.prompt_tokens) * 100, 0) : '—';
    kTok.set({ value: F.compact(u.total_tokens || 0), foot: t('输入 {a} · 输出 {b}', { a: F.compact(u.prompt_tokens || 0), b: F.compact(u.completion_tokens || 0) }) + ' · ' + t('缓存 {p}', { p: cacheRatio }) });
    const paint = (k, key, color) => { const vals = series.map((b) => b[key] || 0); k.spark.replaceChildren(vals.length > 1 ? sparkline(vals, { color }) : ''); };
    paint(kReq, 'requests', 'var(--c2)'); paint(kCredit, 'credit', 'var(--warn)'); paint(kTok, 'total_tokens', 'var(--c5)');
  }

  /* ---- scheduler ---- */
  const schedDot = h('span.pill-dot'), schedText = h('b'), schedTimes = h('div.muted', { style: 'font-size:12.5px' }), schedMode = h('div.faint', { style: 'font-size:12.5px;line-height:1.5' });
  let schedEnabled = true;
  const runBtn = btn(t('立即巡检保活'), { size: 'sm', icon: 'play', onClick: async () => {
    const r = await post('/scheduler/trigger', {});
    toast(r.msg || t('已触发后台调度执行'), r.ok === false ? 'warn' : 'ok');
    setTimeout(() => sched.trigger(), 2000);
  } });
  const toggleBtn = btn(t('暂停调度'), { size: 'sm', icon: 'pause', onClick: async () => {
    const r = await post('/scheduler/toggle', {});
    ok(r.enabled ? t('后台调度器已启用') : t('后台调度器已暂停'));
    await sched.trigger();
  } });
  async function loadSched(signal) {
    const s = await get('/scheduler', { signal });
    schedEnabled = !!s.enabled;
    schedDot.className = 'pill-dot ' + (s.enabled ? 'good' : '');
    setText(schedText, s.enabled ? t('运行中') : t('已暂停'));
    setText(schedMode, (view() === 'cn' ? s.mode_cn || s.mode : s.mode_intl || s.mode) || '');
    setText(schedTimes, `${t('上次')}：${s.last_run_time || '—'} · ${t('下次')}：${s.next_run_time || '—'}`);
    toggleBtn.lastChild.textContent = s.enabled ? t('暂停调度') : t('启用调度');
    runBtn.disabled = !!s.running;
  }
  const schedCard = card(t('后台定时调度器'), { actions: [runBtn, toggleBtn] },
    h('div.col', { style: 'gap:8px' }, h('div.row', schedDot, schedText), schedTimes, schedMode));

  /* ---- connection ---- */
  const base = location.origin;
  let sampleModel = 'MODEL_ID';
  const snippets = {
    curl: () => `curl ${base}/v1/chat/completions \\\n  -H "Authorization: Bearer YOUR_API_KEY" \\\n  -H "Content-Type: application/json" \\\n  -d '{"model":"${sampleModel}","messages":[{"role":"user","content":"Hello"}]}'`,
    python: () => `from openai import OpenAI\n\nclient = OpenAI(base_url="${base}/v1", api_key="YOUR_API_KEY")\nresp = client.chat.completions.create(\n    model="${sampleModel}",\n    messages=[{"role": "user", "content": "Hello"}],\n)\nprint(resp.choices[0].message.content)`,
    anthropic: () => `import anthropic\n\nclient = anthropic.Anthropic(base_url="${base}", api_key="YOUR_API_KEY")\nmsg = client.messages.create(\n    model="${sampleModel}",\n    max_tokens=1024,\n    messages=[{"role": "user", "content": "Hello"}],\n)\nprint(msg.content[0].text)`,
  };
  let snippet = 'curl';
  const code = codeBlock(snippets.curl());
  const snipSeg = seg([{ value: 'curl', label: 'curl' }, { value: 'python', label: 'OpenAI' }, { value: 'anthropic', label: 'Anthropic' }], snippet, (v) => { snippet = v; code.setText(snippets[v]()); }, { size: 'sm' });
  const keyNote = h('span');
  const connCard = card(t('接入信息'), { actions: [snipSeg] },
    h('div.col', { style: 'gap:12px' },
      h('div.row.wrap', { style: 'gap:10px' },
        h('span.muted', { style: 'font-size:12.5px' }, t('Base URL')),
        h('code', { style: 'background:var(--surface-2);padding:4px 9px;border-radius:7px;border:1px solid var(--line)' }, base + '/v1'),
        btn('', { iconOnly: true, icon: 'copy', size: 'sm', variant: 'ghost', tip: t('复制 Base URL'), onClick: () => copyText(base + '/v1') }),
        keyNote),
      code,
      h('div.faint', { style: 'font-size:12.5px' }, t('把 YOUR_API_KEY 换成「设置」里创建的 Key。'))));

  /* ---- recent + models ---- */
  const recent = recentCard({ view, names });
  const models = modelsCard({ view });

  /* ---- layout ---- */
  const refreshAll = () => { accountsPoll.trigger(); usagePoll.trigger(); sched.trigger(); recent.reload(); models.load().catch(() => {}); };
  root.append(
    pageHead(t('网关'), t('管理账号池出口、查看今日调用量、成功率、最近请求与网关提供的模型。'), btn('', { iconOnly: true, icon: 'refresh', variant: '', tip: t('刷新'), onClick: refreshAll })),
    h('div.stack',
      h('div.grid.cols-3', cards.intl.el, cards.cn.el, exitCard),
      h('div.grid.cols-4', kReq.el, kCredit.el, kRate.el, kTok.el),
      h('div.grid.side', connCard, schedCard),
      recent.el,
      h('div.defer', models.el)));

  /* ---- data ---- */
  const accountsPoll = poller(async (signal) => {
    const [a, s] = await Promise.all([get('/accounts' + qs({ realm: 'all' }), { signal }), loadSettings().catch(() => null)]);
    accounts = a.accounts || [];
    for (const x of accounts) names.set(x.uid, x.nickname || x.uid.slice(0, 8));
    if (s) { keys = s.api_keys || []; pricingOn = s.pricing_enabled !== false; }
    paintRegions();
  }, 15000);
  const usagePoll = poller(loadUsage, 30000);
  const sched = poller(loadSched, 30000);

  const health = () => {
    const required = app.health && app.health.api_key_required;
    keyNote.replaceChildren(badge(required ? t('需要 API Key') : t('无需 API Key'), required ? 'warn' : 'good', { dot: true }));
  };
  health();
  sc.add(on('health', health));

  accountsPoll.start();
  usagePoll.start();
  sched.start();
  recent.start();
  models.load().then(() => {
    // first model of the viewed realm makes the examples copy-paste ready
    get('/v1/models' + qs({ realm: view() })).then((r) => {
      if (r.data && r.data[0]) { sampleModel = r.data[0].id; code.setText(snippets[snippet]()); }
    }).catch(() => {});
  }).catch((e) => toast(errText(e), 'bad'));

  return () => { accountsPoll.stop(); usagePoll.stop(); sched.stop(); recent.stop(); sc.dispose(); };
}
