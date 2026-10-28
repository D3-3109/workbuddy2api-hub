//! File-only usage accounting and historical price resolution.
//!
//! `UsageStore::new(usage_dir)` does not touch disk. `append` writes one terminal
//! request row, preserving caller fields and Python JSON spacing (with `at` first).
//! `query(path, params, metadata)` supports usage totals/recent/perf/timeseries/
//! analytics/by-account and pricing status. Params are a JSON object; string,
//! numeric and first-element-of-array values are accepted. Metadata is
//! `{accounts: [{uid,nickname,realm,domain,credits}], api_keys: [...], realm:
//! "intl", account: {...}}`; `accounts` may also be a uid-keyed object. The
//! optional `account` is the representative credential summary, never a secret.
//! The gateway owns authentication, HTTP envelopes, and display-name redaction.
//!
//! Queries scan incrementally from disk without retaining the log; daily totals
//! additionally cache a verified cursor. Partial final lines are deferred, malformed
//! lines skipped. Lines over 1 MiB are skipped to bound memory. No network, worker
//! threads, persistent aggregate caches, or background pricing refresh are used.

mod analytics;
mod pricing;
mod scan;
mod views;

pub use pricing::policy_id;

use parking_lot::{Mutex, RwLock};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use wbcore::{pyjson, pyval, timeutil};

/// A usage directory. Safe to share via `Arc`; file writes are serialized within
/// this instance. Share one instance per directory (no cross-process transaction).
pub struct UsageStore {
    dir: PathBuf,
    started: f64,
    io_lock: Mutex<()>,
    daily: Mutex<DailyState>,
    enabled: AtomicBool,
    pricing_file: RwLock<Option<PathBuf>>,
}

#[derive(Default)]
struct DailyState {
    day: String,
    cursor: scan::Cursor,
    totals: Value,
}

