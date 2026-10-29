//! Panel-only management handlers. No API key grants management privileges.
//! Disk work and password hashing run off the async workers; no secret is logged.
use crate::{
    state::App,
    wire::{self, Query, Reply},
};
use hyper::{header, HeaderMap};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Write},
    net::SocketAddr,
    path::Path,
    sync::Arc,
};
use wbcore::{
    timeutil,
    types::{Product, Realm},
};

// Serialize panel read/modify/write operations (including password/session rotation).
// The libraries provide their own locks against refresh and request threads.
static MUTATION: Mutex<()> = Mutex::new(());
const UNSUPPORTED_SWITCHES: &[&str] = &[
    "local_web_tools",
    "auto_switch_product",
    "realm_fallback_enabled",
    "log_db_enabled",
    "update_check_enabled",
    "egress_backoff_enabled",
    "remaining_priority_enabled",
    "free_model_credit_fallback",
];

pub async fn get(
    app: Arc<App>,
    path: &str,
    headers: &HeaderMap,
    q: &Query,
    _peer: SocketAddr,
) -> Reply {
    if !matches!(path, "/realm" | "/health" | "/panel/status") && !app.panel_valid(headers) {
        return denied();
    }
    let (path, headers, q) = (path.to_owned(), headers.clone(), q.clone());
    blocking(move || get_local(&app, &path, &headers, &q)).await
}

pub async fn post(
    app: Arc<App>,
    path: &str,
    body: Value,
    headers: &HeaderMap,
    q: &Query,
    peer: SocketAddr,
) -> Reply {
    if !matches!(path, "/panel/login" | "/panel/logout" | "/logout") && !app.panel_valid(headers) {
        return denied();
    }
    if path == "/panel/login" {
        let trusted = &app.env.trusted_proxies;
        let bucket = wbsettings::login_rate_limit_key(
            &peer.ip().to_string(),
            wire::header_text(headers, "x-real-ip"),
            wire::header_text(headers, "x-forwarded-for"),
            trusted,
        );
        let reply = blocking(move || {
            let _guard = MUTATION.lock();
            if let Err(wait) = app.login_limit.check(&bucket) {
                return no_store(wire::retry_error(
                    wait,
                    &format!("too many login attempts, please wait {wait}s"),
                    false,
                ));
            }
            if !app.settings.verify_password(text(&body, "password")) {
                app.login_limit.failure(&bucket);
                return fail(401, "invalid panel password");
            }
            app.login_limit.success(&bucket);
            session_reply(&app)
        })
        .await;
        if reply.status().as_u16() == 401 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        return reply;
    }
    if matches!(path, "/panel/logout" | "/logout") {
        app.settings
            .sessions()
            .revoke(wire::header_text(headers, "x-panel-token").unwrap_or(""));
        return ok(json!({"ok":true}));
    }
    if path == "/accounts/refresh" {
        let targets = match select_accounts(&app, &body, false) {
            Ok(v) => v,
            Err(reply) => return reply,
        };
        let mut results = Vec::new();
        for account in targets {
            let result = crate::dispatch::refresh_account(&app, &account, true).await;
            results.push(json!({"uid":account.uid(),"ok":result.is_ok(),
                "error":if result.is_err() { json!("account refresh failed; credentials and upstream details omitted") } else { Value::Null }}));
        }
        return ok(json!({"results":results}));
    }
    if path == "/shutdown" {
        let signal = app.shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            signal.send_replace(true);
        });
        return ok(json!({"ok":true,"shutting_down":true}));
    }
    let (path, headers, q) = (path.to_owned(), headers.clone(), q.clone());
    blocking(move || {
        let _guard = MUTATION.lock();
        // The session may have been revoked while waiting for a blocking worker.
        if !app.panel_valid(&headers) {
            return denied();
        }
        post_local(&app, &path, body, &headers, &q)
    })
    .await
}

async fn blocking(f: impl FnOnce() -> Reply + Send + 'static) -> Reply {
    match tokio::task::spawn_blocking(f).await {
        Ok(reply) => reply,
        Err(_) => fail(500, "panel operation failed"),
    }
}
fn denied() -> Reply {
    fail(401, "panel session required; sign in to the dashboard")
}
fn fail(status: u16, message: &str) -> Reply {
    no_store(wire::error(status, message, false))
}
fn unsupported() -> Reply {
    fail(
        501,
        "this management feature is not implemented in the Rust gateway",
    )
}
/// The dashboard polls several usage views every 5 seconds, and each one
/// scans the whole usage log. Like the Python baseline's `_STATS_TTL` cache,
/// keep each rendered body for a few seconds, so new requests show up within
/// `USAGE_TTL`. Pricing files, accounts and keys are part of the key, and a
/// log that shrank or was replaced is too, so those changes show immediately.
const USAGE_TTL: std::time::Duration = std::time::Duration::from_secs(5);
const USAGE_ENTRIES: usize = 32;
type UsageEntry = (String, std::time::Instant, String, bytes::Bytes);
static USAGE_CACHE: Mutex<Vec<UsageEntry>> = Mutex::new(Vec::new());

fn usage_cache_key(app: &App, path: &str, params: &Value) -> String {
    let stamp = |name: &str| {
        fs::metadata(app.usage.directory().join(name))
            .ok()
            .map(|m| (m.len(), m.modified().ok()))
    };
    // Appends only grow the log; identify the file, not its current length.
    let log = fs::metadata(app.usage.directory().join("usage.jsonl"))
        .ok()
        .map(|m| (m.created().ok(), m.len() / (64 * 1024 * 1024)));
    let accounts: Vec<_> = app
        .pool
        .accounts()
        .iter()
        .map(|a| (a.uid().to_owned(), a.realm()))
        .collect();
    format!(
        "{path}|{}|{:?}|{:?}|{:?}|{:?}|{:?}|{}|{}",
        wbcore::pyjson::dumps_compact(params),
        log,
        stamp("pricing-policies.jsonl"),
        stamp("pricing-timeline.jsonl"),
        (
            stamp("pricing-overrides.json"),
            stamp("pricing-markup.json")
        ),
        accounts,
        app.realm.read().as_str(),
        wbcore::pyjson::dumps_compact(&app.settings.view()["api_keys"]),
    )
}
fn usage_cache_get(key: &str) -> Option<(String, bytes::Bytes)> {
    let mut cache = USAGE_CACHE.lock();
    let now = std::time::Instant::now();
    cache.retain(|(_, at, _, _)| now.duration_since(*at) < USAGE_TTL);
    cache
        .iter()
        .find(|(k, _, _, _)| k == key)
        .map(|(_, _, tag, body)| (tag.clone(), body.clone()))
}
fn usage_cache_put(key: String, body: bytes::Bytes) -> String {
    use sha2::Digest;
    let tag = format!("\"{}\"", &hex::encode(sha2::Sha256::digest(&body))[..20]);
    let mut cache = USAGE_CACHE.lock();
    if cache.len() >= USAGE_ENTRIES {
        cache.remove(0);
    }
    cache.push((key, std::time::Instant::now(), tag.clone(), body));
    tag
}
fn usage_reply(body: bytes::Bytes, headers: &HeaderMap, tag: &str) -> Reply {
    wire::etag(
        wire::response_bytes(200, "application/json; charset=utf-8", body),
        headers,
        tag,
    )
}

