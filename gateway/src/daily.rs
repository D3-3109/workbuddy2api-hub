//! International daily activity. The reward only counts a completed web-console
//! agent session, so after the light desktop-identity completion this drives
//! one real conversation: create it, fetch its sandbox link, run one ACP turn
//! (initialize, session/load, session/set_model, session/prompt) over
//! streamable HTTP, poll until it completes, then release the session slot.
use crate::state::App;
use crate::tasks::{self, empty, send, Answer, TaskError, TaskResult};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use wbaccounts::Account;
use wbcore::types::{HeaderCase, OutRequest};

const CONVERSATIONS: &str = "https://www.workbuddy.ai/console/as/conversations/";
const ACTIVE: [&str; 3] = ["working", "creating", "pending"];

fn console_headers(account: &Account) -> Vec<(String, String)> {
    tasks::web_headers(account, tasks::INTL_BASE, "https://www.workbuddy.ai/app")
}
async fn console(
    app: &App,
    account: &Arc<Account>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> TaskResult<Answer> {
    let url = format!("{CONVERSATIONS}{path}");
    let req = tasks::request(
        app,
        account,
        method,
        url,
        console_headers(account),
        body.as_ref(),
        30.0,
    );
    send(app, account, req).await
}
fn id_segment(id: &str) -> Option<String> {
    (!id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
    .then(|| id.to_owned())
}

pub enum Created {
    Id(String),
    /// The account already has the maximum number of active web sessions.
    Limit(String),
    Failed {
        error: String,
        retryable: bool,
    },
}
pub async fn create(app: &App, account: &Arc<Account>, prompt: &str, model: &str) -> Created {
    let body = json!({"prompt":prompt,"model":model,"conversationOrigin":"workbuddy-app",
        "plugins":[{"name":"weixinpay","marketplace":"codebuddy-builtin"}]});
    let answer = match console(app, account, "POST", "", Some(body)).await {
        Ok(a) => a,
        Err(e) => {
            return Created::Failed {
                retryable: !e.restricted,
                error: e.message,
            }
        }
    };
    let text = answer.body.to_string().to_lowercase();
    if answer.status == 429
        && (text.contains("10105") || text.contains("maximum number of active conversations"))
    {
        return Created::Limit(answer.msg());
    }
    if !(200..300).contains(&answer.status) {
        return Created::Failed {
            error: format!("HTTP {}: {}", answer.status, answer.msg()),
            retryable: answer.status >= 500 || matches!(answer.status, 408 | 429),
        };
    }
    let code = answer.code();
    match answer.data()["id"].as_str().and_then(id_segment) {
        Some(id) if matches!(code, None | Some(0)) => Created::Id(id),
        _ => Created::Failed {
            error: format!("建会话被拒绝: {}", answer.msg()),
            retryable: false,
        },
    }
}

pub async fn status(app: &App, account: &Arc<Account>, id: &str) -> String {
    console(app, account, "GET", id, None)
        .await
        .ok()
        .and_then(|a| a.data()["status"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Free a session slot: stop always, delete failed attempts. Errors are ignored.
pub async fn release(app: &App, account: &Arc<Account>, id: &str, delete: bool) {
    let _ = console(app, account, "POST", &format!("{id}/stop"), Some(empty())).await;
    if delete {
        let _ = console(app, account, "POST", &format!("{id}/delete"), Some(empty())).await;
    }
}

/// Stop the oldest active sessions this gateway started (their name is one of
/// our prompts) until 8 remain. User conversations are never touched.
pub async fn prune(
    app: &App,
    account: &Arc<Account>,
    ours: &(dyn Fn(&str) -> bool + Sync),
) -> usize {
    let Ok(list) = console(app, account, "GET", "?page=1&pageSize=50", None).await else {
        return 0;
    };
    let mut active: Vec<&Value> = list.data()["conversations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| ACTIVE.contains(&c["status"].as_str().unwrap_or("")))
        .collect();
    if active.len() < 10 {
        return 0;
    }
    active.sort_by(|a, b| {
        let at = |c: &Value| {
            c["createdAt"]
                .as_f64()
                .or_else(|| c["createdAt"].as_str().map(|_| 0.0))
                .unwrap_or(0.0)
        };
        at(a).total_cmp(&at(b)).then_with(|| {
            a["createdAt"]
                .as_str()
                .unwrap_or("")
                .cmp(b["createdAt"].as_str().unwrap_or(""))
        })
    });
    let mut remaining = active.len();
    let mut stopped = 0;
    for c in active {
        if remaining <= 8 {
            break;
        }
        let (Some(id), true) = (
            c["id"].as_str().and_then(id_segment),
            ours(c["name"].as_str().unwrap_or("")),
        ) else {
            continue;
        };
        if console(app, account, "POST", &format!("{id}/stop"), Some(empty()))
            .await
            .is_ok()
        {
            remaining -= 1;
            stopped += 1;
        }
    }
    stopped
}

pub struct Session {
    pub link: String,
    pub token: String,
    pub session_id: String,
    pub cwd: String,
}
pub async fn session(app: &App, account: &Arc<Account>, id: &str) -> TaskResult<Session> {
    let answer = console(app, account, "GET", &format!("{id}/session"), None).await?;
    let d = answer.data();
    let link = d["link"]
        .as_str()
        .or(d["endpoint"].as_str())
        .unwrap_or("")
        .to_owned();
    let token = d["token"].as_str().unwrap_or("").to_owned();
    if link.is_empty() || token.is_empty() {
        return Err(TaskError::new("沙箱未就绪（没有 link/token）"));
    }
    if !valid_link(app, &link) || token.bytes().any(|b| b.is_ascii_control()) {
        return Err(TaskError::new("沙箱地址不可用"));
    }
    Ok(Session {
        link,
        token,
        session_id: d["sessionId"]
            .as_str()
            .or(d["session_id"].as_str())
            .unwrap_or(id)
            .to_owned(),
        cwd: d["cwd"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("/workspace")
            .to_owned(),
    })
}
fn valid_link(app: &App, link: &str) -> bool {
    url::Url::parse(link).is_ok_and(|u| {
        (u.scheme() == "https" || (u.scheme() == "http" && app.args.test_upstream.is_some()))
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
    })
}

#[derive(Default)]
pub struct Turn {
    pub ok: bool,
    pub status: String,
    pub events: usize,
    pub chunks: usize,
    pub elapsed_ms: u64,
    pub error: String,
}

fn acp_request(
    app: &App,
    account: &Account,
    s: &Session,
    method: &str,
    extra: Vec<(String, String)>,
    body: Option<Vec<u8>>,
) -> OutRequest {
    let mut headers = extra;
    headers.push(("Authorization".into(), format!("Bearer {}", s.token)));
    headers.push(("User-Agent".into(), tasks::WEB_UA.into()));
    OutRequest {
        method: method.into(),
        url: s.link.clone(),
        headers,
        body,
        case: HeaderCase::Exact,
        timeout_secs: 30.0,
        egress: tasks::egress(app, account),
    }
}

async fn rpc(
    app: &App,
    account: &Account,
    s: &Session,
    conn: &str,
    id: u32,
    method: &str,
    params: Value,
) -> Result<(), String> {
    let body = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
    let headers = vec![
        ("Content-Type".into(), "application/json".into()),
        (
            "Accept".into(),
            "application/json, text/event-stream".into(),
        ),
        ("Acp-Connection-Id".into(), conn.into()),
    ];
    let mut req = acp_request(
        app,
        account,
        s,
        "POST",
        headers,
        Some(serde_json::to_vec(&body).unwrap_or_default()),
    );
    app.rewrite_test_upstream(&mut req);
    let status = tokio::time::timeout(Duration::from_secs(35), async {
        let r = app.http.execute(req).await.map_err(|e| e.to_string())?;
        let status = r.status;
        r.collect(1024 * 1024).await.map_err(|e| e.to_string())?;
        Ok::<_, String>(status)
    })
    .await
    .map_err(|_| format!("{method} 超时"))??;
    if matches!(status, 200 | 202) {
        Ok(())
    } else {
        Err(format!("{method} 返回 HTTP {status}"))
    }
}

/// Count `session/update` notifications and agent message chunks.
fn count_events(buffer: &mut Vec<u8>, chunk: &[u8], events: &AtomicUsize, chunks: &AtomicUsize) {
    buffer.extend_from_slice(chunk);
    while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
        let line: Vec<u8> = buffer.drain(..=end).collect();
        let text = String::from_utf8_lossy(&line);
        let Some(data) = text.trim().strip_prefix("data:") else {
            continue;
        };
        let Ok(message) = serde_json::from_str::<Value>(data.trim()) else {
            continue;
        };
        if message["method"] == "session/update" {
            events.fetch_add(1, Ordering::Relaxed);
            if message["params"]["update"]["sessionUpdate"] == "agent_message_chunk" {
                chunks.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    if buffer.len() > 1024 * 1024 {
        buffer.clear();
    }
}

/// Attach to the sandbox and run one prompt turn, waiting up to `wait` seconds.
pub async fn run_turn(
    app: &App,
    account: &Arc<Account>,
    id: &str,
    s: &Session,
    prompt: &str,
    model: &str,
    wait: u64,
) -> Turn {
    let started = Instant::now();
    let events = Arc::new(AtomicUsize::new(0));
    let chunks = Arc::new(AtomicUsize::new(0));
    let mut turn = Turn::default();
    let headers = vec![("Accept".into(), "text/event-stream".into())];
    let mut req = acp_request(app, account, s, "GET", headers, None);
    app.rewrite_test_upstream(&mut req);
    let opened = tokio::time::timeout(Duration::from_secs(35), app.http.execute(req)).await;
    let mut stream = match opened {
        Ok(Ok(r)) if r.status == 200 => r,
        Ok(Ok(r)) => {
            turn.error = format!("SSE 通道返回 HTTP {}", r.status);
            return turn;
        }
        Ok(Err(e)) => {
            turn.error = format!("SSE 通道连接失败: {e}");
            return turn;
        }
        Err(_) => {
            turn.error = "SSE 通道连接超时".into();
            return turn;
        }
    };
    let conn = stream.header("acp-connection-id").unwrap_or("").to_owned();
    if conn.is_empty() || conn.bytes().any(|b| b.is_ascii_control()) {
        turn.error = "SSE 通道没有返回 Acp-Connection-Id".into();
        return turn;
    }
    let _ = stream.set_read_timeout(Duration::from_secs(wait + 60));
    let reader = {
        let (events, chunks) = (events.clone(), chunks.clone());
        tokio::spawn(async move {
            let mut buffer = Vec::new();
            while let Ok(Some(chunk)) = stream.next_chunk().await {
                count_events(&mut buffer, &chunk, &events, &chunks);
            }
        })
    };
    let steps = async {
        rpc(app, account, s, &conn, 1, "initialize", json!({"protocolVersion":1,
            "clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}})).await?;
        rpc(
            app,
            account,
            s,
            &conn,
            2,
            "session/load",
            json!({"sessionId":s.session_id,"cwd":s.cwd,"mcpServers":[]}),
        )
        .await?;
        // Without set_model the session runs on the account default, which bills.
        if let Err(e) = rpc(
            app,
            account,
            s,
            &conn,
            3,
            "session/set_model",
            json!({"sessionId":s.session_id,"modelId":model}),
        )
        .await
        {
            app.log_tagged(
                "WARN",
                "tasks",
                &format!("[调度器] 网页会话设置模型 {model} 失败: {e}"),
            );
        }
        rpc(
            app,
            account,
            s,
            &conn,
            4,
            "session/prompt",
            json!({"sessionId":s.session_id,"prompt":[{"type":"text","text":prompt}]}),
        )
        .await
    };
    if let Err(e) = steps.await {
        turn.error = e;
    } else {
        let deadline = Instant::now() + Duration::from_secs(wait.max(1));
        loop {
            turn.status = status(app, account, id).await;
            if turn.status == "completed" {
                turn.ok = true;
                break;
            }
            if matches!(turn.status.as_str(), "failed" | "error") {
                turn.error = format!("会话状态={}", turn.status);
                break;
            }
            if Instant::now() >= deadline {
                let shown = if turn.status.is_empty() {
                    "未知"
                } else {
                    &turn.status
                };
                turn.error = format!("会话在 {wait}s 内没有跑完（状态={shown}）");
                break;
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
    reader.abort();
    turn.events = events.load(Ordering::Relaxed);
    turn.chunks = chunks.load(Ordering::Relaxed);
    turn.elapsed_ms = started.elapsed().as_millis() as u64;
    turn
}

static PROMPTS: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| {
    serde_json::from_str(include_str!("../../assets/webprompts.json")).unwrap_or_default()
});
fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            s.as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .collect()
}
fn prompt_pool(settings: &Value) -> Vec<String> {
    let custom = strings(&settings["daily_chat_web_prompts"]);
    if custom.is_empty() {
        PROMPTS.clone()
    } else {
        custom
    }
}
pub fn pick_prompt(settings: &Value) -> String {
    use rand::seq::SliceRandom;
    if settings["daily_chat_web_random_prompt"] != true {
        return "Hi".into();
    }
    prompt_pool(settings)
        .choose(&mut rand::thread_rng())
        .cloned()
        .unwrap_or_else(|| "Hi".into())
}

/// Preferred model, falling back past models that are cooling down or capped.
pub fn daily_model(settings: &Value, account: &Account) -> String {
    let mut candidates = strings(&json!([settings["daily_chat_model"]]));
    candidates
        .extend(["deepseek-v4.1-flash", "hy3", "hy4-preview-f", "hy4-preview"].map(String::from));
    if account.realm() == wbcore::types::Realm::Intl {
        candidates.push("glm-5.3-flash".into());
    }
    let at = wbcore::timeutil::now();
    candidates
        .iter()
        .find(|m| account.throttle_wait(m, at) <= 0.0 && account.cap_until(m, at) <= 0.0)
        .unwrap_or(&candidates[0])
        .clone()
}
fn credit_rank(account: &Account, model: &str) -> f64 {
    wbcatalog::bundled_models(account.realm())
        .iter()
        .find(|m| m["id"] == model)
        .and_then(|m| {
            m["credits"]
                .as_str()?
                .trim()
                .strip_prefix('x')?
                .parse()
                .ok()
        })
        .unwrap_or(f64::INFINITY)
}
/// Web-leg fallback order: configured list or preferred + free models, cheapest first.
pub fn fallback_models(settings: &Value, account: &Account) -> Vec<String> {
    let preferred = daily_model(settings, account);
    let mut models = strings(&settings["daily_chat_web_fallback_models"]);
    if models.is_empty() {
        models.push(preferred.clone());
        models.extend(["hy3", "hy4-preview-f", "hy4-preview"].map(String::from));
        if account.realm() == wbcore::types::Realm::Intl {
            models.push("glm-5.3-flash".into());
        }
    }
    let mut seen = std::collections::HashSet::new();
    models.retain(|m| seen.insert(m.clone()));
    models.sort_by(|a, b| {
        credit_rank(account, a)
            .total_cmp(&credit_rank(account, b))
            .then((a != &preferred).cmp(&(b != &preferred)))
    });
    models
}

/// One web attempt; `(result, retryable, early)` where `early` means the
/// attempt failed before any waiting (create or session phase).
async fn web_once(
    app: &App,
    account: &Arc<Account>,
    settings: &Value,
    prompt: &str,
    model: &str,
    wait: u64,
) -> (Value, bool, bool) {
    let mut created = create(app, account, prompt, model).await;
    if let Created::Limit(_) = created {
        let pool = prompt_pool(settings);
        let ours = |name: &str| {
            name == "Hi"
                || name == prompt
                || pool.iter().any(|p| p == name)
                || PROMPTS.iter().any(|p| p == name)
        };
        prune(app, account, &ours).await;
        created = create(app, account, prompt, model).await;
        if let Created::Limit(msg) = created {
            return (
                json!({"ok":false,"error":format!("网页会话已达 10 个活跃上限，清理后仍无法新建: {msg}")}),
                false,
                true,
            );
        }
    }
    let id = match created {
        Created::Id(id) => id,
        Created::Failed { error, retryable } => {
            return (json!({"ok":false,"error":error}), retryable, true)
        }
        Created::Limit(_) => unreachable!(),
    };
    let sess = match session(app, account, &id).await {
        Ok(s) => s,
        Err(e) => {
            release(app, account, &id, true).await;
            return (
                json!({"ok":false,"conversation":id,"error":format!("session 查询失败: {}", e.message)}),
                !e.restricted,
                true,
            );
        }
    };
    let turn = run_turn(app, account, &id, &sess, prompt, model, wait).await;
    release(app, account, &id, !turn.ok).await;
    let mut out = json!({"ok":turn.ok,"conversation":id,"model":model,"status":turn.status,"events":turn.events,
        "chunks":turn.chunks,"elapsed_ms":turn.elapsed_ms});
    if turn.ok {
        out["msg"] = json!(format!(
            "网页通道会话跑完（{model}）：{} 段输出，{} ms",
            turn.chunks, turn.elapsed_ms
        ));
    } else {
        out["error"] = json!(turn.error);
    }
    (out, true, false)
}

/// The web leg with the configured retry and fallback policy.
pub async fn web_leg(app: &App, account: &Arc<Account>) -> Value {
    let settings = app.settings.current();
    let base = settings["daily_chat_web_timeout"]
        .as_u64()
        .unwrap_or(120)
        .clamp(1, 3600);
    let total = if settings["daily_chat_web_retry"] == false {
        1
    } else {
        settings["daily_chat_web_attempts"]
            .as_u64()
            .unwrap_or(3)
            .clamp(1, 5)
    };
    let backoff = settings["daily_chat_web_retry_backoff"]
        .as_u64()
        .unwrap_or(5)
        .min(60);
    let prompt = pick_prompt(&settings);
    let fallback = settings["daily_chat_web_fallback"] == true;
    let models = if fallback {
        fallback_models(&settings, account)
    } else {
        vec![daily_model(&settings, account)]
    };
    let attempts = if fallback {
        total.max(models.len() as u64)
    } else {
        total
    };
    let mut last = json!({"ok":false,"error":"no attempt"});
    for i in 0..attempts {
        let model = &models[i as usize % models.len()];
        let wait = if fallback {
            base.saturating_mul(1 << i.min(5))
        } else {
            base * (i + 1)
        };
        let (mut res, retryable, early) =
            web_once(app, account, &settings, &prompt, model, wait).await;
        res["attempts"] = json!(i + 1);
        if res["ok"] == true || !retryable || i + 1 == attempts {
            return res;
        }
        if early && backoff > 0 {
            let pause = (backoff << i.min(5)).min(backoff.max(30));
            tokio::time::sleep(Duration::from_secs(pause)).await;
        }
        last = res;
    }
    last
}

/// Light desktop-identity completion; the web leg decides the reward.
async fn desktop_leg(app: &App, account: &Arc<Account>, model: &str) -> TaskResult<()> {
    let settings = app.settings.current();
    let mut opts = crate::dispatch::chat_options(app, &settings, account)
        .map_err(|e| TaskError::new(e.message))?;
    opts.model = Some(model.into());
    opts.request.timeout_secs = 20.0;
    let body = json!({"model":model,"messages":[{"role":"user","content":"Hi"}],"stream":true,"max_tokens":10,"reasoning_effort":"none"});
    let req = wbaccounts::build_chat_request(&account.cred(), &body, &opts)
        .map_err(|_| TaskError::new("无法构造桌面端请求"))?;
    let answer = send(app, account, req).await?;
    if !(200..300).contains(&answer.status) {
        return Err(TaskError::new(format!(
            "HTTP {}: {}",
            answer.status,
            answer.msg()
        )));
    }
    Ok(())
}

pub fn can_daily_chat(account: &Account) -> bool {
    account.realm() == wbcore::types::Realm::Intl && !tasks::today_marker(account, "lastDailyChat")
}

/// Full daily activity. `web` None follows `daily_chat_web`. Writes
/// `lastDailyChat` only when the reward-bearing leg succeeded.
pub async fn daily_chat(app: &App, account: &Arc<Account>, trigger: &str) -> Value {
    if account.realm() != wbcore::types::Realm::Intl {
        return json!({"ok":false,"error":"每日活跃仅适用于国际版账号"});
    }
    if !can_daily_chat(account) {
        return json!({"ok":true,"skipped":true,"msg":"今日活跃已完成，跳过本轮"});
    }
    let res = match tasks::prepare(app, account).await {
        Err(e) => json!({"ok":false,"error":e.message}),
        Ok(()) => {
            let settings = app.settings.current();
            let model = daily_model(&settings, account);
            match desktop_leg(app, account, &model).await {
                Err(e) => json!({"ok":false,"error":e.message}),
                Ok(()) if settings["daily_chat_web"] == false => {
                    json!({"ok":true,"msg":"每日活跃对话成功完成"})
                }
                Ok(()) => {
                    let web = web_leg(app, account).await;
                    if web["ok"] == true {
                        json!({"ok":true,"web":web,"msg":format!("每日活跃对话成功完成（网页通道 {}：{} 段输出，{} ms）",
                            web["status"].as_str().unwrap_or("completed"), web["chunks"], web["elapsed_ms"])})
                    } else {
                        let err = format!("网页通道失败：{}", web["error"].as_str().unwrap_or(""));
                        json!({"ok":false,"web":web,"error":err})
                    }
                }
            }
        }
    };
    if res["ok"] == true {
        tasks::mark_today(account, "lastDailyChat");
    }
    let message = res["msg"]
        .as_str()
        .or(res["error"].as_str())
        .unwrap_or("")
        .to_owned();
    crate::activity::record(
        &app.args.usage_dir,
        account,
        "daily_chat",
        trigger,
        res["ok"] == true,
        &message,
    );
    res
}

/// Manual web leg only; never writes `lastDailyChat`.
pub async fn daily_chat_web(app: &App, account: &Arc<Account>, trigger: &str) -> Value {
    if !can_daily_chat(account) {
        return json!({"ok":true,"skipped":true,"msg":"今日活跃已完成，跳过网页活跃"});
    }
    let res = match tasks::prepare(app, account).await {
        Err(e) => json!({"ok":false,"error":e.message}),
        Ok(()) => web_leg(app, account).await,
    };
    let message = res["msg"]
        .as_str()
        .or(res["error"].as_str())
        .unwrap_or("")
        .to_owned();
    crate::activity::record(
        &app.args.usage_dir,
        account,
        "daily_chat",
        trigger,
        res["ok"] == true,
        &message,
    );
    res
}
