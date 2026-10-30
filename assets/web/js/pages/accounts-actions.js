// Accounts page: single-account actions. Edits are optimistic, then replaced by the account
// view the server returns; on failure the previous view is restored.
import { download, qs } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import { toast, ok, fail, errText, confirmDialog, promptDialog, checkbox } from '../lib/ui.js';
import { api, bodyError } from './accounts-store.js';
import { accountName, productLabel } from './accounts-model.js';

add({
  '账号已成功启用': 'Account enabled', '账号已停用': 'Account disabled', '操作失败: {msg}': 'Action failed: {msg}', '权重设置失败: {msg}': 'Could not set weight: {msg}',
  '备注保存失败: {msg}': 'Could not save the note: {msg}', '出口已更新': 'Exit updated', '出口更新失败: {msg}': 'Could not update the exit: {msg}',
  '已切换为 {label}': 'Switched to {label}', '切换失败: {msg}': 'Switch failed: {msg}', '切换出站身份': 'Switch identity',
  '将该账号的出站身份切换为 {label}？\n\n不同身份对应不同的出站指纹、端点和配额通道。': 'Switch this account’s outbound identity to {label}?\n\nEach identity uses a different fingerprint, endpoint and quota channel.',
  '测试通过 ({ms}ms) [{model}]: {reply}': 'Test passed ({ms}ms) [{model}]: {reply}', '测试失败: {msg}': 'Test failed: {msg}', '请 {n} 秒后重试': 'retry in {n}s',
  '账号凭证刷新成功': 'Token refreshed', '刷新失败: {msg}': 'Refresh failed: {msg}', '积分已刷新': 'Credits refreshed', '积分刷新失败: {msg}': 'Credit refresh failed: {msg}', '需要重新登录': 'Needs re-login',
  '删除账号': 'Delete account', '确定删除账号 {name}？\n凭证文件会从磁盘移除，无法恢复。': 'Delete account {name}?\nThe credential file is removed from disk and cannot be restored.',
  '已成功删除账号 {name}': 'Deleted account {name}', '账号已不存在': 'The account no longer exists', '删除失败: {msg}': 'Delete failed: {msg}',
  '导出账号': 'Export accounts', '将导出全部区域的账号。文件包含明文 accessToken 和 refreshToken，请妥善保管，不要发给别人。': 'Exporting the accounts of all regions. The file contains plain-text access and refresh tokens. Keep it safe and do not share it.', '已导出账号 → {name}': 'Exported accounts → {name}', '将导出 {n} 个账号。文件包含明文 accessToken 和 refreshToken，请妥善保管，不要发给别人。': 'Exporting {n} accounts. The file contains plain-text access and refresh tokens. Keep it safe and do not share it.',
  '仅导出元数据（不含凭证，无法再导入）': 'Metadata only (no credentials, cannot be re-imported)', '已导出 {n} 个账号 → {name}': 'Exported {n} accounts → {name}', '导出失败: {msg}': 'Export failed: {msg}',
  '编辑备注：{name}': 'Edit note: {name}', '自定义权重': 'Custom weight', '权重 (1-100)': 'Weight (1-100)', '请输入 1 到 100 的整数': 'Enter an integer from 1 to 100',
  '保存': 'Save', '权重已调整：该账号将优先接单': 'Weight updated: this account is preferred', '权重已恢复默认': 'Weight reset to default',
  '备注已保存': 'Note saved', '备注已清除': 'Note cleared', '最多 200 个字符': 'At most 200 characters', '留空清除；可用来标记「批量」/「自用」': 'Leave empty to clear; handy for tags like “bulk” or “personal”',
  '该账号已参与任务': 'Tasks enabled for this account', '该账号已停止任务': 'Tasks disabled for this account', '已保存禁用模型：{n} 个': 'Blocked models saved: {n}', '已取消模型禁用': 'Model blocks cleared',
  '账号备注': 'Account note', '导出': 'Export',
});

const stamp = () => new Date().toISOString().replace(/\D/g, '').slice(0, 14);