fn no_store(mut reply: Reply) -> Reply {
    reply.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    reply
}
fn ok(mut value: Value) -> Reply {
    mask_names(&mut value);
    no_store(wire::json_reply(200, value))
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn yes(value: Option<&String>) -> bool {
    value.is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}
fn setting_error(error: wbsettings::Error) -> Reply {
    match error {
        wbsettings::Error::Validation(_) => {
            fail(400, "invalid settings value; no settings were saved")
        }
        wbsettings::Error::NotFound(_) => fail(404, "no such settings entry"),
        _ => fail(500, "could not persist settings"),
    }
}
fn account_error(error: wbaccounts::Error) -> Reply {
    match error {
        wbaccounts::Error::Invalid(_) | wbaccounts::Error::Json(_) => {
            fail(400, "invalid account data or account identity conflict")
        }
        _ => fail(
            500,
            "account operation could not be persisted; reload before retrying",
        ),
    }
}
fn session_reply(app: &App) -> Reply {
    no_store(wire::json_reply(
        200,
        json!({"ok":true,"token":app.settings.sessions().issue(),
        "using_default_password":app.settings.password_is_default()}),
    ))
}

fn get_local(app: &App, path: &str, headers: &HeaderMap, q: &Query) -> Reply {
    if !matches!(path, "/realm" | "/health" | "/panel/status") && !app.panel_valid(headers) {
        return denied();
    }
    match path {
        "/realm" => ok(json!({"current":app.realm.read().as_str(),"options":["intl","cn"]})),
        "/health" => {
            let realm = *app.realm.read();
            let accounts = app.pool.accounts();
            let accounts = accounts
                .iter()
                .filter(|a| a.realm() == realm)
                .collect::<Vec<_>>();
            let mut value = json!({"ok":true,"realm":realm.as_str(),"accounts":accounts.len(),
                "accounts_ready":accounts.iter().filter(|a| a.ready("")).count(),
                "api_key_required":app.api_auth_required()});
            if let Ok(key) = app.authenticate(headers, q) {
                if let Some(account) = accounts.iter().find(|account| {
                    key.as_ref()
                        .is_none_or(|k| wbsettings::key_allows_account(k, account.uid()))
                }) {
                    let data = account.to_json();
                    value["uid"] = json!(account.uid());
                    value["domain"] = data["domain"].clone();
                    value["issuer"] =
                        wbaccounts::jwt_claims(text(&data, "accessToken"))["iss"].clone();
                    value["expires_at"] = data["expiresAt"].clone();
                    value["credential_file"] = json!(account
                        .path()
                        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned())));
                }
            }
            ok(value)
        }
        "/panel/status" => ok(
            json!({"panel_password_required":true,"panel_password_is_default":app.settings.password_is_default(),
            "authenticated":app.panel_valid(headers),"api_key_set":!app.extra_keys.is_empty() || !app.settings.keys().is_empty()}),
        ),
        "/settings" => ok(settings_view(app)),
        "/settings/reveal" => {
            let id = q.get("id").map(String::as_str).unwrap_or("");
            match app.settings.reveal_key(id) {
                Some(key) => no_store(wire::json_reply(200, json!({"id":id,"key":key}))),
                None => fail(404, "no such key"),
            }
        }
        "/accounts" => {
            let realm = match query_realm(app, headers, q, true) {
                Ok(r) => r,
                Err(reply) => return reply,
            };
            let accounts = account_views(app, realm);
            let usable = app
                .pool
                .accounts()
                .iter()
                .filter(|a| realm.is_none_or(|r| a.realm() == r) && a.ready(""))
                .count();
            ok(json!({"accounts":accounts,"storage":app.args.accounts_dir,"usable":usable}))
        }
        "/accounts/export" => export_accounts(app, q),
        "/proxy/slots" => ok(json!({"slots":proxy_views(app)})),
        "/usage" | "/v1/usage" | "/usage/recent" | "/usage/perf" | "/usage/timeseries"
        | "/usage/analytics" | "/usage/by-account" | "/pricing" => {
            let mut params = serde_json::to_value(q).unwrap_or_else(|_| json!({}));
            if !q.contains_key("realm") && path != "/usage/analytics" {
                if let Some(realm) = wire::header_text(headers, "x-realm") {
                    params["realm"] = json!(realm);
                }
            }
            if params.get("bucket").is_none() {
                if let Some(bucket) = q.get("bucket_seconds") {
                    params["bucket"] = json!(bucket);
                }
            }
            app.usage
                .set_pricing_enabled(app.settings.current()["pricing_enabled"] != false);
            let key = usage_cache_key(app, path, &params);
            if let Some((tag, body)) = usage_cache_get(&key) {
                return usage_reply(body, headers, &tag);
            }
            match app.usage.query(path, &params, &app.metadata()) {
                Ok(mut value) => {
                    scrub_secrets(&mut value, &log_secrets(app));
                    mask_names(&mut value);
                    let body = bytes::Bytes::from(wbcore::pyjson::dumps_u(&value));
                    let tag = usage_cache_put(key, body.clone());
                    usage_reply(body, headers, &tag)
                }
                Err(e) if e.kind() == io::ErrorKind::Unsupported => unsupported(),
                Err(_) => fail(500, "could not read usage or pricing history"),
            }
        }
        "/logs" => logs(app, q, false),
        "/logs/export" => logs(app, q, true),
        "/updates" => ok(
            json!({"current_version":wbcore::RUST_VERSION,"runtime":"rust","baseline_commit":wbcore::BASELINE_COMMIT,
            "upstream_version":wbcore::VERSION,"synced_upstream_commit":wbcore::SYNCED_UPSTREAM_COMMIT,"sync_scope":"selected-core-fixes",
            "pinned":true,"latest_version":null,"latest_tag":null,"update_available":false,"enabled":false,
            "checking":false,"install_enabled":false,"rollback_enabled":false,"self_update_supported":false,
            "last_attempt":null,"last_success":null,"last_error":null,"release_url":null,"published_at":null,
            "msg":"Rust alpha based on 477ba36 with selected v1.8.0 fixes from 4f7ae6f; not full upstream parity. Update the binary or container externally."}),
        ),
        "/console" | "/console/state" => ok(json!({"supported":true,"mode":"web","visible":false})),
        // Read-only views of features this build does not run. The dashboard
        // loads them on every visit; an empty, explicitly unsupported payload
        // lets it render an empty state instead of a red error each time.
        // Every action endpoint behind them still answers 501.
        "/tasks" => ok(json!({"supported":false,"tasks":[],"accounts":[],"summary":{}})),
        "/activity/history" => ok(json!({"supported":false,"rows":[],"total":0})),
        "/accounts/credits/grants" => ok(json!({"supported":false,"rows":[],"summary":{}})),
        "/updates/backups" => ok(
            json!({"supported":false,"backups":[],"rollback_enabled":false,
            "current_version":wbcore::RUST_VERSION}),
        ),
        "/agents" => ok(agents_view(app)),
        // The only background work in this build is the maintenance loop:
        // near-expiry token refresh every minute and a daily catalog refresh.
        "/scheduler" => {
            let enabled = !app.args.offline && app.args.test_upstream.is_none();
            let mode = "凭证到期前自动刷新（每分钟检查）· 模型目录每日刷新";
            ok(
                json!({"supported":false,"enabled":enabled,"mode":mode,"mode_intl":mode,"mode_cn":mode,
                "last_run_time":null,"next_run_time":null,"windows":[],"quota_scans":[]}),
            )
        }
        _ => unsupported(),
    }
}

