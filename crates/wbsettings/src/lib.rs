//! Synchronous, network-free settings store for the gateway.
//!
//! `Settings::load` takes a settings.json path (an existing directory is also
//! accepted). Clones share sessions and concurrency counters. All writes are
//! serialized in-process, read the latest file, validate before writing, and
//! atomically replace it. No cross-process locking is provided.
//!
//! `snapshot`, `keys`, and `proxy_slots` are INTERNAL APIs and contain secrets.
//! Only `view` and the result of `patch` are suitable for GET/POST /settings.
//! HTTP authorization, scheduling side effects, proxy probing, and account
//! binding cleanup belong to the gateway, not this crate.

mod keys;
mod proxies;
mod schema;
mod security;

pub use keys::{key_allows_account, key_allows_model, mask_key, KeySlot};
pub use schema::{deep_merge, defaults, LIMIT_KEYS};
pub use security::{
    login_rate_limit_key, LoginRateLimiter, PanelSessions, DEFAULT_PANEL_PASSWORD,
    LOGIN_FAILURE_DELAY, PBKDF2_ROUNDS, SESSION_TTL,
};

use parking_lot::Mutex;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};
use wbcore::{ids, types::Realm};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Json(serde_json::Error),
    Validation(String),
    NotFound(String),
    RateLimited {
        key_id: String,
        limit: u64,
        in_flight: u64,
    },
}
impl Error {
    pub fn is_validation(&self) -> bool {
        matches!(self, Self::Validation(_))
    }
    pub fn is_rate_limited(&self) -> bool {
        matches!(self, Self::RateLimited { .. })
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "settings I/O: {e}"),
            Self::Json(e) => write!(f, "settings JSON: {e}"),
            Self::Validation(e) | Self::NotFound(e) => f.write_str(e),
            Self::RateLimited { key_id, limit, in_flight } => write!(f,
                "Rate limit reached: API key '{key_id}' reached its concurrency limit ({in_flight} in flight, limit {limit})"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Json(e) => Some(e),
            _ => None,
        }
    }
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error::Validation(message.into())
}

// Independent Settings instances must not race their read/modify/write cycles.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// File identity used to detect external edits without rereading the file.
/// Atomic replacement changes the inode on Unix; size/mtime cover other edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    inode: (u64, u64),
}

