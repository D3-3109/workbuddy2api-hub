// Logs: live terminal-style viewer for the gateway's in-memory log ring.
//  - incremental polling (GET /logs?since_id=) every 2 s, only while mounted + visible
//  - client-side filters over a 2000-line buffer; the DOM holds at most RENDER_CAP lines
import { h, setText, setAttr, scope, debounce, reconcile, rafThrottle } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { get, post, qs, download } from '../lib/api.js';
import { t, add } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { poller } from '../lib/poll.js';
import { btn, badge, pageHead, empty, spinner, toast, ok, fail, errText, confirmDialog, copyText, switchEl } from '../lib/ui.js';
import { createStore, makeMatcher, formatLine, BUFFER_MAX, LEVELS } from './logs-store.js';
import { createTerminal, RENDER_CAP } from './logs-view.js';

add({
  '日志': 'Logs',
  '实时捕获反向代理请求、流式转发、调度巡检、任务打卡与异常告警 (内存保留最新 2000 条)': 'Live capture of proxied requests, streaming, scheduler runs, task check-ins and alerts (the latest 2000 lines are kept in memory)',
  '实时监听中': 'Live', '已暂停监听': 'Paused', '暂停实时监听': 'Pause live updates', '恢复实时监听': 'Resume live updates',
  '刷新': 'Refresh', '复制': 'Copy', '导出': 'Export', '清空': 'Clear',
  '重新拉取全部日志': 'Reload all logs', '复制当前过滤结果': 'Copy the filtered lines', '按当前过滤条件导出为文件': 'Export the filtered lines to a file', '清空内存中的日志': 'Clear the in-memory log',
  '全部级别': 'All levels', '全部模块': 'All modules', '日志级别': 'Level', '日志模块': 'Module',
  '过滤关键字 (如 deepseek, 11128, 401, error)...': 'Filter keywords (e.g. deepseek, 11128, 401, error)...', '清除搜索': 'Clear search',
  '跟随最新': 'Follow latest', '自动换行': 'Wrap lines', '跳到最新': 'Jump to latest', '{n} 条新日志': '{n} new',
  '显示 {a} / {b} 行': 'Showing {a} / {b} lines', '仅渲染最新 {n} 行；更早的内容请用过滤或导出查看': 'Only the newest {n} lines are rendered; use filters or export to see older output',
  '正在拉取网关日志...': 'Loading gateway logs...', '暂无日志记录': 'No log lines yet', '网关日志会实时出现在这里。': 'Gateway log lines appear here live.',
  '暂无匹配的日志记录': 'No matching log lines', '清除过滤': 'Clear filters', '日志加载失败': 'Failed to load logs', '重试': 'Retry',
  '正在拉取...': 'Fetching...', '就绪 · {t}': 'Ready · {t}', '已暂停 · {t}': 'Paused · {t}', '连接中断: {e}': 'Connection lost: {e}',
  '网关日志': 'Gateway log',
  '日志镜像丢弃了 {n} 行': 'Log mirror dropped {n} lines', 'stderr 队列已满，部分日志行未能收录。': 'The stderr queue was full, so some lines were not captured.',
  'stderr 管道已中断': 'stderr pipe closed', 'stderr 管道已中断，日志镜像已停用。': 'The stderr pipe broke and the log mirror is disabled.',
  '暂无日志可复制': 'No log lines to copy', '已复制 {n} 行日志': 'Copied {n} log lines', '暂无日志可导出': 'No log lines to export', '日志已导出': 'Log exported',
  '清空日志': 'Clear logs', '确定要清空内存中的网关日志吗？': 'Clear the gateway log held in memory?', '网关日志已清空': 'Gateway log cleared',
  '网关': 'Gateway', '对话': 'Chat', '任务福利': 'Tasks', '调度器': 'Scheduler', '账号': 'Accounts', '智能体': 'Agents', '定价': 'Pricing', '出口': 'Exit',
  '上游': 'Upstream', '网络工具': 'Web tools', '模型@@module': 'Models', '鉴权': 'Auth', '设置': 'Settings', '更新': 'Updates', '用量': 'Usage', '系统': 'System',
});

export const usesRealm = false;