/// Connection details for manually configuring clients. Local client
/// detection and one-click configuration are not part of this build.
fn agents_view(app: &App) -> Value {
    let models: Vec<Value> = app.catalog.read().list(*app.realm.read())["data"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|m| json!({"id":m["id"],"context_window":m["context_window"]}))
                .collect()
        })
        .unwrap_or_default();
    let keys: Vec<Value> = app.settings.view()["api_keys"]
        .as_array()
        .map(|keys| {
            keys.iter()
                .map(|k| json!({"id":k["id"],"name":k["name"],"realm":k["realm"],"enabled":k["enabled"]}))
                .collect()
        })
        .unwrap_or_default();
    json!({"supported":false,"clients":[],"models":models,"keys":keys})
}

fn post_local(app: &App, path: &str, body: Value, _headers: &HeaderMap, _q: &Query) -> Reply {
    match path {
        "/panel/password" => {
            if !app.settings.verify_password(text(&body, "current")) {
                return fail(401, "current password is wrong");
            }
            let password = text(&body, "new");
            if password.chars().count() < 4 {
                return fail(400, "new password must contain at least 4 characters");
            }
            if !app.args.host.is_loopback() && password == wbsettings::DEFAULT_PANEL_PASSWORD {
                return fail(
                    400,
                    "a network listener requires a non-default panel password",
                );
            }
            match app.settings.set_password(password) {
                Ok(()) => session_reply(app),
                Err(e) => setting_error(e),
            }
        }
        "/settings/save" => {
            if !app.args.host.is_loopback() && body["auth_disabled"] == true {
                return fail(
                    400,
                    "authentication cannot be disabled on a network listener",
                );
            }
            if body.as_object().is_some_and(|fields| {
                fields.keys().any(|key| {
                    key.starts_with("daily_chat_")
                        || key == "schedule"
                        || key == "restart_scheduler"
                })
            }) {
                return fail(501, "daily activity scheduler settings are not supported by this Rust build; stored configuration was not changed");
            }
            if UNSUPPORTED_SWITCHES.iter().any(|key| body[*key] == true)
                || body["quota_scan_models"]
                    .as_array()
                    .is_some_and(|xs| !xs.is_empty())
            {
                return unsupported();
            }
            if body.get("identity_versions").is_some()
                && !app.settings.verify_password(text(&body, "panel_password"))
            {
                return fail(
                    401,
                    "current panel password is required for identity_versions",
                );
            }
            match app.settings.patch(body) {
                Ok(_) => {
                    app.usage
                        .set_pricing_enabled(app.settings.snapshot()["pricing_enabled"] != false);
                    ok(settings_view(app))
                }
                Err(e) => setting_error(e),
            }
        }
        "/realm" => {
            let Some(realm) = Realm::parse(text(&body, "realm")) else {
                return fail(400, "realm must be intl or cn");
            };
            let at = timeutil::now();
            let data = json!({"realm":realm.as_str(),"updated_at":at,"updated_iso":timeutil::local_iso(at)});
            if atomic_json(&app.args.accounts_dir.join("active_realm.json"), &data).is_err() {
                return fail(500, "could not persist realm; active realm was not changed");
            }
            *app.realm.write() = realm;
            ok(json!({"ok":true,"current":realm.as_str(),"persisted":true}))
        }
        "/accounts/import" => import_accounts(app, body),
        "/accounts/set" => set_account(app, &body),
        "/accounts/set-all" => {
            let Some(enabled) = body["enabled"].as_bool() else {
                return fail(400, "enabled must be a boolean");
            };
            mutate_accounts(app, app.pool.accounts(), json!({"enabled":enabled}), None)
        }
        "/accounts/product" => {
            let Some(product) = Product::parse_strict(text(&body, "product")) else {
                return fail(400, "product must be workbuddy, vscode or cli");
            };
            let targets = match select_accounts(app, &body, true) {
                Ok(v) => v,
                Err(reply) => return reply,
            };
            mutate_accounts(
                app,
                targets,
                json!({"product":product.as_str()}),
                Some(product.as_str()),
            )
        }
        "/accounts/delete" => {
            let uid = text(&body, "uid");
            if uid.is_empty() {
                return fail(400, "uid required");
            }
            match app.pool.delete(uid, true) {
                Ok(deleted) => ok(json!({"deleted":deleted,"accounts":account_views(app,None)})),
                Err(e) => account_error(e),
            }
        }
        "/proxy/slots" => ok(json!({"slots":proxy_views(app)})),
        "/proxy/slots/save" => save_proxies(app, &body),
        "/logs/clear" => {
            app.logs.lock().clear();
            ok(json!({"ok":true,"mode":"memory"}))
        }
        "/console/toggle" => {
            ok(json!({"ok":true,"mode":"web","url":"/dashboard?tab=logs","visible":false}))
        }
        _ => unsupported(),
    }
}

