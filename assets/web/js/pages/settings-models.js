// Settings: model list / API behaviour, global prompt, outbound identity.
import { h, setText } from '../lib/dom.js';
import { t, add } from '../lib/i18n.js';
import { btn, checkbox, field, alertBox, badge, fail } from '../lib/ui.js';
import { section, row, subhead, switchRow, segRow, storedBadge, byteLen, errMsg, dialog } from './settings-kit.js';

add({
  '模型与接口': 'Models and API', '模型清单、协议兼容与全局提示词': 'Model list, protocol compatibility and the global prompt',
  '开放 Anthropic Messages 接口': 'Expose the Anthropic Messages API', '开放 /v1/messages，供 Claude Code 和 Anthropic SDK 使用，内部会转换成 OpenAI chat。注意：cache_control 不会保留，长会话可能多消耗积分。': 'Expose /v1/messages for Claude Code and the Anthropic SDK; requests are converted to OpenAI chat internally. cache_control is not preserved, so long sessions may burn more credits.',
  '推理档位映射': 'Reasoning-effort mapping', '客户端的推理档位不在模型支持范围内时，改写为模型默认档位（模型无法关闭思考时，「关闭」也会被改写）。': 'When the client’s reasoning level is not supported by the model, rewrite it to the model default (also “off” when the model cannot disable thinking).',
  '按 Key 过滤模型清单': 'Filter the model list by key', '/v1/models 只返回当前 Key 可用的模型；未限定模型的 Key 仍能看到全部。': '/v1/models only returns models the calling key may use. Keys without a model restriction still see everything.',
  '只列出可用模型': 'List available models only', '/v1/models 只列出至少有一个账号能服务的模型（国际版 PRO 专属模型需要付费账号）。': '/v1/models only lists models that at least one pool account can serve (Global PRO-only models need a paid account).',
  '免费模型积分兜底': 'Paid fallback for the free model', '所有国际版账号的免费额度用尽（错误码 6004）时，回退到付费的 deepseek-v4.1-flash-sg（x0.03 积分）。会真实消耗积分。': 'When every Global account has used up the free quota (code 6004), fall back to the paid deepseek-v4.1-flash-sg (x0.03 credits). This spends real credits.',
  '本地网页工具': 'Local web tools', '由网关自己执行 web_search / web_fetch 工具调用（DuckDuckGo、Bing、维基百科等轮换），拦截内网地址；每次调用多一轮上游请求。': 'The gateway runs web_search / web_fetch tool calls itself (DuckDuckGo, Bing, Wikipedia and others in rotation), blocks private-network URLs, and costs an extra upstream round per call.',
  '全局系统提示词': 'Global system prompt', '透传': 'Pass through', '追加': 'Append', '替换': 'Replace',
  '透传不改写；追加在客户端系统提示词之前插入；替换会覆盖客户端的 system / developer 消息。内部请求（每日活跃、保活、签到）不受影响。': 'Pass through leaves requests alone; Append inserts before the client system prompt; Replace overrides the client’s system / developer messages. Internal requests (daily activity, keepalive, check-in) are not affected.',
  '提示词内容': 'Prompt text', '追加或替换模式下使用。留空则使用内置默认提示词，最大 64 KB，保存后立即生效。': 'Used in Append and Replace modes. Empty uses the built-in default prompt. Up to 64 KB; takes effect immediately.',
  '留空使用内置默认提示词': 'Empty uses the built-in default prompt', '保存': 'Save', '撤销': 'Revert', '提示词不能超过 65536 字节': 'The prompt cannot exceed 65536 bytes',
  '出站身份': 'Outbound identity', '上游看到的客户端身份': 'The client identity upstream sees',
  '自动切换出站身份': 'Auto-switch identity', '上游对某个账号+模型返回 429 时，换一个身份重试（WB / VSC / CLI），每轮最多 4 次，并保存新身份。': 'On an upstream 429 for an account+model, retry under another identity (WB / VSC / CLI), up to 4 tries per round, and keep the new identity.',
  '可切换的身份': 'Switchable identities', '至少保留一个身份。': 'Keep at least one identity.', '至少选择一个身份': 'Select at least one identity',
  '数据用途声明': 'Data-use declaration', '模型请求里 X-Private-Data 头的值。自动沿用各身份的官方默认；「不用于优化」表示数据不用于模型优化；「可用于优化」表示允许。': 'Value of the X-Private-Data header on model requests. Auto keeps each identity’s official default; “Not for optimisation” keeps data out of model training; “May be used” allows it.',
  '自动': 'Auto', '不用于优化': 'Not for optimisation', '可用于优化': 'May be used',
  '出站身份版本号': 'Identity versions', '覆盖请求里携带的客户端版本号。只有默认版本有对应的请求头和请求体，自定义值可能被上游校验拒绝，甚至导致账号被封。': 'Override the client versions sent upstream. Only the defaults have matching headers and bodies; custom values may be rejected by upstream validation or get accounts banned.',
  '已修改 {n} 项': '{n} overridden', '全部为默认': 'All defaults', '修改版本号': 'Edit versions', '默认 {v}': 'Default {v}', '已修改': 'Modified',
  'WorkBuddy 国际版': 'WorkBuddy Global', 'WorkBuddy 国内版': 'WorkBuddy China', '桌面端内置 CLI': 'Desktop built-in CLI', 'VSCode 扩展': 'VSCode extension', '宿主 VSCode': 'Host VSCode', 'Node 版本': 'Node version',
  '修改出站身份版本号': 'Edit identity versions', '留空即使用默认版本。自定义版本可能导致上游校验失败或账号被封，请谨慎修改。': 'Leave a field empty to use the default. Custom versions may fail upstream validation or get accounts banned; change them with care.',
  '面板密码': 'Panel password', '修改版本号需要再次输入当前面板密码。': 'Changing versions requires your current panel password.', '请输入面板密码': 'Enter the panel password',
  '版本号只能包含字母、数字、点、下划线和连字符，且以字母或数字开头（最长 64 位）': 'Versions may contain letters, digits, dots, underscores and hyphens, must start with a letter or digit, and are at most 64 characters',
  '恢复全部默认': 'Reset all to defaults', '取消': 'Cancel',
});

