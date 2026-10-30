// Settings: egress & proxy slots (editable draft, saved as a whole) plus dispatch/timeout switches.
import { h, reconcile } from '../lib/dom.js';
import { get, post } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import { badge, btn, switchEl, empty, confirmDialog, unsupported, ok, fail, toast } from '../lib/ui.js';
import { section, subhead, switchRow, numRow, errMsg } from './settings-kit.js';

add({
  '出口与代理': 'Egress and proxies', '账号代理槽、探测与上游超时': 'Account proxy slots, probing and upstream timeouts',
  '账号代理槽': 'Account proxy slots', '未保存': 'Unsaved', '自动发现 mihomo 端口': 'Discover mihomo ports', '添加槽位': 'Add slot', '保存槽位': 'Save slots', '放弃修改': 'Discard',
  '把账号绑定到不同的代理出口。绑定数只统计已启用的账号，禁用的账号会释放槽位。在「账号」页的「出口」下拉里为账号选择槽位；留空表示直连。': 'Bind accounts to different proxy exits. The bound count only includes enabled accounts; disabled accounts release their slot. Pick a slot per account in the Accounts page “exit” dropdown; empty means direct.',
  '还没有代理槽位': 'No proxy slots yet', '添加一个代理地址后，就可以在账号页把账号绑定到它。': 'Add a proxy URL, then bind accounts to it on the Accounts page.',
  '名称': 'Name', '代理地址': 'Proxy URL', '出口信息': 'Exit info', '启用': 'Enabled', '绑定': 'Bound', '测试': 'Test', '删除槽位': 'Delete slot',
  '未探测': 'Not probed', '自动命名': 'Auto', '支持 http / https / socks5 / socks5h，不含路径': 'Use http, https, socks5 or socks5h without a path',
  '槽位「{name}」已绑定 {n} 个账号，删除并保存后这些账号会回退直连。': 'Slot “{name}” is bound to {n} accounts. After you delete it and save, those accounts fall back to direct.',
  '删除代理槽': 'Delete proxy slot', '删除': 'Delete', '放弃未保存的槽位修改？': 'Discard unsaved slot changes?',
  '代理槽位已保存': 'Proxy slots saved', '有 {n} 个账号未能解除绑定': '{n} accounts could not be unbound',
  '代理探测未启用': 'Proxy probing is not enabled', '测试完成：{ip}': 'Test finished: {ip}',
  '调度与超时': 'Dispatch and timeouts',
  '限流后窗口探测': 'Window probe after rate limits', '账号+模型被限流且上游给出恢复时间时，到点发送一个极小请求（约一句 Hi）来开启新的额度窗口，最多重试 20 次，间隔 15 秒。': 'When an account+model is rate limited and upstream gives a reset time, send one tiny request (about one “Hi”) at that time to start the next quota window. Retries up to 20 times, 15 s apart.',
  '出口退避': 'Egress backoff', '按出口统计传输失败和 502/503/504，并以 1 秒起翻倍至 15 秒（带抖动）退避，成功后清零；不再按账号退避。': 'Count transport failures and 502/503/504 per egress and back off from 1 s doubling up to 15 s (with jitter); reset on success. Replaces per-account backoff.',
  '地区回退': 'Realm fallback', '国际版请求被限流、超日额度或全部冷却时，对两个地区都有的模型改走国内版（单向）。显式指定地区的请求不受影响。': 'When a Global request is rate limited, over its daily quota or fully cooling down, retry on China for models both realms have (one-way). Requests that name a region are unaffected.',
  '上游响应头超时': 'Upstream header timeout', '单次尝试等待上游响应头的最长时间（秒）。等待期间客户端收不到任何数据，建议不超过客户端的首字节预算。': 'Longest wait for upstream response headers per attempt, in seconds. The client sees nothing meanwhile; keep it below the client’s first-byte budget.',
  '上游空闲超时': 'Upstream idle timeout', '流式响应中上游持续无数据多久后判定中断（秒）。': 'How long a streaming response may stay silent before it is treated as broken, in seconds.',
});

const SCHEMES = new Set(['http:', 'https:', 'socks5:', 'socks5h:']);
function validUrl(u) {
  let x;
  try { x = new URL(u); } catch { return false; }
  return SCHEMES.has(x.protocol) && !!x.hostname && (x.pathname === '' || x.pathname === '/') && !x.search && !x.hash;
}

let seq = 0;