const POLL_MS = 2000;
const SLOW_POLL_MS = 10000;   // after repeated failures
const INCREMENTAL_LIMIT = 200;
const LEVEL_DOT = { INFO: 'info', WARN: 'warn', ERROR: 'bad', DEBUG: 'dim' };

// display order of known modules; unknown tags follow alphabetically
const TAG_ORDER = ['gateway', 'chat', 'tasks', 'scheduler', 'accounts', 'agents', 'pricing', 'egress', 'upstream', 'webtools', 'catalog', 'auth', 'settings', 'update', 'usage', 'system'];

function tagLabels() {
  return {
    gateway: t('网关'), chat: t('对话'), tasks: t('任务福利'), scheduler: t('调度器'), accounts: t('账号'), agents: t('智能体'), pricing: t('定价'),
    egress: t('出口'), upstream: t('上游'), webtools: t('网络工具'), catalog: t('模型@@module'), auth: t('鉴权'), settings: t('设置'), update: t('更新'),
    usage: t('用量'), system: t('系统'),
  };
}

function filterChip(label, dot, onClick) {
  const count = h('span.count');
  const el = h('button.chip', { type: 'button', 'aria-pressed': 'false', onClick }, dot ? h('i.lg-dot.' + dot) : null, h('span.lg-chip-l', label), count);
  el.countEl = count;
  return el;
}

