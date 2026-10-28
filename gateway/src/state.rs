use crate::cli::Args;
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};
use tokio::sync::{watch, Semaphore};
use wbcore::{timeutil::now, types::Realm};

pub struct App {
    pub args: Args,
    pub settings: wbsettings::Settings,
    pub pool: Arc<wbaccounts::AccountPool>,
    pub catalog: RwLock<wbcatalog::Catalog>,
    pub usage: wbusage::UsageStore,
    pub http: wbhttp::Client,
    pub realm: RwLock<Realm>,
    pub extra_keys: Vec<String>,
    pub slots: Arc<Semaphore>,
    pub login_limit: wbsettings::LoginRateLimiter,
    pub refresh_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub restrictions: Mutex<HashMap<Realm, RestrictionWave>>,
    pub logs: Mutex<VecDeque<Value>>,
    pub shutdown: watch::Sender<bool>,
    pub started: f64,
}

#[derive(Default)]
pub struct RestrictionWave {
    pub events: HashMap<String, f64>,
    pub until: f64,
}

impl App {
    pub fn panel_valid(&self, headers: &hyper::HeaderMap) -> bool {
        self.settings
            .sessions()
            .valid(crate::wire::header_text(headers, "x-panel-token").unwrap_or(""))
    }
    pub fn api_auth_required(&self) -> bool {
        !(self.args.host.is_loopback()
            && self.settings.snapshot()["auth_disabled"] == json!(true)
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
        let mut logs = self.logs.lock();
        let id = logs.back().and_then(|v| v["id"].as_u64()).unwrap_or(0) + 1;
        if logs.len() >= 2000 {
            logs.pop_front();
        }
        logs.push_back(json!({"id":id,"ts":wbcore::timeutil::local_stamp(now()),"time":now(),"level":level,"tag":"gateway","msg":message}));
        drop(logs);
        if let Some(dropped) = crate::log_channel::emit(level, message) {
            if dropped == 1 || dropped % 1000 == 0 {
                let mut logs = self.logs.lock();
                let id = logs.back().and_then(|v| v["id"].as_u64()).unwrap_or(0) + 1;
                if logs.len() >= 2000 {
                    logs.pop_front();
                }
                logs.push_back(json!({"id":id,"ts":wbcore::timeutil::local_stamp(now()),"time":now(),"level":"WARN","tag":"system","msg":format!("stderr log queue is full; {dropped} lines dropped")}));
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
