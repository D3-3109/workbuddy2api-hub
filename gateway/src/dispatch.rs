use crate::{
    state::App,
    wire::{self, Query, Reply},
};
use bytes::Bytes;
use hyper::HeaderMap;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::sync::mpsc;
use wbaccounts::{Account, AccountLease, PickOptions, Readiness};
use wbcore::{aggregate::Aggregator, sse::LineSplitter, timeutil::now, types::Realm};
use wbtranslate::{ResponseOptions, TranslationOptions};

const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 2 * 1024 * 1024;

pub fn desktop_chat_compat_enabled() -> bool {
    std::env::var("WB_DESKTOP_CHAT_COMPAT").is_ok_and(|v| v.trim() == "1")
}

pub fn key_allows_model(key: Option<&Value>, model: &str) -> bool {
    match key {
        Some(key) => wbsettings::key_allows_model(key, model),
        None => true,
    }
}

#[derive(Clone)]
enum Protocol {
    Chat,
    Responses(ResponseOptions),
    Compaction(ResponseOptions),
    Messages,
}

pub async fn chat(
    app: Arc<App>,
    path: &str,
    payload: Value,
    headers: &HeaderMap,
    q: &Query,
    key: Option<Value>,
    pin: Option<String>,
) -> Reply {
    let anthropic = path.contains("messages");
    let realm = match app.request_realm(headers, q, key.as_ref()) {
        Ok(r) => r,
        Err(()) => return wire::error(400, "realm must be intl or cn", anthropic),
    };
    let mut settings = app.settings.snapshot();
    settings["system_prompt"] = json!(app.args.system_prompt);
    for (env, field) in [
        ("WB_BACKGROUND_MODEL_TARGET", "background_model_target"),
        ("WB_BACKGROUND_MODEL_NAMES", "background_model_names"),
        ("WB_DEFAULT_REASONING_EFFORT", "default_reasoning_effort"),
    ] {
        if let Ok(v) = std::env::var(env) {
            settings[field] = json!(v);
        }
    }
    if settings["prompt"]["text"]
        .as_str()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        if let Some(file) = settings["prompt"]["file"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            match std::fs::read_to_string(file) {
                Ok(s) if s.len() <= 65536 => settings["prompt"]["text"] = json!(s),
                _ => {
                    return wire::error(
                        500,
                        "configured prompt file is unreadable or larger than 64 KiB",
                        anthropic,
                    )
                }
            }
        }
    }
    let options = TranslationOptions {
        preserve_request_contract: true,
        default_max_output_tokens: None,
        compaction: path.ends_with("/compact"),
        local_web_tools: settings["local_web_tools"] == json!(true),
    };
    if path.ends_with("/count_tokens") {
        let _key_slot = match key
            .as_ref()
            .map(|k| app.settings.try_acquire_key(k))
            .transpose()
        {
            Ok(g) => g,
            Err(_) => return wire::retry_error(1, "API key concurrency limit reached", true),
        };
        return match wbtranslate::count_tokens(&payload, &options) {
            Ok(v) => wire::json_reply(200, v),
            Err(e) => wire::error(e.status, e.message, true),
        };
    }
    let original_stream = wbcore::pyval::truthy(&payload["stream"]);
    let (chat_payload, protocol) = if path.contains("responses") {
        match wbtranslate::responses_to_chat(&payload, &options) {
            Ok(t) => {
                let opts = ResponseOptions {
                    model: payload["model"].as_str().unwrap_or("").into(),
                    metadata: t.metadata,
                    response_id: None,
                    created_at: None,
                };
                (
                    t.chat,
                    if t.compaction {
                        Protocol::Compaction(opts)
                    } else {
                        Protocol::Responses(opts)
                    },
                )
            }
            Err(e) => return wire::error(e.status, e.message, false),
        }
    } else if anthropic {
        match wbtranslate::messages_to_chat(&payload, &options) {
            Ok(t) => (t.chat, Protocol::Messages),
            Err(e) => return wire::error(e.status, e.message, true),
        }
    } else {
        (payload.clone(), Protocol::Chat)
    };
    let lookup_model = match wbchat::resolve_model(&chat_payload, &settings) {
        Ok(m) => m,
        Err(e) => return wire::error(400, e.to_string(), anthropic),
    };
    let catalog = app.catalog.read().meta(realm, &lookup_model);
    let normalized = match wbchat::normalize_request(&chat_payload, &settings, &catalog) {
        Ok(n) => n,
        Err(e) => return wire::error(400, e.to_string(), anthropic),
    };
    let model = normalized.model.clone();
    if !key_allows_model(key.as_ref(), &model) {
        return wire::error(
            400,
            "API key does not permit the requested model",
            anthropic,
        );
    }
    let key_slot = match key
        .as_ref()
        .map(|k| app.settings.try_acquire_key(k))
        .transpose()
    {
        Ok(g) => g,
        Err(_) => return wire::retry_error(1, "API key concurrency limit reached", anthropic),
    };
    let global = match tokio::time::timeout(
        Duration::from_secs(app.args.chat_slot_wait),
        app.slots.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(g)) => g,
        _ => {
            return wire::error(
                503,
                "gateway is at its concurrent chat limit; retry shortly",
                anthropic,
            )
        }
    };
    let started = now();
    let session = extract_session(headers, &payload, &normalized.body, &settings);
    let mut pick = PickOptions {
        realm: Some(realm),
        model: model.clone(),
        session_key: session.0,
        page: session.1,
        now: now(),
        ..Default::default()
    };
    if let Some(ids) = key
        .as_ref()
        .and_then(|v| v["accounts"].as_array())
        .filter(|ids| !ids.is_empty())
    {
        pick.allowed = Some(
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        );
    }
    if let Some(reference) = pin.as_deref() {
        let account = match app.pool.find_unique(reference) {
            Ok(a) => a,
            Err(e) => return wire::error(404, e.to_string(), anthropic),
        };
        if account.realm() != realm {
            return wire::error(409, "pinned account belongs to another realm", anthropic);
        }
        if pick
            .allowed
            .as_ref()
            .is_some_and(|s| !s.contains(account.uid()))
        {
            return wire::error(403, "API key does not permit the pinned account", anthropic);
        }
        pick.allowed = Some(HashSet::from([account.uid().to_string()]));
    }
    if let Err(e) = apply_guards(&app) {
        return wire::error(500, e, anthropic);
    }
    // This guard exists before the first upstream await. Dropping a cancelled
    // service future records the observed work once, including pre-header aborts.
    let mut audit = RequestRecord::new(
        app.clone(),
        model.clone(),
        key,
        original_stream,
        started,
        normalized.body.clone(),
    );
    let opened = open_upstream(
        &app,
        &normalized.body,
        &settings,
        pick,
        pin.is_some(),
        &mut audit,
    )
    .await;
    let (upstream, lease) = match opened {
        Ok(v) => v,
        Err(e) => {
            audit.complete("failed", Some(&e));
            return if let Some(wait) = e.retry_after {
                wire::retry_error(wait, &e.message, anthropic)
            } else {
                wire::error(e.status, e.message, anthropic)
            };
        }
    };
    let protocol = match protocol {
        Protocol::Responses(mut opts) => {
            opts.model = model.clone();
            Protocol::Responses(opts)
        }
        Protocol::Compaction(mut opts) => {
            opts.model = model.clone();
            Protocol::Compaction(opts)
        }
        p => p,
    };
    if original_stream {
        let (tx, reply) = wire::sse_reply();
        tokio::spawn(async move {
            let _global = global;
            let _key = key_slot;
            let _lease = lease;
            run_stream(upstream, tx, protocol, audit).await;
        });
        reply
    } else {
        let _global = global;
        let _key = key_slot;
        let mut aggregated = match collect_chat(upstream, &model, &mut audit).await {
            Ok(value) => value,
            Err(error) => {
                audit.complete("upstream_aborted", Some(&error));
                return wire::error(error.status, error.message, anthropic);
            }
        };
        aggregated.as_object_mut().map(|m| {
            m.remove("elapsed_ms");
            m.remove("first_chunk_at");
        });
        let result = match protocol {
            Protocol::Chat => Ok(aggregated),
            Protocol::Responses(opts) => wbtranslate::chat_to_response(&aggregated, &opts),
            Protocol::Compaction(opts) => wbtranslate::chat_to_compaction(&aggregated, &opts),
            Protocol::Messages => wbtranslate::chat_to_messages(&aggregated),
        };
        match result {
            Ok(v) => {
                lease.account().note_success(&model, now());
                audit.complete("completed", None);
                wire::json_reply(200, v)
            }
            Err(e) => {
                let failure = OpenFailure::new(e.status, e.message);
                audit.complete("upstream_aborted", Some(&failure));
                wire::error(failure.status, failure.message, anthropic)
            }
        }
    }
}

