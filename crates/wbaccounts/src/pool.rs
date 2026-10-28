use crate::{account::number, import_document, Account, AccountSlot, Error, Readiness, Result};
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use wbcore::{ids, pyjson, timeutil, types::Realm};

const WINDOW_STATE_NAME: &str = "model_windows.json";
const MAX_WINDOW_BYTES: usize = 4 * 1024 * 1024;
const MAX_WINDOW_ROWS: usize = 10_000;
// Serialize merges across pools in this process. Atomic replacement also lets
// readers see a whole snapshot; concurrent external writers are not coordinated.
static WINDOW_WRITES: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug)]
pub struct PickOptions {
    pub realm: Option<Realm>,
    pub model: String,
    pub session_key: String,
    pub exclude: HashSet<String>,
    /// None = unrestricted; Some(empty) = allow no accounts (fail closed).
    pub allowed: Option<HashSet<String>>,
    pub page: Option<(usize, usize)>,
    pub now: f64,
}
impl Default for PickOptions {
    fn default() -> Self {
        Self {
            realm: None,
            model: String::new(),
            session_key: String::new(),
            exclude: HashSet::new(),
            allowed: None,
            page: None,
            now: timeutil::now(),
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum PoolError {
    Busy,
    NoUsableAccount,
    Restricted { retry_after: f64 },
}
impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str("every usable account is already serving a request"),
            Self::NoUsableAccount => f.write_str("no usable account"),
            Self::Restricted { retry_after } => {
                write!(f, "realm restricted; retry after {retry_after:.0}s")
            }
        }
    }
}
impl std::error::Error for PoolError {}
#[derive(Debug)]
pub struct AccountLease {
    pub account: Arc<Account>,
    _slot: AccountSlot,
}
impl AccountLease {
    pub fn account(&self) -> &Arc<Account> {
        &self.account
    }
}

