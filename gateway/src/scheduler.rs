//! Background reward scheduler: hourly grid from `settings.schedule`, token
//! keepalive, CN check-in / cat travel / streak bonus, intl daily activity on
//! its own HH:MM slots (with a 12 h catch-up window and one start-up catch-up),
//! and a one-account-per-tick balance refresher. One cycle runs at a time.
use crate::{cn, daily, state::App, tasks};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::Semaphore;
use wbaccounts::Account;
use wbcore::{
    timeutil::{self, now},
    types::Realm,
};

const SLOT_GRACE_MINUTES: i64 = 720;
const LOG_LINES: usize = 60;
const CREDITS_TICK: u64 = 1800;
const CREDITS_PARK: f64 = 6.0 * 3600.0;

pub struct Scheduler {
    enabled: AtomicBool,
    running: AtomicBool,
    catchup: AtomicBool,
    last_run: Mutex<Option<String>>,
    logs: Mutex<VecDeque<String>>,
    slots_done: Mutex<(String, HashSet<String>)>,
    credits_parked: Mutex<HashMap<String, f64>>,
}
impl Default for Scheduler {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            running: AtomicBool::new(false),
            catchup: AtomicBool::new(true),
            last_run: Mutex::new(None),
            logs: Mutex::new(VecDeque::new()),
            slots_done: Mutex::new((String::new(), HashSet::new())),
            credits_parked: Mutex::new(HashMap::new()),
        }
    }
}

/// Releases the single-cycle flag even if the cycle future is dropped.
struct Running<'a>(&'a AtomicBool);
impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub struct Grid {
    pub checkin: Vec<u32>,
    pub travel: Vec<u32>,
    pub keepalive: Vec<u32>,
    pub cat: Vec<u32>,
    pub daily_chat: Vec<String>,
}
impl Grid {
    pub fn load(settings: &Value) -> Self {
        let hours = |key: &str| -> Vec<u32> {
            let mut v: Vec<u32> = settings["schedule"][key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|h| h.as_u64().filter(|h| *h < 24).map(|h| h as u32))
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        Self {
            checkin: hours("checkin"),
            travel: hours("travel"),
            keepalive: hours("keepalive"),
            cat: hours("cat"),
            daily_chat: settings["schedule"]["daily_chat"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect(),
        }
    }
    pub fn all_hours(&self) -> Vec<u32> {
        let mut all: Vec<u32> = [&self.checkin, &self.travel, &self.keepalive, &self.cat]
            .into_iter()
            .flatten()
            .copied()
            .collect();
        all.sort_unstable();
        all.dedup();
        all
    }
}
fn minutes(slot: &str) -> Option<i64> {
    let (h, m) = slot.split_once(':')?;
    let (h, m): (i64, i64) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}
/// The slot due at `current` minutes of the day, given the slots already run.
pub fn due_slot(slots: &[String], done: &HashSet<String>, current: i64) -> Option<String> {
    slots.iter().find_map(|slot| {
        let target = minutes(slot)?;
        (!done.contains(slot) && current >= target && current - target <= SLOT_GRACE_MINUTES)
            .then(|| slot.clone())
    })
}
fn keepalive_lead() -> f64 {
    std::env::var("WB_KEEPALIVE_LEAD_HOURS")
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|h| (1.0..=720.0).contains(h))
        .unwrap_or(48.0)
        * 3600.0
}
pub fn keepalive_due(expires_at: f64, at: f64, lead: f64) -> bool {
    expires_at <= 0.0 || expires_at - at < lead
}

impl Scheduler {
    pub fn log(&self, app: &App, message: &str) {
        let line = format!("[{}] {message}", timeutil::local_stamp(now()));
        {
            let mut logs = self.logs.lock();
            if logs.len() >= LOG_LINES {
                logs.pop_front();
            }
            logs.push_back(line);
        }
        let task = [
            "签到", "打卡", "活跃", "猫猫", "连登", "夜猫", "抽奖", "兑换", "任务",
        ]
        .iter()
        .any(|k| message.contains(k));
        let level = if message.starts_with('!') {
            "WARN"
        } else {
            "INFO"
        };
        app.log_tagged(
            level,
            if task { "tasks" } else { "scheduler" },
            &format!("[调度器] {message}"),
        );
    }
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
    pub fn toggle(&self) -> bool {
        !self.enabled.fetch_xor(true, Ordering::AcqRel)
    }
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Run one cycle unless another is in progress.
    pub async fn execute(&self, app: &Arc<App>, reason: &str, force: bool, daily_only: bool) {
        if self
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.log(app, &format!("跳过本次巡检 ({reason})：上一轮仍在执行"));
            return;
        }
        let _running = Running(&self.running);
        *self.last_run.lock() = Some(timeutil::local_stamp(now()));
        self.log(app, &format!("开始执行任务 ({reason})..."));
        self.cycle(app, force, daily_only).await;
    }

