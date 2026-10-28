//! Panel-authenticated, bounded account networking. The router owns authorization.
//! Never log URLs/bodies: OAuth state and upstream diagnostics can contain secrets.
use crate::{
    state::App,
    wire::{self, Query, Reply},
};
use hyper::header;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};
use wbaccounts::{Account, OAuthPoll, Readiness, ResponseAction};
use wbcore::{
    timeutil::now,
    types::{Egress, InResponse, OutRequest, Realm},
};

const LOGIN_TTL: Duration = Duration::from_secs(600);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const MAX_LOGINS: usize = 128;
const TEST_LIMIT: usize = 32 * 1024;
const JSON_LIMIT: usize = 256 * 1024;
const CATALOG_LIMIT: usize = 2 * 1024 * 1024;

static LOGINS: LazyLock<Mutex<HashMap<String, Arc<Mutex<Login>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static CATALOG_GATE: LazyLock<Mutex<HashMap<Realm, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone)]
enum LoginPhase {
    Starting,
    Pending,
    Polling,
    Complete(Value),
    Failed,
    Cancelled,
}
struct Login {
    owner: PathBuf,
    realm: Realm,
    platform: String,
    upstream_state: String,
    expires: Instant,
    next_poll: Instant,
    phase: LoginPhase,
}
impl Login {
    fn begin_poll(&mut self, at: Instant) -> PollDecision {
        if at >= self.expires {
            return PollDecision::Expired;
        }
        match &self.phase {
            LoginPhase::Complete(value) => PollDecision::Complete(value.clone()),
            LoginPhase::Cancelled => PollDecision::Cancelled,
            LoginPhase::Failed => PollDecision::Failed,
            LoginPhase::Starting | LoginPhase::Polling => PollDecision::Wait(2),
            LoginPhase::Pending if at < self.next_poll => {
                PollDecision::Wait((self.next_poll - at).as_secs() + 1)
            }
            LoginPhase::Pending => {
                self.phase = LoginPhase::Polling;
                self.next_poll = at + POLL_INTERVAL;
                PollDecision::Request(
                    self.realm,
                    self.platform.clone(),
                    self.upstream_state.clone(),
                )
            }
        }
    }
    fn finish(&mut self, phase: LoginPhase) {
        if matches!(self.phase, LoginPhase::Polling | LoginPhase::Starting) {
            if !matches!(phase, LoginPhase::Pending) {
                self.upstream_state.clear();
            }
            self.phase = phase;
        }
    }
}
enum PollDecision {
    Expired,
    Complete(Value),
    Cancelled,
    Failed,
    Wait(u64),
    Request(Realm, String, String),
}
// If a future is dropped after an upstream token request, do NOT replay that
// potentially consumed request. Require a fresh explicit login instead.
struct LoginFlight(Arc<Mutex<Login>>);
impl Drop for LoginFlight {
    fn drop(&mut self) {
        self.0.lock().finish(LoginPhase::Failed);
    }
}
struct StartingLogin {
    key: String,
    retained: bool,
}
impl Drop for StartingLogin {
    fn drop(&mut self) {
        if !self.retained {
            LOGINS.lock().remove(&self.key);
        }
    }
}

pub async fn get(app: Arc<App>, path: &str, q: &Query) -> Option<Reply> {
    Some(match path {
        "/accounts/login/poll" => {
            login_poll(app, q.get("state").map(String::as_str).unwrap_or("")).await
        }
        "/accounts/credits" | "/accounts/credits/detail" | "/accounts/credits/grants" => {
            billing_unsupported()
        }
        _ => return None,
    })
}
pub async fn post(app: Arc<App>, path: &str, payload: Value) -> Option<Reply> {
    Some(match path {
        "/accounts/login/start" => login_start(app, &payload).await,
        "/accounts/login/cancel" => login_cancel(&app, text(&payload, "state")),
        "/accounts/test" => account_test(app, &payload).await,
        "/catalog/refresh" => catalog_refresh(app, &payload).await,
        "/accounts/credits" | "/accounts/credits/fetch" | "/accounts/credits/detail" => {
            billing_unsupported()
        }
        _ => return None,
    })
}
fn reply(status: u16, value: Value) -> Reply {
    no_store(wire::json_reply(status, value))
}
fn fail(status: u16, message: &str) -> Reply {
    no_store(wire::error(status, message, false))
}
fn no_store(mut reply: Reply) -> Reply {
    reply.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    reply
}
fn wait(seconds: u64, message: &str) -> Reply {
    no_store(wire::retry_error(seconds, message, false))
}
fn billing_unsupported() -> Reply {
    fail(
        501,
        "billing aggregation is not implemented; no credit fetch or account change was performed",
    )
}
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
fn realm(app: &App, value: &Value) -> Result<Realm, Reply> {
    match value.get("realm") {
        None => Ok(*app.realm.read()),
        Some(Value::String(s)) if s.is_empty() => Ok(*app.realm.read()),
        Some(Value::String(s)) => {
            Realm::parse(s).ok_or_else(|| fail(400, "realm must be intl or cn"))
        }
        _ => Err(fail(400, "realm must be intl or cn")),
    }
}
fn request_options() -> wbaccounts::RequestOptions {
    wbaccounts::RequestOptions {
        cn_chat_base: std::env::var("WB_CN_CHAT_UPSTREAM").ok(),
        timeout_secs: 15.0,
        ..Default::default()
    }
}

