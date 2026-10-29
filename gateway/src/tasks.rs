//! Reward-task upstream calls: CN check-in, growth center, cat travel and the
//! streak bonus. Every call skips restricted accounts and realm holds, and a
//! structured 11140 answer quarantines the account exactly like a chat request.
//! No telemetry is fabricated: progress events (`/v2/report`) are never sent;
//! tasks are only claimed after the upstream itself reports them complete.
use crate::state::App;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use wbaccounts::{Account, Readiness};
use wbcore::{
    timeutil::{self, now},
    types::{Egress, HeaderCase, OutRequest, Realm},
};

pub const CN_BILL_BASE: &str = "https://www.codebuddy.cn";
pub const CN_WEB_BASE: &str = "https://www.workbuddy.cn";
pub const INTL_BASE: &str = "https://www.workbuddy.ai";
pub const WEB_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36 Edg/140.0.0.0";
const BODY_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct TaskError {
    pub message: String,
    pub restricted: bool,
}
impl TaskError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            restricted: false,
        }
    }
}
pub type TaskResult<T> = Result<T, TaskError>;

/// A completed upstream exchange. `body` is the parsed JSON or `Null`.
pub struct Answer {
    pub status: u16,
    pub body: Value,
}
impl Answer {
    pub fn code(&self) -> Option<i64> {
        wbaccounts::json_code(&self.body)
    }
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status) && self.code() == Some(0)
    }
    pub fn data(&self) -> &Value {
        &self.body["data"]
    }
    pub fn msg(&self) -> String {
        let text = self.body["msg"]
            .as_str()
            .or_else(|| self.body["message"].as_str())
            .unwrap_or("");
        let text = crate::activity::safe_message(text);
        if !text.is_empty() {
            text
        } else if (200..300).contains(&self.status) {
            format!("code={}", self.code().map_or("?".into(), |c| c.to_string()))
        } else {
            format!("HTTP {}", self.status)
        }
    }
}

pub fn cn_chat_base(app: &App) -> String {
    app.env
        .cn_chat_upstream
        .clone()
        .unwrap_or_else(|| "https://copilot.tencent.com".into())
        .trim_end_matches('/')
        .to_owned()
}
pub fn growth_base(app: &App, realm: Realm) -> String {
    match realm {
        Realm::Cn => cn_chat_base(app),
        Realm::Intl => INTL_BASE.into(),
    }
}
pub fn egress(app: &App, account: &Account) -> Egress {
    let proxy = account.resolve_proxy_in(&app.settings.proxy_slots_shared());
    if proxy.is_empty() {
        Egress::Direct
    } else {
        Egress::Proxy(proxy)
    }
}
pub fn short(account: &Account) -> String {
    account.uid().chars().take(8).collect()
}
pub fn display_name(account: &Account) -> String {
    account.public_metadata()["nickname"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| short(account))
}
pub fn is_enterprise(account: &Account) -> bool {
    let cred = account.cred();
    cred.is_api_key() || !cred.enterprise_id.is_empty()
}
pub fn today_marker(account: &Account, key: &str) -> bool {
    account.to_json()[key]
        .as_str()
        .is_some_and(|s| s.starts_with(&timeutil::local_day_key(now())))
}
pub fn mark_today(account: &Account, key: &str) {
    let _ = account.set_task_marker(key, &timeutil::local_stamp(now()));
}

/// Seconds the account must still wait because of its own restriction or a
/// realm-wide hold; 0 when tasks may run.
pub fn blocked_for(app: &App, account: &Account) -> f64 {
    let until = account.to_json()["restrictedUntil"].as_f64().unwrap_or(0.0);
    (until - now())
        .max(app.restriction_wait(account.realm()) as f64)
        .max(app.pool.restriction_hold_remaining(account.realm(), now()))
        .max(0.0)
}

