//! Panel routes for the scheduler and reward tasks. The router has already
//! verified the panel session before dispatching here.
use crate::{
    cn, daily, scheduler,
    state::App,
    tasks::{self, display_name},
    wire::{self, Query, Reply},
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};
use wbaccounts::Account;
use wbcore::types::Realm;

const TASKS_TTL: Duration = Duration::from_secs(20);
static SNAPSHOTS: LazyLock<Mutex<HashMap<String, (Instant, Value)>>> =
    LazyLock::new(Default::default);

fn invalidate() {
    SNAPSHOTS.lock().clear();
}
fn reply(value: Value) -> Reply {
    crate::panel::ok(value)
}
fn fail(status: u16, message: &str) -> Reply {
    wire::error(status, message, false)
}

pub async fn get(app: Arc<App>, path: &str, q: &Query) -> Option<Reply> {
    Some(match path {
        "/scheduler" => reply(app.scheduler.status(&app)),
        "/activity/history" => match crate::activity::query(&app.args.usage_dir, q) {
            Ok(v) => reply(v),
            Err(message) => fail(400, &message),
        },
        "/tasks" => growth_view(&app, q.get("uid").map(String::as_str).unwrap_or("")).await,
        _ => return None,
    })
}

pub async fn post(app: Arc<App>, path: &str, payload: &Value) -> Option<Reply> {
    if !matches!(
        path,
        "/scheduler/trigger"
            | "/scheduler/toggle"
            | "/tasks/run"
            | "/tasks/claim"
            | "/tasks/travel"
            | "/tasks/streak-bonus"
            | "/accounts/checkin"
            | "/accounts/daily-chat"
            | "/accounts/daily-chat-web"
    ) {
        return None;
    }
    if payload.get("uid").is_some_and(|v| !v.is_string()) {
        return Some(fail(400, "uid must be a string"));
    }
    let uid = payload["uid"].as_str().unwrap_or("");
    Some(match path {
        "/scheduler/trigger" => reply(scheduler::trigger(&app)),
        "/scheduler/toggle" => {
            let enabled = app.scheduler.toggle();
            let line = if enabled {
                "后台定时调度器已启用"
            } else {
                "后台定时调度器已暂停"
            };
            app.scheduler.log(&app, line);
            let mut status = app.scheduler.status(&app);
            status["ok"] = json!(true);
            reply(status)
        }
        "/accounts/checkin" => checkin(&app, uid).await,
        "/accounts/daily-chat" => daily_chat(&app, uid, payload.get("uids"), false).await,
        "/accounts/daily-chat-web" => daily_chat(&app, uid, None, true).await,
        "/tasks/claim" => claim(&app, uid, payload["code"].as_str().unwrap_or("").trim()).await,
        _ => cn_batch(&app, path, uid, payload["claim"] != false).await,
    })
}

/// One named account of `realm`, or every task-enabled account of it.
fn targets(app: &App, realm: Realm, uid: &str) -> Result<Vec<Arc<Account>>, &'static str> {
    if !uid.is_empty() && uid != "all" {
        return match app.pool.get(uid) {
            Some(a) if a.realm() == realm => Ok(vec![a]),
            _ if realm == Realm::Cn => Err("未找到指定的国内版账号"),
            _ => Err("未找到指定的国际版账号"),
        };
    }
    Ok(app
        .pool
        .accounts()
        .into_iter()
        .filter(|a| a.realm() == realm && a.tasks_enabled())
        .collect())
}
fn account_views(app: &App) -> Vec<Value> {
    app.pool
        .accounts()
        .iter()
        .map(|a| a.public_metadata())
        .collect()
}
fn row(account: &Account, mut result: Value) -> Value {
    result["uid"] = json!(account.uid());
    result["nickname"] = account.public_metadata()["nickname"].clone();
    result
}
fn log_result(app: &App, account: &Account, what: &str, result: &Value) {
    let ok = result["ok"] == true;
    let detail = if ok {
        result["msg"].as_str()
    } else {
        result["error"].as_str().or(result["msg"].as_str())
    };
    app.log_tagged(
        if ok { "INFO" } else { "WARN" },
        "tasks",
        &format!(
            "[任务福利] 账号 [{}] {what}: {}",
            display_name(account),
            detail.unwrap_or("")
        ),
    );
}

async fn checkin(app: &Arc<App>, uid: &str) -> Reply {
    invalidate();
    let accounts = match targets(app, Realm::Cn, uid) {
        Ok(v) => v,
        Err(m) => return fail(404, m),
    };
    let mut results = Vec::new();
    for account in accounts.iter().filter(|a| !tasks::is_enterprise(a)) {
        let result = cn::checkin(app, account, "manual").await;
        log_result(app, account, "每日签到", &result);
        results.push(row(account, result));
    }
    reply(json!({"results":results,"accounts":account_views(app)}))
}

async fn daily_chat(app: &Arc<App>, uid: &str, uids: Option<&Value>, web_only: bool) -> Reply {
    let accounts = match uids {
        // An explicit list (even empty) selects exactly those accounts.
        Some(Value::Array(list)) if uid.is_empty() => {
            let wanted: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
            targets(app, Realm::Intl, "")
                .unwrap_or_default()
                .into_iter()
                .filter(|a| wanted.contains(&a.uid()))
                .collect()
        }
        Some(v) if !v.is_array() && !v.is_null() => return fail(400, "uids must be a list"),
        _ => match targets(app, Realm::Intl, uid) {
            Ok(v) => v,
            Err(m) => return fail(404, m),
        },
    };
    let mut results = Vec::new();
    for account in &accounts {
        let result = if web_only {
            daily::daily_chat_web(app, account, "manual").await
        } else {
            daily::daily_chat(app, account, "manual").await
        };
        log_result(
            app,
            account,
            if web_only {
                "网页通道打卡"
            } else {
                "每日活跃打卡"
            },
            &result,
        );
        results.push(row(account, result));
    }
    reply(json!({"results":results,"accounts":account_views(app)}))
}