impl UsageStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let dir = if dir.is_absolute() {
            dir
        } else {
            std::env::current_dir().unwrap_or_default().join(dir)
        };
        Self {
            dir,
            started: timeutil::now(),
            io_lock: Mutex::new(()),
            daily: Mutex::new(DailyState::default()),
            enabled: AtomicBool::new(true),
            pricing_file: RwLock::new(std::env::var_os("WB_PRICING_FILE").map(PathBuf::from)),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.dir
    }
    pub fn set_pricing_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }
    /// Set a factory snapshot file. Invalid/missing snapshots use the embedded one.
    pub fn set_pricing_file(&self, path: impl Into<PathBuf>) {
        *self.pricing_file.write() = Some(path.into());
    }

    fn pricing(&self) -> io::Result<pricing::Pricing> {
        pricing::Pricing::load(
            &self.dir,
            self.pricing_file.read().as_deref(),
            self.enabled.load(Ordering::Relaxed),
        )
    }

    /// Append exactly one object; missing `at`/`iso` are filled, and successful
    /// rows without `cost_policy` receive the current assignment (or null).
    /// Existing fields, including legacy absence of `key`, are otherwise retained.
    pub fn append(&self, row: Value) -> io::Result<()> {
        let _guard = self.io_lock.lock();
        let object = row
            .as_object()
            .ok_or_else(|| invalid("usage row must be an object"))?;
        let at = object
            .get("at")
            .cloned()
            .unwrap_or_else(|| json!(timeutil::now()));
        let mut out = Map::new();
        out.insert("at".into(), at.clone());
        for (k, v) in object {
            if k != "at" {
                out.insert(k.clone(), v.clone());
            }
        }
        out.entry("iso")
            .or_insert_with(|| json!(timeutil::local_iso(num(&at))));
        if !truth(&out.get("error").cloned().unwrap_or(Value::Null))
            && !out.contains_key("cost_policy")
        {
            let model = out.get("model").and_then(Value::as_str).unwrap_or("");
            out.insert(
                "cost_policy".into(),
                self.pricing()?
                    .current_id(model)
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            );
        }
        fs::create_dir_all(&self.dir)?;
        append_json(&self.dir.join("usage.jsonl"), &Value::Object(out))
    }

    /// Query JSON payloads. Unknown or postponed routes return `Unsupported`;
    /// I/O failures remain errors, never fabricated zero counts.
    pub fn query(&self, path: &str, params: &Value, metadata: &Value) -> io::Result<Value> {
        let _guard = self.io_lock.lock();
        let context = Context::new(params, metadata);
        let log = self.dir.join("usage.jsonl");
        match path.trim_end_matches('/') {
            "/usage" | "/v1/usage" => views::totals(&log, &context, &self.pricing()?, self.started),
            "/usage/recent" => views::recent(&log, &context, &self.pricing()?),
            "/usage/perf" => views::perf(&log, &context),
            "/usage/timeseries" => views::timeseries(&log, &context),
            "/usage/analytics" => analytics::query(&log, &context, &self.pricing()?),
            "/usage/by-account" => views::by_account(&log),
            "/pricing" => Ok(self.pricing()?.status(&self.dir)),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("unsupported usage route: {path}"),
            )),
        }
    }

    pub fn cost_for_row(&self, row: &Value) -> io::Result<Value> {
        Ok(self.pricing()?.cost(row))
    }
    pub fn current_policy_id(&self, model: &str) -> io::Result<Option<String>> {
        Ok(self.pricing()?.current_id(model))
    }

    /// Persist a caller-provided pricing snapshot; no fetch is performed. Returns
    /// `{new_policies,timeline_changed,assignment}`. `merge` preserves assignments
    /// absent from this snapshot. Disabled pricing refuses policy creation.
    pub fn record_policies(&self, doc: &Value, at: f64, merge: bool) -> io::Result<Value> {
        let _guard = self.io_lock.lock();
        if !self.enabled.load(Ordering::Relaxed) {
            return Err(invalid("pricing is disabled"));
        }
        self.pricing()?.record(&self.dir, doc, at, merge)
    }

    /// Explicitly postponed: remote refresh belongs to the gateway's HTTP layer.
    pub fn refresh_pricing(&self) -> io::Result<Value> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "remote pricing refresh is not implemented; supply a snapshot to record_policies",
        ))
    }

    /// Daily guard counters, including failed rows with partial usage, excluding
    /// client aborts. Missing file is an empty history; unreadable file is an error.
    pub fn daily_totals(&self) -> io::Result<Value> {
        self.daily_totals_at(timeutil::now())
    }

    /// Deterministic-clock variant useful for daily guards and tests.
    pub fn daily_totals_at(&self, now: f64) -> io::Result<Value> {
        let _guard = self.io_lock.lock();
        let mut state = self.daily.lock();
        let path = self.dir.join("usage.jsonl");
        let day = timeutil::local_day_key(now);
        if state.day != day || !state.cursor.matches(&path)? {
            *state = DailyState {
                day,
                totals: json!({"tokens":{},"credits":{},"models":{}}),
                ..Default::default()
            };
        }
        let lo = timeutil::local_midnight(now, 0);
        let offset = state.cursor.offset;
        // Fold transactionally: a read failure must not leave counters advanced
        // while retaining the old cursor (which would double-count on retry).
        let mut totals = state.totals.clone();
        let end = scan::scan_from(&path, offset, |row| {
            let Some(row) = row else { return };
            let account = text(&row["account"]);
            if account.is_empty() || outcome(&row) == "client_aborted" || number(&row, "at") < lo {
                return;
            }
            let model = text(&row["model"]);
            let tokens = number(&row, "total_tokens");
            bump(&mut totals["tokens"], &account, tokens);
            if number(&row, "credit") != 0.0 {
                if totals["credits"][&account].is_null() {
                    totals["credits"][&account] = json!(0.0);
                }
                bump(&mut totals["credits"], &account, number(&row, "credit"));
            }
            if !model.is_empty() {
                if !totals["models"][&account].is_object() {
                    totals["models"][&account] = json!({});
                }
                bump(&mut totals["models"][&account], &model, tokens);
            }
        })?;
        let cursor = scan::Cursor::capture(&path, end)?;
        state.cursor = cursor;
        state.totals = totals;
        Ok(state.totals.clone())
    }
}

pub(crate) fn append_json(path: &Path, row: &Value) -> io::Result<()> {
    let mut bytes = pyjson::dumps_u(row).into_bytes();
    bytes.push(b'\n');
    if bytes.len() > scan::MAX_LINE {
        return Err(invalid("JSONL row exceeds 1 MiB"));
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&bytes)?;
    file.flush()
}

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
pub(crate) fn num(v: &Value) -> f64 {
    pyval::py_float(v).filter(|v| v.is_finite()).unwrap_or(0.0)
}
pub(crate) fn number(v: &Value, k: &str) -> f64 {
    num(&v[k])
}
pub(crate) fn truth(v: &Value) -> bool {
    pyval::truthy(v)
}
pub(crate) fn text(v: &Value) -> String {
    if truth(v) {
        pyval::py_str(v)
    } else {
        String::new()
    }
}
pub(crate) fn string_or(v: &Value, default: &str) -> String {
    let s = text(v);
    if s.is_empty() {
        default.into()
    } else {
        s
    }
}
pub(crate) fn outcome(v: &Value) -> String {
    string_or(
        &v["outcome"],
        if truth(&v["error"]) {
            "failed"
        } else {
            "completed"
        },
    )
}
pub(crate) fn bump(v: &mut Value, key: &str, by: f64) {
    let sum = number(v, key) + by;
    let float = v[key].as_number().map(|n| n.is_f64()).unwrap_or(false);
    v[key] = if !float && sum.fract() == 0.0 && sum.abs() < i64::MAX as f64 {
        json!(sum as i64)
    } else {
        json!(sum)
    };
}
pub(crate) const TOKENS: [&str; 5] = [
    "prompt_tokens",
    "completion_tokens",
    "reasoning_tokens",
    "cached_tokens",
    "total_tokens",
];
pub(crate) fn add_tokens(target: &mut Value, row: &Value) {
    for key in TOKENS {
        bump(target, key, number(row, key));
    }
}