#[derive(Debug)]
pub struct LoadIssue {
    pub path: PathBuf,
    pub message: String,
}
#[derive(Default)]
struct Selection {
    cursor: usize,
    pages: HashMap<(usize, usize), usize>,
    manual: HashMap<String, i64>,
    expiry: HashMap<String, i64>,
    remaining: HashMap<(String, String), i64>,
    remaining_state: HashMap<String, i64>,
    affinity: SessionAffinity,
    events: HashMap<Realm, HashMap<String, f64>>,
    holds: HashMap<Realm, f64>,
}
/// Mutate the existing pool (upsert/import/reload), never replace live Account Arcs.
/// Retired accounts remain indexed internally so delete/re-add cannot reset slots.
pub struct AccountPool {
    accounts: RwLock<Vec<Arc<Account>>>,
    retired: Mutex<HashMap<String, Arc<Account>>>,
    selection: Mutex<Selection>,
    mutation: Mutex<()>,
}
impl std::fmt::Debug for AccountPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountPool")
            .field("len", &self.accounts.read().len())
            .finish()
    }
}
impl AccountPool {
    pub fn new(accounts: Vec<Arc<Account>>) -> Self {
        let mut seen = HashSet::new();
        let accounts = accounts
            .into_iter()
            .filter(|a| seen.insert(a.uid().to_owned()))
            .collect();
        Self {
            accounts: RwLock::new(accounts),
            retired: Mutex::new(HashMap::new()),
            selection: Mutex::new(Selection::default()),
            mutation: Mutex::new(()),
        }
    }
    /// Sorted account files plus future persisted model windows. A corrupt window
    /// sidecar is reported separately and never prevents loading the account files.
    pub fn load_dir(dir: impl AsRef<Path>) -> Result<(Self, Vec<LoadIssue>)> {
        let dir = dir.as_ref();
        let (accounts, mut issues) = read_dir(dir)?;
        let pool = Self::new(accounts);
        pool.restore_windows_on_load(dir, &mut issues);
        Ok((pool, issues))
    }
    /// Restore Python's model_windows.json (v1 envelope, legacy map or bare list).
    /// Returns the number of extended model cooldowns; no error text is replayed.
    /// A missing file is harmless; malformed/oversized files return an error.
    pub fn restore_window_cooldowns(&self, dir: impl AsRef<Path>, now: f64) -> Result<usize> {
        if !now.is_finite() {
            return Err(Error::Invalid("invalid model window clock".into()));
        }
        let document = read_window_document(dir.as_ref())?;
        let accounts: HashMap<_, _> = self
            .accounts()
            .into_iter()
            .map(|a| (a.uid().to_owned(), a))
            .collect();
        let mut restored = 0;
        for row in window_rows(&document)? {
            let Some((uid, model, reset)) = window_fields(row, now) else {
                continue;
            };
            if let Some(account) = accounts.get(uid) {
                restored += account.arm_model_cooldown(model, reset, None, now) as usize;
            }
        }
        Ok(restored)
    }
    fn restore_windows_on_load(&self, dir: &Path, issues: &mut Vec<LoadIssue>) {
        if let Err(error) = self.restore_window_cooldowns(dir, timeutil::now()) {
            issues.push(LoadIssue {
                path: dir.join(WINDOW_STATE_NAME),
                message: format!("model window restore skipped: {error}"),
            });
        }
    }
    /// Record a new upstream 429 reset without issuing a probe or any network I/O.
    /// The gateway calls this for RateLimited.reset_at. Future rows are merged
    /// monotonically and atomically; expired/invalid rows are pruned. Unknown
    /// fields survive. Corrupt files and capacity limits fail rather than evicting
    /// live windows. Memory remains cooled even if persistence fails.
    /// Returns false for an unknown account, invalid reset, or an unchanged file.
    pub fn record_model_window(
        &self,
        dir: impl AsRef<Path>,
        uid: &str,
        model: &str,
        reset_at: f64,
        now: f64,
    ) -> Result<bool> {
        if uid.is_empty()
            || model.is_empty()
            || !reset_at.is_finite()
            || !now.is_finite()
            || reset_at <= now
        {
            return Ok(false);
        }
        let Some(account) = self.get(uid) else {
            return Ok(false);
        };
        account.arm_model_cooldown(model, reset_at, None, now);
        let dir = dir.as_ref();
        let _lock = WINDOW_WRITES.lock();
        let mut document = read_window_document(dir)?;
        let source_rows = window_rows(&document)?;
        let source_count = source_rows.len();
        let mut rows = BTreeMap::<(String, String), Value>::new();
        for row in source_rows {
            let Some((uid, model, reset)) = window_fields(row, now) else {
                continue;
            };
            let key = (uid.to_owned(), model.to_owned());
            if let Some(existing) = rows.get_mut(&key) {
                let end = number(&existing["reset_at"]).unwrap_or(0.0).max(reset);
                for (key, value) in row.as_object().unwrap() {
                    existing
                        .as_object_mut()
                        .unwrap()
                        .entry(key.clone())
                        .or_insert_with(|| value.clone());
                }
                existing["reset_at"] = json!(end);
            } else {
                rows.insert(key, row.clone());
            }
        }
        let mut changed = source_count != rows.len();
        let key = (uid.to_owned(), model.to_owned());
        if let Some(row) = rows.get_mut(&key) {
            let existing = number(&row["reset_at"]).unwrap_or(0.0);
            // An older on-disk deadline must not shorten this request, and a
            // shorter new request must not erase a later on-disk deadline.
            account.arm_model_cooldown(model, existing, None, now);
            if reset_at > existing {
                row["reset_at"] = json!(reset_at);
                row["attempts"] = json!(0);
                row["last_try"] = json!(0.0);
                row["last_result"] = json!("");
                changed = true;
            }
        } else {
            if rows.len() >= MAX_WINDOW_ROWS {
                return Err(Error::Invalid("model window row limit exceeded".into()));
            }
            rows.insert(
                key,
                json!({"uid":uid,"model":model,"reset_at":reset_at,
                "attempts":0,"last_try":0.0,"last_result":""}),
            );
            changed = true;
        }
        if !changed {
            return Ok(false);
        }
        if !document.is_object() {
            document = json!({});
        }
        if document.get("version").is_none() {
            document["version"] = json!(1);
        }
        document["saved_at"] = json!(timeutil::local_stamp(now));
        document["windows"] = Value::Array(rows.into_values().collect());
        write_window_document(dir, &document)?;
        Ok(true)
    }
    pub fn accounts(&self) -> Vec<Arc<Account>> {
        self.accounts.read().clone()
    }
    pub fn get(&self, uid: &str) -> Option<Arc<Account>> {
        self.accounts
            .read()
            .iter()
            .find(|a| a.uid() == uid)
            .cloned()
    }
    pub fn find_unique(&self, reference: &str) -> Result<Arc<Account>> {
        let snapshot = self.accounts();
        if let Some(a) = snapshot.iter().find(|a| a.uid() == reference) {
            return Ok(a.clone());
        }
        for matches in [
            snapshot
                .iter()
                .filter(|a| a.to_json()["nickname"].as_str() == Some(reference))
                .collect::<Vec<_>>(),
            snapshot
                .iter()
                .filter(|a| a.uid().starts_with(reference))
                .collect(),
        ] {
            match matches.len() {
                1 => return Ok(matches[0].clone()),
                n if n > 1 => return Err(Error::Invalid("account reference is ambiguous".into())),
                _ => {}
            }
        }
        Err(Error::Invalid("account reference was not found".into()))
    }
    /// Replace persisted data in place, retaining in-flight counts and penalties.
    pub fn upsert(&self, incoming: Arc<Account>, dir: Option<&Path>) -> Result<Arc<Account>> {
        let _lock = self.mutation.lock();
        self.upsert_inner(incoming, dir)
    }
    fn upsert_inner(&self, incoming: Arc<Account>, dir: Option<&Path>) -> Result<Arc<Account>> {
        let old = self
            .get(incoming.uid())
            .or_else(|| self.retired.lock().get(incoming.uid()).cloned());
        let account = if let Some(old) = old {
            old.replace_persisted(&incoming, dir)?;
            old
        } else {
            if let Some(dir) = dir {
                incoming.save_to(dir)?;
            } else {
                incoming.save()?;
            }
            incoming
        };
        if self.get(account.uid()).is_none() {
            self.accounts.write().push(account.clone());
        }
        self.retired.lock().remove(account.uid());
        Ok(account)
    }
    pub fn import(
        &self,
        blob: &Value,
        realm: Option<Realm>,
        dir: &Path,
    ) -> Result<Vec<Arc<Account>>> {
        let rows = import_document(blob, realm)?;
        let _lock = self.mutation.lock();
        // Validate all identities before any write; I/O failures may still partially commit.
        for incoming in &rows {
            if let Some(old) = self
                .get(incoming.uid())
                .or_else(|| self.retired.lock().get(incoming.uid()).cloned())
            {
                if old.realm() != incoming.realm() {
                    return Err(Error::Invalid(
                        "import would change an existing account realm".into(),
                    ));
                }
            }
        }
        rows.into_iter()
            .map(|a| self.upsert_inner(a, Some(dir)))
            .collect()
    }
    pub fn delete(&self, uid: &str, delete_file: bool) -> Result<bool> {
        let _lock = self.mutation.lock();
        let Some(account) = self.get(uid) else {
            return Ok(false);
        };
        if delete_file {
            if let Some(path) = account.path() {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        self.accounts.write().retain(|a| a.uid() != uid);
        self.retired.lock().insert(uid.into(), account);
        Ok(true)
    }
    /// Reload readable files, preserving Arc identity and runtime limits/slots.
    /// Missing/invalid files do not silently evict live accounts; explicit delete does.
    pub fn reload_dir(&self, dir: impl AsRef<Path>) -> Result<Vec<LoadIssue>> {
        let dir = dir.as_ref();
        let (rows, mut issues) = read_dir(dir)?;
        let _lock = self.mutation.lock();
        for row in rows {
            self.upsert_inner(row, None)?;
        }
        self.restore_windows_on_load(dir, &mut issues);
        Ok(issues)
    }
    pub fn count_ready(&self, options: &PickOptions) -> usize {
        self.accounts()
            .iter()
            .filter(|a| {
                eligible(a, options) && a.readiness(&options.model, options.now) == Readiness::Ready
            })
            .count()
    }
    pub fn refresh_candidates(&self, options: &PickOptions) -> Vec<Arc<Account>> {
        self.accounts()
            .into_iter()
            .filter(|a| {
                eligible(a, options)
                    && a.readiness(&options.model, options.now) == Readiness::NeedsRefresh
            })
            .collect()
    }
    pub fn apply_remaining_weights(&self, weights: HashMap<(String, String), i64>) {
        let mut s = self.selection.lock();
        s.remaining = weights.into_iter().filter(|(_, w)| *w > 1).collect();
        if s.remaining.is_empty() {
            s.remaining_state.clear();
        }
    }
    pub fn demote(&self, key: &str, uid: &str) {
        self.selection.lock().affinity.demote(key, Some(uid));
    }
    pub fn bind(&self, key: &str, uid: &str, now: f64) {
        self.selection.lock().affinity.bind(key, uid, now);
    }
    pub fn restriction_hold_remaining(&self, realm: Realm, now: f64) -> f64 {
        (self
            .selection
            .lock()
            .holds
            .get(&realm)
            .copied()
            .unwrap_or(0.0)
            - now)
            .max(0.0)
    }
    pub fn note_account_restriction(&self, realm: Realm, uid: &str, now: f64) {
        let mut s = self.selection.lock();
        let events = s.events.entry(realm).or_default();
        events.retain(|_, at| now - *at <= 900.0);
        events.insert(uid.into(), now);
        if events.len() >= 2 {
            let hold = s.holds.entry(realm).or_default();
            *hold = hold.max(now + 1800.0);
        }
    }
    pub fn pick(&self, options: &PickOptions) -> Option<Arc<Account>> {
        let mut group: Vec<_> = self
            .accounts()
            .into_iter()
            .filter(|a| {
                options.realm.is_none_or(|r| a.realm() == r)
                    && options
                        .allowed
                        .as_ref()
                        .is_none_or(|ids| ids.contains(a.uid()))
            })
            .collect();
        let mut s = self.selection.lock();
        group.retain(|a| {
            s.holds
                .get(&a.realm())
                .is_none_or(|until| *until <= options.now)
        });
        if group.is_empty() {
            return None;
        }
        let ready = |a: &Arc<Account>| {
            !options.exclude.contains(a.uid())
                && a.readiness(&options.model, options.now) == Readiness::Ready
        };
        if options.page.is_none() && !options.session_key.is_empty() {
            for uid in s.affinity.recent(&options.session_key, options.now) {
                if let Some(a) = group.iter().find(|a| a.uid() == uid && ready(a)) {
                    s.affinity.bind(&options.session_key, a.uid(), options.now);
                    return Some(a.clone());
                }
            }
        }
        let page_group = page_slice(&group, options.page);
        let chosen = pick_group(&page_group, options, &mut s).or_else(|| {
            if options.page.is_some() {
                let mut fallback = options.clone();
                fallback.page = None;
                pick_group(&group, &fallback, &mut s)
            } else {
                None
            }
        });
        if let Some(a) = &chosen {
            if options.page.is_none() && !options.session_key.is_empty() {
                s.affinity.bind(&options.session_key, a.uid(), options.now);
            }
        }
        chosen
    }
    pub fn acquire(
        &self,
        options: &PickOptions,
        limit: usize,
    ) -> std::result::Result<AccountLease, PoolError> {
        if let Some(realm) = options.realm {
            let wait = self.restriction_hold_remaining(realm, options.now);
            if wait > 0.0 {
                return Err(PoolError::Restricted { retry_after: wait });
            }
        }
        let mut pick = options.clone();
        let mut busy = false;
        while let Some(account) = self.pick(&pick) {
            if let Some(slot) = account.try_acquire_slot(limit) {
                return Ok(AccountLease {
                    account,
                    _slot: slot,
                });
            }
            busy = true;
            pick.exclude.insert(account.uid().into());
        }
        Err(if busy {
            PoolError::Busy
        } else {
            PoolError::NoUsableAccount
        })
    }
}
// Reject existing links/non-files. The account directory is operator-controlled;
// this is not a sandbox against an external process racing filesystem changes.
fn validate_window_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            Error::Invalid("model window state must be a regular file, not a link".into()),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
fn read_window_document(dir: &Path) -> Result<Value> {
    let path = dir.join(WINDOW_STATE_NAME);
    validate_window_path(&path)?;
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({"version":1,"windows":[]}));
        }
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take((MAX_WINDOW_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_WINDOW_BYTES {
        return Err(Error::Invalid("model window file limit exceeded".into()));
    }
    // Match wb_window_probe's utf-8-sig reader for hand-edited Windows files.
    Ok(serde_json::from_slice(
        bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes),
    )?)
}
fn window_rows(document: &Value) -> Result<Vec<&Value>> {
    let stored = if document.is_object() {
        &document["windows"]
    } else {
        document
    };
    let rows: Vec<_> = match stored {
        Value::Array(rows) => rows.iter().collect(),
        Value::Object(rows) => rows.values().collect(),
        _ => {
            return Err(Error::Invalid(
                "model windows must be an array or object".into(),
            ))
        }
    };
    if rows.len() > MAX_WINDOW_ROWS {
        return Err(Error::Invalid("model window row limit exceeded".into()));
    }
    Ok(rows)
}
fn window_fields(row: &Value, now: f64) -> Option<(&str, &str, f64)> {
    let uid = row["uid"].as_str().filter(|s| !s.is_empty())?;
    let model = row["model"].as_str().filter(|s| !s.is_empty())?;
    let reset = number(&row["reset_at"]).filter(|reset| *reset > now)?;
    Some((uid, model, reset))
}
fn write_window_document(dir: &Path, document: &Value) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(document)?;
    if bytes.len() > MAX_WINDOW_BYTES {
        return Err(Error::Invalid("model window file limit exceeded".into()));
    }
    fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".model_windows.{}.tmp", ids::uuid4_hex()));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        let path = dir.join(WINDOW_STATE_NAME);
        validate_window_path(&path)?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
