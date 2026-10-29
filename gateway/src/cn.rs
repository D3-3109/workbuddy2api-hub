//! CN growth-center actions: daily check-in, cat travel, streak bonus, task
//! accept/claim and the read-only growth snapshot.
use crate::state::App;
use crate::tasks::{self, display_name, empty, growth, send, Answer, TaskError, TaskResult};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use wbaccounts::Account;
use wbcore::{timeutil, types::Realm};

async fn pause(seconds: f64) {
    tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
}
fn num(v: &Value) -> f64 {
    wbcore::pyval::py_float(v).unwrap_or(0.0)
}

/// `checked` / `already` / `pending` / `failed`; only the first two set the marker.
pub fn checkin_state(code: Option<i64>, msg: &str) -> &'static str {
    if ["处理中", "请勿重复操作", "系统繁忙", "稍后再试"]
        .iter()
        .any(|k| msg.contains(k))
    {
        return "pending";
    }
    match code {
        Some(0) if msg.contains("失败") => "failed",
        Some(0) => "checked",
        Some(10001) if msg.contains("已签到") => "already",
        _ => "failed",
    }
}

pub fn can_checkin(account: &Account) -> bool {
    account.realm() == Realm::Cn
        && !tasks::is_enterprise(account)
        && !tasks::today_marker(account, "lastCheckin")
}

/// One check-in attempt, recorded in the activity history.
pub async fn checkin(app: &App, account: &Arc<Account>, trigger: &str) -> Value {
    let result = checkin_upstream(app, account).await;
    let (ok, message) = match &result {
        Ok((state, msg)) => (*state == "checked" || *state == "already", msg.clone()),
        Err(e) => (false, e.message.clone()),
    };
    crate::activity::record(
        &app.args.usage_dir,
        account,
        "checkin",
        trigger,
        ok,
        &message,
    );
    match result {
        Ok((state, msg)) if ok => json!({"ok":true,"state":state,"msg":msg}),
        Ok((state, msg)) => json!({"ok":false,"state":state,"msg":msg,"error":msg}),
        Err(e) => json!({"ok":false,"error":e.message,"restricted":e.restricted}),
    }
}
async fn checkin_upstream(app: &App, account: &Arc<Account>) -> TaskResult<(&'static str, String)> {
    if account.realm() != Realm::Cn {
        return Err(TaskError::new("签到仅适用于国内版账号"));
    }
    if tasks::is_enterprise(account) {
        return Err(TaskError::new("企业版 / API Key 账号不支持签到"));
    }
    tasks::prepare(app, account).await?;
    let mut opts = wbaccounts::RequestOptions::default();
    opts.timeout_secs = 15.0;
    opts.proxy = account.resolve_proxy_in(&app.settings.proxy_slots_shared());
    opts.cn_chat_base = app.env.cn_chat_upstream.clone();
    let req = wbaccounts::build_billing_request(
        &account.cred(),
        wbaccounts::BillingOperation::Checkin,
        &empty(),
        &opts,
    )
    .map_err(|_| TaskError::new("无法构造签到请求"))?;
    let answer = send(app, account, req).await?;
    let msg = answer.msg();
    let state = checkin_state(answer.code(), &msg);
    // A 4xx body that says "already checked in" counts too.
    if state == "checked" && !(200..300).contains(&answer.status) {
        return Ok(("failed", msg));
    }
    if state == "checked" || state == "already" {
        tasks::mark_today(account, "lastCheckin");
    }
    Ok((state, msg))
}

/// Growth task list in the dashboard's row shape.
pub async fn growth_tasks(app: &App, account: &Arc<Account>) -> TaskResult<Vec<Value>> {
    let answer = growth(app, account, "GET", "/v2/activity/growth/tasks", None).await?;
    if !answer.ok() {
        return Err(TaskError::new(format!(
            "任务列表查询失败: {}",
            answer.msg()
        )));
    }
    Ok(answer.data()["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            let code = t["task_code"].as_str().unwrap_or("");
            json!({"task_code":code,"name":t["title"].as_str().filter(|s| !s.is_empty()).unwrap_or(code),
                "description":t["description"].as_str().or(t["task_desc"].as_str()).unwrap_or(""),
                "jump_url":t["jump_url"].as_str().unwrap_or(""),
                "status":t["accept_status"].as_str().unwrap_or("not_accepted"),
                "task_type":t["task_type"].as_str().unwrap_or(""),
                "current":t["progress"]["current"].as_i64().unwrap_or(0),
                "target":t["progress"]["target"].as_i64().unwrap_or(1),
                "reward_credit":t["reward_credit"].clone(),"reward_energy":t["reward_energy"].clone()})
        })
        .collect())
}

