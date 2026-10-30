// China-region view: Buddy gas station, streak, cat travel and growth numbers plus the four task actions.
import { h, setText } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { add, t } from '../lib/i18n.js';
import * as F from '../lib/fmt.js';
import { kpi, btn, card, checkbox } from '../lib/ui.js';
import { defs } from './tasks-actions.js';
import { can, gate, isDone, num, catState, taskView } from './tasks-model.js';

add({
  '本期 Buddy 加油站': 'Buddy gas station', '当前连续登录天数': 'Login streak', '补签卡余额': 'Make-up cards', '成长抽奖次数': 'Lottery chances',
  '猫猫旅行状态': 'Cat travel', '成长能量余额': 'Growth energy', '成长任务进度': 'Growth task progress', '已获取福利总积分': 'Total reward credits',
  '无国内账号': 'No China accounts', '今日已领': 'Claimed today', '部分已领': 'Partly claimed', '今日未领': 'Not claimed yet',
  '天': 'days', '张': 'cards', '次': 'times', '兑换档位：{list}': 'Redemption tiers: {list}', '待领奖 {n}': 'Ready to claim {n}', '全部已处理': 'All handled',
  '任务操作': 'Task actions', '操作范围：{s}': 'Scope: {s}', '加载中…': 'Loading…', '查询失败': 'Query failed',
  '领取今日 Buddy 加油站签到奖励。已签到的账号会返回「今日已签到」，企业版和 API Key 账号会被跳过。': 'Claims today’s Buddy gas-station check-in. Accounts already checked in report that, and enterprise and API-key accounts are skipped.',
  '接取尚未接取的任务；上游已判定完成的任务才会领取奖励，不会伪造任务进度。': 'Accepts tasks that are not accepted yet, then claims tasks the upstream already reports as finished. No progress is faked.',
  '同时领取已达成任务的奖励': 'Also claim rewards of finished tasks',
  '猫猫归来时领取旅行奖励，在家待命时派它出发；每天的旅行次数有上限。': 'Claims the reward when the cat is back and sends it out when it is idle. There is a daily limit.',
  '昨天漏签且有补签卡时自动补签，兑换连登档位，并用完全部抽奖次数（最多 50 次）。': 'Uses a make-up card if yesterday was missed, redeems streak tiers and spends all lottery chances (up to 50).',
});

const AUTO_KEY = 'wb-growth-auto-claim';
const autoClaim = () => { try { return localStorage.getItem(AUTO_KEY) !== '0'; } catch { return true; } };
const fmtNum = (v) => (num(v) == null ? '—' : F.int(num(v)));