fn eligible(a: &Account, o: &PickOptions) -> bool {
    o.realm.is_none_or(|r| a.realm() == r)
        && !o.exclude.contains(a.uid())
        && o.allowed.as_ref().is_none_or(|ids| ids.contains(a.uid()))
}
fn read_dir(dir: &Path) -> Result<(Vec<Arc<Account>>, Vec<LoadIssue>)> {
    if !dir.exists() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut paths = fs::read_dir(dir)?
        .filter_map(|r| r.ok().map(|r| r.path()))
        .filter(|p| {
            p.extension().is_some_and(|e| e == "json")
                && !matches!(
                    p.file_name().and_then(|s| s.to_str()),
                    Some(
                        "settings.json"
                            | "active_realm.json"
                            | "model_windows.json"
                            | "quota_scan.json"
                    )
                )
        })
        .collect::<Vec<_>>();
    paths.sort();
    let mut rows = Vec::new();
    let mut issues = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        match Account::load(&path) {
            Ok(a) if seen.insert(a.uid().to_owned()) => rows.push(Arc::new(a)),
            Ok(_) => issues.push(LoadIssue {
                path,
                message: "duplicate uid skipped".into(),
            }),
            Err(e) => issues.push(LoadIssue {
                path,
                message: e.to_string(),
            }),
        }
    }
    Ok((rows, issues))
}
fn page_slice(group: &[Arc<Account>], page: Option<(usize, usize)>) -> Vec<Arc<Account>> {
    let Some((index, size)) = page else {
        return group.to_vec();
    };
    if size <= 1 || size >= group.len() {
        return group.to_vec();
    }
    let mut uids = group.iter().map(|a| a.uid()).collect::<Vec<_>>();
    uids.sort();
    group
        .iter()
        .filter(|a| uids.binary_search(&a.uid()).unwrap_or(0) % size == index % size)
        .cloned()
        .collect()
}
fn pick_group(group: &[Arc<Account>], o: &PickOptions, s: &mut Selection) -> Option<Arc<Account>> {
    let ready: Vec<_> = group
        .iter()
        .filter(|a| {
            !o.exclude.contains(a.uid()) && a.readiness(&o.model, o.now) == Readiness::Ready
        })
        .cloned()
        .collect();
    let manual = ready
        .iter()
        .filter_map(|a| {
            let w = a.weight();
            (w > 1).then(|| (a.clone(), a.uid().to_owned(), w))
        })
        .collect::<Vec<_>>();
    if !manual.is_empty() {
        return smooth(&manual, &mut s.manual, true);
    }
    let expiry = ready
        .iter()
        .filter_map(|a| {
            a.expiring_weight()
                .map(|w| (a.clone(), a.uid().to_owned(), w))
        })
        .collect::<Vec<_>>();
    if !expiry.is_empty() {
        return smooth(&expiry, &mut s.expiry, true);
    }
    let remaining = ready
        .iter()
        .filter_map(|a| {
            s.remaining
                .get(&(a.uid().into(), o.model.clone()))
                .map(|w| (a.clone(), format!("{}\0{}", a.uid(), o.model), *w))
        })
        .collect::<Vec<_>>();
    if !o.model.is_empty() && !remaining.is_empty() {
        return smooth(&remaining, &mut s.remaining_state, false);
    }
    let start = o
        .page
        .and_then(|p| s.pages.get(&p).copied())
        .unwrap_or(s.cursor);
    for offset in 0..group.len() {
        let i = (start + offset) % group.len();
        if ready.iter().any(|a| a.uid() == group[i].uid()) {
            let next = (i + 1) % group.len();
            if let Some(p) = o.page {
                s.pages.insert(p, next);
            } else {
                s.cursor = next;
            }
            return Some(group[i].clone());
        }
    }
    None
}
fn smooth(
    entries: &[(Arc<Account>, String, i64)],
    state: &mut HashMap<String, i64>,
    prune: bool,
) -> Option<Arc<Account>> {
    if prune {
        state.retain(|key, _| entries.iter().any(|(_, k, _)| key == k));
    }
    let mut total = 0;
    let mut best = None;
    let mut score = i64::MIN;
    for (i, (_, key, weight)) in entries.iter().enumerate() {
        let current = state.entry(key.clone()).or_default();
        *current += weight;
        total += weight;
        if *current > score {
            score = *current;
            best = Some(i);
        }
    }
    let i = best?;
    *state.get_mut(&entries[i].1).unwrap() -= total;
    Some(entries[i].0.clone())
}

