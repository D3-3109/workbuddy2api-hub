// Settings: daily web activity (Global accounts) and its schedule.
import { h, setText } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { t, add } from '../lib/i18n.js';
import { badge, btn, alertBox, field } from '../lib/ui.js';
import { section, row, subhead, switchRow, selectRow, numRow, byteLen, errMsg, dialog } from './settings-kit.js';
import { picker } from './settings-keys-edit.js';

add({
  '每日活跃': 'Daily activity', '国际版账号的每日网页活跃打卡与时间表': 'Daily web check-in activity and schedule for Global accounts',
  '附带网页渠道会话': 'Also run a web-channel session', '每日签到时，除桌面身份的对话外，再跑一个网页渠道会话。仅对国际版账号生效。': 'On the daily check-in, besides the desktop-identity chat, also run one web-channel session. Global accounts only.',
  '活跃使用的模型': 'Activity model', '内置默认': 'Built-in default', '免费': 'free', '（免费 {n}x）': '(free {n}x)', '（{n}x）': '({n}x)', '（不在当前目录里）': '(not in the current catalogue)',
  '每日活跃发送「Hi」所用的模型。免费模型排在最前；留空使用内置默认。': 'Model used for the activity “Hi” chat. Free models come first; empty uses the built-in default.',
  '固定活跃时间': 'Fixed activity times', '每天在这些时间点各跑一次活跃。网关会在没有自定义时填入签到整点，所以这里不会为空；点击「恢复跟随签到」可重置。': 'Run the activity at each of these times every day. The gateway fills in the check-in hours when nothing is customised, so this is never empty; use “Follow check-in” to reset.',
  '添加时间': 'Add time', '添加': 'Add', '保存': 'Save', '恢复跟随签到': 'Follow check-in', '移除 {t}': 'Remove {t}', '最多 24 个时间点': 'At most 24 times', '已存在': 'Already added',
  '会话未完成时再试': 'Retry unfinished sessions', '等待后网页会话仍在「工作中」时，再跑下一轮。': 'Run another round when a web session is still “working” after the wait.',
  '最大轮数': 'Maximum rounds', '含第一轮，范围 1 到 {max}。': 'Including the first round. Range 1 to {max}.',
  '首轮等待（秒）': 'First-round wait (s)', '第 N 轮等待 N × 该值。范围 1 到 {max}。': 'Round N waits N × this value. Range 1 to {max}.',
  '并行账号数': 'Parallel accounts', '同时跑活跃的账号数，范围 1 到 {max}。': 'Accounts running the activity at once. Range 1 to {max}.',
  '建会话失败退避（秒）': 'Session-creation backoff (s)', '创建会话失败后，第 N 轮等待 基数 × 2^(N-1)，单次上限取 30 秒与基数中较大者；0 为不退避。范围 0 到 {max}。': 'After a session-creation failure, round N waits base × 2^(N-1), capped at the larger of 30 s and the base. 0 means no backoff. Range 0 to {max}.',
  '每轮等待：{seq} 秒': 'Waits per round: {seq} s', '最坏情况下，每个卡住的账号需要 {n} 秒。': 'Worst case per stuck account: {n} s.', '失败退避：{seq} 秒': 'Failure backoff: {seq} s', '不退避': 'No backoff',
  '失败后换备选模型': 'Switch model after a failed round', '每轮失败后改用备选列表里的下一个模型。': 'After each failed round, switch to the next model in the fallback list.',
  '备选模型': 'Fallback models', '备选模型按「免费、便宜、昂贵」自动排序，逐个尝试一遍后在轮数允许时重复。留空使用默认免费集合。': 'Ordered automatically free, then cheap, then expensive. Each is tried once, then repeated if rounds allow. Empty uses the default free set.',
  '已选 {n} 个': '{n} selected', '使用默认免费集合': 'Default free set', '选择模型…': 'Choose models…', '选择备选模型': 'Choose fallback models', '最多选择 20 个模型': 'Select at most 20 models',
  '随机提示词': 'Random prompt', '每次网页活跃随机挑一条提示词；关闭则始终发送「Hi」。': 'Pick a random prompt for each web activity. Off always sends “Hi”.',
  '提示词池': 'Prompt pool', '留空使用内置的 300 条。': 'Empty uses the 300 built-in prompts.', '自定义 {n} 条': '{n} custom prompts', '使用内置 300 条': 'Built-in 300 prompts', '编辑…': 'Edit…',
  '编辑提示词池': 'Edit prompt pool', '每行一条，最多 500 条，每条不超过 200 字节；重复行会合并。留空使用内置提示词。': 'One prompt per line, up to 500, each at most 200 bytes. Duplicates are merged. Empty uses the built-in prompts.',
  '最多 500 条提示词': 'At most 500 prompts', '第 {n} 行超过 200 字节': 'Line {n} is longer than 200 bytes', '清空（使用内置）': 'Clear (use built-in)', '取消': 'Cancel',
  '重试与并发': 'Retries and concurrency', '备选模型与提示词': 'Fallback models and prompts', '固定整点任务': 'Fixed-hour tasks', '这些任务按整点运行，在此只读显示。': 'These tasks run on fixed hours and are shown read-only here.', '签到': 'Check-in', '出行': 'Travel', '保活': 'Keepalive', '猫咪': 'Cat',
});