/// Energy, streak, makeup cards, lottery chances and travel state.
pub async fn growth_summary(app: &App, account: &Arc<Account>) -> Value {
    let mut out = json!({"energy":0,"streak_days":0,"travel":{"state":"unknown"}});
    if let Ok(a) = growth(app, account, "GET", "/v2/activity/growth/energy", None).await {
        out["energy"] = a.data()["balance"].clone();
    }
    if let Ok(a) = growth(app, account, "GET", "/activity/growth/streak", None).await {
        let d = a.data();
        out["streak_days"] = d["streak"]["days"].clone();
        out["makeup_cards"] = d["makeup_cards"]["balance"].clone();
        out["tiers"] = d["redemption_status"]["tiers"].clone();
    }
    if let Ok(a) = growth(
        app,
        account,
        "GET",
        "/activity/growth/lottery/summary",
        None,
    )
    .await
    {
        out["lottery_chances"] = a.data()["chances"].clone();
    }
    if let Ok(a) = growth(
        app,
        account,
        "GET",
        "/activity/growth/buddy/travel/status",
        None,
    )
    .await
    {
        if a.data().is_object() {
            out["travel"] = a.data().clone();
        }
    }
    out
}

/// Claim one completed task; falls back to the web domain on HTTP 400.
pub async fn claim_task(app: &App, account: &Arc<Account>, code: &str) -> Value {
    if code.is_empty()
        || code.len() > 128
        || !code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return json!({"ok":false,"msg":"任务代码无效"});
    }
    let path = format!("/activity/growth/tasks/{code}/claim");
    let first = growth(app, account, "POST", &path, Some(empty())).await;
    let answer = match first {
        Ok(a) if a.status == 400 => {
            let mut headers = tasks::web_headers(
                account,
                tasks::CN_WEB_BASE,
                &format!("{}/profile/growth-center", tasks::CN_WEB_BASE),
            );
            headers.push(("x-client-platform".into(), "web".into()));
            headers.push(("X-Domain".into(), tasks::CN_WEB_BASE.into()));
            let url = format!("{}{path}", tasks::CN_WEB_BASE);
            let req = tasks::request(app, account, "POST", url, headers, Some(&empty()), 15.0);
            send(app, account, req).await
        }
        other => other,
    };
    match answer {
        Ok(a) if a.ok() => {
            json!({"ok":true,"credit":a.data()["credit"],"energy":a.data()["energy"]})
        }
        Ok(a) => json!({"ok":false,"msg":a.msg()}),
        Err(e) => json!({"ok":false,"msg":e.message}),
    }
}