/* ---------------- models and API ---------------- */
function promptRow(S) {
  const MAX = 65536;
  const ta = h('textarea.textarea', { rows: '5', spellcheck: 'false', 'aria-label': t('提示词内容'), placeholder: t('留空使用内置默认提示词') });
  const counter = h('span.faint.s-count');
  const saveBtn = btn(t('保存'), { size: 'sm', variant: 'primary', icon: 'check', disabled: true, onClick: save });
  let edited = false;
  const revertBtn = btn(t('撤销'), { size: 'sm', variant: 'ghost', disabled: true, onClick: () => { edited = false; ta.value = S.val('prompt.text') || ''; update(); r.error(''); } });
  const r = row({ title: t('提示词内容'), help: t('追加或替换模式下使用。留空则使用内置默认提示词，最大 64 KB，保存后立即生效。'), ctl: h('div.s-area', ta, h('div.row.between', counter, h('div.row.gap-1', revertBtn, saveBtn))), cls: 'stack-ctl' });
  const dirty = () => ta.value !== (S.val('prompt.text') || '');
  function update() {
    const n = byteLen(ta.value);
    setText(counter, `${n} / ${MAX}`);
    counter.classList.toggle('tx-bad', n > MAX);
    saveBtn.disabled = !dirty() || n > MAX;
    revertBtn.disabled = !dirty();
  }
  async function save() {
    if (byteLen(ta.value) > MAX) { r.error(t('提示词不能超过 65536 字节')); return; }
    r.error('');
    try { await S.save({ prompt: { text: ta.value } }); edited = false; } catch (e) { r.error(errMsg(e)); }
    update();
  }
  ta.addEventListener('input', () => { edited = true; r.error(''); update(); });
  ta.addEventListener('keydown', (e) => { if (e.key === 'Enter' && (e.ctrlKey || e.metaKey) && !saveBtn.disabled) { e.preventDefault(); save(); } });
  S.bind(() => { const v = S.val('prompt.text') || ''; if (!edited && ta.value !== v) ta.value = v; update(); });
  return r;
}