fn extract_session(
    headers: &HeaderMap,
    payload: &Value,
    body: &Value,
    settings: &Value,
) -> (String, Option<(usize, usize)>) {
    for h in [
        "x-conversation-id",
        "conversation-id",
        "x-session-id",
        "session-id",
    ] {
        if let Some(s) = wire::header_text(headers, h).filter(|s| !s.trim().is_empty()) {
            return (s.trim().into(), None);
        }
    }
    for v in [
        &payload["conversation_id"],
        &payload["session_id"],
        &payload["metadata"]["conversation_id"],
    ] {
        if let Some(s) = v.as_str().filter(|s| !s.trim().is_empty()) {
            return (s.trim().into(), None);
        }
    }
    if std::env::var("WB_AFFINITY_BY_PREFIX").is_ok_and(|s| s == "0") {
        return (String::new(), None);
    }
    let msgs = body["messages"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    wbaccounts::affinity_route(
        msgs,
        settings["affinity_stable_key_enabled"] == json!(true),
        400,
        3,
    )
}

pub fn apply_guards(app: &App) -> Result<(), String> {
    let totals = app
        .usage
        .daily_totals()
        .map_err(|_| "could not read daily usage counters".to_string())?;
    let catalog = app.catalog.read();
    for a in app.pool.accounts() {
        let realm = a.realm();
        let uid = a.uid();
        let mut g = wbaccounts::Guards::default();
        g.reserve_credits = app.settings.limit("reserve_credits", realm) as f64;
        g.expiring_window_days = app.settings.limit("expiring_window_days", realm) as f64;
        g.daily_token_limit = app.settings.limit("daily_token_limit", realm);
        g.daily_tokens_today = Some(totals["tokens"][uid].as_u64().unwrap_or(0));
        g.daily_credit_limit = app.settings.limit("daily_credit_limit", realm) as f64;
        g.daily_credits_today = Some(totals["credits"][uid].as_f64().unwrap_or(0.0));
        g.model_daily_token_limit = app.settings.limit("model_daily_token_limit", realm);
        g.model_daily_tokens = totals["models"][uid]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_u64().unwrap_or(0)))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(rows) = catalog.list(realm)["data"].as_array() {
            for row in rows {
                if let Some(id) = row["id"].as_str() {
                    if matches!(row["credits"].as_str(), Some("x0.00" | "x0" | "0" | "0.00")) {
                        g.free_models.insert(id.into());
                    }
                    if catalog.requires_pro(realm, id) {
                        g.pro_models.insert(id.into());
                    }
                }
            }
        }
        a.set_guards(g);
    }
    Ok(())
}

#[derive(Debug)]
struct OpenFailure {
    status: u16,
    message: String,
    retry_after: Option<u64>,
    restricted: bool,
}
impl OpenFailure {
    fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after: None,
            restricted: false,
        }
    }
    fn restriction() -> Self {
        Self {
            restricted: true,
            ..Self::new(403, "upstream account restricted (code 11140)")
        }
    }
    fn retry(wait: u64, message: &str) -> Self {
        Self {
            retry_after: Some(wait.max(1)),
            ..Self::new(429, message)
        }
    }
}

// Only protocol error fields are authoritative. Never search generated choices,
// reasoning, tool arguments, or arbitrary JSON descendants for restriction text.
fn structured_failure(value: &Value) -> Option<OpenFailure> {
    let code = wbaccounts::json_code(value);
    let error = value
        .get("error")
        .filter(|e| !e.is_null() && *e != &json!(false));
    let error_code = error.and_then(wbaccounts::json_code);
    let error_finish = value["choices"]
        .as_array()
        .is_some_and(|choices| choices.iter().any(|c| c["finish_reason"] == "error"));
    let has_error =
        error.is_some() || code.is_some_and(|c| c != 0) || value["type"] == "error" || error_finish;
    if !has_error {
        return None;
    }
    let restricted = code == Some(11140)
        || error_code == Some(11140)
        || error.is_some_and(|e| {
            [e.as_str(), e["message"].as_str(), e["msg"].as_str()]
                .into_iter()
                .flatten()
                .any(wbaccounts::restriction_is_account_level)
        })
        || ((code.is_some_and(|c| c != 0) || value["type"] == "error")
            && [value["message"].as_str(), value["msg"].as_str()]
                .into_iter()
                .flatten()
                .any(wbaccounts::restriction_is_account_level));
    Some(if restricted {
        OpenFailure::restriction()
    } else {
        OpenFailure::new(502, "upstream returned a stream error")
    })
}