const ct = (credits) => { const n = parseFloat(String(credits || '').replace(/^x/i, '')); return Number.isFinite(n) ? n : null; };
const HH = (n) => String(n).padStart(2, '0') + ':00';

/* ---- activity model (dropdown filled from the Global catalogue) ---- */
function modelRow(S, env) {
  const dflt = S.view.daily_chat_model_default || '';
  const r = selectRow(S, {
    key: 'daily_chat_model', title: t('活跃使用的模型'), help: t('每日活跃发送「Hi」所用的模型。免费模型排在最前；留空使用内置默认。'),
    options: [{ value: '', label: t('内置默认') + (dflt ? ' · ' + dflt : '') }], width: '260px',
    keepUnknown: (v) => v + ' ' + t('（不在当前目录里）'),
  });
  env.catalog('intl').then((list) => {
    const rows = list.map((m) => ({ id: m.id, n: ct(m.credits) })).filter((m) => m.id);
    rows.sort((a, b) => ((a.n === 0 ? 0 : 1) - (b.n === 0 ? 0 : 1)) || a.id.localeCompare(b.id));
    r.setOptions([{ value: '', label: t('内置默认') + (dflt ? ' · ' + dflt : '') }, ...rows.map((m) => ({
      value: m.id, label: m.id + ' ' + (m.n === 0 ? t('（免费 {n}x）', { n: '0.00' }) : m.n == null ? '' : t('（{n}x）', { n: m.n.toFixed(2) })),
    }))]);
  }).catch(() => {});
  return r;
}

/* ---- fixed activity times ---- */
function timesRow(S) {
  let draft = [...(S.val('schedule.daily_chat') || [])];
  let base = JSON.stringify(draft);
  const chips = h('div.dc-chips');
  const timeIn = h('input.input.narrow', { type: 'time', value: '09:00', 'aria-label': t('添加时间') });
  const addBtn = btn(t('添加'), { size: 'sm', icon: 'plus', onClick: () => {
    const v = timeIn.value;
    if (!v) return;
    if (draft.includes(v)) { r.error(t('已存在')); return; }
    if (draft.length >= 24) { r.error(t('最多 24 个时间点')); return; }
    r.error('');
    draft = [...draft, v].sort();
    paint();
  } });
  const saveBtn = btn(t('保存'), { size: 'sm', variant: 'primary', disabled: true, onClick: () => send(draft) });
  const followBtn = btn(t('恢复跟随签到'), { size: 'sm', variant: 'ghost', onClick: () => send([]) });
  const r = row({
    title: t('固定活跃时间'), help: t('每天在这些时间点各跑一次活跃。网关会在没有自定义时填入签到整点，所以这里不会为空；点击「恢复跟随签到」可重置。'),
    ctl: h('div.dc-times', chips, h('div.row.wrap.gap-1', timeIn, addBtn, saveBtn, followBtn)), cls: 'stack-ctl',
  });
  async function send(list) {
    r.error('');
    try { await S.save({ schedule: { daily_chat: list } }); } catch (e) { r.error(errMsg(e)); }
  }
  function paint() {
    chips.replaceChildren(...draft.map((tm) => h('span.dc-chip', h('span.mono', tm),
      h('button', { type: 'button', 'aria-label': t('移除 {t}', { t: tm }), onClick: () => { draft = draft.filter((x) => x !== tm); paint(); } }, icon('x', 12)))));
    saveBtn.disabled = JSON.stringify(draft) === base;
  }
  S.bind((v) => {
    const list = (v.schedule && v.schedule.daily_chat) || [];
    if (JSON.stringify(draft) === base) draft = [...list];
    base = JSON.stringify(list);
    paint();
  });
  return r;
}