fn stamp(path: &Path) -> io::Result<Option<Stamp>> {
    match fs::metadata(path) {
        Ok(meta) => Ok(Some(Stamp {
            len: meta.len(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            inode: {
                use std::os::unix::fs::MetadataExt;
                (meta.dev(), meta.ino())
            },
        })),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Parsed, migrated and resolved file contents. Immutable once published.
struct Cached {
    stamp: Option<Stamp>,
    raw: Value,
    resolved: Arc<Value>,
    keys: Arc<Vec<Value>>,
    proxies: Arc<Vec<Value>>,
}
impl Cached {
    fn new(stamp: Option<Stamp>, raw: Value) -> Self {
        Self {
            stamp,
            resolved: Arc::new(schema::resolve(&raw)),
            keys: Arc::new(keys::read_all(&raw)),
            proxies: Arc::new(proxies::read_all(&raw)),
            raw,
        }
    }
}

struct Inner {
    path: PathBuf,
    cache: Mutex<Arc<Cached>>,
    sessions: PanelSessions,
    key_slots: Arc<Mutex<HashMap<String, u64>>>,
}
#[derive(Clone)]
pub struct Settings {
    inner: Arc<Inner>,
}

impl Settings {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let path = if path.is_dir() {
            path.join("settings.json")
        } else {
            path.to_owned()
        };
        let _lock = FILE_LOCK.lock();
        let before = stamp(&path).ok().flatten();
        let mut data = read_file(&path)?;
        let mut current = before;
        if schema::migrate(&mut data) {
            // Read-only stores still resolve legacy values; the next write retries.
            if atomic_write(&path, &data).is_ok() {
                current = stamp(&path).ok().flatten();
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                path,
                cache: Mutex::new(Arc::new(Cached::new(current, data))),
                sessions: PanelSessions::new(),
                key_slots: Arc::new(Mutex::new(HashMap::new())),
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Current contents. One `stat` per call; the file is reread and reparsed
    /// only after it changed. As before, an unreadable file keeps the last good
    /// contents and a malformed one resolves to defaults.
    fn state(&self) -> Arc<Cached> {
        let current = match stamp(self.path()) {
            Ok(current) => current,
            Err(_) => return self.inner.cache.lock().clone(),
        };
        {
            let cache = self.inner.cache.lock();
            if cache.stamp == current {
                return cache.clone();
            }
        }
        let _lock = FILE_LOCK.lock();
        // Stamp before reading: a write racing this read leaves a newer stamp
        // on disk, so the next call rereads rather than serving stale data.
        let current = stamp(self.path()).ok().flatten();
        let mut cache = self.inner.cache.lock();
        if cache.stamp == current {
            return cache.clone();
        }
        if let Ok(mut data) = read_file(self.path()) {
            schema::migrate(&mut data);
            *cache = Arc::new(Cached::new(current, data));
        }
        cache.clone()
    }

    fn transaction<T>(&self, edit: impl FnOnce(&mut Value) -> Result<T>) -> Result<T> {
        let _lock = FILE_LOCK.lock();
        let mut data = read_file(self.path())?;
        schema::migrate(&mut data);
        let result = edit(&mut data)?;
        atomic_write(self.path(), &data)?;
        let written = stamp(self.path()).ok().flatten();
        *self.inner.cache.lock() = Arc::new(Cached::new(written, data));
        Ok(result)
    }

    /// Shared resolved configuration; cheap enough for every request. Contains secrets.
    pub fn current(&self) -> Arc<Value> {
        self.state().resolved.clone()
    }

    /// Resolved configuration with defaults and unknown fields. Contains secrets.
    pub fn snapshot(&self) -> Value {
        (*self.current()).clone()
    }

    /// Atomic partial update; validation failure leaves the entire file unchanged.
    /// Unknown stored keys survive; unknown submitted top-level keys are ignored.
    /// `deleted_api_key_ids` (even []) selects upsert instead of replace semantics.
    pub fn patch(&self, patch: Value) -> Result<Value> {
        let object = patch
            .as_object()
            .ok_or_else(|| invalid("settings patch must be an object"))?;
        self.transaction(|data| {
            if object.contains_key("identity_versions")
                && !security::verify_password(
                    data,
                    object
                        .get("panel_password")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                )
            {
                return Err(invalid(
                    "current panel password is required for identity_versions",
                ));
            }
            schema::apply_patch(data, object)?;
            if object.contains_key("api_keys") || object.contains_key("deleted_api_key_ids") {
                keys::apply_patch(data, object)?;
            }
            Ok(view_of(data))
        })
    }

    /// Explicit allowlist: never serializes arbitrary stored extension fields.
    pub fn view(&self) -> Value {
        view_of(&self.state().raw)
    }
    pub fn verify_password(&self, password: &str) -> bool {
        security::verify_password(&self.state().raw, password)
    }
    pub fn password_is_default(&self) -> bool {
        security::password_is_default(&self.state().raw)
    }
    /// Non-default password changes invalidate this store's shared sessions.
    pub fn set_password(&self, password: &str) -> Result<()> {
        if password.chars().count() < 4 {
            return Err(invalid("password must contain at least 4 characters"));
        }
        self.transaction(|data| {
            security::set_password(data, password);
            Ok(())
        })?;
        if password != DEFAULT_PANEL_PASSWORD {
            self.sessions().revoke_all();
        }
        Ok(())
    }
    pub fn sessions(&self) -> &PanelSessions {
        &self.inner.sessions
    }

    /// Live entries, including disabled entries; contains plaintext secrets.
    pub fn keys(&self) -> Vec<Value> {
        self.state()
            .keys
            .iter()
            .filter(|e| e["deleted_at"] == "")
            .cloned()
            .collect()
    }
    pub fn reveal_key(&self, id: &str) -> Option<String> {
        self.keys()
            .iter()
            .find(|e| e["id"].as_str() == Some(id))
            .map(|e| text(e, "key").to_owned())
    }
    /// Launcher keys apply only when there are no live panel keys, disabled or not.
    pub fn match_key(&self, supplied: &str, extra_keys: &[String]) -> Option<Value> {
        let supplied = supplied.trim();
        if supplied.is_empty() {
            return None;
        }
        let state = self.state();
        let mut live = state
            .keys
            .iter()
            .filter(|e| e["deleted_at"] == "")
            .peekable();
        let none_live = live.peek().is_none();
        for entry in live {
            if entry["enabled"] == true
                && security::constant_time_eq(supplied.as_bytes(), text(entry, "key").as_bytes())
            {
                let mut entry = entry.clone();
                entry["source"] = json!("panel");
                return Some(entry);
            }
        }
        if none_live {
            for key in extra_keys {
                let key = key.trim();
                if !key.is_empty()
                    && security::constant_time_eq(supplied.as_bytes(), key.as_bytes())
                {
                    return Some(json!({"id":"launcher", "name":"启动参数", "key":key,
                        "realm":"", "models":[], "accounts":[], "enabled":true,
                        "concurrency_limit":0, "source":"launcher"}));
                }
            }
        }
        None
    }
    pub fn auth_required(&self, extra_keys: &[String]) -> bool {
        let state = self.state();
        state.raw["auth_disabled"] != true
            && (state
                .keys
                .iter()
                .any(|e| e["enabled"] == true && e["deleted_at"] == "")
                || extra_keys.iter().any(|k| !k.trim().is_empty()))
    }
    pub fn ensure_launcher_key(&self) -> Result<(String, bool)> {
        self.transaction(|data| {
            let key = text(data, "launcher_key").trim();
            if !key.is_empty() {
                return Ok((key.to_owned(), false));
            }
            let key = format!("wb-{}", ids::token_urlsafe(24));
            data["launcher_key"] = json!(key);
            Ok((key, true))
        })
    }
    /// Own this guard until the request/stream completes; Drop always releases.
    pub fn try_acquire_key(&self, entry: &Value) -> Result<KeySlot> {
        keys::acquire(self.inner.key_slots.clone(), entry)
    }
    pub fn key_inflight(&self, id: &str) -> u64 {
        *self.inner.key_slots.lock().get(id).unwrap_or(&0)
    }
    pub fn limits(&self) -> Value {
        self.current()["limits"].clone()
    }
    pub fn limit(&self, key: &str, realm: Realm) -> u64 {
        let current = self.current();
        let limits = &current["limits"];
        limits[key][realm.as_str()]
            .as_u64()
            .or_else(|| limits[key]["global"].as_u64())
            .unwrap_or(0)
    }
    /// Internal egress definitions; proxy URLs may contain credentials.
    pub fn proxy_slots(&self) -> Vec<Value> {
        self.state().proxies.as_ref().clone()
    }
    /// Shared proxy slot list for per-request egress resolution.
    pub fn proxy_slots_shared(&self) -> Arc<Vec<Value>> {
        self.state().proxies.clone()
    }
    pub fn find_proxy_slot(&self, id: &str) -> Option<Value> {
        self.proxy_slots().into_iter().find(|s| s["id"] == id)
    }
    pub fn save_proxy_slots(&self, slots: Value) -> Result<Vec<Value>> {
        self.transaction(|data| proxies::save(data, &slots))
    }
    /// Merge probe fields under the same lock as panel writes. Missing slot -> NotFound.
    pub fn update_proxy_slot(&self, id: &str, fields: Value) -> Result<Value> {
        self.transaction(|data| proxies::update(data, id, &fields))
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn view_of(raw: &Value) -> Value {
    let snapshot = schema::resolve(raw);
    let mut view = schema::public_config(&snapshot);
    let all = keys::read_all(raw);
    let live: Vec<_> = all.iter().filter(|e| e["deleted_at"] == "").collect();
    view["api_keys"] = Value::Array(
        live.iter()
            .map(|e| {
                let mut safe = json!({});
                for key in [
                    "id",
                    "name",
                    "realm",
                    "models",
                    "accounts",
                    "enabled",
                    "created_at",
                    "concurrency_limit",
                ] {
                    safe[key] = e[key].clone();
                }
                safe["masked"] = json!(mask_key(text(e, "key")));
                safe["source"] = json!("panel");
                safe
            })
            .collect(),
    );
    view["deleted_api_keys"] = Value::Array(
        all.iter()
            .filter(|e| e["deleted_at"] != "")
            .map(|e| {
                let mut safe = json!({});
                for key in ["id", "name", "realm", "created_at", "deleted_at"] {
                    safe[key] = e[key].clone();
                }
                safe
            })
            .collect(),
    );
    let legacy = text(raw, "api_key");
    view["api_key_set"] = json!(raw["api_key_set"] == true || !live.is_empty());
    view["api_key_set_by_panel"] = view["api_key_set"].clone();
    view["api_key_masked"] = json!(mask_key(legacy));
    view["auth_required"] =
        json!(raw["auth_disabled"] != true && live.iter().any(|e| e["enabled"] == true));
    view["panel_password_is_default"] = json!(security::password_is_default(raw));
    view["version"] = json!(wbcore::VERSION);
    for key in LIMIT_KEYS {
        view[key] = snapshot["limits"][key]["global"].clone();
    }
    for (key, value) in [
        ("daily_chat_web_concurrency_max", 10),
        ("daily_chat_web_concurrency_default", 1),
        ("account_max_concurrent_max", 10),
        ("daily_chat_web_attempts_default", 3),
        ("daily_chat_web_attempts_max", 5),
        ("daily_chat_web_timeout_default", 120),
        ("daily_chat_web_timeout_max", 3600),
        ("daily_chat_web_retry_backoff_default", 5),
        ("daily_chat_web_retry_backoff_max", 60),
        ("keep_backups_max", 3),
    ] {
        view[key] = json!(value);
    }
    view["daily_chat_model_default"] = json!("deepseek-v4.1-flash");
    view
}

fn read_file(path: &Path) -> Result<Value> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice::<Value>(&bytes)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e.into()),
    }
}

fn atomic_write(path: &Path, value: &Value) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("settings path needs a filename"))?
        .to_string_lossy();
    let temporary = parent.join(format!(".{name}.{}.tmp", ids::token_hex(12)));
    let result = (|| -> Result<()> {
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
mod tests;