fn settings_view(app: &App) -> Value {
    let mut view = app.settings.view();
    view["auth_required"] = json!(app.api_auth_required());
    view["runtime"] = json!("rust");
    view["version"] = json!(wbcore::RUST_VERSION);
    view["upstream_version"] = json!(wbcore::VERSION);
    view["synced_upstream_commit"] = json!(wbcore::SYNCED_UPSTREAM_COMMIT);
    view["management_capabilities"] = json!({"self_update":false,"scheduler":false,
        "desktop_import":false,"proxy_probe":false,"log_database":false,"pricing_refresh":false,
        "quota_scan":false,"log_mode":"memory"});
    view["unsupported_settings"] = json!(UNSUPPORTED_SWITCHES);
    for key in UNSUPPORTED_SWITCHES {
        view[*key] = json!(false);
    }
    view["quota_scan_models"] = json!([]);
    view["daily_chat_web"] = json!(false);
    view["daily_chat_supported"] = json!(false);
    view["unsupported_setting_prefixes"] = json!(["daily_chat_"]);
    view
}
fn query_realm(
    app: &App,
    headers: &HeaderMap,
    q: &Query,
    default_current: bool,
) -> Result<Option<Realm>, Reply> {
    let raw = q
        .get("realm")
        .map(String::as_str)
        .or_else(|| wire::header_text(headers, "x-realm"))
        .unwrap_or("");
    match raw {
        "all" => Ok(None),
        "" if default_current => Ok(Some(*app.realm.read())),
        "" => Ok(None),
        _ => Realm::parse(raw)
            .map(Some)
            .ok_or_else(|| fail(400, "realm must be intl, cn or all")),
    }
}
fn account_views(app: &App, realm: Option<Realm>) -> Vec<Value> {
    app.pool
        .accounts()
        .iter()
        .filter(|a| realm.is_none_or(|r| a.realm() == r))
        .map(|a| a.public_metadata())
        .collect()
}
fn select_accounts(
    app: &App,
    body: &Value,
    default_current: bool,
) -> Result<Vec<Arc<wbaccounts::Account>>, Reply> {
    let uid = text(body, "uid");
    if !uid.is_empty() {
        return app
            .pool
            .get(uid)
            .map(|a| vec![a])
            .ok_or_else(|| fail(404, "no such account"));
    }
    let realm = match text(body, "realm") {
        "all" => None,
        "" if default_current => Some(*app.realm.read()),
        "" => None,
        raw => Some(Realm::parse(raw).ok_or_else(|| fail(400, "realm must be intl, cn or all"))?),
    };
    Ok(app
        .pool
        .accounts()
        .into_iter()
        .filter(|a| realm.is_none_or(|r| a.realm() == r))
        .collect())
}
fn set_account(app: &App, body: &Value) -> Reply {
    let uid = text(body, "uid");
    if uid.is_empty() {
        return fail(400, "uid required");
    }
    let Some(account) = app.pool.get(uid) else {
        return fail(404, "no such account");
    };
    let mut patch = Map::new();
    for key in [
        "enabled",
        "tasksEnabled",
        "weight",
        "note",
        "disabledModels",
        "proxySlot",
        "proxy",
    ] {
        if let Some(value) = body.get(key) {
            patch.insert(key.into(), value.clone());
        }
    }
    if patch.is_empty() {
        return fail(400, "nothing to update");
    }
    if patch
        .get("weight")
        .is_some_and(|v| !v.as_i64().is_some_and(|n| (1..=100).contains(&n)))
    {
        return fail(400, "weight must be an integer between 1 and 100");
    }
    if let Some(slot) = patch.get("proxySlot") {
        let Some(slot) = slot.as_str() else {
            return fail(400, "proxySlot must be a string");
        };
        if !slot.is_empty() && app.settings.find_proxy_slot(slot).is_none() {
            return fail(400, "no such proxy slot");
        }
    }
    if let Some(proxy) = patch.get("proxy") {
        let Some(proxy) = proxy.as_str() else {
            return fail(400, "proxy must be a string");
        };
        if !valid_proxy(proxy) {
            return fail(400, "proxy must be an HTTP, HTTPS or SOCKS5 URL");
        }
    }
    match account.patch(&Value::Object(patch)) {
        Ok(()) => ok(json!({"account":account.public_metadata()})),
        Err(e) => account_error(e),
    }
}
fn mutate_accounts(
    app: &App,
    targets: Vec<Arc<wbaccounts::Account>>,
    patch: Value,
    product: Option<&str>,
) -> Reply {
    let mut changed = Vec::new();
    let mut failed = Vec::new();
    for account in targets {
        match account.patch(&patch) {
            Ok(()) => changed.push(account.uid().to_owned()),
            Err(_) => failed
                .push(json!({"uid":account.uid(),"error":"account update could not be persisted"})),
        }
    }
    let mut value = json!({"ok":failed.is_empty(),"changed":changed,"failed":failed,"accounts":account_views(app,None)});
    if let Some(product) = product {
        value["product"] = json!(product);
    }
    mask_names(&mut value);
    no_store(wire::json_reply(
        if failed.is_empty() { 200 } else { 500 },
        value,
    ))
}

