use crate::{Error, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};
use wbcore::{
    ids, pyjson, timeutil,
    types::{is_static_api_key, Cred, InResponse, Product, Realm},
};

pub(crate) fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_owned()
}
pub(crate) fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str()?.parse().ok())
        .filter(|x| x.is_finite())
}
pub fn normalize_epoch(v: &Value) -> i64 {
    let n = number(v).unwrap_or(0.0).max(0.0);
    (if n > 1e11 { n / 1000.0 } else { n }) as i64
}
/// JWT payload inspection only; this does NOT verify a signature or authorize a user.
pub fn jwt_claims(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|s| URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}
pub fn api_key_id(token: &str) -> String {
    token
        .get(3..)
        .unwrap_or_default()
        .split('.')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect()
}
pub fn realm_evidence(token: &str, domain: &str) -> Option<Realm> {
    let issuer = text(&jwt_claims(token), "iss");
    for s in [issuer.as_str(), domain] {
        if ["copilot.tencent.com", "codebuddy.cn", "workbuddy.cn"]
            .iter()
            .any(|x| s.contains(x))
        {
            return Some(Realm::Cn);
        }
        if ["workbuddy.ai", "codebuddy.ai"]
            .iter()
            .any(|x| s.contains(x))
        {
            return Some(Realm::Intl);
        }
    }
    None
}
pub fn default_domain(realm: Realm) -> &'static str {
    if realm == Realm::Cn {
        "copilot.tencent.com"
    } else {
        "www.workbuddy.ai"
    }
}
pub fn is_free_cap_model(realm: Realm, model: &str) -> bool {
    matches!(model, "hy4-preview-f" | "hy3")
        || realm == Realm::Intl && model == "deepseek-v4.1-flash"
}
fn safe_uid(uid: &str) -> String {
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
        .to_owned()
}