fn restrict_account(app: &App, account: &Arc<Account>) -> OpenFailure {
    let at = now();
    // Update both in-memory barriers even if persistence fails. Never rotate on
    // a restriction whose durable write failed.
    let saved = account.note_restricted("upstream account restricted (code 11140)", at);
    app.restricted(account.realm(), account.uid());
    app.pool
        .note_account_restriction(account.realm(), account.uid(), at);
    if saved.is_err() {
        app.log("ERROR", "could not persist upstream account restriction");
    }
    OpenFailure::restriction()
}

#[derive(Default)]
struct ObservedStream {
    usage: Option<Value>,
    credit: Option<f64>,
    first: Option<f64>,
    saw_done: bool,
    choices: HashMap<i64, bool>,
}
impl ObservedStream {
    fn capture(&mut self, value: &Value) {
        if self.first.is_none() {
            self.first = Some(now());
        }
        if let Some(usage) = value
            .get("usage")
            .filter(|u| u.is_object() && !u.as_object().unwrap().is_empty())
        {
            if let Some(credit) =
                wbcore::pyval::py_float(&usage["credit"]).filter(|n| n.is_finite() && *n >= 0.0)
            {
                self.credit = Some(self.credit.unwrap_or(0.0).max(credit));
            }
            wbcore::usage::keep_best(&mut self.usage, usage.clone());
            if let Some(best) = &mut self.usage {
                wbcore::usage::normalize_cache_aliases(best);
                if let Some(credit) = self.credit {
                    best["credit"] = json!(credit);
                }
            }
        }
    }
    /// True means feed this line to the protocol adapter. Metadata/comments are
    /// ignored. Both newline-terminated and final buffered lines use this path.
    fn line(&mut self, line: &str) -> Result<bool, OpenFailure> {
        let data = wbcore::sse::strip_data_prefix(line);
        if data.is_empty()
            || data.starts_with("event:")
            || data.starts_with("id:")
            || data.starts_with("retry:")
        {
            return Ok(false);
        }
        if data == "[DONE]" {
            self.saw_done = true;
            return Ok(true);
        }
        let value: Value = serde_json::from_str(data)
            .map_err(|_| OpenFailure::new(502, "invalid JSON in upstream event stream"))?;
        if !value.is_object() {
            return Err(OpenFailure::new(502, "invalid upstream event"));
        }
        self.capture(&value); // An error event may still carry billable usage.
        if let Some(error) = structured_failure(&value) {
            return Err(error);
        }
        if let Some(choices) = value["choices"].as_array() {
            if self.saw_done && !choices.is_empty() {
                return Err(OpenFailure::new(502, "upstream emitted choices after DONE"));
            }
            for (ordinal, choice) in choices.iter().enumerate() {
                let index = choice["index"].as_i64().unwrap_or(ordinal as i64);
                let finished = choice["finish_reason"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty());
                let has_delta = choice["delta"].as_object().is_some_and(|m| !m.is_empty());
                let state = self.choices.entry(index).or_insert(false);
                if finished {
                    *state = true;
                } else if has_delta {
                    *state = false;
                }
            }
        }
        Ok(true)
    }
    fn finish(&self) -> Result<(), OpenFailure> {
        // Some providers close after an explicit finish_reason without DONE.
        // Require every observed choice to finish in that case; midtext EOF is
        // never promoted to successful completion.
        if !self.choices.is_empty() && (self.saw_done || self.choices.values().all(|done| *done)) {
            Ok(())
        } else {
            Err(OpenFailure::new(
                502,
                "upstream stream ended without a completion signal",
            ))
        }
    }
}

struct RequestRecord {
    app: Arc<App>,
    account: Option<Arc<Account>>,
    model: String,
    key: Option<Value>,
    stream: bool,
    started: f64,
    body: Value,
    observed: ObservedStream,
    recorded: bool,
}
impl RequestRecord {
    fn new(
        app: Arc<App>,
        model: String,
        key: Option<Value>,
        stream: bool,
        started: f64,
        body: Value,
    ) -> Self {
        Self {
            app,
            account: None,
            model,
            key,
            stream,
            started,
            body,
            observed: ObservedStream::default(),
            recorded: false,
        }
    }
    fn line(&mut self, line: &str) -> Result<bool, OpenFailure> {
        match self.observed.line(line) {
            Err(error) if error.restricted => {
                if let Some(account) = &self.account {
                    return Err(restrict_account(&self.app, account));
                }
                Err(error)
            }
            result => result,
        }
    }
    fn complete(&mut self, outcome: &str, failure: Option<&OpenFailure>) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        record(
            &self.app,
            self.account.as_ref(),
            &self.model,
            self.key.as_ref(),
            self.stream,
            self.started,
            self.observed.first,
            self.observed.usage.as_ref(),
            outcome,
            failure.map(|e| (e.status, e.message.as_str())),
            &self.body,
        );
    }
}
impl Drop for RequestRecord {
    fn drop(&mut self) {
        self.complete("client_aborted", None);
    }
}