fn export_accounts(app: &App, q: &Query) -> Reply {
    let realm = match q.get("realm").map(String::as_str).unwrap_or("") {
        "" | "all" => None,
        raw => match Realm::parse(raw) {
            Some(r) => Some(r),
            None => return fail(400, "realm must be intl, cn or all"),
        },
    };
    let ids = q.get("uid").map(|v| {
        v.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect::<HashSet<_>>()
    });
    if ids
        .as_ref()
        .is_some_and(|ids| ids.iter().any(|uid| app.pool.get(uid).is_none()))
    {
        return fail(404, "no such account");
    }
    let include_secrets = !q
        .get("secrets")
        .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "0" | "false" | "no"));
    let mut value =
        wbaccounts::export_document(&app.pool.accounts(), realm, include_secrets, ids.as_ref());
    mask_names(&mut value);
    if yes(q.get("download")) {
        download(
            "application/json; charset=utf-8",
            "workbuddy-accounts.json",
            wbcore::pyjson::dumps_pretty(&value).into_bytes(),
        )
    } else {
        no_store(wire::json_reply(200, value))
    }
}
fn import_accounts(app: &App, body: Value) -> Reply {
    // Never interpret an invalid dryRun value as permission to write.
    for key in ["dryRun", "overwrite"] {
        if body.get(key).is_some_and(|v| !v.is_boolean()) {
            return fail(400, "dryRun and overwrite must be booleans");
        }
    }
    if body.get("realm").is_some_and(|v| !v.is_string()) {
        return fail(400, "realm must be intl or cn");
    }
    let realm = match body.get("realm").and_then(Value::as_str).unwrap_or("") {
        "" => None,
        raw => match Realm::parse(raw) {
            Some(r) => Some(r),
            None => return fail(400, "realm must be intl or cn"),
        },
    };
    let mut document = body.get("data").cloned().unwrap_or_else(|| body.clone());
    if let Some(raw) = document.as_str() {
        document = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => return fail(400, "cannot read the account document"),
        };
    }
    let incoming = match wbaccounts::import_document(&document, realm) {
        Ok(v) => v,
        Err(e) => return account_error(e),
    };
    if let Err(reply) = check_import_paths(app, &incoming) {
        return reply;
    }
    let count = incoming.len();
    let overwrite = body
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let dry_run = body.get("dryRun").and_then(Value::as_bool).unwrap_or(false);
    let (mut added, mut updated, mut skipped, mut invalid) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut candidates = Vec::new();
    for account in incoming {
        if let Some(existing) = app.pool.get(account.uid()) {
            if !overwrite {
                skipped.push(
                    json!({"uid":account.uid(),"reason":"already exists; overwrite disabled"}),
                );
                continue;
            }
            if existing.realm() != account.realm() {
                return fail(400, "import cannot change an existing account realm");
            }
            updated.push(
                json!({"uid":account.uid(),"nickname":account.public_metadata()["nickname"]}),
            );
        } else {
            added.push(
                json!({"uid":account.uid(),"nickname":account.public_metadata()["nickname"]}),
            );
        }
        candidates.push(account);
    }
    if dry_run {
        return ok(
            json!({"dryRun":true,"count":count,"result":{"added":added,"updated":updated,"skipped":skipped,"invalid":[]},
            "accounts":account_views(app,None)}),
        );
    }
    // Report partial disk failures honestly; never claim a batch is atomic.
    let mut committed = HashSet::new();
    for (index, account) in candidates.into_iter().enumerate() {
        match app.pool.upsert(account,Some(&app.args.accounts_dir)) {
            Ok(a) => { committed.insert(a.uid().to_owned()); }
            Err(_) => invalid.push(json!({"index":index,"reason":"account could not be persisted; batch may be partially applied"})),
        }
    }
    added.retain(|a| committed.contains(text(a, "uid")));
    updated.retain(|a| committed.contains(text(a, "uid")));
    let status = if invalid.is_empty() { 200 } else { 500 };
    let mut value = json!({"count":count,"imported":committed.len(),"result":{"added":added,"updated":updated,"skipped":skipped,"invalid":invalid},
        "accounts":account_views(app,None)});
    mask_names(&mut value);
    no_store(wire::json_reply(status, value))
}
fn file_stem(uid: &str) -> String {
    uid.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_ascii_lowercase()
}
fn check_import_paths(app: &App, incoming: &[Arc<wbaccounts::Account>]) -> Result<(), Reply> {
    let mut owners = HashMap::<String, String>::new();
    for account in app.pool.accounts() {
        owners.insert(file_stem(account.uid()), account.uid().to_owned());
        if let Some(path) = account.path() {
            if let Some(stem) = path.file_stem() {
                owners.insert(
                    stem.to_string_lossy().to_ascii_lowercase(),
                    account.uid().to_owned(),
                );
            }
        }
    }
    for account in incoming {
        let stem = file_stem(account.uid());
        if stem.is_empty()
            || stem.len() > 180
            || matches!(
                stem.as_str(),
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
            || (stem.len() == 4
                && (stem.starts_with("com") || stem.starts_with("lpt"))
                && stem.as_bytes()[3].is_ascii_digit())
        {
            return Err(fail(
                400,
                "account uid is not safe for account-file storage",
            ));
        }
        if owners.get(&stem).is_some_and(|uid| uid != account.uid()) {
            return Err(fail(
                400,
                "account uid collides with an existing account filename",
            ));
        }
        let path = app.args.accounts_dir.join(format!("{stem}.json"));
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(fail(400, "account destination must not be a symbolic link"));
        }
        if path.exists() && !owners.contains_key(&stem) {
            return Err(fail(
                409,
                "account destination already exists and is not a loaded account",
            ));
        }
        owners.insert(stem, account.uid().to_owned());
    }
    Ok(())
}

fn valid_proxy(raw: &str) -> bool {
    raw.is_empty()
        || url::Url::parse(raw).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
                && url.host_str().is_some()
                && (url.path().is_empty() || url.path() == "/")
                && url.query().is_none()
                && url.fragment().is_none()
        })
}
fn proxy_views(app: &App) -> Vec<Value> {
    let accounts = app.pool.accounts();
    app.settings
        .proxy_slots()
        .into_iter()
        .map(|mut slot| {
            let id = text(&slot, "id").to_owned();
            slot["bound"] = json!(accounts
                .iter()
                .filter(|a| a.enabled() && a.proxy_slot() == id)
                .count());
            let name = text(&slot, "name").to_owned();
            slot["label"] = json!(if name.is_empty() { id } else { name });
            slot
        })
        .collect()
}
fn save_proxies(app: &App, body: &Value) -> Reply {
    let Some(slots) = body["slots"].as_array() else {
        return fail(400, "slots must be an array");
    };
    if slots
        .iter()
        .any(|s| !s.is_object() || !s["url"].is_string() || !valid_proxy(text(s, "url")))
    {
        return fail(400, "invalid proxy slot URL");
    }
    let saved = match app.settings.save_proxy_slots(json!(slots)) {
        Ok(v) => v,
        Err(e) => return setting_error(e),
    };
    let ids = saved
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect::<HashSet<_>>();
    let mut failed = Vec::new();
    for account in app.pool.accounts() {
        let slot = account.proxy_slot();
        if !slot.is_empty()
            && !ids.contains(slot.as_str())
            && account.patch(&json!({"proxySlot":""})).is_err()
        {
            failed.push(json!({"uid":account.uid(),"error":"could not persist removal of proxy-slot binding"}));
        }
    }
    let status = if failed.is_empty() { 200 } else { 500 };
    no_store(wire::json_reply(
        status,
        json!({"slots":proxy_views(app),"ok":failed.is_empty(),"failed":failed}),
    ))
}