/* ---- retry numbers and the derived wait sequences ---- */
function retryRows(S) {
  const v = S.view;
  const attempts = numRow(S, { key: 'daily_chat_web_attempts', title: t('最大轮数'), help: t('含第一轮，范围 1 到 {max}。', { max: v.daily_chat_web_attempts_max || 5 }), min: 1, max: v.daily_chat_web_attempts_max || 5 });
  const timeout = numRow(S, { key: 'daily_chat_web_timeout', title: t('首轮等待（秒）'), help: t('第 N 轮等待 N × 该值。范围 1 到 {max}。', { max: v.daily_chat_web_timeout_max || 3600 }), min: 1, max: v.daily_chat_web_timeout_max || 3600 });
  const conc = numRow(S, { key: 'daily_chat_web_concurrency', title: t('并行账号数'), help: t('同时跑活跃的账号数，范围 1 到 {max}。', { max: v.daily_chat_web_concurrency_max || 10 }), min: 1, max: v.daily_chat_web_concurrency_max || 10 });
  const backoff = numRow(S, { key: 'daily_chat_web_retry_backoff', title: t('建会话失败退避（秒）'), help: t('创建会话失败后，第 N 轮等待 基数 × 2^(N-1)，单次上限取 30 秒与基数中较大者；0 为不退避。范围 0 到 {max}。', { max: v.daily_chat_web_retry_backoff_max || 60 }), min: 0, max: v.daily_chat_web_retry_backoff_max || 60 });
  const status = h('div.s-block.dc-status');
  S.bind((s) => {
    const retry = s.daily_chat_web_retry !== false;
    attempts.setDisabled(!retry);
    const n = retry ? s.daily_chat_web_attempts || 1 : 1;
    const base = s.daily_chat_web_timeout || 0, bo = s.daily_chat_web_retry_backoff || 0;
    const waits = Array.from({ length: n }, (_, i) => base * (i + 1));
    const total = waits.reduce((a, b) => a + b, 0);
    const back = bo === 0 ? t('不退避') : Array.from({ length: Math.max(1, n - 1) }, (_, i) => Math.min(bo * 2 ** i, Math.max(30, bo))).join(' / ');
    setText(status, [t('每轮等待：{seq} 秒', { seq: waits.join(' / ') }), t('失败退避：{seq} 秒', { seq: back }), t('最坏情况下，每个卡住的账号需要 {n} 秒。', { n: total })].join(' · '));
  });
  return [attempts.el, timeout.el, conc.el, backoff.el, status];
}

/* ---- fallback models ---- */
function fallbackRow(S, env) {
  const summary = h('div.dc-sum');
  const r = row({
    title: t('备选模型'), help: t('备选模型按「免费、便宜、昂贵」自动排序，逐个尝试一遍后在轮数允许时重复。留空使用默认免费集合。'),
    ctl: [summary, btn(t('选择模型…'), { size: 'sm', icon: 'list', onClick: () => { chooseDialog(); } })],
  });
  r.ctlEl.classList.add('wrap');
  function chooseDialog() {
    const pk = picker();
    const cur = S.val('daily_chat_web_fallback_models') || [];
    pk.preselect(cur);
    env.catalog('intl').then((list) => pk.setItems(list.filter((m) => m.id).map((m) => ({ value: m.id, label: m.id, sub: m.credits || '' })).sort((a, b) => a.value.localeCompare(b.value)))).catch(() => pk.setItems([]));
    const err = h('div');
    const m = dialog({
      title: t('选择备选模型'), size: 'narrow', body: [pk.el, err],
      actions: [btn(t('取消'), { onClick: () => m.close(false) }), btn(t('保存'), { variant: 'primary', onClick: async () => {
        const vals = pk.values();
        if (vals.length > 20) { err.replaceChildren(alertBox('warn', t('最多选择 20 个模型'))); return; }
        try { await S.save({ daily_chat_web_fallback_models: vals }); m.close(true); } catch (e) { err.replaceChildren(alertBox('bad', errMsg(e))); }
      } })],
    });
  }
  S.bind((v) => {
    const list = v.daily_chat_web_fallback_models || [];
    summary.replaceChildren(list.length ? badge(t('已选 {n} 个', { n: list.length }), 'accent', { sm: true, tip: list.join('\n') }) : badge(t('使用默认免费集合'), '', { sm: true, outline: true }));
  });
  return r;
}