async fn open_upstream(
    app: &App,
    body: &Value,
    settings: &Value,
    mut pick: PickOptions,
    pinned: bool,
    audit: &mut RequestRecord,
) -> Result<(wbhttp::Response, AccountLease), OpenFailure> {
    let realm = pick.realm.unwrap_or(Realm::Intl);
    // A pinned request still gets its one same-account retry after refresh.
    let max = if pinned {
        2
    } else {
        app.pool
            .accounts()
            .len()
            .saturating_mul(2)
            .saturating_add(2)
    };
    let mut fallback_refreshed = false;
    let mut last = OpenFailure::new(
        503,
        "no usable account for this realm/model; import or enable an account in the panel",
    );
    let mut refreshed = HashSet::new();
    let turn = wbcore::ids::new_message_id();
    for _ in 0..max {
        pick.now = now();
        let hold = app.restriction_wait(realm);
        if hold > 0 {
            return Err(OpenFailure::retry(
                hold,
                "upstream restriction wave: realm temporarily paused",
            ));
        }
        let limit = settings["account_max_concurrent"].as_u64().unwrap_or(0) as usize;
        let lease = match app.pool.acquire(&pick, limit) {
            Ok(l) => l,
            Err(wbaccounts::PoolError::Busy) => {
                return Err(OpenFailure::retry(
                    1,
                    "all usable accounts are serving other requests",
                ));
            }
            Err(wbaccounts::PoolError::Restricted { retry_after }) => {
                return Err(OpenFailure::retry(
                    retry_after.ceil() as u64,
                    "upstream restriction wave: realm temporarily paused",
                ));
            }
            Err(wbaccounts::PoolError::NoUsableAccount) => {
                // Healthy credentials always win. At most one bounded fallback
                // refresh per request; never serially refresh the whole pool.
                if fallback_refreshed {
                    break;
                }
                let Some(account) = refresh_fallback(&app.pool, &pick) else {
                    break;
                };
                fallback_refreshed = true;
                match refresh_checked(app, &account, false).await {
                    Ok(()) => continue,
                    Err(error) if error.restricted => return Err(error),
                    Err(error) => return Err(error),
                }
            }
        };
        let account = lease.account().clone();
        audit.account = Some(account.clone());
        audit.observed = ObservedStream::default();
        pick.exclude.insert(account.uid().to_string());
        let mut opts = wbaccounts::ChatOptions::default();
        opts.desktop_chat_compat = desktop_chat_compat_enabled();
        opts.request.proxy = account.resolve_proxy(&json!(app.settings.proxy_slots()));
        opts.request.cn_chat_base = std::env::var("WB_CN_CHAT_UPSTREAM").ok();
        opts.request.timeout_secs = settings["upstream"]["header_timeout_seconds"]
            .as_f64()
            .unwrap_or(120.0);
        opts.session_key = pick.session_key.clone();
        opts.turn_request_id = Some(turn.clone());
        opts.model = Some(pick.model.clone());
        opts.now = now();
        opts.device_token = settings["upstream"]["device_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        if opts.device_token.is_none() {
            if let Some(path) = settings["upstream"]["device_token_file"]
                .as_str()
                .filter(|s| !s.is_empty())
            {
                let bytes = std::fs::read(path).map_err(|_| {
                    OpenFailure::new(500, "configured device token file cannot be read")
                })?;
                if bytes.len() > 1024 {
                    return Err(OpenFailure::new(
                        500,
                        "device token file exceeds 1024 bytes",
                    ));
                }
                opts.device_token = Some(
                    String::from_utf8(bytes)
                        .map_err(|_| OpenFailure::new(500, "device token must be UTF-8"))?
                        .trim()
                        .into(),
                );
            }
        }
        let versions = &settings["identity_versions"];
        macro_rules! version {
            ($field:ident) => {
                if let Some(v) = versions[stringify!($field)]
                    .as_str()
                    .filter(|s| !s.is_empty())
                {
                    opts.versions.$field = v.to_string();
                }
            };
        }
        version!(desktop_intl);
        version!(desktop_cn);
        version!(desktop_cli);
        version!(vscode_product);
        version!(vscode_ide);
        version!(cli);
        version!(sdk);
        version!(node);
        if versions["node"].as_str().unwrap_or("").trim().is_empty() {
            if let Ok(node) = std::env::var("WB_IDENTITY_NODE_VERSION") {
                if !node.trim().is_empty() {
                    opts.versions.node = node.trim().to_owned();
                }
            }
        }
        for (name, target) in [
            ("WB_IDENTITY_OS", &mut opts.versions.os),
            ("WB_IDENTITY_ARCH", &mut opts.versions.arch),
        ] {
            if let Ok(v) = std::env::var(name) {
                *target = v;
            }
        }
        opts.private_data = match settings["identity"]["private_data"].as_str() {
            Some("true") => Some(true),
            Some("false") => Some(false),
            _ => None,
        };
        let mut req = wbaccounts::build_chat_request(&account.cred(), body, &opts)
            .map_err(|e| OpenFailure::new(400, e.to_string()))?;
        app.rewrite_test_upstream(&mut req);
        let response = app.http.execute(req).await;
        match response {
            Err(e) => {
                // A POST may have reached the upstream before a transport error.
                // Do not replay ambiguously delivered, billable requests.
                if e.safe_to_retry() {
                    last = OpenFailure::new(502, e.to_string());
                    continue;
                }
                return Err(OpenFailure::new(502, e.to_string()));
            }
            Ok(mut response) if (200..300).contains(&response.status) => {
                if response
                    .header("content-type")
                    .is_some_and(|v| v.to_ascii_lowercase().contains("application/json"))
                {
                    let bytes = response
                        .collect(1024 * 1024)
                        .await
                        .map_err(|e| OpenFailure::new(502, e.to_string()))?;
                    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                        audit.observed.capture(&value);
                        if structured_failure(&value).is_some_and(|e| e.restricted) {
                            return Err(restrict_account(app, &account));
                        }
                    }
                    return Err(OpenFailure::new(
                        502,
                        "upstream returned JSON instead of the requested event stream",
                    ));
                }
                response
                    .set_read_timeout(Duration::from_secs(
                        settings["upstream"]["idle_timeout_seconds"]
                            .as_u64()
                            .unwrap_or(300),
                    ))
                    .map_err(|e| OpenFailure::new(502, e.to_string()))?;
                return Ok((response, lease));
            }
            Ok(response) => {
                let status = response.status;
                let parsed = response
                    .into_in_response(1024 * 1024)
                    .await
                    .map_err(|e| OpenFailure::new(502, e.to_string()))?;
                let error_json: Value = serde_json::from_slice(&parsed.body).unwrap_or(Value::Null);
                audit.observed.capture(&error_json);
                let code = wbaccounts::json_code(&error_json);
                let restricted = structured_failure(&error_json).is_some_and(|e| e.restricted)
                    || wbaccounts::classify_response(&parsed)
                        == wbaccounts::ResponseAction::Restricted;
                let message = if let Some(c) = code {
                    format!("upstream HTTP {status} (code {c})")
                } else {
                    format!("upstream HTTP {status}")
                };
                app.pool.demote(&pick.session_key, account.uid());
                if restricted {
                    return Err(restrict_account(app, &account));
                }
                // A charged HTTP error is not safe to rotate/replay. Retain its
                // observed usage and account identity for the final audit row.
                if audit.observed.usage.is_some() {
                    return Err(OpenFailure::new(status, message));
                }
                match status {
                    401 if !refreshed.contains(account.uid()) => {
                        refreshed.insert(account.uid().to_string());
                        drop(lease);
                        match refresh_checked(app, &account, true).await {
                            Ok(()) => {
                                pick.exclude.remove(account.uid());
                                last = OpenFailure::new(401, message);
                                continue;
                            }
                            Err(error) if error.restricted => return Err(error),
                            Err(error) => {
                                last = error;
                            }
                        }
                    }
                    429 => {
                        let action = account
                            .apply_response(&parsed, &pick.model, now())
                            .map_err(|_| {
                                OpenFailure::new(500, "could not persist model cooldown")
                            })?;
                        if action == wbaccounts::ResponseAction::Restricted {
                            return Err(restrict_account(app, &account));
                        }
                        if let wbaccounts::ResponseAction::RateLimited {
                            reset_at: Some(reset),
                            ..
                        } = action
                        {
                            if app
                                .pool
                                .record_model_window(
                                    &app.args.accounts_dir,
                                    account.uid(),
                                    &pick.model,
                                    reset,
                                    now(),
                                )
                                .is_err()
                            {
                                app.log("WARN", "model cooldown remains active in memory but its restart checkpoint could not be saved");
                            }
                        }
                        let wait = account.throttle_wait(&pick.model, now()).ceil().max(1.0) as u64;
                        last = OpenFailure::retry(wait, &message);
                    }
                    402 => {
                        account.note_balance_cooled();
                        last = OpenFailure::new(status, message);
                    }
                    500..=599 if !matches!(code, Some(11102 | 11129 | 11134 | 11150 | 11158)) => {
                        account.note_failure(&message, now());
                        last = OpenFailure::new(status, message);
                    }
                    _ => return Err(OpenFailure::new(status, message)),
                }
            }
        }
    }
    Err(last)
}

