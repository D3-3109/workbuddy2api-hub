// Accounts page: client-driven bulk operations. The server has no bulk endpoints worth using
// (one request per account keeps progress, cancel and stop-on-restriction under our control).
import { h, setText } from '../lib/dom.js';
import { t, add } from '../lib/i18n.js';
import { badge, btn, modal, confirmDialog, errText } from '../lib/ui.js';
import { api, bodyError } from './accounts-store.js';
import { accountName, productLabel, statusOf } from './accounts-model.js';

add({
  '停止': 'Stop', '关闭': 'Close', '停用账号': 'Disable', '删除': 'Delete', '成功 {n}': 'OK {n}', '失败 {n} 项': 'Failed {n}', '跳过 {n}': 'Skipped {n}', '未执行 {n}': 'Not run {n}', '已完成': 'Done', '已停止': 'Stopped', '已停止：{msg}': 'Stopped: {msg}',
  '已成功': 'OK', '失败': 'Failed', '跳过': 'Skipped', '完成': 'Done',
  '批量启用': 'Enable accounts', '批量停用': 'Disable accounts', '批量设置权重': 'Set weight', '批量切换出站身份': 'Switch identity', '批量绑定出口': 'Assign exits', '批量测试': 'Test accounts',
  '批量刷新凭证': 'Refresh tokens', '批量刷新积分': 'Refresh credits', '批量删除': 'Delete accounts', '补全缺失积分': 'Fill missing credits',
  '已是目标状态': 'Already in that state', '权重已是 {n}': 'Weight is already {n}', '已是该身份': 'Already that identity', '已绑定出口': 'Already has an exit', '没有可用的代理槽': 'No enabled proxy slots',
  '已停用': 'Disabled', '凭证失效，需要重新登录': 'Token invalid; sign in again', '账号受限 {left}': 'Restricted ({left} left)', '冷却中': 'Cooling down', '静态 API Key 不能刷新': 'Static API keys cannot be refreshed',
  '没有 refresh token': 'No refresh token', '前序请求被上游限制，已停止': 'Stopped after an upstream restriction',
  '确定停用选中的 {n} 个账号？它们将不再接收 API 流量。': 'Disable the {n} selected accounts? They will stop taking API traffic.',
  '确定把选中的 {n} 个账号的出站身份切换为 {label}？': 'Switch the outbound identity of the {n} selected accounts to {label}?',
  '确定删除选中的 {n} 个账号？凭证文件会从磁盘移除，无法恢复。': 'Delete the {n} selected accounts? Credential files are removed from disk and cannot be restored.',
  '每个账号发一条最小请求（真实调用上游，消耗极少量额度），逐个串行。受限 / 冷却 / 凭证失效的账号会被跳过。': 'Sends one minimal request per account (real upstream call, tiny credit cost), one at a time. Restricted, cooling or expired accounts are skipped.',
  '逐个刷新 OAuth 凭证；静态 API Key 与没有 refresh token 的账号会被跳过。': 'Refreshes OAuth tokens one by one. Static API keys and accounts without a refresh token are skipped.',
  '逐个向上游查询积分，每个账号间隔 1 秒；遇到账号受限会自动停止。': 'Queries credits upstream one account at a time, 1 s apart. Stops on an account restriction.',
  '上游限制了账号，已停止后续请求': 'Upstream restricted an account; remaining requests were not sent', '{n}ms · {reply}': '{n}ms · {reply}',
  '最近一次错误未恢复': 'Last error not cleared', '共 {n} 个账号': '{n} accounts', '批量操作完成：成功 {a}，失败 {b}，跳过 {c}': 'Batch finished: {a} OK, {b} failed, {c} skipped',
});

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * Generic sequential runner with a progress dialog.
 *  items: accounts; skipOf(a) -> reason|null; run(a) -> { ok, message, stop? }; gapMs between calls.
 * Resolves with { ok, failed, skipped, cancelled } when finished or closed.
 */