/// Refresh a near-expiry token before a task touches the upstream.
pub async fn prepare(app: &App, account: &Arc<Account>) -> TaskResult<()> {
    let wait = blocked_for(app, account);
    if wait > 0.0 {
        return Err(TaskError {
            message: format!("账号或区域风控暂停中，剩余 {}s", wait.ceil() as u64),
            restricted: true,
        });
    }
    if account.cred().access_token.is_empty() {
        return Err(TaskError::new("账号没有可用凭证"));
    }
    if matches!(account.readiness("", now()), Readiness::NeedsRefresh) {
        crate::dispatch::refresh_account(app, account, false)
            .await
            .map_err(|_| TaskError::new("凭证刷新失败，请重新登录"))?;
    }
    Ok(())
}

/// Desktop-identity headers for growth endpoints (`account.headers("chat")`).
pub fn chat_headers(app: &App, account: &Account) -> TaskResult<Vec<(String, String)>> {
    let settings = app.settings.current();
    let opts = crate::dispatch::chat_options(app, &settings, account)
        .map_err(|e| TaskError::new(e.message))?;
    wbaccounts::build_chat_headers(&account.cred(), &opts)
        .map_err(|_| TaskError::new("无法构造账号请求头"))
}
pub fn billing_headers(account: &Account) -> TaskResult<Vec<(String, String)>> {
    wbaccounts::build_billing_headers(&account.cred(), None)
        .map_err(|_| TaskError::new("无法构造计费请求头"))
}
/// Web console headers: bearer and uid only, no desktop fingerprint.
pub fn web_headers(account: &Account, origin: &str, referer: &str) -> Vec<(String, String)> {
    let cred = account.cred();
    vec![
        (
            "Authorization".into(),
            format!("Bearer {}", cred.access_token),
        ),
        ("X-User-Id".into(), cred.uid),
        ("Content-Type".into(), "application/json".into()),
        ("Accept".into(), "application/json, text/plain, */*".into()),
        ("Origin".into(), origin.into()),
        ("Referer".into(), referer.into()),
        ("User-Agent".into(), WEB_UA.into()),
    ]
}

pub fn request(
    app: &App,
    account: &Account,
    method: &str,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<&Value>,
    timeout: f64,
) -> OutRequest {
    OutRequest {
        method: method.into(),
        url,
        headers,
        body: body.map(|b| wbcore::pyjson::dumps_compact(b).into_bytes()),
        case: HeaderCase::Title,
        timeout_secs: timeout,
        egress: egress(app, account),
    }
}

/// Execute once with a wall-clock deadline. Account-level restrictions are
/// persisted and returned as errors; every other answer is returned as data.
pub async fn send(app: &App, account: &Arc<Account>, mut req: OutRequest) -> TaskResult<Answer> {
    let wait = blocked_for(app, account);
    if wait > 0.0 {
        return Err(TaskError {
            message: format!("账号或区域风控暂停中，剩余 {}s", wait.ceil() as u64),
            restricted: true,
        });
    }
    let deadline = Duration::from_secs_f64(req.timeout_secs + 5.0);
    app.rewrite_test_upstream(&mut req);
    let response = tokio::time::timeout(deadline, async {
        app.http
            .execute(req)
            .await
            .map_err(|e| TaskError::new(format!("网络错误: {e}")))?
            .into_in_response(BODY_LIMIT)
            .await
            .map_err(|e| TaskError::new(format!("读取响应失败: {e}")))
    })
    .await
    .map_err(|_| TaskError::new("上游请求超时"))??;
    let body: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
    let structured = crate::dispatch::structured_failure(&body).is_some_and(|f| f.restricted);
    let raw = !(200..300).contains(&response.status)
        && wbaccounts::restriction_is_account_level(&response.text());
    if structured || raw {
        crate::dispatch::restrict_account(app, account);
        return Err(TaskError {
            message: "上游账号风控限制 (11140)，已暂停该账号".into(),
            restricted: true,
        });
    }
    Ok(Answer {
        status: response.status,
        body,
    })
}

/// GET/POST a growth-center endpoint with desktop-identity headers.
pub async fn growth(
    app: &App,
    account: &Arc<Account>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> TaskResult<Answer> {
    let url = format!("{}{path}", growth_base(app, account.realm()));
    let headers = chat_headers(app, account)?;
    let req = request(app, account, method, url, headers, body.as_ref(), 10.0);
    send(app, account, req).await
}

pub fn uuid() -> String {
    wbcore::ids::uuid4_str()
}
pub fn empty() -> Value {
    json!({})
}