/// Accept every not-yet-accepted, non-auto task (no progress is faked), then
/// claim the tasks the upstream already reports as complete.
pub async fn run_growth(app: &App, account: &Arc<Account>, claim: bool) -> (Vec<String>, f64) {
    let mut logs = vec![format!("开始处理账号 {} 的成长任务", display_name(account))];
    let mut earned = 0.0;
    if let Err(e) = tasks::prepare(app, account).await {
        logs.push(format!("! {}", e.message));
        return (logs, earned);
    }
    let mut list = match growth_tasks(app, account).await {
        Ok(l) => l,
        Err(e) => {
            logs.push(format!("! {}", e.message));
            return (logs, earned);
        }
    };
    let pending: Vec<String> = list
        .iter()
        .filter(|t| t["status"] == "not_accepted" && t["task_type"] != "auto")
        .filter_map(|t| t["task_code"].as_str().map(str::to_owned))
        .collect();
    for chunk in pending.chunks(20) {
        let body = json!({"task_codes":chunk});
        match growth(
            app,
            account,
            "POST",
            "/v2/activity/growth/tasks/accept",
            Some(body),
        )
        .await
        {
            Ok(a) if a.ok() => logs.push(format!("✓ 已接取 {} 个任务", chunk.len())),
            Ok(a) => logs.push(format!("! 接取失败: {}", a.msg())),
            Err(e) => logs.push(format!("! 接取失败: {}", e.message)),
        }
        pause(1.0).await;
    }
    if !pending.is_empty() {
        if let Ok(l) = growth_tasks(app, account).await {
            list = l;
        }
    }
    for t in &list {
        let done = t["status"] == "completed"
            || (t["status"] != "claimed" && t["current"].as_i64() >= t["target"].as_i64());
        if t["status"] == "claimed" || !done {
            continue;
        }
        let name = t["name"].as_str().unwrap_or("");
        if !claim {
            logs.push(format!("⏸ 任务 [{name}] 已达成，等待手动领取"));
            continue;
        }
        let r = claim_task(app, account, t["task_code"].as_str().unwrap_or("")).await;
        if r["ok"] == true {
            earned += num(&r["credit"]);
            logs.push(format!("✓ 任务 [{name}] 领奖成功: +{} 积分", r["credit"]));
        } else {
            logs.push(format!(
                "! 任务 [{name}] 领奖失败: {}",
                r["msg"].as_str().unwrap_or("")
            ));
        }
        pause(1.0).await;
    }
    let open = list
        .iter()
        .filter(|t| t["status"] != "claimed" && t["status"] != "completed")
        .count();
    if open > 0 {
        logs.push(format!(
            "{open} 个任务需要在客户端里真实完成后才会计入进度（网关不伪造上报事件）"
        ));
    }
    (logs, earned)
}