async fn claim(app: &Arc<App>, uid: &str, code: &str) -> Reply {
    if uid.is_empty() || uid == "all" {
        return reply(json!({"ok":false,"msg":"请先选择一个国内版账号"}));
    }
    let account = match targets(app, Realm::Cn, uid) {
        Ok(mut v) if !v.is_empty() => v.remove(0),
        _ => return reply(json!({"ok":false,"msg":"未找到指定的国内版账号"})),
    };
    invalidate();
    if let Err(e) = tasks::prepare(app, &account).await {
        return reply(json!({"ok":false,"msg":e.message}));
    }
    let mut result = cn::claim_task(app, &account, code).await;
    if result["ok"] == true && result["msg"].is_null() {
        result["msg"] = json!("领取成功");
    }
    reply(result)
}

async fn cn_batch(app: &Arc<App>, path: &str, uid: &str, claim: bool) -> Reply {
    invalidate();
    let accounts = match targets(app, Realm::Cn, uid) {
        Ok(v) if !v.is_empty() => v,
        Ok(_) => return reply(json!({"ok":false,"msg":"未找到参与任务的国内版账号"})),
        Err(m) => return reply(json!({"ok":false,"msg":m})),
    };
    let mut logs = Vec::new();
    let mut lines = Vec::new();
    let mut results = Vec::new();
    let mut credit = 0.0;
    for (i, account) in accounts.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        let name = display_name(account);
        if let Err(e) = tasks::prepare(app, account).await {
            lines.push(format!("{name}: {}", e.message));
            logs.push(format!("! [{name}] {}", e.message));
            continue;
        }
        match path {
            "/tasks/run" => {
                logs.push(format!(
                    "====== 账号 [{name}] ({}/{}) ======",
                    i + 1,
                    accounts.len()
                ));
                let (account_logs, earned) = cn::run_growth(app, account, claim).await;
                credit += earned;
                logs.extend(account_logs.into_iter().map(|l| format!("  {l}")));
            }
            "/tasks/travel" => {
                let r = cn::cat_travel(app, account).await;
                lines.push(format!("{name}: {}", r["msg"].as_str().unwrap_or("")));
                results.push(
                    json!({"uid":account.uid(),"nickname":name,"action":r["action"],
                    "msg":r["msg"],"reward_credit":r["credit"]}),
                );
                log_result(
                    app,
                    account,
                    "猫猫旅行",
                    &json!({"ok":r["ok"],"msg":r["msg"],"error":r["msg"]}),
                );
            }
            _ => {
                let r = cn::streak_bonus(app, account).await;
                lines.push(format!("{name}: {}", r["msg"].as_str().unwrap_or("")));
                log_result(app, account, "连登管家与抽奖", &r);
            }
        }
    }
    if path == "/tasks/run" {
        logs.push(format!(
            "====== 全部 {} 个账号任务执行完毕，累计新增积分: +{credit} ======",
            accounts.len()
        ));
        app.log_tagged(
            "INFO",
            "tasks",
            &format!(
                "[任务福利] 成长任务执行完成，{} 个账号，新增积分 +{credit}",
                accounts.len()
            ),
        );
        return reply(
            json!({"ok":true,"credit_added":credit,"logs":logs,"accounts_count":accounts.len()}),
        );
    }
    reply(
        json!({"ok":true,"msg":lines.join("\n"),"results":results,"accounts_count":accounts.len()}),
    )
}

async fn growth_view(app: &Arc<App>, uid: &str) -> Reply {
    let accounts: Vec<Arc<Account>> = targets(app, Realm::Cn, "")
        .unwrap_or_default()
        .into_iter()
        .filter(|a| !tasks::is_enterprise(a))
        .collect();
    if accounts.is_empty() {
        return reply(
            json!({"supported":true,"tasks":[],"summary":{},"accounts":[],"msg":"未找到参与任务的国内版账号"}),
        );
    }
    let account = accounts
        .iter()
        .find(|a| a.uid() == uid)
        .unwrap_or(&accounts[0])
        .clone();
    let list: Vec<Value> = accounts
        .iter()
        .map(|a| json!({"uid":a.uid(),"nickname":display_name(a)}))
        .collect();
    let cached = SNAPSHOTS
        .lock()
        .get(account.uid())
        .filter(|(at, _)| at.elapsed() < TASKS_TTL)
        .map(|(_, v)| v.clone());
    let snapshot = match cached {
        Some(v) => v,
        None => {
            if let Err(e) = tasks::prepare(app, &account).await {
                return reply(
                    json!({"supported":true,"tasks":[],"summary":{},"accounts":list,"msg":e.message}),
                );
            }
            let (list_result, summary) = tokio::join!(
                cn::growth_tasks(app, &account),
                cn::growth_summary(app, &account)
            );
            let value = json!({"tasks":list_result.as_ref().map(|v| json!(v)).unwrap_or(json!([])),
                "summary":summary,"msg":list_result.err().map(|e| e.message)});
            SNAPSHOTS
                .lock()
                .insert(account.uid().to_owned(), (Instant::now(), value.clone()));
            value
        }
    };
    let mut out = snapshot;
    out["supported"] = json!(true);
    out["accounts"] = json!(list);
    out["account"] = account.public_metadata();
    reply(out)
}