export function modelsCard(S) {
  return section('models', t('模型与接口'), { sub: t('模型清单、协议兼容与全局提示词') },
    switchRow(S, { key: 'enable_anthropic_messages', title: t('开放 Anthropic Messages 接口'), help: t('开放 /v1/messages，供 Claude Code 和 Anthropic SDK 使用，内部会转换成 OpenAI chat。注意：cache_control 不会保留，长会话可能多消耗积分。') }).el,
    switchRow(S, { key: 'reasoning_effort_map', title: t('推理档位映射'), help: t('客户端的推理档位不在模型支持范围内时，改写为模型默认档位（模型无法关闭思考时，「关闭」也会被改写）。') }).el,
    switchRow(S, { key: 'filter_models_by_key', title: t('按 Key 过滤模型清单'), help: t('/v1/models 只返回当前 Key 可用的模型；未限定模型的 Key 仍能看到全部。') }).el,
    switchRow(S, { key: 'models_only_available', title: t('只列出可用模型'), help: t('/v1/models 只列出至少有一个账号能服务的模型（国际版 PRO 专属模型需要付费账号）。') }).el,
    switchRow(S, { key: 'free_model_credit_fallback', title: t('免费模型积分兜底'), help: t('所有国际版账号的免费额度用尽（错误码 6004）时，回退到付费的 deepseek-v4.1-flash-sg（x0.03 积分）。会真实消耗积分。') }).el,
    switchRow(S, { key: 'local_web_tools', title: t('本地网页工具'), help: t('由网关自己执行 web_search / web_fetch 工具调用（DuckDuckGo、Bing、维基百科等轮换），拦截内网地址；每次调用多一轮上游请求。') }).el,
    subhead(t('全局系统提示词')),
    segRow(S, {
      key: 'prompt.mode', title: t('全局系统提示词'), help: t('透传不改写；追加在客户端系统提示词之前插入；替换会覆盖客户端的 system / developer 消息。内部请求（每日活跃、保活、签到）不受影响。'),
      options: [{ value: 'passthrough', label: t('透传') }, { value: 'append', label: t('追加') }, { value: 'custom', label: t('替换') }],
    }).el,
    promptRow(S).el);
}

/* ---------------- outbound identity ---------------- */
const VERSION_FIELDS = [
  ['desktop_intl', () => t('WorkBuddy 国际版')], ['desktop_cn', () => t('WorkBuddy 国内版')], ['desktop_cli', () => t('桌面端内置 CLI')],
  ['vscode_product', () => t('VSCode 扩展')], ['vscode_ide', () => t('宿主 VSCode')], ['cli', () => 'CodeBuddy CLI'],
  ['sdk', () => 'OpenAI Node SDK'], ['node', () => t('Node 版本')],
];
const VERSION_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

function versionsDialog(S) {
  const defaults = S.view.identity_version_defaults || {};
  const over = S.view.identity_versions || {};
  const inputs = VERSION_FIELDS.map(([k, label]) => {
    const input = h('input.input.mono', { type: 'text', value: over[k] || '', placeholder: defaults[k] || '', autocomplete: 'off', spellcheck: 'false', 'data-k': k });
    return { k, input, el: field(label(), input) };
  });
  const pw = h('input.input', { type: 'password', autocomplete: 'current-password', placeholder: t('面板密码') });
  const ePw = h('div.err'), eMisc = h('div');
  let m;
  async function submit() {
    setText(ePw, ''); eMisc.replaceChildren();
    const map = {};
    for (const { k, input } of inputs) {
      const v = input.value.trim();
      input.removeAttribute('aria-invalid');
      if (!v) continue;
      if (!VERSION_RE.test(v)) { input.setAttribute('aria-invalid', 'true'); eMisc.replaceChildren(alertBox('bad', t('版本号只能包含字母、数字、点、下划线和连字符，且以字母或数字开头（最长 64 位）'))); input.focus(); return; }
      map[k] = v;
    }
    if (!pw.value) { setText(ePw, t('请输入面板密码')); pw.focus(); return; }
    try {
      await S.save({ identity_versions: map, panel_password: pw.value }, { keepSession: true });
      m.close(true);
    } catch (e) {
      if (e.status === 401) { setText(ePw, errMsg(e)); pw.select(); } else eMisc.replaceChildren(alertBox('bad', errMsg(e)));
    }
  }
  pw.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); submit(); } });
  m = dialog({
    title: t('修改出站身份版本号'), size: 'wide',
    body: [alertBox('warn', t('留空即使用默认版本。自定义版本可能导致上游校验失败或账号被封，请谨慎修改。')), h('div.kd-grid', inputs.map((i) => i.el)), field(t('面板密码'), pw, t('修改版本号需要再次输入当前面板密码。'), ePw), eMisc],
    left: btn(t('恢复全部默认'), { variant: 'ghost', onClick: () => { for (const i of inputs) i.input.value = ''; } }),
    actions: [btn(t('取消'), { onClick: () => m.close(false) }), btn(t('保存'), { variant: 'primary', onClick: submit })],
  });
}