fn refresh_fallback(pool: &wbaccounts::AccountPool, pick: &PickOptions) -> Option<Arc<Account>> {
    if pool.count_ready(pick) > 0 {
        return None;
    }
    pool.refresh_candidates(pick).into_iter().next()
}

pub async fn refresh_account(app: &App, account: &Arc<Account>, force: bool) -> Result<(), String> {
    refresh_checked(app, account, force)
        .await
        .map_err(|e| e.message)
}

async fn refresh_checked(
    app: &App,
    account: &Arc<Account>,
    force: bool,
) -> Result<(), OpenFailure> {
    let before = account.cred().access_token;
    let lock = app.refresh_lock(account.uid());
    let _guard = tokio::time::timeout(Duration::from_secs(2), lock.lock())
        .await
        .map_err(|_| OpenFailure::new(503, "account refresh is already in progress"))?;
    if account.to_json()["restrictedUntil"].as_f64().unwrap_or(0.0) > now()
        || app.restriction_wait(account.realm()) > 0
    {
        return Err(OpenFailure::restriction());
    }
    if account.cred().access_token != before {
        return Ok(());
    }
    if !force && !matches!(account.readiness("", now()), Readiness::NeedsRefresh) {
        return Ok(());
    }
    if force && account.throttle_wait("", now()) > 0.0 {
        return Err(OpenFailure::new(503, "account refresh is cooling down"));
    }
    let operation = async {
        let mut opts = wbaccounts::RequestOptions::default();
        opts.timeout_secs = 10.0;
        opts.proxy = account.resolve_proxy(&json!(app.settings.proxy_slots()));
        opts.cn_chat_base = std::env::var("WB_CN_CHAT_UPSTREAM").ok();
        let mut request = wbaccounts::build_refresh_request(&account.cred(), &opts)
            .map_err(|_| OpenFailure::new(502, "could not build token refresh request"))?;
        app.rewrite_test_upstream(&mut request);
        let response = app
            .http
            .execute(request)
            .await
            .map_err(|e| OpenFailure::new(502, e.to_string()))?
            .into_in_response(1024 * 1024)
            .await
            .map_err(|e| OpenFailure::new(502, e.to_string()))?;
        let value: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        if structured_failure(&value).is_some_and(|e| e.restricted)
            || (!(200..300).contains(&response.status)
                && wbaccounts::classify_response(&response)
                    == wbaccounts::ResponseAction::Restricted)
        {
            return Err(restrict_account(app, account));
        }
        account
            .apply_refresh_response(&response)
            .map_err(|_| OpenFailure::new(502, "upstream token refresh failed"))
    };
    let result = tokio::time::timeout(Duration::from_secs(15), operation)
        .await
        .unwrap_or_else(|_| Err(OpenFailure::new(504, "upstream token refresh timed out")));
    if result.as_ref().is_err_and(|e| !e.restricted) {
        // This message deliberately contains no auth-failure keywords: transient
        // refresh failures must cool down, not permanently expire credentials.
        account.note_error("refresh temporarily unavailable", None, 30.0, None, now());
    }
    result
}

pub async fn maintenance(app: Arc<App>) {
    let mut shutdown = app.shutdown.subscribe();
    let mut ticks = tokio::time::interval(Duration::from_secs(60));
    let mut catalog_at: HashMap<Realm, f64> = HashMap::new();
    loop {
        tokio::select! {_=shutdown.changed()=>break,_=ticks.tick()=>{}}
        for account in app.pool.accounts() {
            if matches!(account.readiness("", now()), Readiness::NeedsRefresh) {
                let _ = refresh_account(&app, &account, false).await;
            }
        }
        for realm in Realm::ALL {
            if now() - catalog_at.get(&realm).copied().unwrap_or(0.0) < 86400.0 {
                continue;
            }
            catalog_at.insert(realm, now());
            if app.restriction_wait(realm) > 0 {
                continue;
            }
            let Some(account) =
                app.pool.accounts().into_iter().find(|a| {
                    a.realm() == realm && matches!(a.readiness("", now()), Readiness::Ready)
                })
            else {
                continue;
            };
            let proxy = account.resolve_proxy(&json!(app.settings.proxy_slots()));
            let override_base = std::env::var("WB_CN_CHAT_UPSTREAM").ok();
            let requests = wbcatalog::config_requests(
                &account.cred(),
                wbcore::types::Egress::Proxy(proxy),
                override_base.as_deref(),
            );
            for request in requests {
                let Ok(response) = app.http.execute(request).await else {
                    continue;
                };
                let status = response.status;
                let Ok(response) = response.into_in_response(8 * 1024 * 1024).await else {
                    break; // Unknown error bodies are never a reason to rotate identity.
                };
                let payload: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
                if structured_failure(&payload).is_some_and(|e| e.restricted)
                    || (!(200..300).contains(&status)
                        && wbaccounts::classify_response(&response)
                            == wbaccounts::ResponseAction::Restricted)
                {
                    restrict_account(&app, &account);
                    break;
                }
                if status == 403 {
                    break;
                }
                if status != 200 || !payload.is_object() {
                    continue;
                }
                if app.catalog.write().ingest(realm, payload).is_ok() {
                    break;
                }
            }
        }
    }
}