/* ---- prompt pool ---- */
function poolRow(S) {
  const summary = h('div.dc-sum');
  const r = row({
    title: t('提示词池'), help: t('留空使用内置的 300 条。'),
    ctl: [summary, btn(t('编辑…'), { size: 'sm', icon: 'edit', onClick: () => { editDialog(); } })],
  });
  r.ctlEl.classList.add('wrap');
  function editDialog() {
    const ta = h('textarea.textarea.mono', { rows: '12', spellcheck: 'false', value: (S.val('daily_chat_web_prompts') || []).join('\n') });
    const err = h('div');
    const parse = () => [...new Set(ta.value.split(/\r?\n/).map((x) => x.trim()).filter(Boolean))];
    async function save(list) {
      err.replaceChildren();
      if (list.length > 500) { err.replaceChildren(alertBox('warn', t('最多 500 条提示词'))); return; }
      const badIdx = list.findIndex((x) => byteLen(x) > 200);
      if (badIdx >= 0) { err.replaceChildren(alertBox('warn', t('第 {n} 行超过 200 字节', { n: badIdx + 1 }))); return; }
      try { await S.save({ daily_chat_web_prompts: list }); m.close(true); } catch (e) { err.replaceChildren(alertBox('bad', errMsg(e))); }
    }
    const m = dialog({
      title: t('编辑提示词池'), size: 'wide', body: [field(null, ta, t('每行一条，最多 500 条，每条不超过 200 字节；重复行会合并。留空使用内置提示词。')), err],
      left: btn(t('清空（使用内置）'), { variant: 'ghost', onClick: () => save([]) }),
      actions: [btn(t('取消'), { onClick: () => m.close(false) }), btn(t('保存'), { variant: 'primary', onClick: () => save(parse()) })],
    });
  }
  S.bind((v) => {
    const n = (v.daily_chat_web_prompts || []).length;
    summary.replaceChildren(n ? badge(t('自定义 {n} 条', { n }), 'accent', { sm: true }) : badge(t('使用内置 300 条'), '', { sm: true, outline: true }));
  });
  return r;
}

/* ---- read-only fixed-hour tasks ---- */
function hoursRow(S) {
  const kv = h('dl.kv.dc-kv');
  const r = row({ title: t('固定整点任务'), help: t('这些任务按整点运行，在此只读显示。'), ctl: [], cls: 'stack-ctl' });
  r.ctlEl.appendChild(kv);
  const names = [['checkin', () => t('签到')], ['travel', () => t('出行')], ['keepalive', () => t('保活')], ['cat', () => t('猫咪')]];
  S.bind((v) => {
    const sc = v.schedule || {};
    kv.replaceChildren(...names.flatMap(([k, label]) => [h('dt', label()), h('dd.mono', (sc[k] || []).map(HH).join('  ') || '—')]));
  });
  return r;
}

export function dailyCard(S, env) {
  return section('daily', t('每日活跃'), { sub: t('国际版账号的每日网页活跃打卡与时间表') },
    switchRow(S, { key: 'daily_chat_web', title: t('附带网页渠道会话'), help: t('每日签到时，除桌面身份的对话外，再跑一个网页渠道会话。仅对国际版账号生效。') }).el,
    modelRow(S, env).el,
    timesRow(S).el,
    subhead(t('重试与并发')),
    switchRow(S, { key: 'daily_chat_web_retry', title: t('会话未完成时再试'), help: t('等待后网页会话仍在「工作中」时，再跑下一轮。') }).el,
    ...retryRows(S),
    subhead(t('备选模型与提示词')),
    switchRow(S, { key: 'daily_chat_web_fallback', title: t('失败后换备选模型'), help: t('每轮失败后改用备选列表里的下一个模型。') }).el,
    fallbackRow(S, env).el,
    switchRow(S, { key: 'daily_chat_web_random_prompt', title: t('随机提示词'), help: t('每次网页活跃随机挑一条提示词；关闭则始终发送「Hi」。') }).el,
    poolRow(S).el,
    hoursRow(S).el);
}