/// Daily statistics are supplied by the gateway/usage crate, not folded here.
/// None means unknown and must never block dispatch. Model token keys are exact.
#[derive(Clone, Debug)]
pub struct Guards {
    pub reserve_credits: f64,
    pub expiring_window_days: f64,
    pub daily_token_limit: u64,
    pub daily_tokens_today: Option<u64>,
    pub daily_credit_limit: f64,
    pub daily_credits_today: Option<f64>,
    pub model_daily_token_limit: u64,
    pub model_daily_tokens: HashMap<String, u64>,
    /// Shared across accounts; rebuilt only when the model catalog changes.
    pub free_models: Arc<HashSet<String>>,
    pub pro_models: Arc<HashSet<String>>,
}
impl Default for Guards {
    fn default() -> Self {
        Self {
            reserve_credits: 0.0,
            expiring_window_days: 7.0,
            daily_token_limit: 0,
            daily_tokens_today: None,
            daily_credit_limit: 0.0,
            daily_credits_today: None,
            model_daily_token_limit: 0,
            model_daily_tokens: HashMap::new(),
            free_models: Arc::default(),
            pro_models: Arc::default(),
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum BlockReason {
    Disabled,
    MissingCredential,
    Expired,
    ModelDisabled,
    ProRequired,
    Restricted,
    CoolingDown,
    Reserve,
    DailyTokens,
    DailyCredits,
    ModelDailyTokens,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Readiness {
    Ready,
    NeedsRefresh,
    Blocked {
        reason: BlockReason,
        retry_after: f64,
    },
}
#[derive(Default)]
pub(crate) struct Runtime {
    pub cooldown: f64,
    pub balance: f64,
    pub breaker: f64,
    pub degrade: f64,
    pub soft: u32,
    pub unscoped: u32,
    pub fails: u32,
    pub unknown: u32,
    pub last_fail: Option<f64>,
    pub last_soft: Option<f64>,
    pub models: HashMap<String, f64>,
    pub models_at: HashMap<String, f64>,
    pub expired: bool,
    pub error: String,
    pub detail: String,
}
pub(crate) struct State {
    pub doc: Value,
    pub runtime: Runtime,
    pub guards: Guards,
}
/// Shared account. Identity/uid/realm are immutable; tokens and limits use short locks.
/// No method opens a network connection. Secret fields are omitted from Debug.
pub struct Account {
    uid: String,
    realm: Realm,
    pub(crate) state: RwLock<State>,
    path: Mutex<Option<PathBuf>>,
    save_lock: Mutex<()>,
    inflight: AtomicUsize,
    refreshing: AtomicBool,
    fail_window: RwLock<f64>,
}
impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("uid", &self.uid)
            .field("realm", &self.realm)
            .finish_non_exhaustive()
    }
}
impl Account {
    pub fn from_json(input: &Value) -> Result<Self> {
        if !input.is_object() {
            return Err(Error::Invalid("account must be an object".into()));
        }
        let token = text(input, "accessToken").trim().to_owned();
        if token.is_empty() {
            return Err(Error::Invalid("account has no accessToken".into()));
        }
        let api = is_static_api_key(&token);
        let claims = jwt_claims(&token);
        let uid = match text(input, "uid").trim() {
            "" if api => format!("apikey-{}", api_key_id(&token)),
            "" => text(&claims, "sub"),
            s => s.into(),
        };
        if uid.is_empty() || uid == "apikey-" {
            return Err(Error::Invalid("cannot determine account uid".into()));
        }
        let realm = if api {
            Realm::Cn
        } else {
            Realm::parse(&text(input, "realm"))
                .or_else(|| realm_evidence(&token, &text(input, "domain")))
                .unwrap_or(Realm::Intl)
        };
        let domain = text(input, "domain");
        let product = Product::normalize(&text(input, "product"));
        let mut nickname = text(input, "nickname");
        if nickname.contains("$wbEncrypted") {
            nickname.clear();
        }
        let enabled = input["enabled"].as_bool().unwrap_or(true);
        let mut disabled = Vec::new();
        if let Some(values) = input["disabledModels"].as_array() {
            for value in values {
                if let Some(s) = value.as_str() {
                    let s = s.trim().to_lowercase();
                    if !s.is_empty() && !disabled.contains(&s) {
                        disabled.push(s);
                    }
                }
            }
        }
        let exp = normalize_epoch(&input["expiresAt"]);
        let enterprise = text(input, "enterpriseId");
        let mut doc = json!({
            "uid":uid, "nickname":nickname, "domain":if domain.is_empty() { default_domain(realm).into() } else { domain },
            "realm":realm.as_str(), "platform":input["platform"].as_str().filter(|s| !s.is_empty()).unwrap_or("CLI"),
            "product":product.as_str(), "enterpriseId":if enterprise.is_empty() { text(&claims,"enterprise_id") } else { enterprise },
            "accessToken":token, "refreshToken":text(input,"refreshToken"),
            "expiresAt":if api { 0 } else if exp > 0 { exp } else { normalize_epoch(&claims["exp"]) },
            "addedAt":number(&input["addedAt"]).unwrap_or_else(timeutil::now),
            "source":input["source"].as_str().unwrap_or(if api { "api-key" } else { "oauth" }), "apiKey":api,
            "proxySlot":text(input,"proxySlot"), "proxy":text(input,"proxy"), "enabled":enabled,
            "tasksEnabled":input["tasksEnabled"].as_bool().unwrap_or(enabled),
            "weight":number(&input["weight"]).map(|n| (n as i64).clamp(1,100)).unwrap_or(1),
            "note":text(input,"note").chars().take(200).collect::<String>(), "disabledModels":disabled,
            "restrictedUntil":number(&input["restrictedUntil"]).unwrap_or(0.0).max(0.0),
            "credits":if input["credits"].is_object() { input["credits"].clone() } else { Value::Null },
            "tier":if input["tier"].is_object() { input["tier"].clone() } else { Value::Null },
            "lastCheckin":input["lastCheckin"].as_str(), "lastDailyChat":input["lastDailyChat"].as_str(),
            "lastActivityReport":input["lastActivityReport"].as_str(), "modelCaps":{}
        });
        let mut runtime = Runtime::default();
        if let Some(caps) = input["modelCaps"].as_object() {
            for (model, cap) in caps {
                let at = number(&cap["at"]).unwrap_or(0.0);
                let reset = number(&cap["reset"]).unwrap_or(0.0);
                if is_free_cap_model(realm, model) && at > 0.0 && reset > 0.0 {
                    doc["modelCaps"][model] = json!({"at":at,"reset":reset});
                    runtime.models.insert(model.clone(), reset);
                    runtime.models_at.insert(model.clone(), at);
                }
            }
        }
        Ok(Self {
            uid,
            realm,
            state: RwLock::new(State {
                doc,
                runtime,
                guards: Guards::default(),
            }),
            path: Mutex::new(None),
            save_lock: Mutex::new(()),
            inflight: AtomicUsize::new(0),
            refreshing: AtomicBool::new(false),
            fail_window: RwLock::new(600.0),
        })
    }
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let a = Self::from_json(&serde_json::from_slice(&fs::read(path)?)?)?;
        *a.path.lock() = Some(path.to_owned());
        Ok(a)
    }
    pub fn uid(&self) -> &str {
        &self.uid
    }
    pub fn realm(&self) -> Realm {
        self.realm
    }
    pub fn cred(&self) -> Cred {
        let s = self.state.read();
        let d = &s.doc;
        Cred {
            uid: self.uid.clone(),
            access_token: text(d, "accessToken"),
            refresh_token: text(d, "refreshToken"),
            realm: self.realm,
            product: Product::normalize(&text(d, "product")),
            enterprise_id: text(d, "enterpriseId"),
            domain: text(d, "domain"),
            platform: text(d, "platform"),
        }
    }
    /// Full persisted account JSON, including credentials. Never expose publicly.
    pub fn to_json(&self) -> Value {
        self.state.read().doc.clone()
    }
    pub fn public_metadata(&self) -> Value {
        let now = timeutil::now();
        let s = self.state.read();
        let mut d = s.doc.clone();
        let access = text(&d, "accessToken");
        let refresh = text(&d, "refreshToken");
        let proxy = text(&d, "proxy");
        let object = d.as_object_mut().unwrap();
        object.remove("accessToken");
        object.remove("refreshToken");
        object.remove("proxy"); // Proxy URLs can contain passwords; never publish the literal URL.
        d["hasProxy"] = json!(!proxy.is_empty());
        d["hasRefreshToken"] = json!(!refresh.is_empty());
        d["credentialMask"] = json!(if is_static_api_key(&access) {
            "***"
        } else {
            ""
        });
        d["tokenExpired"] = json!(
            access.is_empty()
                || s.runtime.expired
                || d["expiresAt"]
                    .as_i64()
                    .is_some_and(|e| e > 0 && e as f64 <= now)
        );
        d["needRelogin"] = d["tokenExpired"].clone();
        d["cooldownUntil"] = json!(deadline(&s, ""));
        d["inCooldown"] = json!(deadline(&s, "") > now);
        d["lastError"] = json!(s.runtime.error); // Scrub exact credentials below, including nested server metadata.
        d["fails"] = json!(s.runtime.fails);
        d["softStreak"] = json!(s.runtime.soft);
        d["unscopedStreak"] = json!(s.runtime.unscoped);
        d["inFlight"] = json!(self.in_flight());
        d["reserveCredits"] = json!(s.guards.reserve_credits);
        d["dailyTokenLimit"] = json!(s.guards.daily_token_limit);
        d["dailyTokensToday"] = json!(s.guards.daily_tokens_today);
        d["dailyCreditLimit"] = json!(s.guards.daily_credit_limit);
        d["dailyCreditsToday"] = json!(s.guards.daily_credits_today);
        d["modelDailyTokenLimit"] = json!(s.guards.model_daily_token_limit);
        d["modelDailyTokens"] = json!(s.guards.model_daily_tokens);
        d["restrictedFor"] =
            json!((number(&s.doc["restrictedUntil"]).unwrap_or(0.0) - now).max(0.0));
        d["cooldownFor"] = json!((deadline(&s, "") - now).max(0.0));
        d["breakerFor"] = json!((s.runtime.breaker - now).max(0.0));
        d["degradeFor"] = json!((s.runtime.degrade - now).max(0.0));
        d["lastErrorDetail"] = if s.runtime.detail.is_empty() {
            Value::Null
        } else {
            json!(s.runtime.detail)
        };
        d["failWindow"] = json!(*self.fail_window.read());
        d["lastFailFor"] = json!(s.runtime.last_fail.map(|at| (now - at).max(0.0)));
        let exp = s.doc["expiresAt"].as_i64().unwrap_or(0);
        d["expiresIn"] = if d["tokenExpired"] == true {
            json!("expired")
        } else if exp > 0 {
            json!(timeutil::human_left(exp as f64 - now))
        } else {
            Value::Null
        };
        d["file"] = json!(self
            .path()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned())));
        let g = &s.guards;
        d["reserveBlocked"] = json!(
            g.reserve_credits > 0.0
                && s.doc["credits"]["remain"]
                    .as_f64()
                    .is_some_and(|r| r.trunc() <= g.reserve_credits)
        );
        d["dailyLimitBlocked"] = json!(
            g.daily_token_limit > 0
                && g.daily_tokens_today
                    .is_some_and(|n| n >= g.daily_token_limit)
        );
        d["creditLimitReached"] = json!(
            g.daily_credit_limit > 0.0
                && g.daily_credits_today
                    .is_some_and(|n| n >= g.daily_credit_limit)
        );
        let personal = !is_static_api_key(&access) && text(&s.doc, "enterpriseId").is_empty();
        d["canCheckin"] = json!(self.realm == Realm::Cn && personal);
        d["canDailyChat"] = json!(self.realm == Realm::Intl);
        d["canReportActivity"] = json!(self.realm == Realm::Cn && personal);
        d["machineId"] = json!(ids::machine_id(&self.uid));
        d["sessionId"] = json!(ids::session_id(&self.uid));
        let mut model_ends = s.runtime.models.clone();
        if let Some(caps) = s.doc["modelCaps"].as_object() {
            for (model, cap) in caps {
                let end = model_ends.entry(model.clone()).or_default();
                *end = end.max(number(&cap["reset"]).unwrap_or(0.0));
            }
        }
        let mut active = model_ends
            .into_iter()
            .filter(|(_, until)| *until > now)
            .collect::<Vec<_>>();
        active.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        d["modelCooldowns"] = Value::Array(
            active
                .into_iter()
                .map(|(model, until)| {
                    let mut entry = json!({"model":model,"expiresAt":until as i64});
                    if let Some(at) = s.runtime.models_at.get(&model) {
                        entry["startedAt"] = json!(*at as i64);
                    }
                    if number(&s.doc["modelCaps"][&model]["reset"]).is_some_and(|reset| reset > now)
                    {
                        entry["cappedAt"] =
                            json!(number(&s.doc["modelCaps"][&model]["at"]).unwrap_or(0.0) as i64);
                    }
                    entry
                })
                .collect(),
        );
        redact(&mut d, &[&access, &refresh, &proxy]);
        d
    }
    pub fn path(&self) -> Option<PathBuf> {
        self.path.lock().clone()
    }
    pub fn save(&self) -> Result<()> {
        let path = self.path();
        if let Some(p) = path {
            self.save_to(p.parent().unwrap_or(Path::new(".")))?;
        }
        Ok(())
    }
    pub fn save_to(&self, dir: impl AsRef<Path>) -> Result<PathBuf> {
        let s = self.state.read();
        self.persist_document(&s.doc, dir.as_ref())
    }
    fn persist_if_bound(&self, doc: &Value, dir: Option<&Path>) -> Result<()> {
        let path = self.path();
        let directory = dir.or_else(|| path.as_ref().and_then(|p| p.parent()));
        if let Some(dir) = directory {
            self.persist_document(doc, dir)?;
        }
        Ok(())
    }
    fn persist_document(&self, doc: &Value, dir: &Path) -> Result<PathBuf> {
        // Serialize collision check+replace across distinct Account objects too.
        static FILE_WRITES: Mutex<()> = Mutex::new(());
        let _guard = self.save_lock.lock();
        let _files = FILE_WRITES.lock();
        fs::create_dir_all(dir)?;
        let mut name = safe_uid(&self.uid);
        if name.is_empty() {
            name = ids::uuid4_hex();
        }
        // Keep a loaded filename in the same directory: canonicalizing it would
        // leave stale credentials behind for the next sorted directory load.
        let existing = self.path();
        let canonical_dir = fs::canonicalize(dir)?;
        let final_path = existing
            .filter(|path| {
                path.parent()
                    .and_then(|parent| fs::canonicalize(parent).ok())
                    .is_some_and(|parent| parent == canonical_dir)
            })
            .unwrap_or_else(|| dir.join(format!("{name}.json")));
        match fs::read(&final_path) {
            Ok(bytes) => {
                let previous: Value = serde_json::from_slice(&bytes).map_err(|_| {
                    Error::Invalid("refusing to overwrite an invalid existing account file".into())
                })?;
                let previous_uid = Account::from_json(&previous)
                    .map(|a| a.uid().to_owned())
                    .map_err(|_| {
                        Error::Invalid(
                            "refusing to overwrite an invalid existing account file".into(),
                        )
                    })?;
                if previous_uid != self.uid {
                    return Err(Error::Invalid(
                        "account filename collides with a different uid".into(),
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let tmp = dir.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            ids::uuid4_hex()
        ));
        let result = (|| -> Result<()> {
            let mut options = fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp)?;
            file.write_all(pyjson::dumps_pretty(doc).as_bytes())?;
            drop(file);
            fs::rename(&tmp, &final_path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        *self.path.lock() = Some(final_path.clone());
        Ok(final_path)
    }
    /// Operator metadata edits only. Immutable uid/realm and credentials are rejected;
    /// credential replacement goes through pool upsert and preserves runtime state.
    pub fn patch(&self, patch: &Value) -> Result<()> {
        let map = patch
            .as_object()
            .ok_or_else(|| Error::Invalid("account patch must be an object".into()))?;
        const ALLOWED: &[&str] = &[
            "nickname",
            "platform",
            "product",
            "enterpriseId",
            "domain",
            "proxySlot",
            "proxy",
            "enabled",
            "tasksEnabled",
            "weight",
            "note",
            "disabledModels",
        ];
        if map.keys().any(|k| !ALLOWED.contains(&k.as_str())) {
            return Err(Error::Invalid(
                "unsupported or immutable account patch field".into(),
            ));
        }
        for (key, value) in map {
            let valid = match key.as_str() {
                "enabled" | "tasksEnabled" => value.is_boolean(),
                "weight" => value.as_i64().is_some(),
                "disabledModels" => value
                    .as_array()
                    .is_some_and(|xs| xs.iter().all(Value::is_string)),
                "product" => value.as_str().and_then(Product::parse_strict).is_some(),
                _ => value.is_string(),
            };
            if !valid {
                return Err(Error::Invalid(format!("invalid account patch field {key}")));
            }
        }
        let mut s = self.state.write();
        let mut doc = s.doc.clone();
        for (k, v) in map {
            doc[k] = v.clone();
        }
        let normalized = Account::from_json(&doc)?;
        let doc = normalized.to_json();
        self.persist_if_bound(&doc, None)?;
        s.doc = doc;
        Ok(())
    }
    pub(crate) fn replace_persisted(&self, incoming: &Account, dir: Option<&Path>) -> Result<()> {
        if self.uid != incoming.uid || self.realm != incoming.realm {
            return Err(Error::Invalid(
                "cannot replace an account with a different uid/realm".into(),
            ));
        }
        let mut doc = incoming.to_json();
        let mut s = self.state.write();
        doc["restrictedUntil"] = json!(number(&doc["restrictedUntil"])
            .unwrap_or(0.0)
            .max(number(&s.doc["restrictedUntil"]).unwrap_or(0.0)));
        if let Some(caps) = s.doc["modelCaps"].as_object() {
            for (model, cap) in caps {
                if number(&cap["reset"]).unwrap_or(0.0)
                    > number(&doc["modelCaps"][model]["reset"]).unwrap_or(0.0)
                {
                    doc["modelCaps"][model] = cap.clone();
                }
            }
        }
        self.persist_if_bound(&doc, dir)?;
        if doc["accessToken"] != s.doc["accessToken"] {
            s.runtime.expired = false;
        }
        s.doc = doc;
        Ok(())
    }
    pub fn set_guards(&self, guards: Guards) {
        self.state.write().guards = guards;
    }
    pub fn guards(&self) -> Guards {
        self.state.read().guards.clone()
    }
    pub fn set_fail_window(&self, seconds: f64) {
        *self.fail_window.write() = if seconds.is_finite() {
            seconds.max(0.0)
        } else {
            600.0
        };
    }
    pub fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.patch(&json!({"enabled":enabled}))
    }
    pub fn set_weight(&self, weight: i64) -> Result<()> {
        self.patch(&json!({"weight":weight}))
    }
    pub fn set_disabled_models(&self, models: &[String]) -> Result<()> {
        self.patch(&json!({"disabledModels":models}))
    }
    pub fn weight(&self) -> i64 {
        self.state.read().doc["weight"].as_i64().unwrap_or(1)
    }
    pub fn enabled(&self) -> bool {
        self.state.read().doc["enabled"].as_bool().unwrap_or(false)
    }
    pub fn proxy(&self) -> String {
        text(&self.state.read().doc, "proxy")
    }
    pub fn proxy_slot(&self) -> String {
        text(&self.state.read().doc, "proxySlot")
    }
    pub fn resolve_proxy(&self, slots: &Value) -> String {
        self.resolve_proxy_in(slots.as_array().map(Vec::as_slice).unwrap_or(&[]))
    }
    /// Same as `resolve_proxy`, without wrapping the slot list in a JSON array.
    pub fn resolve_proxy_in(&self, slots: &[Value]) -> String {
        let s = self.state.read();
        let slot = s.doc["proxySlot"].as_str().unwrap_or("");
        if !slot.is_empty() {
            for row in slots {
                if row["id"].as_str() == Some(slot) && row["enabled"].as_bool().unwrap_or(true) {
                    return text(row, "url");
                }
            }
        }
        text(&s.doc, "proxy")
    }
    pub fn readiness(&self, model: &str, now: f64) -> Readiness {
        let s = self.state.read();
        let block = |reason, retry_after| Readiness::Blocked {
            reason,
            retry_after,
        };
        if !s.doc["enabled"].as_bool().unwrap_or(false) {
            return block(BlockReason::Disabled, 0.0);
        }
        if text(&s.doc, "accessToken").is_empty() {
            return block(BlockReason::MissingCredential, 0.0);
        }
        if s.runtime.expired {
            return block(BlockReason::Expired, 0.0);
        }
        if s.doc["disabledModels"].as_array().is_some_and(|xs| {
            // Equal to comparing against `model.trim().to_lowercase()`, without allocating.
            let wanted = model.trim();
            xs.iter().any(|v| {
                v.as_str().is_some_and(|stored| {
                    stored
                        .chars()
                        .eq(wanted.chars().flat_map(char::to_lowercase))
                })
            })
        }) {
            return block(BlockReason::ModelDisabled, 0.0);
        }
        let paid = s.doc["tier"]["paid"]
            .as_bool()
            .or_else(|| s.doc["credits"]["is_paid_user"].as_bool())
            .unwrap_or(false);
        if !paid && s.guards.pro_models.contains(model) {
            return block(BlockReason::ProRequired, 0.0);
        }
        let restricted = number(&s.doc["restrictedUntil"]).unwrap_or(0.0);
        if restricted > now {
            return block(BlockReason::Restricted, restricted - now);
        }
        let wait = (deadline(&s, model) - now).max(0.0);
        if wait > 0.0 {
            return block(BlockReason::CoolingDown, wait);
        }
        let g = &s.guards;
        if g.reserve_credits > 0.0
            && s.doc["credits"]["remain"]
                .as_f64()
                .is_some_and(|r| r.trunc() <= g.reserve_credits)
        {
            return block(BlockReason::Reserve, 0.0);
        }
        let midnight = (timeutil::local_midnight(now, -1) - now).max(60.0);
        if g.daily_token_limit > 0
            && g.daily_tokens_today
                .is_some_and(|n| n >= g.daily_token_limit)
        {
            return block(BlockReason::DailyTokens, midnight);
        }
        if !model.is_empty()
            && !g.free_models.contains(model)
            && g.daily_credit_limit > 0.0
            && g.daily_credits_today
                .is_some_and(|n| n >= g.daily_credit_limit)
        {
            return block(BlockReason::DailyCredits, midnight);
        }
        if !model.is_empty()
            && g.model_daily_token_limit > 0
            && g.model_daily_tokens
                .get(model)
                .is_some_and(|n| *n >= g.model_daily_token_limit)
        {
            return block(BlockReason::ModelDailyTokens, midnight);
        }
        let exp = s.doc["expiresAt"].as_i64().unwrap_or(0);
        if exp > 0 && exp as f64 - now <= 120.0 {
            return if text(&s.doc, "refreshToken").is_empty() {
                block(BlockReason::Expired, 0.0)
            } else {
                Readiness::NeedsRefresh
            };
        }
        Readiness::Ready
    }
    pub fn ready(&self, model: &str) -> bool {
        self.readiness(model, timeutil::now()) == Readiness::Ready
    }
    pub fn throttle_wait(&self, model: &str, now: f64) -> f64 {
        (deadline(&self.state.read(), model) - now).max(0.0)
    }
    pub fn cap_until(&self, model: &str, now: f64) -> f64 {
        number(&self.state.read().doc["modelCaps"][model]["reset"])
            .filter(|x| *x > now)
            .unwrap_or(0.0)
    }
    pub fn expiring_weight(&self) -> Option<i64> {
        let s = self.state.read();
        let window = s.guards.expiring_window_days;
        if window <= 0.0 {
            return None;
        }
        let days = s.doc["credits"]["packages"]
            .as_array()?
            .iter()
            .filter(|p| {
                p["no_expiry"] != true
                    && p["is_expired"] != true
                    && p["package_code"] != "enterprise"
                    && p["remain"].as_f64().is_some_and(|n| n > 0.0)
            })
            .filter_map(|p| p["days_left"].as_f64())
            .filter(|n| *n >= 0.0)
            .min_by(f64::total_cmp)?;
        if days > window {
            None
        } else {
            Some((wbcore::pyval::py_round(window - days, 0) as i64 + 1).max(1))
        }
    }
    pub fn consume_credit(&self, amount: f64) {
        if !amount.is_finite() || amount <= 0.0 {
            return;
        }
        let mut s = self.state.write();
        let c = &mut s.doc["credits"];
        let Some(remain) = c["remain"].as_f64() else {
            return;
        };
        let remain = wbcore::pyval::py_round((remain - amount).max(0.0), 2);
        let used = wbcore::pyval::py_round(c["used"].as_f64().unwrap_or(0.0) + amount, 2);
        c["remain"] = json!(remain);
        c["used"] = json!(used);
        if let Some(size) = c["size"].as_f64().filter(|n| *n > 0.0) {
            c["remain_percent"] = json!(format!("{:.1}%", remain / size * 100.0));
            c["used_percent"] = json!(format!("{:.1}%", used.min(size) / size * 100.0));
        }
    }
    /// Persist a normalized billing tier atomically, preserving credentials,
    /// restrictions and concurrent account metadata. Missing paid means unknown.
    pub fn set_tier(&self, tier: Value) -> Result<()> {
        let map = tier
            .as_object()
            .ok_or_else(|| Error::Invalid("tier must be an object".into()))?;
        let mut normalized = json!({});
        for (key, value) in map {
            let valid = match key.as_str() {
                "paid" => value.is_null() || value.is_boolean(),
                "package" | "package_code" | "source" => value.is_string(),
                "protected_price" => value.is_boolean(),
                "trial" => value.as_i64().is_some(),
                "checked_at" => value.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0),
                _ => false,
            };
            if !valid {
                return Err(Error::Invalid(format!("invalid tier field {key}")));
            }
            normalized[key] = value.clone();
        }
        let mut s = self.state.write();
        let mut doc = s.doc.clone();
        doc["tier"] = normalized;
        self.persist_if_bound(&doc, None)?;
        s.doc = doc;
        Ok(())
    }
    pub fn set_credits(&self, credits: Value) -> Result<()> {
        if !credits.is_object() {
            return Err(Error::Invalid("credits must be an object".into()));
        }
        let mut s = self.state.write();
        let mut doc = s.doc.clone();
        doc["credits"] = credits;
        self.persist_if_bound(&doc, None)?;
        if doc["credits"]["remain"]
            .as_f64()
            .is_some_and(|n| n.trunc() > 0.0)
        {
            s.runtime.balance = 0.0;
        }
        s.doc = doc;
        Ok(())
    }
    pub fn note_error(
        &self,
        message: &str,
        model: Option<&str>,
        cooldown: f64,
        until: Option<f64>,
        now: f64,
    ) {
        let mut s = self.state.write();
        s.runtime.error = message.chars().take(200).collect();
        if crate::auth_failure_text(message) {
            s.runtime.expired = true;
        }
        let end = until
            .map(|u| now + (u - now).max(1.0))
            .unwrap_or(now + cooldown.max(0.0));
        if let Some(model) = model {
            s.runtime.models.insert(model.into(), end);
            s.runtime.models_at.insert(model.into(), now);
        } else {
            s.runtime.cooldown = end;
        }
    }
    pub fn note_soft_rate(&self, now: f64) -> f64 {
        let mut s = self.state.write();
        decay_soft(&mut s.runtime, now, *self.fail_window.read());
        s.runtime.soft = s.runtime.soft.saturating_add(1);
        let wait = backoff(s.runtime.soft, 600.0, 7200.0, 1);
        s.runtime.cooldown = s.runtime.cooldown.max(now + wait);
        s.runtime.error = "HTTP 429 (account soft rate)".into();
        wait
    }
    /// Advance the unscoped ladder; caller applies the returned wait to its model.
    pub fn note_unscoped_rate(&self, _model: &str, now: f64) -> f64 {
        let mut s = self.state.write();
        decay_soft(&mut s.runtime, now, *self.fail_window.read());
        s.runtime.unscoped = s.runtime.unscoped.saturating_add(1);
        backoff(s.runtime.unscoped, 60.0, 600.0, 1)
    }
    pub fn note_failure(&self, message: &str, now: f64) {
        self.failure(message, now, false);
    }
    pub fn note_unknown_failure(&self, message: &str, now: f64) {
        self.failure(message, now, true);
    }
    fn failure(&self, message: &str, now: f64, unknown: bool) {
        let mut s = self.state.write();
        let r = &mut s.runtime;
        let window = *self.fail_window.read();
        if window > 0.0 && r.last_fail.is_some_and(|t| now - t > window) {
            r.fails = 0;
            r.unknown = 0;
        }
        r.last_fail = Some(now);
        r.fails = r.fails.saturating_add(1);
        r.error = message.chars().take(200).collect();
        if unknown {
            r.unknown = r.unknown.saturating_add(1);
        }
        if r.fails >= 3 {
            r.breaker = r.breaker.max(now + backoff(r.fails, 1800.0, 21600.0, 3));
        }
        if r.unknown >= 5 {
            r.degrade = r.degrade.max(now + backoff(r.unknown, 600.0, 7200.0, 5));
        }
    }
    /// Persist the first restriction deadline. Repeated hits do not extend it.
    pub fn note_restricted(&self, detail: &str, now: f64) -> Result<()> {
        let mut s = self.state.write();
        if number(&s.doc["restrictedUntil"]).unwrap_or(0.0) <= now {
            s.doc["restrictedUntil"] = json!(now + 7200.0);
        }
        s.runtime.error = "HTTP 403 (account restricted)".into();
        s.runtime.detail = detail.chars().take(2000).collect();
        drop(s);
        self.save()
    }
    pub fn note_balance_cooled(&self) {
        let mut s = self.state.write();
        s.runtime.balance = s.runtime.balance.max(timeutil::next_local_4am());
        s.runtime.error = "HTTP 402 (insufficient credits)".into();
    }
    /// Restore a known window without replaying its error or shortening a live limit.
    /// The probe's last_try is not a cooldown start time; pass None when restoring it.
    pub fn arm_model_cooldown(
        &self,
        model: &str,
        until: f64,
        started_at: Option<f64>,
        now: f64,
    ) -> bool {
        if model.is_empty() || !until.is_finite() || !now.is_finite() || until <= now {
            return false;
        }
        let mut s = self.state.write();
        let end = s.runtime.models.entry(model.into()).or_default();
        if *end >= until {
            return false;
        }
        *end = until;
        if let Some(started) = started_at.filter(|at| at.is_finite() && *at > 0.0 && *at <= until) {
            s.runtime.models_at.entry(model.into()).or_insert(started);
        }
        true
    }
    /// Clear only a routine model window and its start time, never a live quota cap.
    pub fn clear_model_cooldown(&self, model: &str, now: f64) {
        if model.is_empty() || !now.is_finite() {
            return;
        }
        let mut s = self.state.write();
        if number(&s.doc["modelCaps"][model]["reset"]).is_some_and(|reset| reset > now) {
            return;
        }
        s.runtime.models.remove(model);
        s.runtime.models_at.remove(model);
    }
    /// A served request clears the credential park, but never an active 11140
    /// restriction or a live 6004 cap, even when the success arrived late.
    pub fn note_success(&self, model: &str, now: f64) {
        let mut s = self.state.write();
        let cap = number(&s.doc["modelCaps"][model]["reset"]).unwrap_or(0.0);
        if cap <= now {
            s.runtime.models.remove(model);
            s.runtime.models_at.remove(model);
        }
        let r = &mut s.runtime;
        r.soft = 0;
        r.unscoped = 0;
        r.fails = 0;
        r.unknown = 0;
        r.last_fail = None;
        r.last_soft = None;
        r.breaker = 0.0;
        r.degrade = 0.0;
        r.cooldown = 0.0;
        r.expired = false;
        r.error.clear();
        r.detail.clear();
    }
    pub fn note_model_cap(&self, model: &str, reset: f64, now: f64) -> Result<bool> {
        if !is_free_cap_model(self.realm, model) || !reset.is_finite() || reset <= 0.0 {
            return Ok(false);
        }
        let mut s = self.state.write();
        let old = number(&s.doc["modelCaps"][model]["reset"]).unwrap_or(0.0);
        let changed = old != reset;
        if changed {
            s.doc["modelCaps"][model] = json!({"at":now,"reset":reset});
        }
        let end = s.runtime.models.entry(model.into()).or_default();
        *end = end.max(reset);
        drop(s);
        if changed {
            self.save()?;
        }
        Ok(changed)
    }
    /// Clear routine errors, never restrictions, breakers, or live quota caps.
    pub fn clear_error(&self, model: Option<&str>, now: f64) {
        let mut s = self.state.write();
        let caps = s.doc["modelCaps"].clone();
        s.runtime.models.retain(|m, _| {
            model.is_some_and(|x| x != m) || number(&caps[m]["reset"]).is_some_and(|n| n > now)
        });
        s.runtime.models_at.retain(|m, _| {
            model.is_some_and(|x| x != m) || number(&caps[m]["reset"]).is_some_and(|n| n > now)
        });
        s.runtime.cooldown = 0.0;
        s.runtime.balance = 0.0;
        s.runtime.error.clear();
        s.runtime.detail.clear();
    }
    /// Successful refresh persists credentials; never clears restrictions or model caps.
    pub fn apply_refresh_response(&self, response: &InResponse) -> Result<()> {
        if self.cred().is_api_key() {
            return Err(Error::Invalid("static API keys cannot be refreshed".into()));
        }
        if !(200..300).contains(&response.status) {
            if response.status == 401 {
                self.state.write().runtime.expired = true;
            }
            return Err(Error::Upstream {
                status: response.status,
                code: crate::response_code(response),
            });
        }
        let value: Value = serde_json::from_slice(&response.body)?;
        let data = if value["data"]["data"].is_object() {
            &value["data"]["data"]
        } else {
            &value["data"]
        };
        let token = text(data, "accessToken");
        if token.is_empty() {
            self.state.write().runtime.expired = true;
            return Err(Error::Invalid("refresh returned no token".into()));
        }
        let claims = jwt_claims(&token);
        let sub = text(&claims, "sub");
        if !sub.is_empty() && sub != self.uid {
            return Err(Error::Invalid(
                "refresh returned a different account identity".into(),
            ));
        }
        let mut s = self.state.write();
        let mut doc = s.doc.clone();
        doc["accessToken"] = json!(token);
        let refresh = text(data, "refreshToken");
        if !refresh.is_empty() {
            doc["refreshToken"] = json!(refresh);
        }
        let exp = normalize_epoch(&claims["exp"]);
        if exp > 0 {
            doc["expiresAt"] = json!(exp);
        }
        self.persist_if_bound(&doc, None)?;
        s.doc = doc;
        s.runtime.expired = false;
        s.runtime.cooldown = 0.0;
        s.runtime.error.clear();
        Ok(())
    }
    pub fn in_flight(&self) -> usize {
        self.inflight.load(Ordering::Acquire)
    }
    /// Even unlimited acquisitions are tracked, so enabling a cap mid-stream is safe.
    pub fn try_acquire_slot(self: &Arc<Self>, limit: usize) -> Option<AccountSlot> {
        let mut current = self.inflight.load(Ordering::Acquire);
        loop {
            if current == usize::MAX || limit > 0 && current >= limit {
                return None;
            }
            match self.inflight.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(AccountSlot {
                        account: self.clone(),
                    })
                }
                Err(actual) => current = actual,
            }
        }
    }
    /// Non-blocking refresh serialization, safe to hold across an async network await.
    pub fn try_refresh_slot(self: &Arc<Self>) -> Option<RefreshSlot> {
        self.refreshing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(RefreshSlot {
            account: self.clone(),
        })
    }
}
fn deadline(s: &State, model: &str) -> f64 {
    [
        s.runtime.cooldown,
        s.runtime.balance,
        s.runtime.breaker,
        s.runtime.degrade,
        number(&s.doc["restrictedUntil"]).unwrap_or(0.0),
        *s.runtime.models.get(model).unwrap_or(&0.0),
        number(&s.doc["modelCaps"][model]["reset"]).unwrap_or(0.0),
    ]
    .into_iter()
    .fold(0.0, f64::max)
}
fn decay_soft(r: &mut Runtime, now: f64, window: f64) {
    if window > 0.0 && r.last_soft.is_some_and(|t| now - t > window) {
        r.soft = 0;
        r.unscoped = 0;
    }
    r.last_soft = Some(now);
}
pub fn backoff(count: u32, base: f64, cap: f64, offset: u32) -> f64 {
    (base * 2f64.powi(count.saturating_sub(offset).min(20) as i32)).min(cap)
}
fn redact(v: &mut Value, secrets: &[&str]) {
    match v {
        Value::String(s) => {
            for secret in secrets {
                if !secret.is_empty() {
                    *s = s.replace(secret, "[redacted]");
                }
            }
        }
        Value::Array(xs) => {
            for x in xs {
                redact(x, secrets);
            }
        }
        Value::Object(m) => {
            m.retain(|k, _| {
                !matches!(
                    k.to_ascii_lowercase().replace(['_', '-'], "").as_str(),
                    "accesstoken"
                        | "refreshtoken"
                        | "authorization"
                        | "password"
                        | "secret"
                        | "devicetoken"
                )
            });
            for x in m.values_mut() {
                redact(x, secrets);
            }
        }
        _ => {}
    }
}
#[derive(Debug)]
pub struct AccountSlot {
    account: Arc<Account>,
}
impl Drop for AccountSlot {
    fn drop(&mut self) {
        self.account.inflight.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Debug)]