export function cnSection(store, growth) {
  const k = {
    checkin: kpi({ label: t('本期 Buddy 加油站'), icon: 'gift' }),
    streak: kpi({ label: t('当前连续登录天数'), icon: 'calendar' }),
    makeup: kpi({ label: t('补签卡余额'), icon: 'tag' }),
    lottery: kpi({ label: t('成长抽奖次数'), icon: 'sparkle' }),
    cat: kpi({ label: t('猫猫旅行状态'), icon: 'send' }),
    energy: kpi({ label: t('成长能量余额'), icon: 'zap' }),
    progress: kpi({ label: t('成长任务进度'), icon: 'list' }),
    reward: kpi({ label: t('已获取福利总积分'), icon: 'coins' }),
  };
  const vCheckin = k.checkin.el.querySelector('.kpi-value');
  const vCat = k.cat.el.querySelector('.kpi-value');
  const kpis = h('div.grid.cols-4', ...Object.values(k).map((x) => x.el));

  function paintKpis() {
    const { target } = store.scope();
    const eligible = target.filter((a) => can('checkin', a).ok);
    const n = eligible.length;
    const states = eligible.map((a) => store.statusOf(a));
    const done = states.filter(isDone).length;
    const failedN = states.filter((s) => s.key === 'failed').length;
    if (!n) { k.checkin.set({ value: '—', foot: t('无国内账号') }); vCheckin.style.color = ''; }
    else {
      const label = done === n ? t('今日已领') : done ? t('部分已领') : t('今日未领');
      k.checkin.set({ value: `${done}/${n}`, foot: failedN ? `${label} · ${t('{n} 个失败', { n: failedN })}` : label });
      vCheckin.style.color = done === n ? 'var(--good)' : done ? 'var(--accent)' : 'var(--warn)';
    }

    const st = growth.state();
    const d = st.data;
    const sm = (d && d.summary) || {};
    const travel = sm.travel || {};
    // zeros without any other field mean the upstream calls failed, not that the balance is 0
    const blind = !d || sm.energy === undefined || (d.msg && travel.state === 'unknown');
    const wait = d && d.supported === false ? t('本网关版本不提供') : d || st.status === 'error' ? t('查询失败') : st.status === 'loading' ? t('加载中…') : ' ';
    const val = (v) => (blind ? '—' : fmtNum(v));
    k.streak.set({ value: val(sm.streak_days), unit: blind ? '' : t('天'), foot: blind ? wait : tiersText(sm.tiers) });
    k.makeup.set({ value: val(sm.makeup_cards), unit: blind || num(sm.makeup_cards) == null ? '' : t('张'), foot: blind ? wait : ' ' });
    k.lottery.set({ value: val(sm.lottery_chances), unit: blind || num(sm.lottery_chances) == null ? '' : t('次'), foot: blind ? wait : ' ' });
    k.energy.set({ value: val(sm.energy), foot: blind ? wait : ' ' });
    const cat = blind ? null : catState(travel);
    k.cat.set({ value: cat ? cat.label : '—', foot: cat ? cat.desc || ' ' : wait });
    vCat.style.fontSize = '22px';
    vCat.style.color = cat && cat.kind === 'good' ? 'var(--good)' : cat && cat.kind === 'info' ? 'var(--info)' : '';

    const list = d && Array.isArray(d.tasks) ? d.tasks : null;
    if (list && list.length) {
      const views = list.map(taskView);
      const claimable = views.filter((x) => x.claimable).length;
      k.progress.set({ value: `${views.filter((x) => x.key === 'claimed').length}/${list.length}`, foot: claimable ? t('待领奖 {n}', { n: claimable }) : t('全部已处理') });
    } else k.progress.set({ value: '—', foot: blind ? wait : ' ' });

    const g = store.grantTotal('cn');
    k.reward.set({ value: g.value, foot: g.note });
  }

  function tiersText(tiers) {
    const names = (Array.isArray(tiers) ? tiers : []).map((x) => (x && typeof x === 'object' ? x.tier ?? x.days ?? x.name : x)).filter((x) => x != null && x !== '').map(String);
    return names.length ? t('兑换档位：{list}', { list: names.slice(0, 6).join(' · ') }) : ' ';
  }

  /* ---- actions ---- */
  function verdictFor(action) {
    const { list, target } = store.scope();
    if (!list.length) return { ok: false, why: t('还没有国内版账号') };
    if (store.sel !== 'all') return can(action, target[0]);
    return target.some((a) => can(action, a).ok) ? { ok: true, why: '' } : { ok: false, why: t('没有已开启任务的国内版账号') };
  }
  const exec = (def) => (e, el) => store.exec(def(store.sel), el);
  const checkinBtn = btn(t('Buddy 加油站签到'), { variant: 'primary', icon: 'check', onClick: exec(defs.checkin) });
  const runBtn = btn(t('接取并领取已完成任务'), { icon: 'play', onClick: (e, el) => store.exec(defs.growthRun(store.sel, auto.input.checked), el) });
  const travelBtn = btn(t('猫猫旅行'), { icon: 'send', onClick: exec(defs.travel) });
  const streakBtn = btn(t('连登管家与抽奖'), { icon: 'sparkle', onClick: exec(defs.streak) });
  const auto = checkbox({ checked: autoClaim(), label: t('同时领取已达成任务的奖励'), onChange: (v) => { try { localStorage.setItem(AUTO_KEY, v ? '1' : '0'); } catch { /* blocked */ } } });

  const tile = (ic, title, desc, ...ctl) => h('div.tk-act', h('h3', icon(ic, 16, 'ic'), title), h('p', desc), h('div.tk-act-ctl', ...ctl));
  const scope = h('div.tk-scope');
  const why = h('div.tk-why', { hidden: true });
  const actions = card(t('任务操作'), { actions: [scope] },
    h('div.tk-actions',
      tile('gift', t('Buddy 加油站签到'), t('领取今日 Buddy 加油站签到奖励。已签到的账号会返回「今日已签到」，企业版和 API Key 账号会被跳过。'), checkinBtn),
      tile('list', t('接取并领取已完成任务'), t('接取尚未接取的任务；上游已判定完成的任务才会领取奖励，不会伪造任务进度。'), auto, runBtn),
      tile('send', t('猫猫旅行'), t('猫猫归来时领取旅行奖励，在家待命时派它出发；每天的旅行次数有上限。'), travelBtn),
      tile('sparkle', t('连登管家与抽奖'), t('昨天漏签且有补签卡时自动补签，兑换连登档位，并用完全部抽奖次数（最多 50 次）。'), streakBtn)),
    why);

  function paintActions() {
    const vCheck = verdictFor('checkin'), vGrowth = verdictFor('growth');
    gate(checkinBtn, store, vCheck);
    for (const b of [runBtn, travelBtn, streakBtn]) gate(b, store, vGrowth);
    auto.input.disabled = !vGrowth.ok;
    const bad = !vCheck.ok ? vCheck : !vGrowth.ok ? vGrowth : null;
    setText(why, bad ? bad.why : '');
    why.hidden = !bad;
    setText(scope, t('操作范围：{s}', { s: store.scopeText() }));
  }

  return { top: [kpis, actions], paint() { paintKpis(); paintActions(); } };
}