fn logs(app: &App, q: &Query, export: bool) -> Reply {
    let since = q
        .get("since_id")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let limit = if export {
        5000
    } else {
        q.get("limit")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(200)
            .clamp(1, 2000)
    };
    let level = q.get("level").map(|s| s.to_uppercase()).unwrap_or_default();
    let tag = q.get("tag").map(|s| s.to_lowercase()).unwrap_or_default();
    let search = q
        .get("search")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    // Walk newest-first and stop at the limit, serializing only returned rows.
    let (mut rows, max_id, total) = {
        let ring = app.logs.lock();
        let matches = |e: &&crate::state::LogEntry| {
            (level.is_empty() || e.level.eq_ignore_ascii_case(&level))
                && (tag.is_empty() || e.tag.eq_ignore_ascii_case(&tag))
                && (search.is_empty()
                    || e.msg.to_lowercase().contains(&search)
                    || e.tag.to_lowercase().contains(&search))
        };
        let rows: Vec<Value> = if since == 0 {
            let mut newest: Vec<Value> = ring
                .iter()
                .rev()
                .filter(matches)
                .take(limit)
                .map(|e| e.to_json())
                .collect();
            newest.reverse();
            newest
        } else {
            ring.since(since)
                .filter(matches)
                .map(|e| e.to_json())
                .collect()
        };
        (rows, ring.max_id(), ring.len())
    };
    let secrets = log_secrets(app);
    for row in &mut rows {
        scrub_secrets(row, &secrets);
        mask_names(row);
    }
    if export {
        let lines = rows
            .iter()
            .map(|r| {
                format!(
                    "[{}] [{}] [{}] {}\n",
                    text(r, "ts"),
                    text(r, "level"),
                    text(r, "tag"),
                    text(r, "msg")
                )
            })
            .collect::<String>();
        download(
            "text/plain; charset=utf-8",
            "wb-proxy.log",
            lines.into_bytes(),
        )
    } else {
        ok(
            json!({"total":total,"logs":rows,"max_id":max_id,"mode":"memory",
            "stderr_dropped":crate::log_channel::dropped(),"stderr_closed":crate::log_channel::closed()}),
        )
    }
}
fn log_secrets(app: &App) -> Vec<String> {
    let mut secrets = app.extra_keys.clone();
    for key in app.settings.keys() {
        secrets.push(text(&key, "key").to_owned());
    }
    secrets.push(text(&app.settings.snapshot()["upstream"], "device_token").to_owned());
    for account in app.pool.accounts() {
        let cred = account.cred();
        secrets.extend([cred.access_token, cred.refresh_token, account.proxy()]);
    }
    for slot in app.settings.proxy_slots() {
        secrets.push(text(&slot, "url").to_owned());
    }
    let urls = secrets.clone();
    for raw in urls {
        if let Ok(url) = url::Url::parse(&raw) {
            if let Some(password) = url.password() {
                secrets.push(password.to_owned());
            }
        }
    }
    secrets.retain(|s| !s.is_empty());
    secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    secrets.dedup();
    secrets
}
fn scrub_secrets(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(s) => {
            for secret in secrets {
                *s = s.replace(secret, "[redacted]");
            }
            // Even expired/removed credentials must not be echoed from log text.
            // Omit the whole message when it explicitly names a credential field.
            let lower = s.to_ascii_lowercase().replace(['_', '-'], "");
            if [
                "password",
                "authorization",
                "bearer ",
                "accesstoken",
                "refreshtoken",
                "paneltoken",
                "apikey",
                "devicetoken",
            ]
            .iter()
            .any(|marker| lower.contains(marker))
            {
                *s = "[redacted sensitive log text]".into();
            }
        }
        Value::Array(xs) => {
            for x in xs {
                scrub_secrets(x, secrets);
            }
        }
        Value::Object(map) => {
            map.retain(|key, _| {
                !matches!(
                    key.to_ascii_lowercase().replace(['_', '-'], "").as_str(),
                    "accesstoken"
                        | "refreshtoken"
                        | "password"
                        | "authorization"
                        | "paneltoken"
                        | "devicetoken"
                )
            });
            for v in map.values_mut() {
                scrub_secrets(v, secrets);
            }
        }
        _ => {}
    }
}
// Display masking must never rewrite credential/identity strings in an export.
fn mask_names(value: &mut Value) {
    match value {
        Value::String(s) => *s = mask_phone(s),
        Value::Array(xs) => {
            for x in xs {
                mask_names(x);
            }
        }
        Value::Object(map) => {
            for (key, v) in map {
                if !matches!(
                    key.as_str(),
                    "uid"
                        | "id"
                        | "key"
                        | "token"
                        | "accessToken"
                        | "refreshToken"
                        | "access_token"
                        | "refresh_token"
                        | "url"
                        | "proxy"
                        | "proxySlot"
                        | "domain"
                        | "enterpriseId"
                        | "model"
                        | "account"
                        | "credentialMask"
                        | "machineId"
                        | "sessionId"
                        | "path"
                        | "storage"
                        | "file"
                ) || v.is_object()
                    || v.is_array()
                {
                    mask_names(v);
                }
            }
        }
        _ => {}
    }
}
fn mask_phone(raw: &str) -> String {
    let chars = raw.chars().collect::<Vec<_>>();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if i + 11 <= chars.len()
            && chars[i] == '1'
            && ('3'..='9').contains(&chars[i + 1])
            && chars[i..i + 11].iter().all(char::is_ascii_digit)
            && (i == 0 || !chars[i - 1].is_ascii_digit())
            && (i + 11 == chars.len() || !chars[i + 11].is_ascii_digit())
        {
            out.extend(&chars[i..i + 3]);
            out.push_str("****");
            out.extend(&chars[i + 7..i + 11]);
            i += 11;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}
fn download(content_type: &str, filename: &str, bytes: Vec<u8>) -> Reply {
    let mut reply = no_store(wire::response(200, content_type, bytes));
    if let Ok(value) =
        header::HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
    {
        reply
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    reply
}
fn atomic_json(path: &Path, value: &Value) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".panel-{}.tmp", wbcore::ids::token_hex(16)));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(wbcore::pyjson::dumps_pretty(value).as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use http_body_util::BodyExt;
    use std::path::PathBuf;
    use tokio::sync::Semaphore;

    struct Fixture {
        app: Arc<App>,
        dir: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("wb-panel-test-{}", wbcore::ids::token_hex(12)));
            fs::create_dir_all(&dir).unwrap();
            let mut args = crate::cli::Args::try_parse_from(["wb2api", "--offline"]).unwrap();
            args.host = "127.0.0.1".parse().unwrap();
            args.panel_password = None;
            args.api_key = None;
            args.accounts_dir = dir.join("accounts");
            args.usage_dir = dir.join("usage");
            fs::create_dir_all(&args.accounts_dir).unwrap();
            let settings =
                wbsettings::Settings::load(args.accounts_dir.join("settings.json")).unwrap();
            let usage = wbusage::UsageStore::new(args.usage_dir.clone());
            let mut app = App::new(
                args,
                settings,
                wbaccounts::AccountPool::new(vec![]),
                usage,
                wbhttp::Client::new().unwrap(),
                Realm::Intl,
                vec!["test-launcher-secret".into()],
            );
            app.slots = Arc::new(Semaphore::new(1));
            let app = Arc::new(app);
            Self { app, dir }
        }
        fn headers(&self) -> HeaderMap {
            let mut headers = HeaderMap::new();
            headers.insert(
                "x-panel-token",
                header::HeaderValue::from_str(&self.app.settings.sessions().issue()).unwrap(),
            );
            headers
        }
        fn account(&self) -> Arc<wbaccounts::Account> {
            self.app.pool.import(&json!({"uid":"test-account","accessToken":"ck_fixture.secret-part","nickname":"13812345678"}),None,&self.app.args.accounts_dir).unwrap().remove(0)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
    fn peer() -> SocketAddr {
        "203.0.113.1:40000".parse().unwrap()
    }
    async fn payload(reply: Reply) -> Value {
        serde_json::from_slice(&reply.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    #[tokio::test]
    async fn upstream_daily_chat_settings_do_not_claim_scheduler_support() {
        let f = Fixture::new();
        f.app
            .settings
            .patch(json!({"daily_chat_web":true}))
            .unwrap();
        let before = f.app.settings.snapshot();
        let headers = f.headers();
        for input in [
            json!({"daily_chat_budget_seconds":300}),
            json!({"daily_chat_models":["gpt-5.5"]}),
            json!({"daily_chat_web":true}),
            json!({"schedule":{"checkin":[9]}}),
        ] {
            let reply = post(
                f.app.clone(),
                "/settings/save",
                input,
                &headers,
                &Query::new(),
                peer(),
            )
            .await;
            assert_eq!(reply.status(), 501);
            assert_eq!(f.app.settings.snapshot(), before);
        }
        let view =
            payload(get(f.app.clone(), "/settings", &headers, &Query::new(), peer()).await).await;
        assert_eq!(view["daily_chat_supported"], false);
        assert_eq!(view["daily_chat_web"], false);
        assert_eq!(view["upstream_version"], "1.8.0");
        assert_eq!(view["version"], wbcore::RUST_VERSION);
    }

    #[tokio::test]
    async fn management_requires_header_session_even_with_valid_api_key() {
        let f = Fixture::new();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_static("Bearer test-launcher-secret"),
        );
        let query = Query::from([("token".into(), f.app.settings.sessions().issue())]);
        for path in [
            "/settings",
            "/settings/reveal",
            "/accounts/export",
            "/proxy/slots",
            "/usage",
            "/v1/usage",
            "/pricing",
            "/logs",
            "/updates",
            "/console/state",
            "/unknown",
        ] {
            assert_eq!(
                get(f.app.clone(), path, &headers, &query, peer())
                    .await
                    .status(),
                401,
                "{path}"
            );
        }
        for path in [
            "/settings/save",
            "/accounts/import",
            "/accounts/delete",
            "/realm",
            "/shutdown",
            "/panel/password",
            "/unknown",
        ] {
            assert_eq!(
                post(f.app.clone(), path, json!({}), &headers, &query, peer())
                    .await
                    .status(),
                401,
                "{path}"
            );
        }
        for path in ["/realm", "/health", "/panel/status"] {
            assert_eq!(
                get(
                    f.app.clone(),
                    path,
                    &HeaderMap::new(),
                    &Query::new(),
                    peer()
                )
                .await
                .status(),
                200,
                "{path}"
            );
        }
        let headers = f.headers();
        // Read-only views of unported features render as empty, never as success.
        for path in [
            "/tasks",
            "/activity/history",
            "/accounts/credits/grants",
            "/updates/backups",
            "/agents",
        ] {
            let reply = get(f.app.clone(), path, &headers, &Query::new(), peer()).await;
            assert_eq!(reply.status(), 200, "{path}");
            assert_eq!(payload(reply).await["supported"], false, "{path}");
        }
        for path in ["/tasks/claim", "/agents/configure", "/updates/rollback"] {
            assert_eq!(
                post(
                    f.app.clone(),
                    path,
                    json!({}),
                    &headers,
                    &Query::new(),
                    peer()
                )
                .await
                .status(),
                501,
                "{path}"
            );
        }
        assert_eq!(
            post(
                f.app.clone(),
                "/updates/install",
                json!({}),
                &headers,
                &Query::new(),
                peer()
            )
            .await
            .status(),
            501
        );
    }

    #[tokio::test]
    async fn import_dry_run_and_default_no_overwrite_are_non_destructive() {
        let f = Fixture::new();
        let headers = f.headers();
        let query = Query::new();
        let doc =
            json!({"uid":"test-account","accessToken":"ck_fixture.secret-part","nickname":"first"});
        let reply = post(
            f.app.clone(),
            "/accounts/import",
            json!({"data":doc,"dryRun":true}),
            &headers,
            &query,
            peer(),
        )
        .await;
        assert_eq!(reply.status(), 200);
        assert_eq!(
            payload(reply).await["result"]["added"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(f.app.pool.accounts().is_empty());
        assert_eq!(fs::read_dir(&f.app.args.accounts_dir).unwrap().count(), 0);
        assert_eq!(
            post(
                f.app.clone(),
                "/accounts/import",
                json!({"data":doc,"dryRun":"true"}),
                &headers,
                &query,
                peer()
            )
            .await
            .status(),
            400
        );
        assert!(f.app.pool.accounts().is_empty());
        assert_eq!(
            post(
                f.app.clone(),
                "/accounts/import",
                doc.clone(),
                &headers,
                &query,
                peer()
            )
            .await
            .status(),
            200
        );
        let file = f.app.args.accounts_dir.join("test-account.json");
        let original = fs::read(&file).unwrap();
        let mut replacement = doc;
        replacement["nickname"] = json!("second");
        let reply = post(
            f.app.clone(),
            "/accounts/import",
            replacement.clone(),
            &headers,
            &query,
            peer(),
        )
        .await;
        assert_eq!(
            payload(reply).await["result"]["skipped"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(fs::read(&file).unwrap(), original);
        let reply = post(
            f.app.clone(),
            "/accounts/import",
            json!({"data":replacement,"overwrite":true,"dryRun":true}),
            &headers,
            &query,
            peer(),
        )
        .await;
        assert_eq!(reply.status(), 200);
        assert_eq!(fs::read(&file).unwrap(), original);
        assert_eq!(
            f.app.pool.get("test-account").unwrap().public_metadata()["nickname"],
            "first"
        );
        assert_eq!(
            post(
                f.app.clone(),
                "/accounts/import",
                json!({"uid":"settings","accessToken":"ck_other.secret"}),
                &headers,
                &query,
                peer()
            )
            .await
            .status(),
            400
        );
    }

    #[tokio::test]
    async fn safe_views_reveal_only_at_explicit_endpoints_and_logs_scrub_text() {
        let f = Fixture::new();
        let headers = f.headers();
        let query = Query::new();
        let account = f.account();
        f.app
            .settings
            .patch(json!({"api_keys":[{"id":"key-one","key":"panel-api-secret","name":"key"}]}))
            .unwrap();
        let view = payload(get(f.app.clone(), "/settings", &headers, &query, peer()).await).await;
        assert!(!view.to_string().contains("panel-api-secret"));
        let reveal = payload(
            get(
                f.app.clone(),
                "/settings/reveal",
                &headers,
                &Query::from([("id".into(), "key-one".into())]),
                peer(),
            )
            .await,
        )
        .await;
        assert_eq!(reveal["key"], "panel-api-secret");
        let accounts = payload(
            get(
                f.app.clone(),
                "/accounts",
                &headers,
                &Query::from([("realm".into(), "all".into())]),
                peer(),
            )
            .await,
        )
        .await;
        assert!(!accounts.to_string().contains(&account.cred().access_token));
        assert_eq!(accounts["accounts"][0]["nickname"], "138****5678");
        f.app
            .logs
            .lock()
            .push("INFO", "test", "password=already-deleted-secret".into());
        f.app
            .logs
            .lock()
            .push("INFO", "test", "credential ck_fixture.secret-part".into());
        let logs = payload(get(f.app.clone(), "/logs", &headers, &query, peer()).await)
            .await
            .to_string();
        assert!(!logs.contains("already-deleted-secret"));
        assert!(!logs.contains("ck_fixture.secret-part"));
        assert_eq!(
            post(
                f.app.clone(),
                "/settings/save",
                json!({"local_web_tools":true}),
                &headers,
                &query,
                peer()
            )
            .await
            .status(),
            501
        );
        assert_eq!(f.app.settings.snapshot()["local_web_tools"], false);
    }

    #[tokio::test]
    async fn proxy_deletion_clears_bindings_and_realm_is_persisted() {
        let f = Fixture::new();
        let headers = f.headers();
        let query = Query::new();
        let account = f.account();
        let reply = post(
            f.app.clone(),
            "/proxy/slots/save",
            json!({"slots":[{"url":"http://127.0.0.1:18080"}]}),
            &headers,
            &query,
            peer(),
        )
        .await;
        assert_eq!(reply.status(), 200);
        let slots = payload(reply).await;
        let id = text(&slots["slots"][0], "id");
        assert_eq!(
            post(
                f.app.clone(),
                "/accounts/set",
                json!({"uid":account.uid(),"proxySlot":id}),
                &headers,
                &query,
                peer()
            )
            .await
            .status(),
            200
        );
        assert_eq!(account.proxy_slot(), id);
        assert_eq!(
            post(
                f.app.clone(),
                "/proxy/slots/save",
                json!({"slots":[]}),
                &headers,
                &query,
                peer()
            )
            .await
            .status(),
            200
        );
        assert_eq!(account.proxy_slot(), "");
        assert_eq!(
            wbaccounts::Account::load(account.path().unwrap())
                .unwrap()
                .proxy_slot(),
            ""
        );
        assert_eq!(
            post(
                f.app.clone(),
                "/realm",
                json!({"realm":"cn"}),
                &headers,
                &query,
                peer()
            )
            .await
            .status(),
            200
        );
        assert_eq!(*f.app.realm.read(), Realm::Cn);
        let persisted: Value = serde_json::from_slice(
            &fs::read(f.app.args.accounts_dir.join("active_realm.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(persisted["realm"], "cn");
    }

    #[tokio::test]
    async fn network_listener_cannot_disable_auth_or_restore_default_password() {
        let mut f = Fixture::new();
        Arc::get_mut(&mut f.app).unwrap().args.host = "0.0.0.0".parse().unwrap();
        f.app
            .settings
            .set_password("network-test-password")
            .unwrap();
        let headers = f.headers();
        let query = Query::new();
        let reply = post(
            f.app.clone(),
            "/settings/save",
            json!({"auth_disabled":true}),
            &headers,
            &query,
            peer(),
        )
        .await;
        assert_eq!(reply.status(), 400);
        assert_eq!(f.app.settings.snapshot()["auth_disabled"], false);
        let reply = post(
            f.app.clone(),
            "/panel/password",
            json!({"current":"network-test-password","new":"admin"}),
            &headers,
            &query,
            peer(),
        )
        .await;
        assert_eq!(reply.status(), 400);
        assert!(!f.app.settings.password_is_default());
        assert!(f.app.panel_valid(&headers));
    }

    #[tokio::test]
    async fn login_rate_limit_trusts_forwarded_ip_only_from_proxy() {
        let f = Fixture::new();
        let query = Query::new();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-real-ip",
            header::HeaderValue::from_static("198.51.100.12"),
        );
        for _ in 0..5 {
            f.app.login_limit.failure("198.51.100.12");
        }
        let reply = post(
            f.app.clone(),
            "/panel/login",
            json!({"password":"admin"}),
            &headers,
            &query,
            "127.0.0.1:40000".parse().unwrap(),
        )
        .await;
        assert_eq!(reply.status(), 429);
        assert!(reply.headers().contains_key(header::RETRY_AFTER));
        // Same supplied header on an untrusted peer cannot select that bucket.
        let reply = post(
            f.app.clone(),
            "/panel/login",
            json!({"password":"admin"}),
            &headers,
            &query,
            peer(),
        )
        .await;
        assert_eq!(reply.status(), 200);
        let token = payload(reply).await;
        assert!(f.app.settings.sessions().valid(text(&token, "token")));
    }

    #[tokio::test]
    async fn password_rotation_and_logout_revoke_sessions() {
        let f = Fixture::new();
        let headers = f.headers();
        let query = Query::new();
        let reply = post(
            f.app.clone(),
            "/panel/password",
            json!({"current":"admin","new":"changed-panel-password"}),
            &headers,
            &query,
            peer(),
        )
        .await;
        assert_eq!(reply.status(), 200);
        let reply = payload(reply).await;
        assert!(!f.app.panel_valid(&headers));
        let mut current = HeaderMap::new();
        current.insert(
            "x-panel-token",
            header::HeaderValue::from_str(text(&reply, "token")).unwrap(),
        );
        assert!(f.app.panel_valid(&current));
        assert_eq!(
            post(
                f.app.clone(),
                "/panel/logout",
                json!({}),
                &current,
                &query,
                peer()
            )
            .await
            .status(),
            200
        );
        assert!(!f.app.panel_valid(&current));
    }

    #[test]
    fn display_masking_preserves_secrets_and_identity() {
        let mut v = json!({"nickname":"13812345678","uid":"13812345678","account":{"nickname":"13912345678"},
            "auth":{"accessToken":"13812345678","refreshToken":"13812345678"},"msg":"phone 13812345678"});
        mask_names(&mut v);
        assert_eq!(v["nickname"], "138****5678");
        assert_eq!(v["uid"], "13812345678");
        assert_eq!(v["account"]["nickname"], "139****5678");
        assert_eq!(v["auth"]["accessToken"], "13812345678");
        assert_eq!(v["msg"], "phone 138****5678");
    }
    #[test]
    fn proxy_and_filename_validation() {
        assert!(valid_proxy("socks5://user:password@127.0.0.1:1080"));
        assert!(!valid_proxy("file:///etc/passwd"));
        assert!(!valid_proxy("http://"));
        assert_eq!(file_stem("../Settings"), "settings");
        assert_eq!(file_stem("a/b"), file_stem("a\\b"));
    }
    #[test]
    fn nested_log_secrets_are_removed() {
        let mut v = json!({"msg":"Bearer supersecret", "metadata":{"access_token":"supersecret"}});
        scrub_secrets(&mut v, &["supersecret".into()]);
        assert_eq!(v["msg"], "[redacted sensitive log text]");
        assert!(v["metadata"].get("access_token").is_none());
    }
}