/// One request, no retries/redirects, wall-clock deadline and bounded buffering.
/// Preserve a bounded prefix on overflow so restrictions still get classified.
async fn fetch(
    app: &App,
    mut request: OutRequest,
    cap: usize,
) -> Result<(InResponse, bool), Reply> {
    app.rewrite_test_upstream(&mut request);
    let work = async {
        let mut response = app
            .http
            .execute(request)
            .await
            .map_err(|_| fail(502, "upstream transport failed; details omitted"))?;
        let mut body = Vec::new();
        while let Some(chunk) = response
            .next_chunk()
            .await
            .map_err(|_| fail(502, "upstream response could not be read"))?
        {
            let remaining = cap.saturating_sub(body.len());
            body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            if chunk.len() > remaining {
                return Ok((
                    InResponse {
                        status: response.status,
                        headers: vec![],
                        body,
                    },
                    true,
                ));
            }
        }
        Ok((
            InResponse {
                status: response.status,
                headers: vec![],
                body,
            },
            false,
        ))
    };
    tokio::time::timeout(Duration::from_secs(30), work)
        .await
        .unwrap_or_else(|_| Err(fail(504, "upstream request timed out")))
}
fn valid_auth_url(raw: &str) -> bool {
    raw.len() <= 8192
        && !raw.chars().any(char::is_control)
        && url::Url::parse(raw).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
        })
}
fn valid_state(raw: &str) -> bool {
    raw.len() == 48 && raw.bytes().all(|b| b.is_ascii_hexdigit())
}
fn login_entry(app: &App, key: &str) -> Option<Arc<Mutex<Login>>> {
    if !valid_state(key) {
        return None;
    }
    let entry = LOGINS.lock().get(key).cloned()?;
    let owned = entry.lock().owner == app.args.accounts_dir;
    owned.then_some(entry)
}
async fn login_start(app: Arc<App>, payload: &Value) -> Reply {
    let realm = match realm(&app, payload) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let platform = match payload
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or("CLI")
    {
        "CLI" | "cli" | "" => "CLI",
        _ => return fail(400, "only the standard CLI OAuth platform is supported"),
    };
    let at = Instant::now();
    let key = wbcore::ids::token_hex(24);
    let state = Arc::new(Mutex::new(Login {
        owner: app.args.accounts_dir.clone(),
        realm,
        platform: platform.into(),
        upstream_state: String::new(),
        expires: at + LOGIN_TTL,
        next_poll: at,
        phase: LoginPhase::Starting,
    }));
    {
        let mut entries = LOGINS.lock();
        entries.retain(|_, state| state.lock().expires > at);
        if entries.len() >= MAX_LOGINS {
            return wait(
                30,
                "too many active logins; cancel an existing login or wait for expiry",
            );
        }
        entries.insert(key.clone(), state.clone());
    }
    let mut reservation = StartingLogin {
        key: key.clone(),
        retained: false,
    };
    let request = match wbaccounts::build_oauth_start_request(realm, platform, &request_options()) {
        Ok(r) => r,
        Err(_) => return fail(500, "OAuth request configuration is invalid"),
    };
    let (response, oversized) = match fetch(&app, request, JSON_LIMIT).await {
        Ok(r) => r,
        Err(r) => return r,
    };
    if oversized {
        return fail(502, "OAuth response exceeds the size limit");
    }
    if wbaccounts::response_code(&response).is_some_and(|code| code != 0)
        || has_restriction(&response)
    {
        return fail(502, "OAuth start was rejected by upstream");
    }
    let started = match wbaccounts::parse_oauth_start(&response, now()) {
        Ok(s) if s.state.len() <= 4096 && valid_auth_url(&s.auth_url) => s,
        _ => return fail(502, "upstream returned an invalid OAuth start response"),
    };
    {
        let mut state = state.lock();
        state.upstream_state = started.state;
        state.phase = LoginPhase::Pending;
    }
    reservation.retained = true;
    reply(
        200,
        json!({"state":key,"authUrl":started.auth_url,"expiresAt":started.expires_at,"expiresIn":600,"realm":realm.as_str()}),
    )
}
fn login_cancel(app: &App, key: &str) -> Reply {
    let Some(state) = login_entry(app, key) else {
        return reply(200, json!({"ok":true,"status":"unknown"}));
    };
    let mut state = state.lock();
    if matches!(state.phase, LoginPhase::Complete(_)) {
        return fail(
            409,
            "login already completed; cancellation does not delete an imported account",
        );
    }
    state.phase = LoginPhase::Cancelled;
    state.upstream_state.clear();
    reply(200, json!({"ok":true,"status":"cancelled"}))
}
async fn login_poll(app: Arc<App>, key: &str) -> Reply {
    let Some(state) = login_entry(&app, key) else {
        return reply(
            200,
            json!({"status":"unknown","message":"login state is unknown"}),
        );
    };
    let decision = state.lock().begin_poll(Instant::now());
    let (realm, platform, upstream_state) = match decision {
        PollDecision::Expired => {
            return reply(
                200,
                json!({"status":"expired","message":"login expired; start again"}),
            )
        }
        PollDecision::Cancelled => {
            return reply(200, json!({"status":"error","message":"login cancelled"}))
        }
        PollDecision::Failed => {
            return reply(
                200,
                json!({"status":"error","message":"login failed; start again"}),
            )
        }
        PollDecision::Complete(account) => {
            return reply(200, json!({"status":"ok","account":account}))
        }
        PollDecision::Wait(seconds) => {
            return reply(
                200,
                json!({"status":"pending","message":"waiting for authorization","retry_after":seconds}),
            )
        }
        PollDecision::Request(r, p, s) => (r, p, s),
    };
    let _flight = LoginFlight(state.clone());
    let request =
        match wbaccounts::build_oauth_poll_request(realm, &upstream_state, &request_options()) {
            Ok(r) => r,
            Err(_) => return fail(500, "OAuth request configuration is invalid"),
        };
    let (response, oversized) = match fetch(&app, request, JSON_LIMIT).await {
        Ok(r) => r,
        Err(r) => return r,
    };
    if oversized || has_restriction(&response) {
        return fail(502, "OAuth polling failed; start a new login");
    }
    let account = match wbaccounts::parse_oauth_poll(&response, realm, &platform) {
        Ok(OAuthPoll::Pending { .. }) => {
            let mut state = state.lock();
            if Instant::now() >= state.expires {
                return reply(200, json!({"status":"expired"}));
            }
            if matches!(state.phase, LoginPhase::Cancelled) {
                return reply(200, json!({"status":"error","message":"login cancelled"}));
            }
            state.finish(LoginPhase::Pending);
            return reply(
                200,
                json!({"status":"pending","message":"waiting for authorization","retry_after":2}),
            );
        }
        Ok(OAuthPoll::Authorized(a)) if a.realm() == realm => a,
        _ => return fail(502, "OAuth polling failed; start a new login"),
    };
    // Serialize only this uid with ordinary token refresh; never a global lock.
    let refresh_lock = app.refresh_lock(account.uid());
    let _refresh = refresh_lock.lock().await;
    match tokio::task::spawn_blocking(move || {
        let mut state = state.lock();
        if !matches!(state.phase, LoginPhase::Polling) || Instant::now() >= state.expires {
            return fail(409, "login cancelled or expired; account was not imported");
        }
        if !safe_account_destination(&app, account.uid()) {
            return fail(400, "OAuth account identity is unsafe for storage");
        }
        let mut document = account.to_json();
        if let Some(existing) = app.pool.get(account.uid()) {
            if existing.realm() != realm {
                return fail(
                    409,
                    "OAuth account conflicts with an existing account realm",
                );
            }
            // Reauthorization does not reset operator identity, proxy or limits.
            let previous = existing.to_json();
            for field in [
                "product",
                "enabled",
                "tasksEnabled",
                "weight",
                "note",
                "disabledModels",
                "proxySlot",
                "proxy",
                "credits",
                "tier",
                "addedAt",
                "lastCheckin",
                "lastDailyChat",
                "lastActivityReport",
            ] {
                document[field] = previous[field].clone();
            }
        } else {
            document["tasksEnabled"] = json!(false);
        }
        let incoming = match Account::from_json(&document) {
            Ok(a) => Arc::new(a),
            Err(_) => return fail(502, "OAuth account data is invalid"),
        };
        match app.pool.upsert(incoming, Some(&app.args.accounts_dir)) {
            Ok(account) => {
                let metadata = account.public_metadata();
                state.finish(LoginPhase::Complete(metadata.clone()));
                reply(200, json!({"status":"ok","account":metadata}))
            }
            Err(_) => {
                state.finish(LoginPhase::Failed);
                fail(500, "OAuth account could not be saved; start a new login")
            }
        }
    })
    .await
    {
        Ok(r) => r,
        Err(_) => fail(500, "OAuth account could not be saved"),
    }
}
fn safe_account_destination(app: &App, uid: &str) -> bool {
    if !safe_uid(uid) {
        return false;
    }
    let path = app.args.accounts_dir.join(format!("{uid}.json"));
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return false;
    }
    !app.pool
        .accounts()
        .iter()
        .any(|a| a.uid() != uid && a.uid().eq_ignore_ascii_case(uid))
}
fn safe_uid(uid: &str) -> bool {
    let lower = uid.to_ascii_lowercase();
    !uid.is_empty()
        && uid.len() <= 180
        && uid.trim_matches('_') == uid
        && uid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        && !matches!(
            lower.as_str(),
            "settings"
                | "active_realm"
                | "model_windows"
                | "quota_scan"
                | "model"
                | "integration-state"
                | "con"
                | "prn"
                | "aux"
                | "nul"
        )
        && !(lower.len() == 4
            && (lower.starts_with("com") || lower.starts_with("lpt"))
            && lower.as_bytes()[3].is_ascii_digit())
}