    async fn cycle(&self, app: &Arc<App>, force: bool, daily_only: bool) {
        let accounts = app.pool.accounts();
        if accounts.is_empty() {
            self.log(app, "暂无可用的活跃账号，跳过本次巡检");
            return;
        }
        let grid = Grid::load(&app.settings.current());
        let hour = timeutil::local_hour(now());
        let at = |hours: &[u32]| !daily_only && (force || hours.contains(&hour));
        let (run_checkin, run_travel, run_keepalive, run_cat) = (
            at(&grid.checkin),
            at(&grid.travel),
            at(&grid.keepalive),
            at(&grid.cat),
        );
        let catchup = self.catchup.swap(false, Ordering::AcqRel)
            && app.settings.current()["daily_chat_startup_catchup"] != false;
        let run_daily = force || daily_only || catchup;
        let lead = keepalive_lead();
        let mut counts = [0usize; 4];
        let mut daily_targets = Vec::new();
        let mut first = true;
        for account in accounts {
            let tasks_on = account.tasks_enabled();
            let cn_work = account.realm() == Realm::Cn
                && tasks_on
                && !tasks::is_enterprise(&account)
                && (run_checkin || run_travel || run_cat);
            let expires = account.to_json()["expiresAt"].as_f64().unwrap_or(0.0);
            let keepalive = run_keepalive
                && !account.cred().is_api_key()
                && keepalive_due(expires, now(), lead);
            let daily = account.realm() == Realm::Intl
                && tasks_on
                && run_daily
                && daily::can_daily_chat(&account);
            if !(cn_work || keepalive || daily) {
                continue;
            }
            let name = tasks::short(&account);
            let wait = tasks::blocked_for(app, &account);
            if wait > 0.0 {
                self.log(
                    app,
                    &format!(
                        "跳过受限账号 [{name}]：风控暂停剩余 {}s",
                        wait.ceil() as u64
                    ),
                );
                continue;
            }
            if daily {
                daily_targets.push(account.clone());
            }
            if !(cn_work || keepalive) {
                continue;
            }
            // Stagger accounts that actually talk to the upstream (issue #394).
            if !first {
                let pause = 2.0 + rand::random::<f64>() * 3.0;
                tokio::time::sleep(Duration::from_secs_f64(pause)).await;
            }
            first = false;
            if keepalive {
                self.log(
                    app,
                    &format!("账号 [{name}] Token 即将到期或到期时间未知，执行主动保活刷新..."),
                );
                match crate::dispatch::refresh_account(app, &account, true).await {
                    Ok(()) => {
                        counts[0] += 1;
                        self.log(app, &format!("✓ 账号 [{name}] Token 自动保活刷新成功"));
                    }
                    Err(e) => self.log(app, &format!("! 账号 [{name}] Token 保活刷新失败: {e}")),
                }
            }
            if cn_work {
                self.cn_account(app, &account, run_checkin, run_travel, run_cat, &mut counts)
                    .await;
            }
        }
        let daily_count = if daily_targets.is_empty() {
            0
        } else {
            self.daily_batch(app, daily_targets).await
        };
        self.log(app, &format!("巡检完成：Token保活 {} 个，国内签到 {} 个，猫猫日常 {} 个，成长任务领奖 {} 个，国际活跃 {daily_count} 个",
            counts[0], counts[1], counts[2], counts[3]));
    }

