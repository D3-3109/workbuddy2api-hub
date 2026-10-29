use crate::cli::Args;
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};
use tokio::sync::{watch, Semaphore};
use wbcore::{timeutil::now, types::Realm};

/// Process environment read once at startup. These switches are documented as
/// requiring a restart, so reading them per request bought nothing.
#[derive(Default, Clone)]
pub struct Env {
    pub background_model_target: Option<String>,
    pub background_model_names: Option<String>,
    pub default_reasoning_effort: Option<String>,
    pub desktop_chat_compat: bool,
    pub affinity_by_prefix: bool,
    pub cn_chat_upstream: Option<String>,
    pub identity_node_version: Option<String>,
    pub identity_os: Option<String>,
    pub identity_arch: Option<String>,
    pub enable_anthropic_messages: bool,
    pub trusted_proxies: Vec<String>,
}

impl Env {
    pub fn from_process() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        Self {
            background_model_target: var("WB_BACKGROUND_MODEL_TARGET"),
            background_model_names: var("WB_BACKGROUND_MODEL_NAMES"),
            default_reasoning_effort: var("WB_DEFAULT_REASONING_EFFORT"),
            desktop_chat_compat: var("WB_DESKTOP_CHAT_COMPAT").is_some_and(|v| v.trim() == "1"),
            affinity_by_prefix: var("WB_AFFINITY_BY_PREFIX").is_none_or(|s| s != "0"),
            cn_chat_upstream: var("WB_CN_CHAT_UPSTREAM"),
            identity_node_version: var("WB_IDENTITY_NODE_VERSION")
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty()),
            identity_os: var("WB_IDENTITY_OS"),
            identity_arch: var("WB_IDENTITY_ARCH"),
            enable_anthropic_messages: var("WB_ENABLE_ANTHROPIC_MESSAGES")
                .is_some_and(|s| matches!(s.as_str(), "1" | "true" | "yes" | "on")),
            trusted_proxies: var("WB_TRUSTED_PROXIES")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }
}

/// Catalog-derived guard inputs, rebuilt only when the catalog changes.
#[derive(Default)]
pub struct ModelSets {
    pub revision: Option<u64>,
    pub free: HashMap<Realm, Arc<HashSet<String>>>,
    pub pro: HashMap<Realm, Arc<HashSet<String>>>,
}

pub const LOG_CAPACITY: usize = 2000;

pub struct LogEntry {
    pub id: u64,
    pub time: f64,
    pub level: &'static str,
    pub tag: &'static str,
    pub msg: String,
}
impl LogEntry {
    pub fn to_json(&self) -> Value {
        // Same shape as the Python baseline: `ts` is the full local stamp,
        // `time` the HH:MM:SS the log view prints, `at` the epoch seconds.
        json!({"id":self.id,"ts":wbcore::timeutil::local_stamp(self.time),
            "time":wbcore::timeutil::fmt_local(self.time, "%H:%M:%S"),"at":self.time,
            "level":self.level,"tag":self.tag,"msg":self.msg})
    }
}

/// Fixed-capacity in-memory log; ids increase monotonically, also across clears.
#[derive(Default)]
pub struct LogRing {
    entries: VecDeque<LogEntry>,
    next_id: u64,
}
impl LogRing {
    pub fn push(&mut self, level: &'static str, tag: &'static str, msg: String) {
        if self.entries.len() >= LOG_CAPACITY {
            self.entries.pop_front();
        }
        self.next_id += 1;
        self.entries.push_back(LogEntry {
            id: self.next_id,
            time: now(),
            level,
            tag,
            msg,
        });
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn max_id(&self) -> u64 {
        self.entries.back().map_or(0, |e| e.id)
    }
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &LogEntry> {
        self.entries.iter()
    }
    /// Entries with `id > since`, found by binary search instead of a full scan.
    pub fn since(&self, since: u64) -> impl DoubleEndedIterator<Item = &LogEntry> {
        let start = self.entries.partition_point(|e| e.id <= since);
        self.entries.range(start..)
    }
}

fn level_name(level: &str) -> &'static str {
    match level {
        "ERROR" => "ERROR",
        "WARN" => "WARN",
        "DEBUG" => "DEBUG",
        _ => "INFO",
    }
}

pub struct App {
    pub args: Args,
    pub settings: wbsettings::Settings,
    pub pool: Arc<wbaccounts::AccountPool>,
    pub catalog: RwLock<wbcatalog::Catalog>,
    pub usage: Arc<wbusage::UsageStore>,
    pub http: wbhttp::Client,
    pub realm: RwLock<Realm>,
    pub extra_keys: Vec<String>,
    pub slots: Arc<Semaphore>,
    pub login_limit: wbsettings::LoginRateLimiter,
    pub refresh_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub restrictions: Mutex<HashMap<Realm, RestrictionWave>>,
    pub logs: Mutex<LogRing>,
    pub shutdown: watch::Sender<bool>,
    pub started: f64,
    pub env: Env,
    pub model_sets: Mutex<ModelSets>,
    /// Usage rows handed to the blocking pool and not yet written.
    pub pending_writes: std::sync::atomic::AtomicUsize,
}