export function runBatch(host, { title, hint, items, skipOf, run, gapMs = 0, autoClose = false }) {
  return new Promise((resolve) => {
    const sum = { ok: 0, failed: 0, skipped: 0, cancelled: false, stopped: '' };
    let cancelled = false, finished = false;
    const fill = h('i');
    const prog = h('span.num');
    const counts = h('div.row.gap-3.wrap.muted', { style: 'font-size:12.5px' });
    const now = h('div.faint.trunc', { style: 'font-size:12.5px;min-height:18px' });
    const log = h('div.acct-blog', { role: 'log' });
    const stopBtn = btn(t('停止'), { onClick: () => { cancelled = true; stopBtn.disabled = true; } });
    const closeBtn = btn(t('关闭'), { variant: 'primary', onClick: () => m.close() });
    closeBtn.hidden = true;
    const m = modal({
      title, size: 'wide', persistent: true,
      body: [hint ? h('p.muted', { style: 'font-size:13px' }, hint) : null,
        h('div.col', { style: 'gap:8px' }, h('div.row.between', h('span', t('共 {n} 个账号', { n: items.length })), prog), h('div.bar.lg', fill), counts, now), log],
      actions: [stopBtn, closeBtn],
      onClose: () => { cancelled = true; host.dialogs.delete(m); resolve({ ...sum, cancelled: sum.cancelled || !finished }); },
    });
    host.dialogs.add(m);

    let handled = 0;
    function paint() {
      const total = items.length;
      setText(prog, `${handled} / ${total}`);
      fill.style.width = (total ? (handled / total) * 100 : 100).toFixed(1) + '%';
      counts.replaceChildren(badge(t('成功 {n}', { n: sum.ok }), 'good', { sm: true }), badge(t('失败 {n} 项', { n: sum.failed }), sum.failed ? 'bad' : '', { sm: true }), badge(t('跳过 {n}', { n: sum.skipped }), '', { sm: true }));
    }
    function push(kind, a, message) {
      const tone = kind === 'ok' ? 'good' : kind === 'fail' ? 'bad' : '';
      log.appendChild(h('div.acct-brow', badge(kind === 'ok' ? t('已成功') : kind === 'fail' ? t('失败') : t('跳过'), tone, { sm: true }),
        h('span.acct-bname', accountName(a)), h('span.acct-bmsg', { 'data-tip': message || '' }, message || '')));
      log.scrollTop = log.scrollHeight;
    }
    paint();

    (async () => {
      let i = 0;
      for (; i < items.length; i++) {
        if (cancelled) break;
        const a = items[i];
        const why = skipOf ? skipOf(a) : null;
        if (why) { sum.skipped++; handled++; push('skip', a, why); paint(); continue; }
        setText(now, accountName(a));
        let res;
        try { res = await run(a); } catch (e) { res = { ok: false, message: errText(e) }; }
        handled++;
        if (res.ok) sum.ok++; else sum.failed++;
        push(res.ok ? 'ok' : 'fail', a, res.message);
        paint();
        if (res.stop) { sum.stopped = res.message || ''; i++; break; }
        if (gapMs && i < items.length - 1 && !cancelled) await sleep(gapMs);
      }
      const left = items.length - i;
      if (left > 0 && (sum.stopped || cancelled)) {
        sum.cancelled = cancelled;
        for (let j = i; j < items.length; j++) { sum.skipped++; handled++; push('skip', items[j], sum.stopped ? t('前序请求被上游限制，已停止') : t('已停止')); }
        paint();
      }
      finished = true;
      setText(now, sum.stopped ? t('已停止：{msg}', { msg: sum.stopped }) : cancelled ? t('已停止') : t('已完成'));
      stopBtn.hidden = true;
      closeBtn.hidden = false;
      closeBtn.focus();
      host.reload();
      if (autoClose && !sum.failed && !cancelled) setTimeout(() => m.close(), 700);
    })();
  });
}