    async fn cn_account(
        &self,
        app: &Arc<App>,
        account: &Arc<Account>,
        checkin: bool,
        travel: bool,
        cat: bool,
        counts: &mut [usize; 4],
    ) {
        let name = tasks::short(account);
        let gap = || tokio::time::sleep(Duration::from_secs(1));
        if checkin && cn::can_checkin(account) {
            self.log(
                app,
                &format!("检测到国内版账号 [{name}] 今日尚未签到，执行自动签到..."),
            );
            let r = cn::checkin(app, account, "scheduler").await;
            if r["ok"] == true {
                counts[1] += 1;
                self.log(
                    app,
                    &format!(
                        "✓ 账号 [{name}] 自动签到成功: {}",
                        r["msg"].as_str().unwrap_or("")
                    ),
                );
            } else {
                let why = r["error"].as_str().or(r["msg"].as_str()).unwrap_or("");
                self.log(app, &format!("! 账号 [{name}] 自动签到未成功: {why}"));
            }
            gap().await;
        }
        if travel {
            let r = cn::cat_travel(app, account).await;
            if matches!(r["action"].as_str(), Some("claim" | "depart")) {
                counts[2] += 1;
            }
            self.log(
                app,
                &format!(
                    "🐱 账号 [{name}] 猫猫日常处理: {}",
                    r["msg"].as_str().unwrap_or("")
                ),
            );
            gap().await;
            let bonus = cn::streak_bonus(app, account).await;
            for line in bonus["logs"].as_array().into_iter().flatten() {
                self.log(
                    app,
                    &format!("★ 账号 [{name}] {}", line.as_str().unwrap_or("")),
                );
            }
            gap().await;
        }
        if cat {
            let (logs, earned) = cn::run_growth(app, account, true).await;
            if earned > 0.0 {
                counts[3] += 1;
            }
            for line in logs.iter().skip(1) {
                self.log(app, &format!("账号 [{name}] {line}"));
            }
            gap().await;
        }
    }