pub struct RefreshSlot {
    account: Arc<Account>,
}
impl Drop for RefreshSlot {
    fn drop(&mut self) {
        self.account.refreshing.store(false, Ordering::Release);
    }
}

/// Accept flat exports, arrays, nested auth/account and cockpit snake_case rows.
/// Unlike the Python importer, active restrictions/caps are retained, not bypassed.
pub fn import_document(blob: &Value, realm: Option<Realm>) -> Result<Vec<Arc<Account>>> {
    let rows = if let Some(xs) = blob.as_array() {
        xs.clone()
    } else if let Some(xs) = blob["accounts"].as_array() {
        xs.clone()
    } else if blob.is_object() {
        vec![blob.clone()]
    } else {
        return Err(Error::Invalid("expected an account object or array".into()));
    };
    if rows.is_empty() {
        return Err(Error::Invalid("no accounts found".into()));
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        if !row.is_object() {
            return Err(Error::Invalid("account row must be an object".into()));
        }
        let mut d = row.clone();
        for layer in [&row["auth"], &row["account"]] {
            if let Some(map) = layer.as_object() {
                for (k, v) in map {
                    if d[k].is_null() || d[k] == "" {
                        d[k] = v.clone();
                    }
                }
            }
        }
        for (old, new) in [
            ("access_token", "accessToken"),
            ("refresh_token", "refreshToken"),
            ("expires_at", "expiresAt"),
        ] {
            if d[new].is_null() {
                d[new] = d[old].clone();
            }
        }
        if d["nickname"].is_null() {
            d["nickname"] = d["email"].clone();
        }
        let token = text(&d, "accessToken").trim().to_owned();
        if token.is_empty()
            || (!is_static_api_key(&token) && token.bytes().filter(|c| *c == b'.').count() != 2)
        {
            return Err(Error::Invalid(
                "accessToken must be a JWT or static API key".into(),
            ));
        }
        let detected = if is_static_api_key(&token) {
            Realm::Cn
        } else {
            realm
                .or_else(|| realm_evidence(&token, &text(&d, "domain")))
                .or_else(|| Realm::parse(&text(&d, "realm")))
                .unwrap_or(Realm::Intl)
        };
        d["realm"] = json!(detected.as_str());
        d["accessToken"] = json!(token);
        // Normalize domain only when known realm evidence conflicts.
        if realm_evidence("", &text(&d, "domain")).is_some_and(|r| r != detected) {
            d["domain"] = json!(default_domain(detected));
        }
        if d["source"].is_null() {
            d["source"] = json!(if row.get("access_token").is_some() {
                "cockpit"
            } else {
                "import"
            });
        }
        let a = Arc::new(Account::from_json(&d)?);
        if !seen.insert(a.uid().to_owned()) {
            return Err(Error::Invalid("duplicate uid in account import".into()));
        }
        out.push(a);
    }
    Ok(out)
}
pub fn export_document(
    accounts: &[Arc<Account>],
    realm: Option<Realm>,
    include_secrets: bool,
    uids: Option<&HashSet<String>>,
) -> Value {
    let rows: Vec<_> = accounts
        .iter()
        .filter(|a| {
            realm.is_none_or(|r| a.realm() == r) && uids.is_none_or(|ids| ids.contains(a.uid()))
        })
        .map(|a| {
            if include_secrets {
                a.to_json()
            } else {
                a.public_metadata()
            }
        })
        .collect();
    json!({"format":"workbuddy-accounts","version":1,"exportedAt":timeutil::local_stamp(timeutil::now()),"count":rows.len(),"accounts":rows})
}
