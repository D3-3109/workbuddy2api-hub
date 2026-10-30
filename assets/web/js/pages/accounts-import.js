// Accounts page: import accounts from a JSON file or pasted text, with a server-side dry run preview.
import { h, setText, debounce } from '../lib/dom.js';
import { post } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import { badge, btn, modal, checkbox, confirmDialog, toast, fail, errText } from '../lib/ui.js';

add({
  '导入账号': 'Import accounts', '选择 JSON 文件…': 'Choose a JSON file…', '或粘贴 JSON': 'or paste JSON', '粘贴账号 JSON': 'Paste account JSON',
  '支持本网关导出的文件、账号数组、单个账号对象，以及桌面客户端的 {"account":…,"auth":…} 格式。': 'Accepts files exported by this gateway, an array of accounts, a single account object, and the desktop client’s {"account":…,"auth":…} format.',
  '覆盖同 UID 账号': 'Overwrite accounts with the same UID', '确认导入 {n} 个': 'Import {n}', '导入中...': 'Importing…', '取消': 'Cancel',
  '不是合法的 JSON': 'Not valid JSON', '账号列表是空的': 'The account list is empty', '文件里没有找到账号（既没有 accounts 数组，也不是单个账号）': 'No accounts found (no accounts array, and not a single account)',
  '不是对象': 'Not an object', '缺少 accessToken': 'Missing accessToken', 'accessToken 不是 JWT 或 ck_/pt_ Key': 'accessToken is neither a JWT nor a ck_/pt_ key',
  '共 {n} 条': '{n} entries', '读取失败: {msg}': 'Could not read: {msg}', '预检失败: {msg}': 'Pre-check failed: {msg}', '正在预检…': 'Checking…',
  '新增': 'New', '覆盖': 'Overwrite', '跳过': 'Skipped', '无效': 'Invalid', '将新增': 'Will be added', '将覆盖': 'Will be overwritten', '将跳过': 'Will be skipped', '无法解析': 'Cannot be parsed',
  '第 {i} 条 — {reason}': 'Entry {i} — {reason}', '还有 {n} 条…': '{n} more…', '没有可导入的账号。': 'Nothing to import.', '若要覆盖已存在的账号，请勾选下方「覆盖同 UID 账号」。': 'To replace existing accounts, tick “Overwrite accounts with the same UID” below.',
  '将覆盖同 UID 的现有账号（含其凭证、权重、备注等），确定继续？': 'Existing accounts with the same UID will be replaced (credentials, weight, note and so on). Continue?',
  '导入完成：新增 {a}，覆盖 {b}，跳过 {c}，无效 {d}': 'Import finished: {a} added, {b} overwritten, {c} skipped, {d} invalid', '导入失败: {msg}': 'Import failed: {msg}',
  '导入部分完成：已写入 {n} 个，其余失败': 'Partly imported: {n} written, the rest failed', '已导入': 'imported',
});

/** Parse pasted / loaded text into importable rows. Rows that the server would reject outright are filtered out here. */
export function parseDoc(text) {
  let doc;
  try { doc = JSON.parse(text); } catch { return { error: t('不是合法的 JSON') }; }
  let rows;
  if (Array.isArray(doc)) rows = doc;
  else if (doc && Array.isArray(doc.accounts)) rows = doc.accounts;
  else if (doc && typeof doc === 'object' && (doc.accessToken || doc.access_token || doc.auth || doc.account)) rows = [doc];
  else return { error: t('文件里没有找到账号（既没有 accounts 数组，也不是单个账号）') };
  if (!rows.length) return { error: t('账号列表是空的') };
  const valid = [], invalid = [];
  rows.forEach((r, i) => {
    const why = checkRow(r);
    if (why) invalid.push({ index: i, reason: why }); else valid.push(r);
  });
  return { rows: valid, invalid, total: rows.length };
}

function checkRow(r) {
  if (!r || typeof r !== 'object' || Array.isArray(r)) return t('不是对象');
  const sub = (o) => o && typeof o === 'object' && (o.accessToken || o.access_token);
  const tok = r.accessToken || r.access_token || sub(r.auth) || sub(r.account);
  if (typeof tok !== 'string' || !tok) return t('缺少 accessToken');
  if (!/^(ck_|pt_)/.test(tok) && tok.split('.').length !== 3) return t('accessToken 不是 JWT 或 ck_/pt_ Key');
  return '';
}

const short = (uid) => String(uid || '').slice(0, 8);