    /// Intl daily activity, `daily_chat_web_concurrency` accounts at a time.
    async fn daily_batch(&self, app: &Arc<App>, targets: Vec<Arc<Account>>) -> usize {
        let limit = app.settings.current()["daily_chat_web_concurrency"]
            .as_u64()
            .unwrap_or(1)
            .clamp(1, 10) as usize;
        let gate = Arc::new(Semaphore::new(limit));
        let mut jobs = Vec::new();
        for (i, account) in targets.into_iter().enumerate() {
            let (app, gate) = (app.clone(), gate.clone());
            let offset = if limit > 1 {
                (i as f64 * 0.3).min(2.0)
            } else {
                0.0
            };
            jobs.push(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs_f64(offset)).await;
                let _permit = gate.acquire_owned().await.ok()?;
                let name = tasks::short(&account);
                let sched = app.scheduler.clone();
                sched.log(
                    &app,
                    &format!("检测到国际版账号 [{name}] 今日尚未活跃，执行每日活跃打卡对话..."),
                );
                let r = daily::daily_chat(&app, &account, "scheduler").await;
                if r["ok"] == true {
                    sched.log(
                        &app,
                        &format!(
                            "✓ 账号 [{name}] 每日活跃对话成功: {}",
                            r["msg"].as_str().unwrap_or("")
                        ),
                    );
                } else {
                    sched.log(
                        &app,
                        &format!(
                            "! 账号 [{name}] 每日活跃对话失败: {}",
                            r["error"].as_str().unwrap_or("")
                        ),
                    );
                }
                if limit == 1 {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                }
                Some(r["ok"] == true)
            }));
        }
        let mut ok = 0;
        for job in jobs {
            if matches!(job.await, Ok(Some(true))) {
                ok += 1;
            }
        }
        ok
    }

    fn take_slot(&self, grid: &Grid, at: f64) -> Option<String> {
        let day = timeutil::local_day_key(at);
        let current = (timeutil::local_hour(at) * 60 + timeutil::local_minute(at)) as i64;
        let mut done = self.slots_done.lock();
        if done.0 != day {
            *done = (day, HashSet::new());
        }
        let slot = due_slot(&grid.daily_chat, &done.1, current)?;
        done.1.insert(slot.clone());
        Some(slot)
    }

    pub fn next_run(&self, grid: &Grid, at: f64) -> Option<String> {
        let all = grid.all_hours();
        let hour = timeutil::local_hour(at);
        let midnight = timeutil::local_midnight(at, 0);
        let next = all
            .iter()
            .find(|h| **h > hour)
            .map(|h| midnight + *h as f64 * 3600.0)
            .or_else(|| {
                all.first()
                    .map(|h| timeutil::local_midnight(at, -1) + *h as f64 * 3600.0)
            })?;
        Some(timeutil::fmt_local(next, "%Y-%m-%d %H:00:00"))
    }

    pub fn status(&self, app: &App) -> Value {
        let grid = Grid::load(&app.settings.current());
        let hhmm = |hours: &[u32]| {
            hours
                .iter()
                .map(|h| format!("{h:02}:00"))
                .collect::<Vec<_>>()
                .join("/")
        };
        let mode_cn = format!(
            "整点排程 (签到 {} · 猫猫旅行与连登 {} · Token保活 {} · 成长任务领奖 {})",
            hhmm(&grid.checkin),
            hhmm(&grid.travel),
            hhmm(&grid.keepalive),
            hhmm(&grid.cat)
        );
        let mode_intl = format!(
            "每日活跃 {} · Token保活 {}",
            grid.daily_chat.join("/"),
            hhmm(&grid.keepalive)
        );
        let logs: Vec<String> = self
            .logs
            .lock()
            .iter()
            .rev()
            .take(20)
            .rev()
            .cloned()
            .collect();
        let active = !app.args.offline && app.args.test_upstream.is_none();
        json!({"supported":true,"active":active,"enabled":self.enabled(),"running":self.is_running(),
            "mode":mode_cn,"mode_cn":mode_cn,"mode_intl":mode_intl,
            "last_run_time":self.last_run.lock().clone().unwrap_or_else(|| "尚未运行".into()),
            "next_run_time":self.next_run(&grid, now()).unwrap_or_else(|| "待调度".into()),
            "daily_chat_times":grid.daily_chat,"windows":[],"quota_scans":[],"logs":logs})
    }

    /// One stalest balance per tick; failures are parked for 6 h.
    async fn credits_tick(&self, app: &Arc<App>) {
        let ttl = app.settings.current()["credits_refresh_hours"]
            .as_f64()
            .unwrap_or(12.0);
        if ttl <= 0.0 {
            return;
        }
        let at = now();
        self.credits_parked.lock().retain(|_, until| *until > at);
        let parked = self.credits_parked.lock().clone();
        let stalest = app
            .pool
            .accounts()
            .into_iter()
            .filter(|a| {
                a.enabled() && !a.cred().access_token.is_empty() && !parked.contains_key(a.uid())
            })
            .filter(|a| tasks::blocked_for(app, a) <= 0.0)
            .map(|a| {
                let updated = a.public_metadata()["credits"]["updated_at"]
                    .as_f64()
                    .unwrap_or(0.0);
                (updated, a)
            })
            .filter(|(updated, _)| at - updated >= ttl * 3600.0)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let Some((_, account)) = stalest else {
            return;
        };
        let result = crate::billing::refresh_credits(app, &account).await;
        if result["ok"] != true {
            self.credits_parked
                .lock()
                .insert(account.uid().to_owned(), at + CREDITS_PARK);
        }
    }
}

