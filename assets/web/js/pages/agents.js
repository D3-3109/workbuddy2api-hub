// Agents: "connect your client" page. The Rust gateway neither detects local clients nor
// rewrites their config files, so this page builds the connection details instead:
// base URLs, a key, a default model and ready-to-copy snippets for common clients.
// The key secret is never fetched until the user reveals or copies it.
import { h, setText, setAttr, scope, debounce, reconcile } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { get, qs } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { on, go, loadSettings, loadHealth } from '../lib/ctx.js';
import { btn, badge, card, field, pageHead, alertBox, empty, skeleton, copyText, errText } from '../lib/ui.js';
import { modelCombo } from './agents-combo.js';
import { TABS, blockText } from './agents-snippets.js';

add({
  '智能体': 'Agents', '刷新': 'Refresh', '复制': 'Copy', '前往设置': 'Open Settings', '重试': 'Retry',
  '把 Cursor、Claude Code、Cline 和各类 SDK 指向本网关：选好地址、Key 与模型，复制对应的配置即可。': 'Point Cursor, Claude Code, Cline and SDKs at this gateway: pick the address, key and model, then copy the matching config.',
  '此 Rust 版网关不检测本机客户端，也不改写它们的配置文件。请按下方的地址与示例手动填写。': 'The Rust gateway does not detect local clients or edit their config files. Enter the addresses and examples below by hand.',
  '连接信息': 'Connection', '配置示例': 'Config examples', '可用模型': 'Available models',
  'OpenAI 兼容 Base URL': 'OpenAI-compatible Base URL', '含 /v1，适用于 OpenAI SDK、Cursor、Cline、Continue 等。': 'Includes /v1. Used by the OpenAI SDK, Cursor, Cline, Continue and similar.',
  '地址需以 http:// 或 https:// 开头': 'The address must start with http:// or https://',
  '恢复默认': 'Reset', 'Anthropic Base URL': 'Anthropic Base URL', '不含 /v1，随上面的地址自动生成，适用于 Anthropic SDK 与 Claude Code。': 'No /v1. Derived from the address above; used by the Anthropic SDK and Claude Code.',
  'Anthropic Messages 未启用': 'Anthropic Messages is off', '默认关闭。启用前 /v1/messages 会返回 400。': 'Off by default. Until enabled, /v1/messages answers 400.',
  'API Key': 'API Key', '全局 API Key': 'Global API key', '已禁用': 'Disabled',
  '国际版': 'Global', '国内版': 'China',
  '显示密钥': 'Reveal key', '隐藏密钥': 'Hide key', '复制密钥': 'Copy key', '已复制密钥': 'Key copied', '已复制': 'Copied',
  '未启用鉴权，Key 可留空或填任意值。': 'Authentication is off. Leave the key empty or use any value.',
  '面板里还没有创建 Key。把示例里的 <API_KEY> 换成启动网关时使用的 Key，或到「设置」里创建。': 'No keys exist in the panel yet. Replace <API_KEY> with the key the gateway was started with, or create one in Settings.',
  '选择一个面板里的 Key，示例会自动套用；也可保留占位符自行替换。': 'Pick a key from the panel to use in the examples, or keep the placeholder and replace it yourself.',
  '示例中的密钥已遮盖。复制时才会读取完整密钥，不会写入页面。': 'The key is masked in the examples. The full key is read only when you copy it and is not written to the page.',
  '密钥已显示，离开页面或切换 Key 后会清除。': 'The key is shown. It is cleared when you leave the page or pick another key.',
  '未能读取密钥': 'Could not read the key', '该 Key 已不存在，请刷新页面': 'That key no longer exists. Refresh the page.',
  '默认模型': 'Default model', '留空时示例里用 MODEL_ID 占位；也可以直接输入别名。': 'When empty, examples use MODEL_ID. You can also type an alias.',
  '环境变量': 'Env vars', '编辑器': 'Editors',
  '适用于任何 HTTP 客户端，OpenAI 与 Anthropic 两种接口都能用。': 'Works with any HTTP client. Both the OpenAI and the Anthropic interface are available.',
  'OpenAI 官方 SDK，以及兼容 OpenAI 协议的库（LangChain、LlamaIndex 等）。': 'The official OpenAI SDK and libraries that speak the OpenAI protocol (LangChain, LlamaIndex, ...).',
  'Anthropic 官方 SDK。Base URL 不含 /v1，SDK 会自己补上。': 'The official Anthropic SDK. The base URL has no /v1; the SDK adds it.',
  'Claude Code 从环境变量或 ~/.claude/settings.json 读取这些值。': 'Claude Code reads these values from environment variables or ~/.claude/settings.json.',
  '在 Cursor、Cline、Roo Code 的 “OpenAI Compatible” 设置里填写下面的值；Continue 使用 config.yaml。': 'Enter these values in the “OpenAI Compatible” settings of Cursor, Cline and Roo Code. Continue uses config.yaml.',
  '多数兼容 OpenAI / Anthropic 的工具会读取这些环境变量。': 'Most OpenAI / Anthropic compatible tools read these environment variables.',
  '网关当前未启用 Anthropic Messages 接口，这里的示例在启用前会返回 400。请先在「设置」中开启。': 'Anthropic Messages is not enabled on the gateway, so these examples answer 400 until it is. Turn it on in Settings first.',
  '需要 API Key': 'API key required', '无需 API Key': 'No API key needed',
  '智能体列表加载失败': 'Failed to load agent details',
  '搜索模型': 'Search models', '模型 ID': 'Model ID', '上下文': 'Context', '设为默认': 'Use', '默认': 'Default', '复制模型 ID': 'Copy model ID',
  '{n} 个': '{n}', '仅显示前 {n} 项，请用搜索缩小范围。': 'Showing the first {n}. Use search to narrow down.',
  '没有匹配的模型': 'No matching models', '当前出口还没有模型': 'No models for the current exit yet', '请先在「网关」页确认账号状态与模型清单。': 'Check the accounts and the model list on the Gateway page first.',
});