async fn collect_chat(
    mut upstream: wbhttp::Response,
    model: &str,
    audit: &mut RequestRecord,
) -> Result<Value, OpenFailure> {
    let mut agg = Aggregator::new();
    let mut split = LineSplitter::new();
    let (mut total, mut line_bytes) = (0, 0);
    while let Some(bytes) = upstream
        .next_chunk()
        .await
        .map_err(|e| OpenFailure::new(502, e.to_string()))?
    {
        total += bytes.len();
        if total > MAX_RESPONSE_BYTES || !line_bound(&bytes, &mut line_bytes) {
            return Err(OpenFailure::new(502, "upstream SSE buffer limit exceeded"));
        }
        let mut lines = vec![];
        split.push(&bytes, &mut lines);
        for line in lines {
            if audit.line(&line)? {
                agg.push_line(&line);
            }
        }
    }
    if let Some(line) = split.finish() {
        if audit.line(&line)? {
            agg.push_line(&line);
        }
    }
    audit.observed.finish()?;
    let mut value = agg.finish(model, None).obj;
    // Aggregator's synthetic estimates are neither observed billing nor wire
    // usage. Keep the actual upstream block (including credit-only blocks).
    value.as_object_mut().unwrap().remove("usage");
    if let Some(usage) = &audit.observed.usage {
        value["usage"] = usage.clone();
    }
    Ok(value)
}

fn line_bound(bytes: &[u8], pending: &mut usize) -> bool {
    for &b in bytes {
        if b == b'\n' {
            *pending = 0
        } else {
            *pending += 1;
            if *pending > MAX_LINE_BYTES {
                return false;
            }
        }
    }
    true
}

enum Adapter {
    Chat(wbchat::ChatStream),
    Responses(wbtranslate::ResponsesStream),
    Compaction(ResponseOptions, Aggregator, bool),
    Messages(wbtranslate::MessagesStream),
}
impl Adapter {
    fn new(protocol: Protocol, model: &str) -> Self {
        match protocol {
            Protocol::Chat => Self::Chat(wbchat::ChatStream::new(model)),
            Protocol::Responses(opts) => Self::Responses(wbtranslate::ResponsesStream::new(opts)),
            Protocol::Compaction(opts) => Self::Compaction(opts, Aggregator::new(), false),
            Protocol::Messages => Self::Messages(wbtranslate::MessagesStream::new(model)),
        }
    }
    fn line(&mut self, line: &str) -> Vec<String> {
        match self {
            Self::Chat(s) => s.push_line(line),
            Self::Responses(s) => s.push_line(line).into_iter().map(|e| e.to_sse()).collect(),
            Self::Messages(s) => s.push_line(line).into_iter().map(|e| e.to_sse()).collect(),
            Self::Compaction(_, a, _) => {
                a.push_line(line);
                vec![]
            }
        }
    }
    fn finish(&mut self) -> Vec<String> {
        match self {
            Self::Chat(s) => s.finish(),
            Self::Responses(s) => s.finish().into_iter().map(|e| e.to_sse()).collect(),
            Self::Messages(s) => s.finish().into_iter().map(|e| e.to_sse()).collect(),
            Self::Compaction(opts, a, failed) => {
                let result = std::mem::take(a).finish(&opts.model, None);
                match wbtranslate::compaction_events(&result.obj, opts) {
                    Ok(events) => events.into_iter().map(|e| e.to_sse()).collect(),
                    Err(e) => {
                        *failed = true;
                        vec![format!("event: error\ndata: {}\n\n", e.openai())]
                    }
                }
            }
        }
    }
    fn failed(&self) -> bool {
        match self {
            Self::Chat(s) => s.accounting().aborted,
            Self::Responses(s) => s.failed(),
            Self::Messages(s) => s.failed(),
            Self::Compaction(_, _, failed) => *failed,
        }
    }
    fn abort(&mut self, message: &str) -> Vec<String> {
        match self {
            Self::Chat(s) => s.abort(
                json!({"message":message,"type":"upstream_error","code":"upstream_stream_error"}),
            ),
            Self::Responses(s) => s.abort(message).into_iter().map(|e| e.to_sse()).collect(),
            Self::Messages(s) => s.abort(message).into_iter().map(|e| e.to_sse()).collect(),
            Self::Compaction(_, _, _) => vec![format!(
                "event: error\ndata: {}\n\n",
                json!({"type":"error","message":message,"code":"upstream_stream_error","sequence_number":0})
            )],
        }
    }
}

async fn send_frames(tx: &mpsc::Sender<Bytes>, frames: Vec<String>) -> Result<(), ()> {
    for frame in frames {
        match tokio::time::timeout(Duration::from_secs(30), tx.send(Bytes::from(frame))).await {
            Ok(Ok(())) => {}
            _ => return Err(()),
        }
    }
    Ok(())
}

enum ReadResult {
    Data(Option<Bytes>),
    Upstream(String),
    Client,
}
async fn next_stream(upstream: &mut wbhttp::Response, tx: &mpsc::Sender<Bytes>) -> ReadResult {
    let read = upstream.next_chunk();
    tokio::pin!(read);
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(15),
        Duration::from_secs(15),
    );
    loop {
        tokio::select! {
            _=tx.closed()=>return ReadResult::Client,
            result=&mut read=>return match result{Ok(data)=>ReadResult::Data(data),Err(e)=>ReadResult::Upstream(e.to_string())},
            _=heartbeat.tick()=>if send_frames(tx,vec![": keepalive\n\n".into()]).await.is_err(){return ReadResult::Client;}
        }
    }
}