/// The scheduler task: started once with the gateway, stops on shutdown.
pub async fn run(app: Arc<App>) {
    let mut shutdown = app.shutdown.subscribe();
    let sched = app.scheduler.clone();
    sched.log(&app, "后台定时调度器已启动");
    tokio::select! {_ = shutdown.changed() => return, _ = tokio::time::sleep(Duration::from_secs(10)) => {}}
    sched
        .execute(&app, "启动初次初始化巡检", false, false)
        .await;
    let mut credits_at = tokio::time::Instant::now();
    let mut fired_hour: Option<(String, u32)> = None;
    loop {
        tokio::select! {_ = shutdown.changed() => break, _ = tokio::time::sleep(Duration::from_secs(30)) => {}}
        if credits_at.elapsed() >= Duration::from_secs(CREDITS_TICK) {
            credits_at = tokio::time::Instant::now();
            let (app, sched) = (app.clone(), sched.clone());
            tokio::spawn(async move { sched.credits_tick(&app).await });
        }
        if !sched.enabled() {
            continue;
        }
        let at = now();
        let grid = Grid::load(&app.settings.current());
        if let Some(slot) = sched.take_slot(&grid, at) {
            let (app, sched) = (app.clone(), sched.clone());
            tokio::spawn(async move {
                sched
                    .execute(&app, &format!("网页活跃定时命中 ({slot})"), false, true)
                    .await;
            });
        }
        let (day, hour) = (timeutil::local_day_key(at), timeutil::local_hour(at));
        let key = Some((day, hour));
        if timeutil::local_minute(at) == 0 && grid.all_hours().contains(&hour) && fired_hour != key
        {
            fired_hour = key;
            let (app, sched) = (app.clone(), sched.clone());
            tokio::spawn(async move {
                sched
                    .execute(&app, &format!("整点排程命中 ({hour}:00)"), false, false)
                    .await;
            });
        }
    }
}

/// `POST /scheduler/trigger`: every step for every account, once.
pub fn trigger(app: &Arc<App>) -> Value {
    let sched = app.scheduler.clone();
    if sched.is_running() {
        return json!({"ok":false,"msg":"已有巡检正在执行，请稍候再试"});
    }
    let app = app.clone();
    tokio::spawn(async move { sched.execute(&app, "手动立即触发", true, false).await });
    json!({"ok":true,"msg":"已触发后台调度执行"})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slots_fire_once_within_the_grace_window() {
        let slots = vec!["08:30".to_owned(), "20:15".to_owned()];
        let mut done = HashSet::new();
        assert_eq!(due_slot(&slots, &done, 8 * 60 + 29), None);
        assert_eq!(
            due_slot(&slots, &done, 8 * 60 + 30).as_deref(),
            Some("08:30")
        );
        done.insert("08:30".to_owned());
        assert_eq!(due_slot(&slots, &done, 19 * 60), None);
        assert_eq!(due_slot(&slots, &done, 23 * 60).as_deref(), Some("20:15"));
        assert_eq!(
            due_slot(&["00:10".to_owned()], &HashSet::new(), 13 * 60),
            None
        );
    }
    #[test]
    fn keepalive_lead_and_unknown_expiry() {
        assert!(keepalive_due(0.0, 100.0, 10.0));
        assert!(keepalive_due(105.0, 100.0, 10.0));
        assert!(!keepalive_due(200.0, 100.0, 10.0));
    }
    #[test]
    fn grid_reads_and_sorts_hours() {
        let g = Grid::load(
            &json!({"schedule":{"checkin":[21,9,9],"travel":[9],"keepalive":[22],"cat":[1,30],"daily_chat":["08:30"]}}),
        );
        assert_eq!(g.checkin, vec![9, 21]);
        assert_eq!(g.cat, vec![1]);
        assert_eq!(g.all_hours(), vec![1, 9, 21, 22]);
    }
}