export const usesRealm = false;

const MASK = '••••••••••••';
const MODEL_ROWS_MAX = 150;
const defaultBase = () => location.origin + '/v1';
const realmText = (r) => (r === 'cn' ? t('国内版') : r === 'intl' ? t('国际版') : String(r));

export async function mount(root, app) {
  const sc = scope();
  const st = {
    base: defaultBase(),
    keyId: '', revealed: false, secret: '',   // the secret lives here only while revealed
    model: '', tab: 'curl', query: '',
    keys: [], models: [],
    authRequired: app.health ? !!app.health.api_key_required : null,
    anthropicOn: null, supported: false, loaded: false,
  };

  const normBase = () => st.base.trim().replace(/\/+$/, '') || defaultBase();
  const anthBase = () => normBase().replace(/\/v1$/i, '');
  const placeholder = () => (st.authRequired === false ? 'any-value' : '<API_KEY>');
  const previewKey = () => (!st.keyId ? placeholder() : st.revealed && st.secret ? st.secret : MASK);
  const values = (key) => ({ base: normBase(), anth: anthBase(), key, model: st.model });
  const currentTab = () => TABS.find((x) => x.id === st.tab) || TABS[0];

  async function fetchSecret(id) {
    try {
      const r = await get('/settings/reveal' + qs({ id }));
      if (!r || typeof r.key !== 'string' || !r.key) throw new Error(t('未能读取密钥'));
      return r.key;
    } catch (e) {
      if (e && e.status === 404) throw new Error(t('该 Key 已不存在，请刷新页面'));
      throw e;
    }
  }
  const realKey = async () => (st.keyId ? st.secret || fetchSecret(st.keyId) : placeholder());

  /* ---------------- connection card ---------------- */
  const baseInput = h('input.input.mono', { type: 'text', value: st.base, spellcheck: 'false', autocomplete: 'off', 'aria-label': t('OpenAI 兼容 Base URL') });
  const baseReset = btn(t('恢复默认'), { size: 'xs', variant: 'ghost', icon: 'undo', onClick: () => { st.base = defaultBase(); baseInput.value = st.base; paintBase(); paintSnippets(); } });
  baseReset.hidden = true;
  const baseErr = h('div.err', { hidden: true }, t('地址需以 http:// 或 https:// 开头'));
  baseInput.addEventListener('input', () => { st.base = baseInput.value; paintBase(); paintSnippets(); });

  const anthInput = h('input.input.mono', { type: 'text', readonly: true, spellcheck: 'false', autocomplete: 'off', 'aria-label': t('Anthropic Base URL') });
  const anthWarn = h('div.row.wrap.gap-1', { hidden: true },
    badge(t('Anthropic Messages 未启用'), 'warn', { dot: true, tip: t('默认关闭。启用前 /v1/messages 会返回 400。') }),
    btn(t('前往设置'), { size: 'xs', variant: 'ghost', onClick: () => go('settings') }));

  const keySelect = h('select.select', { 'aria-label': t('API Key') });
  keySelect.addEventListener('change', () => { st.keyId = keySelect.value; st.revealed = false; st.secret = ''; paintKey(); paintSnippets(); });
  const secretInput = h('input.input.mono', { type: 'text', readonly: true, spellcheck: 'false', autocomplete: 'off', 'aria-label': t('API Key') });
  const revealBtn = btn(t('显示密钥'), { iconOnly: true, icon: 'eye', onClick: () => toggleReveal() });
  const copyKeyBtn = btn(t('复制密钥'), { iconOnly: true, icon: 'copy', onClick: async () => { await copyText(await realKey(), t('已复制密钥')); } });
  const secretRow = h('div.ag-secret', secretInput, revealBtn, copyKeyBtn);
  const keyHintText = h('span');
  const keyHintBtn = btn(t('前往设置'), { size: 'xs', variant: 'ghost', onClick: () => go('settings') });
  const keyHint = h('div.hint.ag-keyhint', keyHintText, keyHintBtn);

  const combo = modelCombo({ onChange: (id) => { st.model = id; paintSnippets(); paintModels(); } });

  const authBadge = h('span');
  const connCard = card(t('连接信息'), { actions: [authBadge] },
    h('div.col', { style: 'gap:16px' },
      field(t('OpenAI 兼容 Base URL'), h('div.input-affix', baseInput, btn(t('复制'), { iconOnly: true, icon: 'copy', onClick: () => copyText(normBase()) })), t('含 /v1，适用于 OpenAI SDK、Cursor、Cline、Continue 等。'), h('div.col', baseErr, baseReset)),
      field(t('Anthropic Base URL'), h('div.input-affix', anthInput, btn(t('复制'), { iconOnly: true, icon: 'copy', onClick: () => copyText(anthBase()) })), t('不含 /v1，随上面的地址自动生成，适用于 Anthropic SDK 与 Claude Code。'), anthWarn),
      field(t('API Key'), h('div.col', keySelect, secretRow), null, keyHint),
      field(t('默认模型'), combo.el, t('留空时示例里用 MODEL_ID 占位；也可以直接输入别名。'))));

  /* ---------------- snippets card ---------------- */
  const TAB_LABEL = { editors: () => t('编辑器'), env: () => t('环境变量') };
  const tabBar = h('div.tabs.ag-tabs', { role: 'tablist' });
  const tabBtns = new Map();
  for (const tb of TABS) {
    const b = h('button', { type: 'button', role: 'tab', id: 'agtab-' + tb.id, onClick: () => setTab(tb.id) }, tb.label || TAB_LABEL[tb.id]());
    tabBtns.set(tb.id, b);
    tabBar.appendChild(b);
  }
  tabBar.addEventListener('keydown', (e) => {
    if (e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') return;
    const i = TABS.findIndex((x) => x.id === st.tab);
    const next = TABS[(i + (e.key === 'ArrowRight' ? 1 : TABS.length - 1)) % TABS.length];
    setTab(next.id);
    tabBtns.get(next.id).focus();
    e.preventDefault();
  });
  const tabInfo = h('div.ag-tab-info');
  const blocksBox = h('div.ag-blocks', { role: 'tabpanel' });
  const snipCard = card(t('配置示例'), { cls: 'ag-snip' }, tabBar, tabInfo, blocksBox);

  const TAB_HINT = {
    curl: () => t('适用于任何 HTTP 客户端，OpenAI 与 Anthropic 两种接口都能用。'),
    openai: () => t('OpenAI 官方 SDK，以及兼容 OpenAI 协议的库（LangChain、LlamaIndex 等）。'),
    anthropic: () => t('Anthropic 官方 SDK。Base URL 不含 /v1，SDK 会自己补上。'),
    claude: () => t('Claude Code 从环境变量或 ~/.claude/settings.json 读取这些值。'),
    editors: () => t('在 Cursor、Cline、Roo Code 的 “OpenAI Compatible” 设置里填写下面的值；Continue 使用 config.yaml。'),
    env: () => t('多数兼容 OpenAI / Anthropic 的工具会读取这些环境变量。'),
  };

  let boxes = [];
  function makeBox(b, i) {
    const copy = btn(t('复制'), { size: 'xs', icon: 'copy', variant: 'ghost', onClick: () => copyBlock(i) });
    const head = h('div.ag-block-head', h('span.ag-block-title', b.title), copy);
    if (b.kind === 'kv') {
      const rows = b.rows.map((r, j) => {
        const val = h('span.ag-kv-v.mono');
        const c = btn(t('复制'), { iconOnly: true, icon: 'copy', size: 'xs', variant: 'ghost', onClick: () => copyRow(i, j) });
        return { el: h('div.ag-kv-row', h('span.ag-kv-k', r.k), val, c), val };
      });
      return { el: h('div.ag-block', head, h('div.ag-kv', rows.map((r) => r.el))), set: (nb) => nb.rows.forEach((r, j) => setText(rows[j].val, r.v)) };
    }
    const codeEl = h('code');
    return { el: h('div.ag-block', head, h('div.codeblock', h('pre', codeEl))), set: (nb) => setText(codeEl, nb.text) };
  }

  async function copyBlock(i) {
    const key = await realKey();
    await copyText(blockText(currentTab().blocks(values(key))[i]), t('已复制'));
  }

  async function copyRow(i, j) {
    const secret = !!currentTab().blocks(values(previewKey()))[i].rows[j].secret;
    const row = currentTab().blocks(values(secret ? await realKey() : previewKey()))[i].rows[j];
    await copyText(row.v, t('已复制'));
  }

  function paintSnippets() {
    if (!st.loaded) return;
    const tab = currentTab();
    const blocks = tab.blocks(values(previewKey()));
    const shape = tab.id + '|' + blocks.map((b) => b.kind + b.title + (b.rows ? b.rows.length : '')).join(',');
    if (blocksBox.__shape !== shape) {
      blocksBox.__shape = shape;
      boxes = blocks.map(makeBox);
      blocksBox.replaceChildren(...boxes.map((x) => x.el));
    }
    blocks.forEach((b, i) => boxes[i].set(b));
  }

  function paintTabs() {
    for (const [id, b] of tabBtns) {
      const on = id === st.tab;
      setAttr(b, 'aria-selected', String(on));
      setAttr(b, 'tabindex', on ? '0' : '-1');
    }
    blocksBox.setAttribute('aria-labelledby', 'agtab-' + st.tab);
    const tab = currentTab();
    tabInfo.replaceChildren(...[
      h('p.ag-hint', TAB_HINT[tab.id]()),
      tab.needsAnthropic && st.anthropicOn === false
        ? alertBox('warn', t('网关当前未启用 Anthropic Messages 接口，这里的示例在启用前会返回 400。请先在「设置」中开启。'), ' ', btn(t('前往设置'), { size: 'xs', onClick: () => go('settings') }))
        : null].filter(Boolean));
  }

  function setTab(id) {
    if (st.tab === id) return;
    st.tab = id;
    paintTabs();
    paintSnippets();
    tabBtns.get(id).scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }

  /* ---------------- models card ---------------- */
  const modelSearch = h('input.input.sm', {
    type: 'text', placeholder: t('搜索模型'), 'aria-label': t('搜索模型'), style: 'width:190px', autocomplete: 'off',
    onInput: debounce((e) => { st.query = e.target.value.trim().toLowerCase(); paintModels(); }, 120),
  });
  const modelCount = h('span.sub');
  const tbody = h('tbody');
  const modelTable = h('table.table.compact', h('thead', h('tr', h('th', t('模型 ID')), h('th.num', t('上下文')), h('th.num', h('span.sr-only', t('复制'))))), tbody);
  const modelNote = h('div.card-foot', { hidden: true });
  const modelEmpty = h('div', { hidden: true });
  const modelsCard = h('section.card.ag-models',
    h('div.card-head', h('h2', t('可用模型')), modelCount, h('div.actions', modelSearch)),
    h('div.table-wrap', modelTable), modelEmpty, modelNote);

  function modelRow(m) {
    const use = btn(t('设为默认'), { size: 'xs', onClick: () => { st.model = m.id; combo.setValue(m.id); paintSnippets(); paintModels(); } });
    const copy = btn(t('复制模型 ID'), { iconOnly: true, icon: 'copy', size: 'xs', variant: 'ghost', onClick: () => copyText(m.id) });
    const def = badge(t('默认'), 'accent', { sm: true });
    def.hidden = true;
    const tr = h('tr', h('td', h('div.row.gap-1', h('span.mono.ag-mid', m.id), def)),
      h('td.num', m.context_window ? F.compact(m.context_window) : '—'),
      h('td.num', h('div.row.gap-1', { style: 'justify-content:flex-end' }, use, copy)));
    tr.def = def;
    tr.use = use;
    return tr;
  }

  function paintModels() {
    if (!st.loaded) return;
    const q = st.query;
    const all = q ? st.models.filter((m) => m.id.toLowerCase().includes(q)) : st.models;
    const rows = all.slice(0, MODEL_ROWS_MAX);
    reconcile(tbody, rows, (m) => m.id, modelRow, (tr, m) => {
      const isDef = st.model === m.id;
      if (tr.__sig === isDef) return;
      tr.__sig = isDef;
      tr.classList.toggle('selected', isDef);
      tr.def.hidden = !isDef;
      tr.use.disabled = isDef;
    });
    modelTable.hidden = !rows.length;
    modelEmpty.hidden = !!rows.length;
    if (!rows.length) modelEmpty.replaceChildren(st.models.length
      ? empty('layers', t('没有匹配的模型'))
      : empty('layers', t('当前出口还没有模型'), t('请先在「网关」页确认账号状态与模型清单。')));
    setText(modelCount, t('{n} 个', { n: F.int(st.models.length) }));
    modelNote.hidden = all.length <= MODEL_ROWS_MAX;
    if (!modelNote.hidden) setText(modelNote, t('仅显示前 {n} 项，请用搜索缩小范围。', { n: MODEL_ROWS_MAX }));
  }

  /* ---------------- painting of the connection card ---------------- */
  function paintBase() {
    const raw = st.base.trim();
    baseReset.hidden = raw === defaultBase();
    const bad = !!raw && !/^https?:\/\/\S+$/i.test(raw);
    baseErr.hidden = !bad;
    setAttr(baseInput, 'aria-invalid', bad ? 'true' : null);
    if (anthInput.value !== anthBase()) anthInput.value = anthBase();
  }

  function paintAuth() {
    const req = st.authRequired;
    authBadge.replaceChildren(req == null ? '' : badge(req ? t('需要 API Key') : t('无需 API Key'), req ? 'warn' : 'good', { dot: true }));
    anthWarn.hidden = st.anthropicOn !== false;
  }

  function paintKey() {
    secretRow.hidden = !st.keyId;
    if (keySelect.value !== st.keyId) keySelect.value = st.keyId;
    const shown = st.revealed && !!st.secret;
    secretInput.value = st.keyId ? (shown ? st.secret : MASK) : '';
    revealBtn.replaceChildren(icon(shown ? 'eyeOff' : 'eye', 16));
    revealBtn.setAttribute('aria-label', shown ? t('隐藏密钥') : t('显示密钥'));
    revealBtn.setAttribute('data-tip', shown ? t('隐藏密钥') : t('显示密钥'));
    let text;
    let settings = false;
    if (!st.keys.length && st.authRequired === false) text = t('未启用鉴权，Key 可留空或填任意值。');
    else if (!st.keys.length) { text = t('面板里还没有创建 Key。把示例里的 <API_KEY> 换成启动网关时使用的 Key，或到「设置」里创建。'); settings = true; }
    else if (!st.keyId) text = t('选择一个面板里的 Key，示例会自动套用；也可保留占位符自行替换。');
    else text = shown ? t('密钥已显示，离开页面或切换 Key 后会清除。') : t('示例中的密钥已遮盖。复制时才会读取完整密钥，不会写入页面。');
    setText(keyHintText, text);
    keyHintBtn.hidden = !settings;
    paintAuth();
  }

  function fillKeys() {
    keySelect.replaceChildren(
      h('option', { value: '' }, t('全局 API Key')),
      ...st.keys.map((k) => {
        const label = [k.name || k.id, k.realm ? realmText(k.realm) : '', k.enabled === false ? t('已禁用') : ''].filter(Boolean).join(' · ');
        return h('option', { value: k.id, disabled: k.enabled === false }, label);
      }));
    keySelect.value = st.keyId;
  }

  async function toggleReveal() {
    if (st.revealed) { st.revealed = false; st.secret = ''; paintKey(); paintSnippets(); return; }
    const id = st.keyId;
    const key = await fetchSecret(id);
    if (id !== st.keyId) return;   // another key was picked while waiting
    st.secret = key;
    st.revealed = true;
    paintKey();
    paintSnippets();
  }

  /* ---------------- loading ---------------- */
  const content = h('div.stack', card(null, {}, skeleton(5)));
  const notice = alertBox('info', t('此 Rust 版网关不检测本机客户端，也不改写它们的配置文件。请按下方的地址与示例手动填写。'));
  const refreshBtn = btn(t('刷新'), { iconOnly: true, icon: 'refresh', variant: '', onClick: () => load(false) });
  root.append(h('div.p-agents',
    pageHead(t('智能体'), t('把 Cursor、Claude Code、Cline 和各类 SDK 指向本网关：选好地址、Key 与模型，复制对应的配置即可。'), refreshBtn),
    content));

  async function load(first) {
    const [a, settings, health] = await Promise.all([
      get('/agents'),
      loadSettings(true).catch(() => null),
      first && app.health ? app.health : loadHealth().catch(() => null),
    ]);
    st.supported = a.supported === true;
    st.keys = (a.keys || []).filter((k) => k && k.id);
    st.models = (a.models || []).filter((m) => m && m.id).map((m) => ({ id: String(m.id), context_window: m.context_window || null }));
    st.anthropicOn = settings ? settings.enable_anthropic_messages === true : null;
    if (health) st.authRequired = !!health.api_key_required;
    if (st.keyId && !st.keys.some((k) => k.id === st.keyId)) { st.keyId = ''; st.revealed = false; st.secret = ''; }
    if (first && !st.keyId) { const k = st.keys.find((x) => x.enabled !== false); if (k) st.keyId = k.id; }
    const wasLoaded = st.loaded;
    st.loaded = true;
    fillKeys();
    combo.setModels(st.models);
    combo.setValue(st.model);
    baseInput.value = st.base;
    notice.hidden = st.supported;
    paintTabs();
    paintBase();
    paintKey();
    paintSnippets();
    paintModels();
    if (!wasLoaded) content.replaceChildren(notice, h('div.ag-grid', connCard, snipCard), modelsCard);
  }

  function loadFailed(e) {
    content.replaceChildren(h('section.card', empty('alert', t('智能体列表加载失败'), errText(e), btn(t('重试'), { variant: 'primary', onClick: () => start() }))));
  }

  async function start() {
    try { await load(true); } catch (e) { if (!(e && e.unauthorized)) loadFailed(e); }
  }

  sc.add(on('health', (hl) => {
    if (!hl || !st.loaded) return;
    const req = !!hl.api_key_required;
    if (req === st.authRequired) return;
    st.authRequired = req;
    paintKey();
    paintSnippets();
  }));

  start();
  return () => { st.secret = ''; st.revealed = false; sc.dispose(); };
}