export function createActions(host) {
  const { store } = host;
  const A = {};

  /** POST /accounts/set with an optimistic local change; returns the server's account view. */
  A.setRaw = async (uid, patch, { optimistic = true } = {}) => {
    const prev = store.get(uid);
    if (optimistic) store.merge(uid, patch);
    try {
      const r = await api.set(uid, patch);
      if (r.account) store.put(r.account); else if (!optimistic) store.merge(uid, patch);
      return r.account;
    } catch (e) {
      if (prev) store.put(prev);
      throw e;
    }
  };

  /** Run `fn`; toast `prefix: message` on failure and swallow the error (menus and buttons toast already). */
  const guard = async (fn, prefixKey) => {
    try { return await fn(); } catch (e) { if (!(e && e.name === 'AbortError')) fail(t(prefixKey, { msg: errText(e) })); return undefined; }
  };

  A.setEnabled = (a, on) => guard(async () => {
    await A.setRaw(a.uid, { enabled: on });
    ok(on ? t('账号已成功启用') : t('账号已停用'));
  }, '操作失败: {msg}');

  /** Used by the task switch: toasts and rethrows so the switch can revert. */
  A.setTasks = async (a, on) => {
    try {
      await A.setRaw(a.uid, { tasksEnabled: on });
      ok(on ? t('该账号已参与任务') : t('该账号已停止任务'));
    } catch (e) { fail(t('操作失败: {msg}', { msg: errText(e) })); throw e; }
  };

  A.setWeight = (a, w) => guard(async () => {
    await A.setRaw(a.uid, { weight: w });
    ok(w > 1 ? t('权重已调整：该账号将优先接单') : t('权重已恢复默认'));
  }, '权重设置失败: {msg}');

  A.customWeight = async (a) => {
    const v = await promptDialog({
      title: t('自定义权重'), label: t('权重 (1-100)'), value: String(a.weight || 1), type: 'number', confirmText: t('保存'),
      validate: (s) => (/^\d{1,3}$/.test(s) && +s >= 1 && +s <= 100 ? '' : t('请输入 1 到 100 的整数')),
    });
    if (v != null) await A.setWeight(host.store.get(a.uid) || a, Number(v));
  };

  A.saveNote = (a, note) => guard(async () => {
    await A.setRaw(a.uid, { note });
    ok(note ? t('备注已保存') : t('备注已清除'));
  }, '备注保存失败: {msg}');

  A.editNote = async (a) => {
    const v = await promptDialog({
      title: t('编辑备注：{name}', { name: accountName(a) }), label: t('账号备注'), value: a.note || '', hint: t('留空清除；可用来标记「批量」/「自用」'), confirmText: t('保存'),
      validate: (s) => (s.length > 200 ? t('最多 200 个字符') : ''),
    });
    if (v != null && v !== (a.note || '')) await A.saveNote(a, v);
  };

  A.setProxy = (a, slotId) => guard(async () => {
    // a legacy literal proxy URL is only cleared by sending proxy:"" as well
    await A.setRaw(a.uid, a.hasProxy && !slotId ? { proxySlot: '', proxy: '' } : { proxySlot: slotId }, { optimistic: false });
    ok(t('出口已更新'));
    host.loadSlots();
  }, '出口更新失败: {msg}');

  A.setDisabledModels = (a, ids) => guard(async () => {
    await A.setRaw(a.uid, { disabledModels: ids }, { optimistic: false });
    ok(ids.length ? t('已保存禁用模型：{n} 个', { n: ids.length }) : t('已取消模型禁用'));
  }, '操作失败: {msg}');

  A.setProduct = async (a, product) => {
    if (a.product === product) return;
    const label = productLabel(product);
    if (!(await confirmDialog({ title: t('切换出站身份'), message: t('将该账号的出站身份切换为 {label}？\n\n不同身份对应不同的出站指纹、端点和配额通道。', { label }) }))) return;
    await A.applyProduct(a, product, label);
  };
  A.applyProduct = (a, product, label) => guard(async () => {
    await api.product(a.uid, product);
    store.merge(a.uid, { product });
    ok(t('已切换为 {label}', { label: label || productLabel(product) }));
    host.reload();
  }, '切换失败: {msg}');

  A.test = async (a) => {
    let r;
    try { r = await api.test(a.uid); } catch (e) { fail(t('测试失败: {msg}', { msg: errText(e) })); return; }
    if (r.ok) {
      const d = r.data;
      ok(t('测试通过 ({ms}ms) [{model}]: {reply}', { ms: d.elapsed_ms ?? '—', model: d.model || '—', reply: d.reply || 'ok' }));
    } else {
      const msg = bodyError(r.data, r.status) + (r.data.retry_after ? ' (' + t('请 {n} 秒后重试', { n: r.data.retry_after }) + ')' : '');
      toast(t('测试失败: {msg}', { msg }), r.status === 429 || r.status === 409 ? 'warn' : 'bad', 6500);
    }
    host.reload();
  };

  A.refreshToken = (a) => guard(async () => {
    const r = await api.refresh(a.uid);
    const res = (r.results || [])[0];
    if (res && res.ok) ok(t('账号凭证刷新成功'));
    else fail(t('刷新失败: {msg}', { msg: (res && res.error) || t('未知错误') }));
    host.reload();
  }, '刷新失败: {msg}');

  /** One-account credit refresh; returns true when fresh credits arrived. */
  A.refreshCredits = async (a, { quiet = false } = {}) => {
    try {
      const r = await api.credits(a.uid);
      const res = (r.results || [])[0];
      if (res && res.ok && res.credits) {
        store.merge(a.uid, { credits: res.credits, ...(res.tier !== undefined ? { tier: res.tier } : {}), tokenExpired: false });
        if (!quiet) ok(t('积分已刷新'));
        return { ok: true };
      }
      const msg = (res && (res.need_relogin ? t('需要重新登录') : res.error)) || t('未知错误');
      if (!quiet) fail(t('积分刷新失败: {msg}', { msg }));
      return { ok: false, message: msg, res };
    } catch (e) {
      if (!quiet) fail(t('积分刷新失败: {msg}', { msg: errText(e) }));
      return { ok: false, message: errText(e) };
    }
  };

  A.relogin = (a) => host.openLogin(a.realm || host.view);

  A.remove = async (a) => {
    if (!(await confirmDialog({ title: t('删除账号'), message: t('确定删除账号 {name}？\n凭证文件会从磁盘移除，无法恢复。', { name: accountName(a) }), danger: true, confirmText: t('删除') }))) return false;
    return guard(async () => {
      const r = await api.remove(a.uid);
      store.remove(a.uid);
      if (r.deleted === false) toast(t('账号已不存在'), 'warn'); else ok(t('已成功删除账号 {name}', { name: accountName(a) }));
      host.reload();
      return true;
    }, '删除失败: {msg}');
  };

  /** Export by uid list, or a whole realm (`realm` = intl|cn|all). Confirms because the file holds tokens. */
  A.exportAccounts = async ({ uids, realm, count }) => {
    const meta = checkbox({ label: t('仅导出元数据（不含凭证，无法再导入）') });
    const yes = await confirmDialog({
      title: t('导出账号'), message: count == null ? t('将导出全部区域的账号。文件包含明文 accessToken 和 refreshToken，请妥善保管，不要发给别人。') : t('将导出 {n} 个账号。文件包含明文 accessToken 和 refreshToken，请妥善保管，不要发给别人。', { n: count }),
      confirmText: t('导出'), extra: meta,
    });
    if (!yes) return;
    const name = 'workbuddy-accounts-' + (uids && uids.length === 1 ? uids[0].slice(0, 8) + '-' : '') + stamp() + '.json';
    await guard(async () => {
      await download('/accounts/export' + qs({ uid: uids && uids.join(','), realm: uids ? undefined : realm, secrets: meta.input.checked ? '0' : undefined, download: '1' }), name);
      ok(count == null ? t('已导出账号 → {name}', { name }) : t('已导出 {n} 个账号 → {name}', { n: count, name }));
    }, '导出失败: {msg}');
  };
  A.exportUids = (uids) => A.exportAccounts({ uids, count: uids.length });

  return A;
}