/// Cat travel: claim an arrived trip or depart an idle cat.
pub async fn cat_travel(app: &App, account: &Arc<Account>) -> Value {
    if tasks::is_enterprise(account) {
        return json!({"ok":true,"action":"skip","msg":"企业版账号无成长体系，跳过"});
    }
    let status = match growth(
        app,
        account,
        "GET",
        "/activity/growth/buddy/travel/status",
        None,
    )
    .await
    {
        Ok(a) => a.data().clone(),
        Err(e) => return json!({"ok":false,"msg":format!("查询旅行状态失败: {}", e.message)}),
    };
    match status["state"].as_str().unwrap_or("") {
        "arrived" => match growth(
            app,
            account,
            "POST",
            "/activity/growth/buddy/travel/claim",
            Some(empty()),
        )
        .await
        {
            Ok(a) if a.ok() => {
                let credit = a.data()["reward_credit"].clone();
                json!({"ok":true,"action":"claim","credit":credit,"msg":format!("旅行归来领奖成功！获得 {credit} 积分")})
            }
            Ok(a) => json!({"ok":false,"msg":format!("领奖失败: {}", a.msg())}),
            Err(e) => json!({"ok":false,"msg":format!("领奖失败: {}", e.message)}),
        },
        "idle" if status["daily_limit_reached"] == true => {
            json!({"ok":true,"action":"idle","msg":"猫猫今日已完成旅行，明日 00:00 刷新"})
        }
        "idle" => {
            let location = match growth(
                app,
                account,
                "GET",
                "/activity/growth/buddy/travel/config",
                None,
            )
            .await
            {
                Ok(a) => a.data()["locations"][0]["id"].clone(),
                Err(_) => Value::Null,
            };
            let location = if location.is_null() {
                json!(1)
            } else {
                location
            };
            let body = json!({"location_id":location});
            match growth(
                app,
                account,
                "POST",
                "/activity/growth/buddy/travel/depart",
                Some(body),
            )
            .await
            {
                Ok(a) if a.ok() => {
                    let place = a.data()["location"]["name"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned();
                    json!({"ok":true,"action":"depart","msg":format!("猫猫已出发前往「{place}」，预计数小时后归来！")})
                }
                Ok(a) if a.msg().to_lowercase().contains("no active buddy") => json!({"ok":false,
                    "msg":"还没有 Buddy：需先在客户端完成并领取「领取一只 Buddy」任务"}),
                Ok(a) => json!({"ok":false,"msg":format!("派出旅行失败: {}", a.msg())}),
                Err(e) => json!({"ok":false,"msg":format!("派出旅行失败: {}", e.message)}),
            }
        }
        "traveling" => {
            json!({"ok":true,"action":"traveling","msg":"猫猫正在旅行途中，请稍后再来查看！"})
        }
        other => json!({"ok":true,"action":other,"msg":format!("当前状态: {other}")}),
    }
}

fn answered(result: &TaskResult<Answer>) -> Option<&Answer> {
    result.as_ref().ok().filter(|a| a.ok())
}

/// Streak keeper: makeup card for a missed yesterday, gifts, tier redeem, lottery.
pub async fn streak_bonus(app: &App, account: &Arc<Account>) -> Value {
    if tasks::is_enterprise(account) {
        return json!({"ok":true,"logs":[],"msg":"企业版账号无成长体系，跳过"});
    }
    let mut logs: Vec<String> = Vec::new();
    let yesterday = timeutil::local_day_key(timeutil::now() - 86400.0);
    let heatmap = growth(app, account, "GET", "/activity/growth/heatmap", None).await;
    let missed = answered(&heatmap).is_some_and(|a| {
        a.data()["cells"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| c["date"] == yesterday.as_str() && num(&c["score"]) == 0.0)
    });
    let streak = growth(app, account, "GET", "/activity/growth/streak", None).await;
    if missed && answered(&streak).is_some_and(|a| num(&a.data()["makeup_cards"]["balance"]) > 0.0)
    {
        let body = json!({"target_date":yesterday});
        if answered(
            &growth(
                app,
                account,
                "POST",
                "/activity/growth/makeup-cards/use",
                Some(body),
            )
            .await,
        )
        .is_some()
        {
            logs.push(format!("已消耗 1 张补签卡补签昨日 ({yesterday})"));
        }
    }
    if let Ok(headers) = tasks::billing_headers(account) {
        for (path, name) in [
            ("/billing/meter/claim-gift", "新手礼包"),
            ("/billing/meter/claim-compensation", "活动补偿"),
        ] {
            let url = format!("{}{path}", tasks::CN_BILL_BASE);
            let req = tasks::request(
                app,
                account,
                "POST",
                url,
                headers.clone(),
                Some(&empty()),
                10.0,
            );
            if let Some(a) = answered(&send(app, account, req).await) {
                logs.push(format!("领取{name} +{}积分", a.data()["credit"]));
            }
        }
    }
    if let Some(a) = answered(&streak) {
        let status = &a.data()["redemption_status"];
        for tier in status["tiers"].as_array().into_iter().flatten() {
            let name = tier["tier"].as_str().unwrap_or("");
            let state = status[format!("tier_{name}_status")]
                .as_str()
                .unwrap_or("locked");
            if name.is_empty() || state == "locked" || state == "claimed" {
                continue;
            }
            let body = json!({"tier":name,"client_token":tasks::uuid()});
            if answered(&growth(app, account, "POST", "/activity/growth/redeem", Some(body)).await)
                .is_some()
            {
                logs.push(format!("成功兑换连登 {name} 档位"));
            }
        }
    }
    let lottery = growth(
        app,
        account,
        "GET",
        "/activity/growth/lottery/summary",
        None,
    )
    .await;
    let chances =
        answered(&lottery).map_or(0, |a| num(&a.data()["chances"]).clamp(0.0, 50.0) as usize);
    let mut drawn = 0;
    for _ in 0..chances {
        let body = json!({"client_token":tasks::uuid()});
        match growth(
            app,
            account,
            "POST",
            "/activity/growth/lottery/draw",
            Some(body),
        )
        .await
        {
            Ok(a) if a.ok() => drawn += 1,
            Ok(_) => {}
            Err(_) => break,
        }
        pause(1.0).await;
    }
    if drawn > 0 {
        logs.push(format!("完成成长抽奖 {drawn} 次"));
    }
    let msg = if logs.is_empty() {
        "连登状态正常（无需兑换/无抽奖次数）".to_owned()
    } else {
        logs.join("; ")
    };
    json!({"ok":true,"logs":logs,"msg":msg})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checkin_states_follow_the_baseline() {
        assert_eq!(
            checkin_state(Some(0), "签到成功，距离 7 天奖励还不足"),
            "checked"
        );
        assert_eq!(checkin_state(Some(0), "签到失败"), "failed");
        assert_eq!(checkin_state(Some(10001), "今天已签到"), "already");
        assert_eq!(checkin_state(Some(10001), "other"), "failed");
        assert_eq!(checkin_state(Some(0), "系统繁忙，稍后再试"), "pending");
        assert_eq!(checkin_state(None, ""), "failed");
    }
}