/* ------------------------------------------------------------ bulk ops */
export function createBulk(host, actions) {
  const B = {};

  B.setEnabled = async (list, on) => {
    if (!on && !(await confirmDialog({ title: t('批量停用'), message: t('确定停用选中的 {n} 个账号？它们将不再接收 API 流量。', { n: list.length }), danger: true, confirmText: t('停用账号') }))) return null;
    return runBatch(host, {
      title: on ? t('批量启用') : t('批量停用'), items: list, autoClose: true,
      skipOf: (a) => (!!a.enabled === on ? t('已是目标状态') : null),
      run: async (a) => { await actions.setRaw(a.uid, { enabled: on }, { optimistic: false }); return { ok: true, message: t('完成') }; },
    });
  };

  B.setWeight = (list, weight) => runBatch(host, {
    title: t('批量设置权重'), items: list, autoClose: true,
    skipOf: (a) => ((a.weight || 1) === weight ? t('权重已是 {n}', { n: weight }) : null),
    run: async (a) => { await actions.setRaw(a.uid, { weight }, { optimistic: false }); return { ok: true, message: '×' + weight }; },
  });

  B.setProduct = async (list, product) => {
    const label = productLabel(product);
    if (!(await confirmDialog({ title: t('批量切换出站身份'), message: t('确定把选中的 {n} 个账号的出站身份切换为 {label}？', { n: list.length, label }) }))) return null;
    return runBatch(host, {
      title: t('批量切换出站身份'), items: list, autoClose: true,
      skipOf: (a) => ((a.product || 'workbuddy') === product ? t('已是该身份') : null),
      run: async (a) => { await api.product(a.uid, product); host.store.merge(a.uid, { product }); return { ok: true, message: label }; },
    });
  };

  /** slotId: a slot id or '' (direct) to set explicitly, or null to spread unbound accounts over enabled slots. */
  B.setProxy = (list, slotId) => {
    const slots = host.slots().filter((s) => s.enabled !== false);
    const load = new Map(slots.map((s) => [s.id, Number(s.bound) || 0]));
    const auto = slotId == null;
    const pick = () => { let best = null; for (const [id, n] of load) if (best == null || n < load.get(best)) best = id; load.set(best, load.get(best) + 1); return best; };
    return runBatch(host, {
      title: t('批量绑定出口'), items: list, autoClose: true,
      skipOf: (a) => {
        if (!auto) return null;
        if (!slots.length) return t('没有可用的代理槽');
        return a.proxySlot || a.hasProxy ? t('已绑定出口') : null;
      },
      run: async (a) => {
        const id = auto ? pick() : slotId;
        await actions.setRaw(a.uid, a.hasProxy && !id ? { proxySlot: '', proxy: '' } : { proxySlot: id }, { optimistic: false });
        host.loadSlots();
        return { ok: true, message: id || '—' };
      },
    });
  };

  B.test = (list) => runBatch(host, {
    title: t('批量测试'), hint: t('每个账号发一条最小请求（真实调用上游，消耗极少量额度），逐个串行。受限 / 冷却 / 凭证失效的账号会被跳过。'), items: list,
    skipOf: (a) => {
      if (!a.enabled) return t('已停用');
      if (a.tokenExpired) return t('凭证失效，需要重新登录');
      if (a.restrictedFor > 0) return t('账号受限 {left}', { left: Math.round(a.restrictedFor) + 's' });
      if (a.inCooldown) return statusOf(a).label;
      return null;
    },
    run: async (a) => {
      let r = await api.test(a.uid);
      if (r.status === 429 && r.data.retry_after && r.data.retry_after <= 30 && !/realm paused/.test(bodyError(r.data, 429))) {
        await sleep(r.data.retry_after * 1000 + 300);
        r = await api.test(a.uid);
      }
      if (r.ok) return { ok: true, message: t('{n}ms · {reply}', { n: r.data.elapsed_ms ?? '—', reply: r.data.reply || 'ok' }) };
      const msg = bodyError(r.data, r.status);
      return { ok: false, message: msg, stop: /upstream restricted|realm paused/.test(msg) };
    },
  });

  B.refreshTokens = (list) => runBatch(host, {
    title: t('批量刷新凭证'), hint: t('逐个刷新 OAuth 凭证；静态 API Key 与没有 refresh token 的账号会被跳过。'), items: list, gapMs: 150,
    skipOf: (a) => (a.apiKey ? t('静态 API Key 不能刷新') : !a.hasRefreshToken ? t('没有 refresh token') : null),
    run: async (a) => {
      const r = await api.refresh(a.uid);
      const res = (r.results || [])[0];
      return res && res.ok ? { ok: true, message: t('完成') } : { ok: false, message: (res && res.error) || t('未知错误') };
    },
  });

  B.refreshCredits = (list, { missingOnly = false } = {}) => runBatch(host, {
    title: missingOnly ? t('补全缺失积分') : t('批量刷新积分'), hint: t('逐个向上游查询积分，每个账号间隔 1 秒；遇到账号受限会自动停止。'),
    items: missingOnly ? list.filter((a) => !a.credits) : list, gapMs: 1000,
    skipOf: (a) => (!a.enabled ? t('已停用') : a.tokenExpired ? t('凭证失效，需要重新登录') : null),
    run: async (a) => {
      const r = await actions.refreshCredits(a, { quiet: true });
      if (r.ok) return { ok: true, message: t('完成') };
      const res = r.res || {};
      return { ok: false, message: r.message, stop: res.status === 403 && !!res.skipped && res.retry_after > 0 };
    },
  });

  B.remove = async (list) => {
    if (!(await confirmDialog({ title: t('批量删除'), message: t('确定删除选中的 {n} 个账号？凭证文件会从磁盘移除，无法恢复。', { n: list.length }), danger: true, confirmText: t('删除') }))) return null;
    return runBatch(host, {
      title: t('批量删除'), items: list,
      run: async (a) => { await api.remove(a.uid); host.store.remove(a.uid); return { ok: true, message: t('完成') }; },
    });
  };

  return B;
}