#[derive(Default)]
pub struct RestrictionWave {
    pub events: HashMap<String, f64>,
    pub until: f64,
}

impl App {
    pub fn new(
        args: Args,
        settings: wbsettings::Settings,
        pool: wbaccounts::AccountPool,
        usage: wbusage::UsageStore,
        http: wbhttp::Client,
        realm: Realm,
        extra_keys: Vec<String>,
    ) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(args.max_concurrent_chat)),
            args,
            settings,
            pool: Arc::new(pool),
            catalog: RwLock::new(wbcatalog::Catalog::bundled()),
            usage: Arc::new(usage),
            http,
            realm: RwLock::new(realm),
            extra_keys,
            login_limit: wbsettings::LoginRateLimiter::new(),
            refresh_locks: Mutex::new(HashMap::new()),
            restrictions: Mutex::new(HashMap::new()),
            logs: Mutex::new(LogRing::default()),
            shutdown: watch::channel(false).0,
            started: now(),
            env: Env::from_process(),
            model_sets: Mutex::new(ModelSets::default()),
            pending_writes: Default::default(),
        }
    }

    pub fn panel_valid(&self, headers: &hyper::HeaderMap) -> bool {
        self.settings
            .sessions()
            .valid(crate::wire::header_text(headers, "x-panel-token").unwrap_or(""))
    }
    pub fn api_auth_required(&self) -> bool {
        !(self.args.host.is_loopback()
            && self.settings.current()["auth_disabled"] == json!(true)
            && self.extra_keys.is_empty())
    }
    pub fn authenticate(
        &self,
        headers: &hyper::HeaderMap,
        q: &crate::wire::Query,
    ) -> Result<Option<Value>, ()> {
        if self.panel_valid(headers) {
            return Ok(None);
        }
        let key = crate::wire::supplied_key(headers, q);
        if let Some(entry) = self.settings.match_key(&key, &self.extra_keys) {
            return Ok(Some(entry));
        }
        // Revoking the last key must never turn a reverse-proxied loopback
        // service into an anonymous API. The panel can recover key configuration.
        if !self.api_auth_required() {
            return Ok(None);
        }
        Err(())
    }
    pub fn request_realm(
        &self,
        headers: &hyper::HeaderMap,
        q: &crate::wire::Query,
        key: Option<&Value>,
    ) -> Result<Realm, ()> {
        let key_realm = key
            .and_then(|v| v["realm"].as_str())
            .filter(|s| !s.is_empty());
        let r = key_realm
            .or_else(|| crate::wire::header_text(headers, "x-realm"))
            .or_else(|| q.get("realm").map(String::as_str));
        match r {
            Some("all") | None | Some("") => Ok(*self.realm.read()),
            Some(r) => Realm::parse(r).ok_or(()),
        }
    }
    pub fn refresh_lock(&self, uid: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.refresh_locks
            .lock()
            .entry(uid.to_string())
            .or_default()
            .clone()
    }
    pub fn restricted(&self, realm: Realm, uid: &str) {
        let n = now();
        let mut map = self.restrictions.lock();
        let wave = map.entry(realm).or_default();
        wave.events.retain(|_, t| n - *t <= 900.0);
        wave.events.insert(uid.to_string(), n);
        if wave.events.len() >= 2 {
            wave.until = wave.until.max(n + 1800.0);
        }
    }
    pub fn restriction_wait(&self, realm: Realm) -> u64 {
        self.restrictions
            .lock()
            .get(&realm)
            .map(|x| (x.until - now()).max(0.0).ceil() as u64)
            .unwrap_or(0)
    }
    pub fn log(&self, level: &str, message: &str) {
        self.log_tagged(level, "gateway", message);
    }
    pub fn log_tagged(&self, level: &str, tag: &'static str, message: &str) {
        let level = level_name(level);
        self.logs.lock().push(level, tag, message.to_owned());
        if let Some(dropped) = crate::log_channel::emit(level, message) {
            if dropped == 1 || dropped % 1000 == 0 {
                self.logs.lock().push(
                    "WARN",
                    "system",
                    format!("stderr log queue is full; {dropped} lines dropped"),
                );
            }
        }
    }
    pub fn metadata(&self) -> Value {
        json!({"realm":self.realm.read().as_str(),"started":self.started,"accounts":self.pool.accounts().iter().map(|a|a.public_metadata()).collect::<Vec<_>>(),"api_keys":self.settings.view()["api_keys"]})
    }
    pub fn rewrite_test_upstream(&self, request: &mut wbcore::types::OutRequest) {
        if let Some(base) = &self.args.test_upstream {
            if let Ok(original) = url::Url::parse(&request.url) {
                request.url = format!(
                    "{}{}{}",
                    base.trim_end_matches('/'),
                    original.path(),
                    original
                        .query()
                        .map(|q| format!("?{q}"))
                        .unwrap_or_default()
                );
                request.egress = wbcore::types::Egress::Direct;
            }
        }
    }
}