async fn run_stream(
    mut upstream: wbhttp::Response,
    tx: mpsc::Sender<Bytes>,
    protocol: Protocol,
    mut audit: RequestRecord,
) {
    let mut adapter = Adapter::new(protocol, &audit.model);
    let mut splitter = LineSplitter::new();
    let (mut total, mut line_bytes) = (0, 0);
    let mut client_aborted = false;
    let mut failure = None;
    'read: loop {
        let mut lines = vec![];
        let eof = match next_stream(&mut upstream, &tx).await {
            ReadResult::Client => {
                client_aborted = true;
                break;
            }
            ReadResult::Upstream(e) => {
                failure = Some(OpenFailure::new(502, e));
                break;
            }
            ReadResult::Data(None) => {
                if let Some(line) = splitter.finish() {
                    lines.push(line);
                }
                true
            }
            ReadResult::Data(Some(bytes)) => {
                total += bytes.len();
                if total > MAX_RESPONSE_BYTES || !line_bound(&bytes, &mut line_bytes) {
                    failure = Some(OpenFailure::new(502, "upstream SSE buffer limit exceeded"));
                    break;
                }
                splitter.push(&bytes, &mut lines);
                false
            }
        };
        for line in lines {
            match audit.line(&line) {
                Err(error) => {
                    failure = Some(error);
                    break 'read;
                }
                Ok(false) => continue,
                Ok(true) => {}
            }
            if send_frames(&tx, adapter.line(&line)).await.is_err() {
                client_aborted = true;
                break 'read;
            }
            if adapter.failed() {
                failure = Some(OpenFailure::new(
                    502,
                    "upstream protocol translation failed",
                ));
                break 'read;
            }
        }
        if eof {
            failure = audit.observed.finish().err();
            break;
        }
    }
    // Close unread upstream immediately, before an abort frame can block on a
    // slow downstream. Cancellation of next_chunk also closes its owned socket.
    drop(upstream);
    if !client_aborted && failure.is_none() {
        if send_frames(&tx, adapter.finish()).await.is_err() {
            client_aborted = true;
        } else if adapter.failed() {
            failure = Some(OpenFailure::new(
                502,
                "upstream protocol translation failed",
            ));
        }
    } else if !client_aborted {
        if let Some(error) = &failure {
            let _ = send_frames(&tx, adapter.abort(&error.message)).await;
        }
    }
    let outcome = if failure.is_some() {
        "upstream_aborted"
    } else if client_aborted {
        "client_aborted"
    } else {
        "completed"
    };
    if outcome == "completed" {
        if let Some(account) = &audit.account {
            account.note_success(&audit.model, now());
        }
    }
    audit.complete(outcome, failure.as_ref());
}

