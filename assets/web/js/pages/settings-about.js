// Settings: version / runtime info, updates (read-only in the Rust build) and sign-out.
import { h } from '../lib/dom.js';
import { post, session } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import { badge, btn, alertBox, unsupported, toast } from '../lib/ui.js';
import { section, row, subhead, switchRow, selectRow } from './settings-kit.js';

add({
  '运行信息': 'About and runtime', '版本、更新与退出登录': 'Version, updates and sign-out',
  '版本信息': 'Version info', '网关版本': 'Gateway version', '上游基线版本': 'Upstream baseline', '同步的上游提交': 'Synced upstream commit', '基线提交': 'Baseline commit', '同步范围': 'Sync scope', '运行时': 'Runtime',
  '功能支持': 'Capabilities', '调度器': 'Scheduler', '每日任务': 'Daily tasks', '自更新': 'Self-update', '桌面端导入': 'Desktop import', '代理探测': 'Proxy probing', '日志数据库': 'Log database', '价格刷新': 'Price refresh', '额度扫描': 'Quota scan',
  '此版本不支持自更新，请在外部替换二进制文件或容器。': 'This build does not update itself. Replace the binary or container externally.',
  '更新': 'Updates', '自动检查更新': 'Check for updates automatically', '每 24 小时最多检查一次 GitHub 发布，安装始终需要手动操作。': 'Check GitHub releases at most every 24 hours. Installing is always manual.',
  '保留旧版本份数': 'Old versions kept', '更新时在 updates/ 下保留的旧程序副本（每份约 5 MB），用于回滚；0 表示不保留。': 'Old program copies kept under updates/ for rollback (about 5 MB each). 0 keeps none.',
  '不保留': 'None', '立即检查更新': 'Check for updates', '安装更新': 'Install update', '回滚到上一版本': 'Roll back',
  '更新检查已发起': 'Update check started', '危险操作': 'Danger zone', '退出面板登录': 'Sign out of the panel', '销毁当前会话并回到登录页，不影响正在运行的网关。': 'Ends this session and returns to the sign-in page. The gateway keeps running.', '退出登录': 'Sign out',
});

const short = (s) => (s ? String(s).slice(0, 7) : '—');

function infoBlock(S, env) {
  const u = env.updates || {};
  const kv = h('dl.kv.ab-kv');
  const caps = h('div.row.wrap.gap-1');
  S.bind((v) => {
    const rows = [[t('网关版本'), v.version || u.current_version || '—'], [t('运行时'), (v.runtime || u.runtime || 'rust') === 'rust' ? 'Rust' : String(v.runtime)],
      [t('上游基线版本'), v.upstream_version || u.upstream_version || '—'], [t('同步的上游提交'), short(v.synced_upstream_commit || u.synced_upstream_commit)]];
    if (u.baseline_commit) rows.push([t('基线提交'), short(u.baseline_commit)]);
    if (u.sync_scope) rows.push([t('同步范围'), u.sync_scope]);
    kv.replaceChildren(...rows.flatMap(([k, val]) => [h('dt', k), h('dd.mono', val)]));
    const c = v.management_capabilities || {};
    const defs = [['scheduler', t('调度器')], ['daily_tasks', t('每日任务')], ['self_update', t('自更新')], ['desktop_import', t('桌面端导入')], ['proxy_probe', t('代理探测')],
      ['log_database', t('日志数据库')], ['pricing_refresh', t('价格刷新')], ['quota_scan', t('额度扫描')]];
    caps.replaceChildren(...defs.map(([k, label]) => badge(label, c[k] ? 'good' : '', { dot: true, sm: true, outline: !c[k] })));
  });
  const msg = u.msg || t('此版本不支持自更新，请在外部替换二进制文件或容器。');
  return [h('div.s-block', kv), h('div.s-block.ab-caps', h('div.faint.ab-cap-title', t('功能支持')), caps), h('div.s-block', alertBox('info', msg))];
}

function updateButtons(S) {
  const defs = [['/updates/check', t('立即检查更新'), 'refresh'], ['/updates/install', t('安装更新'), 'download'], ['/updates/rollback', t('回滚到上一版本'), 'undo']];
  const can = S.cap('self_update');
  const els = defs.map(([path, label, ic]) => {
    const b = btn(label, { size: 'sm', icon: ic, onClick: async () => { await post(path, {}); toast(t('更新检查已发起'), 'ok'); } });
    if (!can) unsupported(b, t('当前 Rust 版网关未实现此功能'));
    return b;
  });
  return h('div.s-block.row.wrap.gap-1', els);
}

async function signOut() {
  try { await post('/panel/logout', {}); } catch { /* the session is dropped locally anyway */ }
  session.clear();
  location.reload();
}

export function aboutCard(S, env) {
  const dzHead = subhead(t('危险操作'));
  dzHead.classList.add('danger');
  const dzRow = row({
    title: t('退出面板登录'), help: t('销毁当前会话并回到登录页，不影响正在运行的网关。'),
    ctl: btn(t('退出登录'), { variant: 'danger', icon: 'logout', onClick: signOut }),
  });
  return section('about', t('运行信息'), { sub: t('版本、更新与退出登录') },
    subhead(t('版本信息')),
    ...infoBlock(S, env),
    subhead(t('更新')),
    switchRow(S, { key: 'update_check_enabled', title: t('自动检查更新'), help: t('每 24 小时最多检查一次 GitHub 发布，安装始终需要手动操作。') }).el,
    selectRow(S, {
      key: 'keep_backups', title: t('保留旧版本份数'), help: t('更新时在 updates/ 下保留的旧程序副本（每份约 5 MB），用于回滚；0 表示不保留。'), stored: true,
      options: [{ value: '0', label: t('不保留') }, { value: '1', label: '1' }, { value: '2', label: '2' }, { value: '3', label: '3' }],
      patch: (v) => ({ keep_backups: Number(v) }), width: '160px',
    }).el,
    updateButtons(S),
    dzHead, dzRow.el);
}