export async function mount(root, app) {
  const sc = scope();
  const ctl = new AbortController();
  const store = createStore();
  const labels = tagLabels();

  const f = { level: 'ALL', tag: '', q: '', raw: '' };   // q = trimmed + lower-case, raw = trimmed as typed
  const buildMatcher = () => makeMatcher({ level: f.level, tag: f.tag, query: f.q });
  let matcher = buildMatcher();
  let matched = 0;          // buffer lines that pass the filters
  let live = true;
  let loaded = false;       // first reply received
  let needReset = false;    // next sync does a full since_id=0 fetch
  let busy = null;          // single-flight guard
  let fails = 0;
  let lastError = null;
  let lastOk = 0;
  let status = 'loading';   // loading | ok | error

  /* ---------------- terminal + overlays ---------------- */
  const term = createTerminal({ label: t('网关日志'), onState: onTermState });

  const loadingBox = h('div.lg-loading', spinner(), h('span', t('正在拉取网关日志...')));
  const errMsg = h('p');
  const errBox = h('div.empty', icon('alert', 22, 'ic'), h('h3', t('日志加载失败')), errMsg, btn(t('重试'), { variant: 'primary', onClick: () => refresh() }));
  const noneBox = empty('terminal', t('暂无日志记录'), t('网关日志会实时出现在这里。'));
  const noMatchBox = empty('search', t('暂无匹配的日志记录'), null, btn(t('清除过滤'), { size: 'sm', onClick: () => clearFilters() }));

  /* ---------------- toolbar: level + module chips, search ---------------- */
  const levelChips = new Map();
  const levelRow = h('div.lg-levels', { role: 'group', 'aria-label': t('日志级别') });
  const allLevel = filterChip(t('全部级别'), '', () => setLevel('ALL'));
  levelChips.set('ALL', allLevel);
  levelRow.appendChild(allLevel);
  for (const lv of LEVELS) {
    const c = filterChip(lv, LEVEL_DOT[lv], () => setLevel(lv));
    levelChips.set(lv, c);
    levelRow.appendChild(c);
  }

  const tagRow = h('div.lg-tags', { role: 'group', 'aria-label': t('日志模块') });

  const searchInput = h('input.input.sm', {
    type: 'text', enterkeyhint: 'search', placeholder: t('过滤关键字 (如 deepseek, 11128, 401, error)...'), autocomplete: 'off', spellcheck: 'false', 'aria-label': t('过滤关键字 (如 deepseek, 11128, 401, error)...'),
  });
  const clearSearch = btn(t('清除搜索'), { iconOnly: true, icon: 'x', size: 'xs', variant: 'ghost', onClick: () => { searchInput.value = ''; applySearch(); searchInput.focus(); } });
  clearSearch.classList.add('clear');
  clearSearch.hidden = true;
  const searchBox = h('div.input-wrap.lg-search', icon('search', 15, 'ic'), searchInput, clearSearch);

  const applySearch = () => {
    debounced.cancel();
    const raw = searchInput.value.trim();
    clearSearch.hidden = !searchInput.value;
    if (raw === f.raw) return;
    f.raw = raw;
    f.q = raw.toLowerCase();
    rebuild(true);
  };
  const debounced = debounce(applySearch, 200);
  searchInput.addEventListener('input', () => { clearSearch.hidden = !searchInput.value; debounced(); });
  searchInput.addEventListener('keydown', (e) => { if (e.key === 'Enter') applySearch(); });

  const toolbar = h('div.lg-toolbar', h('div.lg-row', levelRow, searchBox), tagRow);

  /* ---------------- chrome (title bar) + footer ---------------- */
  const statusDot = h('span.pill-dot');
  const statusText = h('span.lg-status-t');
  const chrome = h('div.lg-chrome',
    h('span.lg-dots', { 'aria-hidden': 'true' }, h('i'), h('i'), h('i')),
    h('span.lg-title', 'wb-proxy-console.log'),
    h('span.lg-status', { role: 'status' }, statusDot, statusText));

  const countText = h('span.lg-count');
  const capTip = h('span.lg-cap', { hidden: true }, icon('info', 13));
  const warnBox = h('span.lg-warns');
  const wrapSw = switchEl({ checked: true, size: 'sm', label: t('自动换行'), onChange: (on) => term.setWrap(on) });
  const followSw = switchEl({ checked: true, size: 'sm', label: t('跟随最新'), onChange: (on) => term.setFollow(on) });
  const swLabel = (sw, text) => h('span.lg-sw', sw, h('span', { onClick: () => sw.input.click() }, text));
  const foot = h('div.lg-foot',
    h('div.lg-foot-l', countText, capTip, warnBox),
    h('div.lg-foot-r', swLabel(wrapSw, t('自动换行')), swLabel(followSw, t('跟随最新'))));

  /* ---------------- page actions ---------------- */
  const liveDot = h('span.pill-dot.good');
  const liveText = h('span', t('实时监听中'));
  const liveBtn = h('button.btn.lg-live', { type: 'button', 'aria-pressed': 'true', 'data-tip': t('暂停实时监听'), onClick: () => setLive(!live) }, liveDot, liveText);

  const labelled = (label, o) => {
    const b = btn(label, o);
    b.setAttribute('aria-label', label);
    b.classList.add('lg-act');
    return b;
  };
  const refreshBtn = labelled(t('刷新'), { icon: 'refresh', tip: t('重新拉取全部日志'), onClick: () => refresh(true) });
  const copyBtn = labelled(t('复制'), { icon: 'copy', tip: t('复制当前过滤结果'), onClick: () => copyLines() });
  const exportBtn = labelled(t('导出'), { icon: 'download', tip: t('按当前过滤条件导出为文件'), onClick: () => exportLog() });
  const clearBtn = labelled(t('清空'), { icon: 'trash', variant: 'danger', tip: t('清空内存中的日志'), onClick: () => clearLog() });

  root.append(h('div.p-logs',
    pageHead(t('日志'), t('实时捕获反向代理请求、流式转发、调度巡检、任务打卡与异常告警 (内存保留最新 2000 条)'), liveBtn, refreshBtn, copyBtn, exportBtn, clearBtn),
    h('section.card.lg-console', toolbar, chrome, term.el, foot)));

  /* ---------------- painting (all cheap, signature-gated) ---------------- */
  function paintStatus() {
    let text;
    let tone = '';
    if (status === 'loading') text = t('正在拉取...');
    else if (status === 'error') { text = t('连接中断: {e}', { e: lastError ? errText(lastError) : '' }); tone = 'bad'; }
    else if (!live) { text = t('已暂停 · {t}', { t: F.time(lastOk) }); tone = 'warn'; }
    else { text = t('就绪 · {t}', { t: F.time(lastOk) }); tone = 'good'; }
    setText(statusText, text);
    const cls = 'pill-dot' + (tone ? ' ' + tone : '');
    if (statusDot.className !== cls) statusDot.className = cls;
  }

  function paintLive() {
    const cls = 'pill-dot' + (live ? ' good' : '');
    if (liveDot.className !== cls) liveDot.className = cls;
    setText(liveText, live ? t('实时监听中') : t('已暂停监听'));
    setAttr(liveBtn, 'aria-pressed', String(live));
    setAttr(liveBtn, 'data-tip', live ? t('暂停实时监听') : t('恢复实时监听'));
  }

  function paintChip(chip, pressed, n, hide) {
    setAttr(chip, 'aria-pressed', String(pressed));
    setText(chip.countEl, n == null ? '' : F.int(n));
    if (chip.hidden !== !!hide) chip.hidden = !!hide;
  }

  function paintLevels() {
    paintChip(allLevel, f.level === 'ALL', store.entries.length, false);
    for (const lv of LEVELS) {
      const n = store.counts[lv];
      paintChip(levelChips.get(lv), f.level === lv, n, lv === 'DEBUG' && !n && f.level !== 'DEBUG');
    }
  }

  function tagItems() {
    const items = [...store.tags].map(([tk, v]) => ({ tk, tag: v.tag, n: v.n }));
    if (f.tag && !store.tags.has(f.tag)) items.push({ tk: f.tag, tag: f.tag, n: 0 });
    const rank = (tk) => { const i = TAG_ORDER.indexOf(tk); return i < 0 ? TAG_ORDER.length : i; };
    items.sort((a, b) => rank(a.tk) - rank(b.tk) || a.tk.localeCompare(b.tk));
    items.unshift({ tk: '', tag: '', n: store.entries.length });
    return items;
  }

  function paintTags() {
    const items = tagItems();
    // one module only (the usual Rust-build case) makes the row pointless
    tagRow.hidden = items.length <= 2 && !f.tag;
    if (tagRow.hidden) return;
    reconcile(tagRow, items, (it) => it.tk,
      (it) => {
        const c = filterChip(it.tk ? (labels[it.tk] || it.tag) : t('全部模块'), '', () => setTag(it.tk));
        if (it.tk) c.setAttribute('data-tip', it.tag);
        return c;
      },
      () => {});
    for (const it of items) paintChip(tagRow.__rc.get(it.tk), f.tag === it.tk, it.n, false);
  }

  function paintFoot() {
    const shown = term.count;
    setText(countText, t('显示 {a} / {b} 行', { a: F.int(shown), b: F.int(matched) }));
    const capped = matched > shown;
    capTip.hidden = !capped;
    if (capped) setAttr(capTip, 'data-tip', t('仅渲染最新 {n} 行；更早的内容请用过滤或导出查看', { n: F.int(RENDER_CAP) }));
    const sig = store.stderrDropped + ':' + store.stderrClosed;
    if (warnBox.__sig !== sig) {
      warnBox.__sig = sig;
      warnBox.replaceChildren(...[
        store.stderrDropped > 0 ? badge(t('日志镜像丢弃了 {n} 行', { n: F.int(store.stderrDropped) }), 'warn', { sm: true, tip: t('stderr 队列已满，部分日志行未能收录。') }) : null,
        store.stderrClosed ? badge(t('stderr 管道已中断'), 'warn', { sm: true, tip: t('stderr 管道已中断，日志镜像已停用。') }) : null].filter(Boolean));
    }
  }

  function paintOverlay() {
    if (!loaded) term.showOverlay(status === 'error' ? errBox : loadingBox);
    else if (!term.count) term.showOverlay(store.entries.length ? noMatchBox : noneBox);
    else term.showOverlay(null);
  }

  function paintMeta() {
    paintLevels();
    paintTags();
    paintFoot();
    paintOverlay();
    paintStatus();
  }

  function onTermState(s) {
    if (followSw.input.checked !== s.follow) followSw.input.checked = s.follow;
  }

  /* ---------------- filters ---------------- */
  function rebuild(jump) {
    matcher = buildMatcher();
    const list = store.entries.filter(matcher);
    matched = list.length;
    term.render(list.length > RENDER_CAP ? list.slice(-RENDER_CAP) : list, f.q);
    if (jump) term.setFollow(true);
    paintMeta();
  }

  function setLevel(level) { if (f.level !== level) { f.level = level; rebuild(true); } }
  function setTag(tag) { if (f.tag !== tag) { f.tag = tag; rebuild(true); } }
  function clearFilters() {
    f.level = 'ALL'; f.tag = ''; f.q = ''; f.raw = '';
    searchInput.value = ''; clearSearch.hidden = true;
    rebuild(true);
  }

  /* ---------------- data ---------------- */
  const fetchFull = (signal) => get('/logs' + qs({ since_id: 0, limit: BUFFER_MAX }), { signal });

  function apply(res, first, manual) {
    if (res.reset) {
      if (res.changed || first || manual) { rebuild(false); return; }
    } else if (res.fresh.length || res.dropped.length) {
      const fresh = res.fresh.filter(matcher);
      matched += fresh.length - res.dropped.filter(matcher).length;
      term.append(fresh, f.q);
    }
    paintMeta();
  }

  async function run(signal) {
    const manual = needReset;
    const forced = manual || !store.lastId;
    needReset = false;
    if (!loaded || manual) { status = 'loading'; paintStatus(); }
    try {
      let res;
      if (forced) res = store.reset(await fetchFull(signal));
      else {
        const reply = await get('/logs' + qs({ since_id: store.lastId, limit: INCREMENTAL_LIMIT }), { signal });
        if (store.staleReason(reply)) res = store.reset(await fetchFull(signal));
        else {
          res = store.append(reply);
          if (store.isStale()) res = store.reset(await fetchFull(signal));   // lines cleared by someone else
        }
      }
      const first = !loaded;
      loaded = true;
      status = 'ok';
      lastError = null;
      lastOk = Date.now();
      if (fails) { fails = 0; poll.setInterval(POLL_MS); }
      apply(res, first, manual);
      return null;
    } catch (e) {
      if (e && e.name === 'AbortError') { if (manual) needReset = true; return null; }
      if (e && e.unauthorized) return null;   // the login gate takes over, no toast
      fails++;
      if (fails === 3) poll.setInterval(SLOW_POLL_MS);
      status = 'error';
      lastError = e;
      setText(errMsg, errText(e));
      paintOverlay();
      paintStatus();
      return e;
    }
  }

  function sync(signal) {
    if (!busy) busy = run(signal).finally(() => { busy = null; });
    return busy;
  }

  const poll = poller((signal) => sync(signal), POLL_MS);

  async function refresh(notify) {
    needReset = true;
    if (busy) await busy;   // let the request in flight finish, then do the full fetch
    const err = await sync(ctl.signal);
    if (err && notify) fail(errText(err));
  }

  function setLive(on) {
    if (live === on) return;
    live = on;
    if (live) poll.start(); else poll.stop();
    paintLive();
    paintStatus();
  }

  /* ---------------- actions ---------------- */
  async function copyLines() {
    const list = store.entries.filter(matcher);
    if (!list.length) { toast(t('暂无日志可复制'), 'warn'); return; }
    await copyText(list.map(formatLine).join('\n'), t('已复制 {n} 行日志', { n: F.int(list.length) }));
  }

  async function exportLog() {
    if (!store.entries.length) { toast(t('暂无日志可导出'), 'warn'); return; }
    const name = 'wb-proxy-' + new Date().toISOString().replace(/[:.]/g, '-') + '.log';
    await download('/logs/export' + qs({ level: f.level === 'ALL' ? '' : f.level, tag: f.tag, search: f.raw }), name);
    ok(t('日志已导出'));
  }

  async function clearLog() {
    if (!(await confirmDialog({ title: t('清空日志'), message: t('确定要清空内存中的网关日志吗？'), confirmText: t('清空'), danger: true }))) return;
    await post('/logs/clear', {});
    store.clear();
    matched = 0;
    rebuild(false);
    ok(t('网关日志已清空'));
  }

  /* ---------------- go ---------------- */
  sc.on(window, 'resize', rafThrottle(() => term.resize()));
  paintLive();
  paintMeta();
  poll.start();

  return () => {
    poll.stop();
    ctl.abort();
    debounced.cancel();
    sc.dispose();
  };
}