export function openImport(host) {
  let parsed = null, fileName = '', preview = null, previewErr = '', checking = false, token = 0, busy = false;

  const fileBtn = btn(t('选择 JSON 文件…'), { icon: 'upload', onClick: pickFile });
  const fileLbl = h('span.muted.trunc');
  const area = h('textarea.textarea.mono', { rows: 5, placeholder: t('粘贴账号 JSON'), spellcheck: 'false', 'aria-label': t('粘贴账号 JSON') });
  const out = h('div.col', { style: 'gap:10px' });
  const over = checkbox({ label: t('覆盖同 UID 账号'), onChange: () => dryRun() });
  const go = btn(t('确认导入 {n} 个', { n: 0 }), { variant: 'primary', icon: 'check', disabled: true, onClick: commit });
  const m = modal({
    title: t('导入账号'), size: 'wide',
    body: [h('p.muted', { style: 'font-size:13px' }, t('支持本网关导出的文件、账号数组、单个账号对象，以及桌面客户端的 {"account":…,"auth":…} 格式。')),
      h('div.row.gap-3.wrap', fileBtn, fileLbl), h('div.faint', { style: 'font-size:12.5px' }, t('或粘贴 JSON')), area, out],
    actions: [btn(t('取消'), { onClick: () => m.close() }), go], left: over,
    onClose: () => { token++; host.dialogs.delete(m); },
  });
  host.dialogs.add(m);
  area.addEventListener('input', debounce(() => { fileName = ''; setText(fileLbl, ''); load(area.value); }, 400));

  function pickFile() {
    const input = h('input', { type: 'file', accept: '.json,application/json', style: 'display:none' });
    input.addEventListener('change', async () => {
      const f = input.files && input.files[0];
      input.remove();
      if (!f) return;
      try {
        const text = await f.text();
        fileName = f.name; setText(fileLbl, f.name); area.value = '';
        load(text);
      } catch (e) { say(t('读取失败: {msg}', { msg: errText(e) }), 'warn'); }
    });
    input.addEventListener('cancel', () => input.remove());
    document.body.appendChild(input);
    input.click();
  }

  function say(text, tone) { out.replaceChildren(h('div', { class: tone ? 'tx-' + tone : 'muted' }, text)); go.disabled = true; }

  function load(text) {
    preview = null; previewErr = '';
    if (!text.trim()) { parsed = null; out.replaceChildren(); paintGo(); return; }
    parsed = parseDoc(text);
    if (parsed.error) { say(t('读取失败: {msg}', { msg: parsed.error }), 'warn'); return; }
    dryRun();
  }

  async function dryRun() {
    if (!parsed || !parsed.rows) return;
    if (!parsed.rows.length) { render(); return; }
    const mine = ++token;
    checking = true; paintGo();
    out.replaceChildren(h('div.muted', t('正在预检…')));
    try {
      const r = await post('/accounts/import', { data: parsed.rows, dryRun: true, overwrite: over.input.checked });
      if (mine !== token) return;
      preview = r.result || {}; previewErr = '';
    } catch (e) {
      if (mine !== token) return;
      preview = null; previewErr = errText(e);
    }
    checking = false;
    render();
  }

  const list = (title, items, fmt, tone) => {
    if (!items.length) return null;
    const rows = items.slice(0, 50).map((x) => h('div.imp-row', h('code', fmt.id(x)), h('span.faint.trunc', fmt.txt(x))));
    if (items.length > 50) rows.push(h('div.faint', t('还有 {n} 条…', { n: items.length - 50 })));
    return h('div.imp-list', h('div.imp-title', badge(title, tone, { sm: true }), h('span.faint', String(items.length))), ...rows);
  };

  function render() {
    const inv = parsed ? parsed.invalid : [];
    const p = preview;
    const added = (p && p.added) || [], updated = (p && p.updated) || [], skipped = (p && p.skipped) || [];
    const kids = [];
    kids.push(h('div.muted', { style: 'font-size:13px' }, (fileName ? fileName + ' · ' : '') + t('共 {n} 条', { n: parsed.total })));
    if (previewErr) kids.push(h('div.tx-warn', t('预检失败: {msg}', { msg: previewErr })));
    if (p) {
      kids.push(h('div.row.gap-1.wrap', badge(t('新增') + ' ' + added.length, 'good'), badge(t('覆盖') + ' ' + updated.length, 'info'),
        badge(t('跳过') + ' ' + skipped.length, ''), badge(t('无效') + ' ' + inv.length, inv.length ? 'warn' : '')));
      const plain = { id: (x) => short(x.uid), txt: (x) => x.nickname || x.reason || '' };
      kids.push(list(t('将新增'), added, plain, 'good'), list(t('将覆盖'), updated, plain, 'info'),
        list(t('将跳过'), skipped, { id: (x) => short(x.uid), txt: (x) => x.reason || '' }, ''));
      if (!added.length && !updated.length) {
        kids.push(h('div.tx-warn', t('没有可导入的账号。') + (skipped.length ? ' ' + t('若要覆盖已存在的账号，请勾选下方「覆盖同 UID 账号」。') : '')));
      }
    }
    kids.push(list(t('无法解析'), inv, { id: (x) => '#' + (x.index + 1), txt: (x) => x.reason }, 'warn'));
    out.replaceChildren(...kids.filter(Boolean));
    paintGo();
  }

  function paintGo() {
    const n = preview ? (preview.added || []).length + (preview.updated || []).length : 0;
    go.lastChild.textContent = busy ? t('导入中...') : t('确认导入 {n} 个', { n });
    go.disabled = busy || checking || n === 0;
  }
  paintGo();

  async function commit() {
    if (!parsed || !parsed.rows || busy) return;
    if (over.input.checked && !(await confirmDialog({ title: t('导入账号'), message: t('将覆盖同 UID 的现有账号（含其凭证、权重、备注等），确定继续？'), danger: true }))) return;
    busy = true; paintGo();
    try {
      const r = await post('/accounts/import', { data: parsed.rows, overwrite: over.input.checked });
      finish(r, parsed.invalid.length);
    } catch (e) {
      // HTTP 500 after a partial write still carries a useful body
      if (e && e.body && e.body.result) { finish(e.body, parsed.invalid.length, true); }
      else fail(t('导入失败: {msg}', { msg: errText(e) }));
    } finally { busy = false; paintGo(); }
  }

  function finish(r, badRows, partial) {
    const res = r.result || {};
    const a = (res.added || []).length, b = (res.updated || []).length, c = (res.skipped || []).length;
    const d = (res.invalid || []).length + badRows;
    toast(partial ? t('导入部分完成：已写入 {n} 个，其余失败', { n: r.imported ?? a + b }) : t('导入完成：新增 {a}，覆盖 {b}，跳过 {c}，无效 {d}', { a, b, c, d }), partial || a + b === 0 ? 'warn' : 'ok');
    host.reload();
    if (a + b > 0) m.close(); else { preview = res; render(); }
  }
  return m;
}