function slotsBlock(S, env, initial) {
  let gen = 0;
  let server = initial;
  let draft = [];
  let dirty = false;

  const dirtyBadge = badge(t('未保存'), 'warn', { sm: true, dot: true });
  dirtyBadge.hidden = true;
  const discover = btn(t('自动发现 mihomo 端口'), { size: 'sm', icon: 'search' });
  const addBtn = btn(t('添加槽位'), { size: 'sm', icon: 'plus', onClick: () => addSlot() });
  const saveBtn = btn(t('保存槽位'), { size: 'sm', variant: 'primary', icon: 'check', disabled: true, onClick: () => saveSlots() });
  const discardBtn = btn(t('放弃修改'), { size: 'sm', variant: 'ghost', disabled: true, onClick: () => resetDraft() });
  if (!S.cap('proxy_probe')) unsupported(discover, t('当前 Rust 版网关未实现此功能'));

  const tbody = h('tbody');
  const wrap = h('div.table-wrap', h('table.table.px-table',
    h('thead', h('tr', h('th', t('名称')), h('th', t('代理地址')), h('th.hide-sm', t('出口信息')), h('th.hide-sm.num', t('绑定')), h('th', t('启用')), h('th'))), tbody));
  const none = empty('server', t('还没有代理槽位'), t('添加一个代理地址后，就可以在账号页把账号绑定到它。'));

  const sigOf = (list) => JSON.stringify(list.map((d) => [d.id, d.name.trim(), d.url.trim(), d.enabled]));
  const serverSig = () => sigOf(server.map((s) => ({ id: s.id, name: s.name || '', url: s.url || '', enabled: s.enabled !== false })));

  function markDirty() {
    dirty = sigOf(draft) !== serverSig();
    dirtyBadge.hidden = !dirty;
    saveBtn.disabled = !dirty;
    discardBtn.disabled = !dirty;
  }

  function fromServer(s) {
    return { _k: gen + ':' + s.id, id: s.id, name: s.name || '', url: s.url || '', enabled: s.enabled !== false, src: s };
  }

  function exitInfo(src) {
    if (!src) return '';
    return [src.country_code || src.country, src.ip_type, src.ip, src.isp].filter(Boolean).join(' · ');
  }

  function createRow(d) {
    const name = h('input.input.sm', { type: 'text', value: d.name, 'aria-label': t('名称'), autocomplete: 'off', maxlength: '64' });
    const url = h('input.input.sm.mono', { type: 'text', value: d.url, placeholder: 'socks5://127.0.0.1:7890', 'aria-label': t('代理地址'), autocomplete: 'off', spellcheck: 'false' });
    const err = h('div.err.px-err', { hidden: true }, t('支持 http / https / socks5 / socks5h，不含路径'));
    const sw = switchEl({ size: 'sm', checked: d.enabled, label: t('启用') });
    const test = btn(t('测试'), { iconOnly: true, icon: 'activity', variant: 'ghost', size: 'sm', onClick: async () => {
      const cur = tr.__d;
      if (!cur.id) return;
      const r = await post('/proxy/slots/test', { id: cur.id });
      toast(r.ok === false ? (r.error || t('代理探测未启用')) : t('测试完成：{ip}', { ip: r.exit_ip || '—' }), r.ok === false ? 'warn' : 'ok');
    } });
    if (!S.cap('proxy_probe')) unsupported(test, t('当前 Rust 版网关未实现此功能'));
    const del = btn(t('删除槽位'), { iconOnly: true, icon: 'trash', variant: 'ghost', size: 'sm', onClick: () => removeSlot(tr.__d) });
    const info = h('td.hide-sm.px-info');
    const bound = h('td.hide-sm.num');
    const tr = h('tr', h('td.px-name', name), h('td.px-url', url, err), info, bound, h('td', sw), h('td.px-actions', h('div.row.gap-1', test, del)));
    name.addEventListener('input', () => { tr.__d.name = name.value; markDirty(); });
    url.addEventListener('input', () => { tr.__d.url = url.value; err.hidden = true; url.removeAttribute('aria-invalid'); markDirty(); });
    sw.input.addEventListener('change', () => { tr.__d.enabled = sw.input.checked; markDirty(); });
    Object.assign(tr, { __d: d, __info: info, __bound: bound, __name: name, __url: url, __err: err });
    updateRow(tr, d);
    return tr;
  }

  function updateRow(tr, d) {
    tr.__d = d;
    const src = d.src;
    const text = exitInfo(src);
    const sig = (src ? src.bound : '') + '|' + text + '|' + (src ? src.label : '');
    if (tr.__sig === sig) return;
    tr.__sig = sig;
    tr.__info.replaceChildren(text ? h('span', text) : h('span.faint', src ? t('未探测') : '—'));
    tr.__bound.replaceChildren(src ? badge(String(src.bound || 0), src && src.bound ? 'accent' : '', { sm: true }) : h('span.faint', '—'));
    tr.__name.placeholder = src && src.label ? src.label : t('自动命名');
  }

  function paint() {
    wrap.hidden = !draft.length;
    none.hidden = !!draft.length;
    reconcile(tbody, draft, (d) => d._k, createRow, updateRow);
  }

  function resetDraft() {
    gen++;
    draft = server.map(fromServer);
    paint();
    markDirty();
  }

  function addSlot() {
    const d = { _k: 'new' + (++seq), id: '', name: '', url: '', enabled: true, src: null };
    draft.push(d);
    paint();
    markDirty();
    const tr = tbody.lastElementChild;
    if (tr) tr.__url.focus();
  }

  async function removeSlot(d) {
    if (d.src && d.src.bound > 0) {
      const yes = await confirmDialog({
        title: t('删除代理槽'), message: t('槽位「{name}」已绑定 {n} 个账号，删除并保存后这些账号会回退直连。', { name: d.name || d.src.label || d.id, n: d.src.bound }),
        confirmText: t('删除'), danger: true,
      });
      if (!yes) return;
    }
    draft = draft.filter((x) => x !== d);
    paint();
    markDirty();
  }

  async function saveSlots() {
    let bad = null;
    for (const tr of tbody.children) {
      const d = tr.__d;
      const ok1 = !d.url.trim() || validUrl(d.url.trim());
      tr.__err.hidden = ok1;
      if (ok1) tr.__url.removeAttribute('aria-invalid'); else { tr.__url.setAttribute('aria-invalid', 'true'); bad = bad || tr.__url; }
    }
    if (bad) { bad.focus(); return; }
    // Blank-URL rows are dropped by the server anyway; probe fields are retained server-side for unchanged slots.
    const slots = draft.filter((d) => d.url.trim()).map((d) => {
      const row = { name: d.name.trim(), url: d.url.trim(), enabled: d.enabled };
      if (d.id) row.id = d.id;
      return row;
    });
    try {
      const r = await post('/proxy/slots/save', { slots });
      server = r.slots || [];
      resetDraft();
      ok(t('代理槽位已保存'));
      if (r.failed && r.failed.length) toast(t('有 {n} 个账号未能解除绑定', { n: r.failed.length }), 'warn');
    } catch (e) {
      fail(errMsg(e));
    }
  }

  async function reload() {
    if (dirty && !(await confirmDialog({ title: t('未保存'), message: t('放弃未保存的槽位修改？'), danger: true }))) return;
    const r = await get('/proxy/slots');
    server = r.slots || [];
    resetDraft();
  }

  resetDraft();
  const head = h('div.s-block.px-head',
    h('div.px-title', h('span', t('账号代理槽')), dirtyBadge),
    h('div.row.wrap.gap-1.px-tools', discover, addBtn, discardBtn, saveBtn));
  const legend = h('div.s-block.px-legend', t('把账号绑定到不同的代理出口。绑定数只统计已启用的账号，禁用的账号会释放槽位。在「账号」页的「出口」下拉里为账号选择槽位；留空表示直连。'));
  return { els: [head, legend, wrap, none], reload };
}