#[allow(clippy::too_many_arguments)]
fn record(
    app: &App,
    account: Option<&Arc<Account>>,
    model: &str,
    key: Option<&Value>,
    stream: bool,
    started: f64,
    first: Option<f64>,
    usage: Option<&Value>,
    outcome: &str,
    failure: Option<(u16, &str)>,
    body: &Value,
) {
    let at = now();
    let elapsed = ((at - started) * 1000.0).max(0.0) as u64;
    let ttft = first.map(|t| ((t - started) * 1000.0).max(0.0) as u64);
    let mut row = json!({"at":at,"iso":wbcore::timeutil::local_iso(at),"model":model,"stream":stream,"outcome":outcome,"elapsed_ms":elapsed,"ttft_ms":ttft,"gen_ms":ttft.map(|t|elapsed.saturating_sub(t)),"key":key.and_then(|k|k["id"].as_str()).unwrap_or("")});
    if let Some(a) = account {
        row["account"] = json!(a.uid());
        row["realm"] = json!(a.realm().as_str());
    }
    if let Some((status, message)) = failure {
        row["error"] = json!(true);
        row["status"] = json!(status);
        row["message"] = json!(message.chars().take(200).collect::<String>());
    }
    if let Some(u) = usage {
        for (k, v) in wbcore::usage::extract_usage(Some(u)) {
            row[k] = v;
        }
    } else {
        row["usage_missing"] = json!(true);
    }
    if let Some(messages) = body["messages"].as_array() {
        let hash = wbcore::ids::sha256_hex(&wbcore::pyjson::dumps_sorted(&json!(messages)));
        row["msgs_sha"] = json!(&hash[..12]);
        row["n_msgs"] = json!(messages.len());
    }
    if failure.is_none() {
        if let Ok(Some(policy)) = app.usage.current_policy_id(model) {
            row["cost_policy"] = json!(policy);
        }
        if let Some(effort) = body["reasoning_effort"].as_str() {
            row["reasoning_effort"] = json!(effort);
        }
    }
    // Credit reported by upstream was spent even if the stream failed or the
    // caller disconnected. Only raw observed usage reaches this function.
    if let (Some(a), Some(u)) = (account, usage) {
        a.consume_credit(wbcore::pyval::py_float(&u["credit"]).unwrap_or(0.0));
    }
    if app.usage.append(row).is_err() {
        app.log("ERROR", "could not persist request usage row");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_patterns() {
        let k = json!({"models":["deepseek-*","gpt-?"]});
        assert!(key_allows_model(Some(&k), "deepseek-v4.1"));
        assert!(!key_allows_model(Some(&k), "claude"));
        assert!(key_allows_model(Some(&k), "GPT-5"));
    }
    #[test]
    fn line_limit() {
        let mut n = MAX_LINE_BYTES;
        assert!(!line_bound(b"x", &mut n));
        assert!(line_bound(b"\nx", &mut n));
    }

    #[test]
    fn generated_restriction_phrases_are_not_errors() {
        let mut observed = ObservedStream::default();
        let frame = json!({"choices":[{"index":0,"delta":{
            "content":"unusual activity 账号存在异常行为 已被限制",
            "tool_calls":[{"function":{"arguments":"{\"code\":11140}"}}]
        },"finish_reason":null}]});
        assert!(observed.line(&format!("data: {frame}")).unwrap());
        assert!(structured_failure(&frame).is_none());
        assert!(
            structured_failure(&json!({"error":{"code":11140}}))
                .unwrap()
                .restricted
        );
        assert!(
            structured_failure(&json!({"error":{"message":"Unusual activity"}}))
                .unwrap()
                .restricted
        );
    }

    #[test]
    fn final_unterminated_error_uses_same_checks_and_keeps_usage() {
        for suffix in ["", "\n\n"] {
            let input = format!(
                "data: {{\"code\":11140,\"usage\":{{\"total_tokens\":9,\"credit\":2}}}}{suffix}"
            );
            let mut split = LineSplitter::new();
            let mut lines = vec![];
            split.push(input.as_bytes(), &mut lines);
            if let Some(line) = split.finish() {
                lines.push(line);
            }
            let mut observed = ObservedStream::default();
            let failure = lines
                .iter()
                .find_map(|line| observed.line(line).err())
                .unwrap();
            assert!(failure.restricted);
            assert_eq!(observed.usage.as_ref().unwrap()["credit"], 2.0);
            assert_eq!(observed.usage.as_ref().unwrap()["total_tokens"], 9);
        }
    }

    #[test]
    fn eof_requires_done_or_all_explicit_choice_finishes() {
        let mut observed = ObservedStream::default();
        observed
            .line(r#"data: {"choices":[{"index":0,"delta":{"content":"partial"}}]}"#)
            .unwrap();
        assert!(observed.finish().is_err());
        assert!(observed.usage.is_none()); // No synthetic usage even on abort.
        observed
            .line(r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#)
            .unwrap();
        assert!(observed.finish().is_ok()); // Provider may omit DONE.
        observed
            .line(r#"data: {"choices":[{"index":1,"delta":{"content":"partial"}}]}"#)
            .unwrap();
        assert!(observed.finish().is_err());
        observed.line("data: [DONE]").unwrap();
        assert!(observed.finish().is_ok());
    }

    #[test]
    fn error_usage_is_captured_without_estimating_partial_text() {
        let mut observed = ObservedStream::default();
        observed
            .line(r#"data: {"choices":[{"delta":{"content":"some generated text"}}]}"#)
            .unwrap();
        assert!(observed.usage.is_none());
        assert!(observed
            .line(r#"data: {"error":{"message":"failed"},"usage":{"total_tokens":12,"credit":3}}"#)
            .is_err());
        assert_eq!(observed.usage.as_ref().unwrap()["total_tokens"], 12);
        assert_eq!(observed.credit, Some(3.0));
    }

    fn account(uid: &str, expires: i64) -> Arc<Account> {
        Arc::new(
            Account::from_json(&json!({"uid":uid,"realm":"intl","accessToken":"test-token",
            "refreshToken":"test-refresh","expiresAt":expires,"credits":{"remain":10,"used":0}}))
            .unwrap(),
        )
    }

    #[test]
    fn healthy_account_prevents_refresh_fallback() {
        let expiring = account("expiring", now() as i64 + 30);
        let healthy = account("healthy", now() as i64 + 3600);
        let pool = wbaccounts::AccountPool::new(vec![expiring.clone(), healthy.clone()]);
        let pick = PickOptions {
            realm: Some(Realm::Intl),
            now: now(),
            ..Default::default()
        };
        assert!(refresh_fallback(&pool, &pick).is_none());
        assert_eq!(
            pool.acquire(&pick, 1).unwrap().account().uid(),
            healthy.uid()
        );
        let pool = wbaccounts::AccountPool::new(vec![expiring.clone()]);
        assert_eq!(
            refresh_fallback(&pool, &pick).unwrap().uid(),
            expiring.uid()
        );
    }

    fn test_app(root: &std::path::Path, account: Arc<Account>) -> Arc<App> {
        use clap::Parser;
        use parking_lot::{Mutex, RwLock};
        let mut args = crate::cli::Args::try_parse_from(["wb2api"]).unwrap();
        args.accounts_dir = root.join("accounts");
        args.usage_dir = root.join("usage");
        let settings = wbsettings::Settings::load(args.accounts_dir.join("settings.json")).unwrap();
        let usage = wbusage::UsageStore::new(args.usage_dir.clone());
        let (shutdown, _) = tokio::sync::watch::channel(false);
        Arc::new(App {
            args,
            settings,
            usage,
            pool: Arc::new(wbaccounts::AccountPool::new(vec![account])),
            catalog: RwLock::new(wbcatalog::Catalog::bundled()),
            http: wbhttp::Client::new().unwrap(),
            realm: RwLock::new(Realm::Intl),
            extra_keys: vec![],
            slots: Arc::new(tokio::sync::Semaphore::new(1)),
            login_limit: wbsettings::LoginRateLimiter::new(),
            refresh_locks: Mutex::new(HashMap::new()),
            restrictions: Mutex::new(HashMap::new()),
            logs: Mutex::new(std::collections::VecDeque::new()),
            shutdown,
            started: now(),
        })
    }

    #[test]
    fn cancellation_finalizer_records_once_and_failure_consumes_observed_credit() {
        let root = std::env::temp_dir().join(format!("wb-dispatch-{}", wbcore::ids::uuid4_hex()));
        let account = account("audit", now() as i64 + 3600);
        let app = test_app(&root, account.clone());
        {
            let mut audit =
                RequestRecord::new(app.clone(), "test".into(), None, false, now(), json!({}));
            audit.account = Some(account.clone());
            audit
                .line(r#"data: {"usage":{"total_tokens":7,"credit":2}}"#)
                .unwrap();
            // Same Drop path as a cancelled Hyper service future.
        }
        {
            let mut audit =
                RequestRecord::new(app.clone(), "test".into(), None, false, now(), json!({}));
            audit.account = Some(account.clone());
            let error = audit
                .line(
                    r#"data: {"error":{"message":"failed"},"usage":{"total_tokens":5,"credit":1}}"#,
                )
                .unwrap_err();
            audit.complete("upstream_aborted", Some(&error));
            audit.complete("completed", None); // Explicit completion and Drop are idempotent.
        }
        let text = std::fs::read_to_string(root.join("usage/usage.jsonl")).unwrap();
        let rows: Vec<Value> = text
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["outcome"], "client_aborted");
        assert_eq!(rows[0]["total_tokens"], 7);
        assert_eq!(rows[1]["outcome"], "upstream_aborted");
        assert_eq!(account.public_metadata()["credits"]["remain"], 7.0);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}