function versionsRow(S) {
  const grid = h('dl.kv.iv-kv');
  const countBadge = h('span');
  const r = row({
    title: t('出站身份版本号'), help: t('覆盖请求里携带的客户端版本号。只有默认版本有对应的请求头和请求体，自定义值可能被上游校验拒绝，甚至导致账号被封。'),
    ctl: btn(t('修改版本号'), { icon: 'edit', onClick: () => { versionsDialog(S); } }),
  });
  r.titleEl.appendChild(countBadge);
  S.bind((v) => {
    const over = v.identity_versions || {}, defs = v.identity_version_defaults || {};
    const n = Object.keys(over).length;
    countBadge.replaceChildren(n ? badge(t('已修改 {n} 项', { n }), 'warn', { sm: true }) : badge(t('全部为默认'), '', { sm: true, outline: true }));
    grid.replaceChildren(...VERSION_FIELDS.flatMap(([k, label]) => {
      const val = over[k] || defs[k] || '—';
      return [h('dt', label()), h('dd.row.gap-1', h('span.mono', val), over[k] && over[k] !== defs[k] ? badge(t('已修改'), 'warn', { sm: true, tip: t('默认 {v}', { v: defs[k] || '—' }) }) : null)];
    }));
  });
  return [r.el, h('div.s-block', grid)];
}

function productsRow(S) {
  const PRODUCTS = [['workbuddy', 'WB'], ['vscode', 'VSC'], ['cli', 'CLI']];
  const list = h('div.pg-list');
  const boxes = new Map();
  for (const [id, label] of PRODUCTS) {
    const c = checkbox({ checked: true, label, onChange: async (on, input) => {
      const cur = new Set(S.val('auto_switch_products') || []);
      if (on) cur.add(id); else cur.delete(id);
      if (!cur.size) { input.checked = true; fail(t('至少选择一个身份')); return; }
      input.disabled = true;
      try { await S.save({ auto_switch_products: PRODUCTS.map((p) => p[0]).filter((p) => cur.has(p)) }); }
      catch (e) { input.checked = !on; fail(errMsg(e)); }
      finally { input.disabled = false; }
    } });
    boxes.set(id, c.input);
    list.appendChild(c);
  }
  S.bind((v) => { const cur = v.auto_switch_products || []; for (const [id, input] of boxes) input.checked = cur.includes(id); });
  return row({ title: t('可切换的身份'), help: t('至少保留一个身份。'), ctl: list, badges: [storedBadge()] });
}

export function identityCard(S) {
  return section('identity', t('出站身份'), { sub: t('上游看到的客户端身份') },
    switchRow(S, { key: 'auto_switch_product', title: t('自动切换出站身份'), help: t('上游对某个账号+模型返回 429 时，换一个身份重试（WB / VSC / CLI），每轮最多 4 次，并保存新身份。') }).el,
    productsRow(S).el,
    segRow(S, {
      key: 'identity.private_data', title: t('数据用途声明'), help: t('模型请求里 X-Private-Data 头的值。自动沿用各身份的官方默认；「不用于优化」表示数据不用于模型优化；「可用于优化」表示允许。'),
      options: [{ value: 'auto', label: t('自动') }, { value: 'true', label: t('不用于优化') }, { value: 'false', label: t('可用于优化') }],
    }).el,
    ...versionsRow(S));
}