pub(crate) struct Context<'a> {
    pub params: &'a Value,
    pub metadata: &'a Value,
    pub accounts: BTreeMap<String, Value>,
    pub account_order: Vec<String>,
}
impl<'a> Context<'a> {
    fn new(params: &'a Value, metadata: &'a Value) -> Self {
        let mut accounts = BTreeMap::new();
        let mut account_order = Vec::new();
        let input = metadata.get("accounts").unwrap_or(metadata);
        if let Some(rows) = input.as_array() {
            for row in rows {
                let uid = text(&row["uid"]);
                if !uid.is_empty() {
                    account_order.push(uid.clone());
                    accounts.insert(uid, row.clone());
                }
            }
        } else if let Some(rows) = input.as_object() {
            for (uid, row) in rows {
                if row.is_object() && (row.get("realm").is_some() || row.get("nickname").is_some())
                {
                    account_order.push(uid.clone());
                    accounts.insert(uid.clone(), row.clone());
                }
            }
        }
        Self {
            params,
            metadata,
            accounts,
            account_order,
        }
    }
    pub fn param(&self, key: &str) -> &Value {
        let v = &self.params[key];
        v.as_array().and_then(|a| a.first()).unwrap_or(v)
    }
    pub fn integer(&self, key: &str, default: i64) -> i64 {
        pyval::py_int(self.param(key)).unwrap_or(default)
    }
    pub fn realm(&self, all_default: bool) -> Option<String> {
        let mut realm = text(self.param("realm")).trim().to_string();
        if realm.is_empty() && !all_default {
            realm = string_or(&self.metadata["realm"], "intl");
        }
        if realm.is_empty() || realm == "all" {
            None
        } else {
            Some(realm)
        }
    }
    pub fn row_realm(&self, row: &Value) -> String {
        let realm = text(&row["realm"]);
        if !realm.is_empty() {
            return realm;
        }
        if let Some(acc) = self.accounts.get(&text(&row["account"])) {
            let realm = text(&acc["realm"]);
            if !realm.is_empty() {
                return realm;
            }
        }
        // An optional live catalogue map overrides the baseline model heuristic.
        let model = text(&row["model"]);
        if let Some(r) = self.metadata["model_realms"][&model]
            .as_str()
            .filter(|r| !r.is_empty())
        {
            return r.into();
        }
        let model = model.to_lowercase();
        if model.starts_with("gpt-") || model.starts_with("gemini-") || model == "grok-4.7" {
            return "intl".into();
        }
        if model.starts_with("minimax-")
            || model.starts_with("deepseek-v4-pro")
            || [
                "glm-5.1",
                "glm-5.0-turbo",
                "glm-4.6v",
                "kimi-k3-1",
                "kimi-k2.7",
                "kimi-k2-thinking",
                "hy3-x",
                "hy4-preview-dev",
                "hy4-preview-x",
            ]
            .contains(&model.as_str())
        {
            return "cn".into();
        }
        string_or(&self.metadata["realm"], "intl")
    }
    pub fn matches(
        &self,
        row: &Value,
        realm: &Option<String>,
        window: (Option<f64>, Option<f64>),
    ) -> bool {
        let at = number(row, "at");
        realm
            .as_ref()
            .map(|r| *r == self.row_realm(row))
            .unwrap_or(true)
            && window.0.map(|lo| at >= lo).unwrap_or(true)
            && window.1.map(|hi| at <= hi).unwrap_or(true)
    }
    pub fn window(&self) -> (Option<f64>, Option<f64>) {
        let range = text(self.param("range")).trim().to_lowercase();
        let (mut lo, mut hi) = match range.as_str() {
            "today" | "day" | "1d" => (Some(timeutil::today_midnight()), None),
            "week" | "w" => (Some(timeutil::local_week_start()), None),
            "month" | "m" => (Some(timeutil::local_month_start()), None),
            "custom" => {
                let parse =
                    |k| pyval::py_float(self.param(k)).filter(|n| n.is_finite() && *n >= 0.0);
                (parse("since"), parse("until"))
            }
            _ => (None, None),
        };
        if let (Some(a), Some(b)) = (lo, hi) {
            if a > b {
                std::mem::swap(&mut lo, &mut hi);
            }
        }
        (lo, hi)
    }
    pub fn keys(&self) -> Vec<Value> {
        self.metadata["api_keys"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
    pub fn key_name(&self, row: &Value) -> String {
        let key = text(&row["key"]);
        if key.is_empty() {
            return key;
        }
        if key == "launcher" {
            return "启动参数".into();
        }
        for k in self.keys() {
            if text(&k["id"]) == key {
                return string_or(&k["name"], &key);
            }
        }
        key
    }
}

#[cfg(test)]
mod tests;