#[derive(Debug)]
pub struct SessionAffinity {
    bindings: HashMap<String, Vec<(String, f64)>>,
    pub ttl: f64,
    pub max_entries: usize,
    pub history: usize,
}
impl Default for SessionAffinity {
    fn default() -> Self {
        Self {
            bindings: HashMap::new(),
            ttl: 7200.0,
            max_entries: 5000,
            history: 3,
        }
    }
}
impl SessionAffinity {
    pub fn recent(&mut self, key: &str, now: f64) -> Vec<String> {
        self.bindings
            .get_mut(key)
            .map(|rows| {
                rows.retain(|(_, end)| *end > now);
                rows.iter().map(|(uid, _)| uid.clone()).collect()
            })
            .unwrap_or_default()
    }
    pub fn get(&mut self, key: &str, now: f64) -> Option<String> {
        let uid = self.recent(key, now).into_iter().next()?;
        self.bind(key, &uid, now);
        Some(uid)
    }
    pub fn bind(&mut self, key: &str, uid: &str, now: f64) {
        if key.is_empty() || self.max_entries == 0 || self.history == 0 {
            return;
        }
        if self.bindings.len() >= self.max_entries {
            self.bindings
                .retain(|_, v| v.first().is_some_and(|(_, end)| *end > now));
        }
        if self.bindings.len() >= self.max_entries && !self.bindings.contains_key(key) {
            if let Some(old) = self
                .bindings
                .iter()
                .min_by(|a, b| a.1[0].1.total_cmp(&b.1[0].1))
                .map(|(k, _)| k.clone())
            {
                self.bindings.remove(&old);
            }
        }
        let rows = self.bindings.entry(key.into()).or_default();
        rows.retain(|(u, end)| u != uid && *end > now);
        rows.insert(0, (uid.into(), now + self.ttl));
        rows.truncate(self.history);
    }
    pub fn demote(&mut self, key: &str, uid: Option<&str>) {
        if let Some(rows) = self.bindings.get_mut(key) {
            if let Some(uid) = uid {
                rows.retain(|(u, _)| u != uid);
            } else if !rows.is_empty() {
                rows.remove(0);
            }
            if rows.is_empty() {
                self.bindings.remove(key);
            }
        }
    }
    pub fn unbind(&mut self, key: &str) {
        self.bindings.remove(key);
    }
}
/// Python-compatible prefix affinity after upstream body normalization.
pub fn affinity_route(
    messages: &[Value],
    stable: bool,
    max_messages: usize,
    page_size: usize,
) -> (String, Option<(usize, usize)>) {
    if messages.is_empty() {
        return (String::new(), None);
    }
    let count = if stable && messages.len() > 2 { 3 } else { 2 };
    let digest = ids::sha256_hex(&pyjson::dumps_sorted_slice(
        &messages[..count.min(messages.len())],
    ));
    let key = format!("pfx-{}", &digest[..16]);
    if max_messages > 0 && messages.len() > max_messages {
        if page_size == 0 {
            return (String::new(), None);
        }
        if page_size > 1 {
            return (
                key,
                Some((
                    usize::from_str_radix(&digest[..8], 16).unwrap_or(0) % page_size,
                    page_size,
                )),
            );
        }
    }
    (key, None)
}