fn has_restriction(response: &InResponse) -> bool {
    let raw = String::from_utf8_lossy(&response.body);
    // Only HTTP failures permit inspection of the raw body. Successful bodies
    // contain generated text/tool arguments or arbitrary catalog metadata.
    if response.status >= 400 && wbaccounts::restriction_is_account_level(&raw) {
        return true;
    }
    if let Ok(value) = serde_json::from_str::<Value>(&raw) {
        return if response.status >= 400 {
            restricted_error_fields(&value)
        } else {
            restricted_json(&value)
        };
    }
    raw.lines()
        .filter_map(|line| line.trim_start().strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
        .any(|value| restricted_json(&value))
}
fn restricted_json(value: &Value) -> bool {
    // A successful response is an error envelope only with a nonzero top-level
    // code or an explicit error object; a message alone is not evidence.
    (wbaccounts::json_code(value).is_some_and(|code| code != 0) && restricted_error_fields(value))
        || value
            .get("error")
            .filter(|error| error.is_object())
            .is_some_and(restricted_error_fields)
}
fn restricted_error_fields(value: &Value) -> bool {
    wbaccounts::json_code(value) == Some(11140)
        || ["msg", "message", "detail"].iter().any(|field| {
            value
                .get(*field)
                .and_then(Value::as_str)
                .is_some_and(wbaccounts::restriction_is_account_level)
        })
        || value
            .get("error")
            .filter(|error| error.is_object())
            .is_some_and(restricted_error_fields)
}
fn probe_response_action(response: &InResponse) -> ResponseAction {
    if has_restriction(response) {
        ResponseAction::Restricted
    } else if (200..300).contains(&response.status) {
        // Never rescan successful content with the broad library classifier.
        ResponseAction::Success
    } else if response.status >= 400 {
        wbaccounts::classify_response(response)
    } else {
        ResponseAction::PassThrough
    }
}
async fn persist_restriction(app: Arc<App>, account: Arc<Account>) -> Reply {
    app.restricted(account.realm(), account.uid());
    app.pool
        .note_account_restriction(account.realm(), account.uid(), now());
    let saved = tokio::task::spawn_blocking(move || {
        account.note_restricted("upstream account restriction", now())
    })
    .await;
    match saved {
        Ok(Ok(())) => fail(
            403,
            "upstream restricted this account; no other account was tested",
        ),
        _ => fail(
            500,
            "upstream restricted this account; restriction could not be persisted",
        ),
    }
}
fn blocked(app: &App, account: &Account) -> Option<Reply> {
    if account.public_metadata()["restrictedUntil"]
        .as_f64()
        .unwrap_or(0.0)
        > now()
    {
        return Some(fail(403, "account is restricted"));
    }
    let seconds = app.restriction_wait(account.realm());
    if seconds > 0 {
        return Some(wait(
            seconds,
            "realm paused after upstream account restrictions",
        ));
    }
    if !account.enabled() {
        return Some(fail(409, "account is disabled"));
    }
    None
}
async fn ready(app: &App, account: &Arc<Account>, model: &str) -> Result<(), Reply> {
    if let Some(r) = blocked(app, account) {
        return Err(r);
    }
    if matches!(account.readiness(model, now()), Readiness::NeedsRefresh) {
        crate::dispatch::refresh_account(app, account, false)
            .await
            .map_err(|_| fail(502, "account token refresh failed; details omitted"))?;
    }
    if let Some(r) = blocked(app, account) {
        return Err(r);
    }
    match account.readiness(model, now()) {
        Readiness::Ready => Ok(()),
        Readiness::Blocked { retry_after, .. } if retry_after > 0.0 => Err(wait(
            retry_after.ceil() as u64,
            "account or model is cooling down",
        )),
        _ => Err(fail(409, "account is not eligible for this operation")),
    }
}
async fn account_test(app: Arc<App>, payload: &Value) -> Reply {
    let uid = text(payload, "uid");
    if uid.is_empty() {
        return fail(
            400,
            "one account uid is required; bulk account tests are not supported",
        );
    }
    let Some(account) = app.pool.get(uid) else {
        return fail(404, "no such account");
    };
    let model = match payload.get("model") {
        None => "deepseek-v4.1-flash",
        Some(Value::String(s))
            if !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._:/-".contains(&c)) =>
        {
            s
        }
        _ => return fail(400, "invalid test model"),
    };
    let started = Instant::now();
    if crate::dispatch::apply_guards(&app).is_err() {
        return fail(500, "could not load account usage guards");
    }
    if let Err(r) = ready(&app, &account, model).await {
        return r;
    }
    let _global = match app.slots.clone().try_acquire_owned() {
        Ok(s) => s,
        Err(_) => return wait(1, "gateway is busy"),
    };
    // A test may not race another test/ordinary request for this account.
    let _slot = match account.try_acquire_slot(1) {
        Some(s) => s,
        None => return wait(2, "account is busy"),
    };
    let settings = app.settings.snapshot();
    let body = json!({"model":model,"messages":[{"role":"user","content":"Reply briefly with OK."}],"max_tokens":16,"stream":true,"stream_options":{"include_usage":true}});
    for attempt in 0..2 {
        if let Some(r) = blocked(&app, &account) {
            return r;
        }
        let mut options = wbaccounts::ChatOptions::default();
        options.desktop_chat_compat = crate::dispatch::desktop_chat_compat_enabled();
        options.request = request_options();
        options.request.proxy = account.resolve_proxy(&json!(app.settings.proxy_slots()));
        options.model = Some(model.into());
        options.device_token = settings["upstream"]["device_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let request = match wbaccounts::build_chat_request(&account.cred(), &body, &options) {
            Ok(r) => r,
            Err(_) => return fail(400, "account chat request configuration is invalid"),
        };
        let (response, oversized) = match fetch(&app, request, TEST_LIMIT).await {
            Ok(r) => r,
            Err(r) => return r,
        };
        if has_restriction(&response) {
            return persist_restriction(app.clone(), account.clone()).await;
        }
        if oversized {
            return fail(502, "account test response exceeds the 32 KiB limit");
        }
        let action = probe_response_action(&response);
        if action == ResponseAction::Refresh && attempt == 0 && !account.cred().is_api_key() {
            if crate::dispatch::refresh_account(&app, &account, true)
                .await
                .is_err()
            {
                return fail(502, "account token refresh failed; details omitted");
            }
            continue;
        }
        if action != ResponseAction::Success {
            let copy = account.clone();
            let status = response.status;
            let model_copy = model.to_owned();
            let window = match action {
                ResponseAction::RateLimited {
                    reset_at: Some(reset),
                    ..
                } => Some(reset),
                _ => None,
            };
            let model_for_window = model.to_owned();
            let pool = app.pool.clone();
            let directory = app.args.accounts_dir.clone();
            let uid_for_window = account.uid().to_owned();
            if !matches!(
                tokio::task::spawn_blocking(move || {
                    let outcome = copy.apply_response(&response, &model_copy, now())?;
                    if let Some(reset) = window {
                        pool.record_model_window(
                            directory,
                            &uid_for_window,
                            &model_for_window,
                            reset,
                            now(),
                        )?;
                    }
                    Ok::<_, wbaccounts::Error>(outcome)
                })
                .await,
                Ok(Ok(_))
            ) {
                return fail(500, "account test outcome could not be persisted");
            }
            return reply(
                502,
                json!({"ok":false,"uid":uid,"model":model,"status":status,"error":"upstream rejected the account test","elapsed_ms":started.elapsed().as_millis() as u64}),
            );
        }
        let summary = match parse_probe(&response.body) {
            Ok(s) => s,
            Err(message) => return fail(502, message),
        };
        account.note_success(model, now());
        // Only a static acknowledgement, not arbitrary upstream text/diagnostics.
        return reply(
            200,
            json!({"ok":true,"uid":uid,"model":model,"reply":"minimal chat completed","output_bytes":summary,
            "elapsed_ms":started.elapsed().as_millis() as u64,"account":account.public_metadata()}),
        );
    }
    fail(502, "account test failed")
}

/// Require a real completed chat, not just HTTP 200 or a fabricated fallback.
fn parse_probe(bytes: &[u8]) -> Result<usize, &'static str> {
    if bytes.len() > TEST_LIMIT {
        return Err("account test response exceeds the 32 KiB limit");
    }
    let raw = std::str::from_utf8(bytes).map_err(|_| "account test returned invalid UTF-8")?;
    let mut output = 0;
    let mut completed = false;
    let mut saw_choice = false;
    let values = if let Ok(value) = serde_json::from_str::<Value>(raw) {
        vec![value]
    } else {
        let mut values = Vec::new();
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty()
                || line.starts_with(':')
                || line.starts_with("event:")
                || line.starts_with("id:")
                || line.starts_with("retry:")
            {
                continue;
            }
            let data = wbcore::sse::strip_data_prefix(line);
            if data == "[DONE]" {
                completed = true;
                continue;
            }
            values.push(
                serde_json::from_str(data)
                    .map_err(|_| "account test returned malformed stream data")?,
            );
        }
        values
    };
    for value in values {
        if value.get("error").is_some_and(|v| !v.is_null())
            || wbaccounts::json_code(&value).is_some_and(|c| c != 0)
        {
            return Err("upstream returned a chat error");
        }
        if let Some(choices) = value["choices"].as_array() {
            for choice in choices {
                saw_choice = true;
                let reason = choice["finish_reason"].as_str().unwrap_or("");
                if matches!(reason, "error" | "content_filter" | "content_review") {
                    return Err("upstream rejected the account test");
                }
                completed |= !reason.is_empty();
                for field in ["message", "delta"] {
                    for key in ["content", "reasoning_content"] {
                        output += choice[field][key].as_str().map(str::len).unwrap_or(0);
                    }
                }
            }
        }
    }
    if !completed || !saw_choice || output == 0 {
        return Err("upstream returned no complete chat output");
    }
    Ok(output)
}
fn take_cooldown(
    gates: &mut HashMap<Realm, Instant>,
    realm: Realm,
    at: Instant,
) -> Result<(), u64> {
    if let Some(until) = gates.get(&realm).filter(|until| **until > at) {
        return Err((*until - at).as_secs() + 1);
    }
    gates.insert(realm, at + Duration::from_secs(30));
    Ok(())
}
async fn catalog_refresh(app: Arc<App>, payload: &Value) -> Reply {
    let realm = match realm(&app, payload) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let account = if !text(payload, "uid").is_empty() {
        match app.pool.get(text(payload, "uid")) {
            Some(a) if a.realm() == realm => a,
            Some(_) => return fail(409, "account belongs to a different realm"),
            None => return fail(404, "no such account"),
        }
    } else {
        let Some(a) = app.pool.accounts().into_iter().find(|a| {
            a.realm() == realm
                && matches!(
                    a.readiness("", now()),
                    Readiness::Ready | Readiness::NeedsRefresh
                )
        }) else {
            return fail(409, "no eligible account for this realm");
        };
        a
    };
    if let Some(r) = blocked(&app, &account) {
        return r;
    }
    let cooldown = take_cooldown(&mut CATALOG_GATE.lock(), realm, Instant::now());
    if let Err(seconds) = cooldown {
        return wait(seconds, "catalog refresh cooldown is 30 seconds");
    }
    if let Err(r) = ready(&app, &account, "").await {
        return r;
    }
    let proxy = account.resolve_proxy(&json!(app.settings.proxy_slots()));
    let egress = if proxy.is_empty() {
        Egress::Direct
    } else {
        Egress::Proxy(proxy)
    };
    let base = std::env::var("WB_CN_CHAT_UPSTREAM").ok();
    // Exactly one standard config request: no identity/account fallback.
    let Some(request) = wbcatalog::config_requests(&account.cred(), egress, base.as_deref())
        .into_iter()
        .next()
    else {
        return fail(500, "catalog request is unavailable");
    };
    let (response, oversized) = match fetch(&app, request, CATALOG_LIMIT).await {
        Ok(r) => r,
        Err(r) => return r,
    };
    if has_restriction(&response) {
        return persist_restriction(app.clone(), account.clone()).await;
    }
    if oversized {
        return fail(
            502,
            "catalog response exceeds the size limit; previous catalog retained",
        );
    }
    if !(200..300).contains(&response.status) {
        return fail(
            502,
            "catalog upstream request failed; previous catalog retained",
        );
    }
    let payload: Value = match serde_json::from_slice(&response.body) {
        Ok(v) => v,
        Err(_) => return fail(502, "invalid catalog response; previous catalog retained"),
    };
    let mut catalog = app.catalog.write();
    if catalog.ingest(realm, payload).is_err() {
        return fail(
            502,
            "catalog response contains no usable catalog; previous catalog retained",
        );
    }
    reply(
        200,
        json!({"ok":true,"realm":realm.as_str(),"count":catalog.entries(realm).len(),"catalog":catalog.provenance(realm),"msg":"catalog updated"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(at: Instant) -> Login {
        Login {
            owner: PathBuf::new(),
            realm: Realm::Intl,
            platform: "CLI".into(),
            upstream_state: "secret".into(),
            expires: at + LOGIN_TTL,
            next_poll: at,
            phase: LoginPhase::Pending,
        }
    }
    #[test]
    fn poll_is_single_flight_rate_limited_and_expires() {
        let at = Instant::now();
        let mut state = state(at);
        assert!(matches!(state.begin_poll(at), PollDecision::Request(..)));
        assert!(matches!(
            state.begin_poll(at + Duration::from_secs(10)),
            PollDecision::Wait(_)
        ));
        state.finish(LoginPhase::Pending);
        assert!(matches!(state.begin_poll(at), PollDecision::Wait(_)));
        assert!(matches!(
            state.begin_poll(at + POLL_INTERVAL),
            PollDecision::Request(..)
        ));
        assert!(matches!(
            state.begin_poll(at + LOGIN_TTL),
            PollDecision::Expired
        ));
    }
    #[test]
    fn cancellation_and_completed_login_cannot_rotate_again() {
        let at = Instant::now();
        let mut state = state(at);
        state.phase = LoginPhase::Cancelled;
        state.finish(LoginPhase::Complete(json!({"uid":"a"})));
        assert!(matches!(state.begin_poll(at), PollDecision::Cancelled));
        state.phase = LoginPhase::Polling;
        state.finish(LoginPhase::Complete(json!({"uid":"a"})));
        assert!(state.upstream_state.is_empty());
        assert!(matches!(state.begin_poll(at), PollDecision::Complete(_)));
    }
    #[test]
    fn abandoned_poll_is_terminal() {
        let at = Instant::now();
        let state = Arc::new(Mutex::new(state(at)));
        state.lock().begin_poll(at);
        drop(LoginFlight(state.clone()));
        assert!(matches!(
            state.lock().begin_poll(at + POLL_INTERVAL),
            PollDecision::Failed
        ));
    }
    #[test]
    fn validates_urls_state_and_storage_names() {
        assert!(valid_auth_url("https://example.invalid/auth?state=secret"));
        for bad in [
            "javascript:alert(1)",
            "https://user:secret@example.invalid/",
            "https://example.invalid/\n",
        ] {
            assert!(!valid_auth_url(bad));
        }
        assert!(valid_state(&"a".repeat(48)));
        assert!(!valid_state("state=secret"));
        assert!(safe_uid("user-123"));
        for bad in ["../settings", "settings", "CON", "com1", "_", "a/b", ""] {
            assert!(!safe_uid(bad));
        }
    }
    #[test]
    fn parses_complete_chat_without_fabricating_success() {
        assert_eq!(
            parse_probe(br#"{"choices":[{"message":{"content":"OK"},"finish_reason":"stop"}]}"#),
            Ok(2)
        );
        assert_eq!(
            parse_probe(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"OK\"}}]}\n\ndata: [DONE]\n"
            ),
            Ok(2)
        );
        for bad in [
            b"{}".as_slice(),
            b"data: [DONE]\n",
            b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n",
            b"data: {\"error\":{\"message\":\"secret\"}}\n",
        ] {
            assert!(parse_probe(bad).is_err());
        }
        assert!(parse_probe(&vec![b'a'; TEST_LIMIT + 1]).is_err());
    }
    #[test]
    fn restrictions_detected_in_http_200_and_stream_errors() {
        for body in [
            r#"{"code":11140}"#,
            r#"{"error":{"code":"11140"}}"#,
            "data: {\"error\":{\"code\":\"11140\"}}\n",
            "data: {\"code\":11140}\n",
            r#"{"code":1,"msg":"\u8d26\u53f7\u5b58\u5728\u5f02\u5e38\u884c\u4e3a"}"#,
            r#"{"error":{"message":"unusual activity"}}"#,
            r#"{"error":{"error":{"code":11140}}}"#,
        ] {
            let response = InResponse {
                status: 200,
                headers: vec![],
                body: body.as_bytes().to_vec(),
            };
            assert!(has_restriction(&response));
            assert_eq!(probe_response_action(&response), ResponseAction::Restricted);
        }
    }
    #[test]
    fn successful_content_and_catalog_metadata_are_not_restrictions() {
        for body in [
            r#"{"choices":[{"message":{"content":"Explain unusual activity and 账号存在异常行为"},"finish_reason":"stop"}]}"#,
            "data: {\"choices\":[{\"delta\":{\"content\":\"unusual activity\"}}]}\n\ndata: [DONE]\n",
            r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"{\"error\":{\"code\":11140},\"message\":\"unusual activity\"}"}}]}}]}"#,
            r#"{"code":0,"data":{"models":[{"description":"unusual activity","error":{"code":11140}}]}}"#,
            r#"{"code":0,"message":"unusual activity"}"#,
            r#"{"msg":"\u8d26\u53f7\u5b58\u5728\u5f02\u5e38\u884c\u4e3a"}"#,
            r#"{"error":"unusual activity"}"#,
            "unusual activity",
        ] {
            let response = InResponse {
                status: 200,
                headers: vec![],
                body: body.as_bytes().to_vec(),
            };
            assert!(!has_restriction(&response));
            assert_eq!(probe_response_action(&response), ResponseAction::Success);
        }
    }
    #[test]
    fn structured_errors_do_not_scan_non_error_payload_fields() {
        for value in [
            json!({"code":1,"data":{"message":"unusual activity","code":11140}}),
            json!({"error":{"message":"another error","data":{"code":11140},"content":"unusual activity"}}),
            json!({"error":{"tool_calls":[{"function":{"arguments":"unusual activity"}}]}}),
        ] {
            assert!(!restricted_json(&value));
        }
    }
    #[test]
    fn http_errors_still_recognize_restriction_diagnostics() {
        for body in [
            "unusual activity",
            r#"{"msg":"\u8d26\u53f7\u5b58\u5728\u5f02\u5e38\u884c\u4e3a"}"#,
        ] {
            assert!(has_restriction(&InResponse {
                status: 403,
                headers: vec![],
                body: body.as_bytes().to_vec(),
            }));
        }
    }
    #[test]
    fn catalog_cooldown_is_per_realm_and_counts_failed_attempts() {
        let at = Instant::now();
        let mut gates = HashMap::new();
        assert!(take_cooldown(&mut gates, Realm::Intl, at).is_ok());
        assert!(take_cooldown(&mut gates, Realm::Intl, at + Duration::from_secs(29)).is_err());
        assert!(take_cooldown(&mut gates, Realm::Cn, at).is_ok());
        assert!(take_cooldown(&mut gates, Realm::Intl, at + Duration::from_secs(30)).is_ok());
    }
}