export function egressCard(S, env) {
  const slots = slotsBlock(S, env, env.slots || []);
  const el = section('egress', t('出口与代理'), { sub: t('账号代理槽、探测与上游超时') },
    ...slots.els,
    subhead(t('调度与超时')),
    switchRow(S, { key: 'window_probe', title: t('限流后窗口探测'), help: t('账号+模型被限流且上游给出恢复时间时，到点发送一个极小请求（约一句 Hi）来开启新的额度窗口，最多重试 20 次，间隔 15 秒。') }).el,
    switchRow(S, { key: 'egress_backoff_enabled', title: t('出口退避'), help: t('按出口统计传输失败和 502/503/504，并以 1 秒起翻倍至 15 秒（带抖动）退避，成功后清零；不再按账号退避。') }).el,
    switchRow(S, { key: 'realm_fallback_enabled', title: t('地区回退'), help: t('国际版请求被限流、超日额度或全部冷却时，对两个地区都有的模型改走国内版（单向）。显式指定地区的请求不受影响。') }).el,
    numRow(S, { key: 'upstream.header_timeout_seconds', title: t('上游响应头超时'), help: t('单次尝试等待上游响应头的最长时间（秒）。等待期间客户端收不到任何数据，建议不超过客户端的首字节预算。'), min: 1, max: 86400, unit: 's' }).el,
    numRow(S, { key: 'upstream.idle_timeout_seconds', title: t('上游空闲超时'), help: t('流式响应中上游持续无数据多久后判定中断（秒）。'), min: 1, max: 86400, unit: 's' }).el);
  el.reload = slots.reload;
  return el;
}
