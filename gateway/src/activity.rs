//! Check-in and daily-activity history (`usage/activity_history.jsonl`).
//! One line per real attempt; messages are redacted before they reach disk and
//! again when read, and reads project onto a fixed field list.
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    sync::LazyLock,
};
use wbaccounts::Account;
use wbcore::timeutil;

pub const FILE: &str = "activity_history.jsonl";
const TASKS: [&str; 2] = ["checkin", "daily_chat"];
const TRIGGERS: [&str; 5] = [
    "scheduler",
    "manual",
    "account_add",
    "account_import",
    "unknown",
];
const MAX_MESSAGE: usize = 200;
const MAX_RECORDS: usize = 5000;
const MAX_AGE_DAYS: f64 = 90.0;
const COMPACT_BYTES: u64 = 2_000_000;
const COMPACT_INTERVAL: f64 = 6.0 * 3600.0;

/// Serializes appends and compaction; holds the time of the last compaction.
static WRITE: Mutex<f64> = Mutex::new(0.0);
static SECRETS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)\b(?:bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}",
        r"(?i)\b(?:access[_-]?token|refresh[_-]?token|api[_-]?key|apikey|authorization|cookie|set-cookie|password|secret|token)\b\s*[:=]\s*\S+",
        r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{4,}\b",
        r"\bsk-[A-Za-z0-9_-]{12,}\b",
        r"\b[0-9a-fA-F]{32,}\b",
        r"\b[A-Za-z0-9+/]{40,}={0,2}",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("activity redaction pattern"))
    .collect()
});

pub fn safe_message(text: &str) -> String {
    let mut line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for pattern in SECRETS.iter() {
        line = pattern.replace_all(&line, "[redacted]").into_owned();
    }
    line.chars().take(MAX_MESSAGE).collect()
}

fn iso_with_offset(ts: f64) -> String {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .map(|t| t.format("%Y-%m-%dT%H:%M:%S%:z").to_string())
        .unwrap_or_default()
}
fn parse_ts(raw: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|t| t.timestamp() as f64)
        .or_else(|| timeutil::parse_local_stamp(raw))
}
fn as_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => matches!(
            s.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "ok" | "success"
        ),
        _ => false,
    }
}
fn clip(v: &Value, n: usize) -> String {
    v.as_str().unwrap_or("").chars().take(n).collect()
}
fn project(row: &Value) -> Value {
    let pick = |key: &str, allowed: &[&str], default: &str| {
        row[key]
            .as_str()
            .filter(|s| allowed.contains(s))
            .unwrap_or(default)
            .to_owned()
    };
    json!({"ts":clip(&row["ts"],40),"uid":clip(&row["uid"],80),"nickname":clip(&row["nickname"],120),
        "realm":pick("realm",&["cn","intl"],""),"task":pick("task",&TASKS,""),
        "trigger":pick("trigger",&TRIGGERS,"unknown"),"ok":as_bool(&row["ok"]),
        "message":safe_message(row["message"].as_str().unwrap_or(""))})
}

fn read_rows(path: &Path) -> Vec<Value> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter(Value::is_object)
        .collect()
}

/// Append one attempt. Never fails the caller: history is diagnostic only.
pub fn record(dir: &Path, account: &Account, task: &str, trigger: &str, ok: bool, message: &str) {
    let nickname = account.public_metadata()["nickname"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let row = json!({"ts":iso_with_offset(timeutil::now()),"uid":account.uid(),"nickname":nickname,
        "realm":account.realm().as_str(),"task":task,
        "trigger":if TRIGGERS.contains(&trigger) { trigger } else { "unknown" },
        "ok":ok,"message":safe_message(message)});
    let path = dir.join(FILE);
    let mut last = WRITE.lock();
    let written = fs::create_dir_all(dir).and_then(|()| {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        file.write_all(format!("{}\n", wbcore::pyjson::dumps_u(&row)).as_bytes())
    });
    if written.is_err() {
        return;
    }
    let big = fs::metadata(&path).is_ok_and(|m| m.len() > COMPACT_BYTES);
    if big || timeutil::now() - *last >= COMPACT_INTERVAL {
        *last = timeutil::now();
        compact(&path);
    }
}

fn compact(path: &Path) {
    let rows = read_rows(path);
    let cutoff = timeutil::now() - MAX_AGE_DAYS * 86400.0;
    let mut keep: Vec<&Value> = rows
        .iter()
        .filter(|r| {
            r["ts"]
                .as_str()
                .and_then(parse_ts)
                .is_none_or(|t| t >= cutoff)
        })
        .collect();
    if keep.len() > MAX_RECORDS {
        keep.drain(..keep.len() - MAX_RECORDS);
    }
    if keep.len() == rows.len() {
        return;
    }
    let mut out = String::new();
    for row in keep {
        out.push_str(&wbcore::pyjson::dumps_u(row));
        out.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    if fs::write(&tmp, out)
        .and_then(|()| fs::rename(&tmp, path))
        .is_err()
    {
        let _ = fs::remove_file(&tmp);
    }
}

/// `GET /activity/history` with the baseline's filters; `Err` is a 400 message.
pub fn query(dir: &Path, q: &crate::wire::Query) -> Result<Value, String> {
    let get = |k: &str| q.get(k).map(|s| s.trim().to_owned()).unwrap_or_default();
    let range = match get("range").to_lowercase().as_str() {
        "" => "7d".to_owned(),
        r => r.to_owned(),
    };
    let cutoff = match range.as_str() {
        "today" => Some(timeutil::today_midnight()),
        "1d" => Some(timeutil::now() - 86400.0),
        "7d" => Some(timeutil::now() - 7.0 * 86400.0),
        "30d" => Some(timeutil::now() - 30.0 * 86400.0),
        "90d" => Some(timeutil::now() - 90.0 * 86400.0),
        "all" => None,
        _ => return Err("range must be one of: today, 1d, 7d, 30d, 90d, all".into()),
    };
    let task = get("task");
    if !task.is_empty() && !TASKS.contains(&task.as_str()) {
        return Err("task must be one of: checkin, daily_chat".into());
    }
    let result = match get("result").to_lowercase().as_str() {
        "" => "",
        "ok" | "success" | "succeeded" => "ok",
        "failed" | "fail" | "error" => "failed",
        _ => return Err("result must be one of: ok, failed".into()),
    };
    let limit = get("limit").parse::<usize>().unwrap_or(100).clamp(1, 1000);
    let uid = get("uid");
    let matched: Vec<Value> = read_rows(&dir.join(FILE))
        .iter()
        .rev()
        .map(project)
        .filter(|r| uid.is_empty() || r["uid"] == uid.as_str())
        .filter(|r| task.is_empty() || r["task"] == task.as_str())
        .filter(|r| result.is_empty() || r["ok"] == (result == "ok"))
        .filter(|r| {
            cutoff.is_none_or(|c| r["ts"].as_str().and_then(parse_ts).is_some_and(|t| t >= c))
        })
        .collect();
    let total = matched.len();
    let rows: Vec<Value> = matched.into_iter().take(limit).collect();
    Ok(
        json!({"supported":true,"rows":rows,"total":total,"limit":limit,"range":range,
        "filters":{"uid":uid,"task":task,"result":result}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redacts_credentials_and_bounds_length() {
        let m =
            safe_message("Bearer abcdefghijklmnop token=xyz eyJabcdefghij.klmnopqrstu.vwxyz123");
        assert!(!m.contains("abcdefghijklmnop") && !m.contains("xyz eyJ"));
        assert_eq!(safe_message(&"字".repeat(500)).chars().count(), MAX_MESSAGE);
    }
    #[test]
    fn projection_drops_unknown_fields_and_parses_ok() {
        let row = project(&json!({"ts":"x","task":"checkin","ok":"false","accessToken":"secret"}));
        assert!(row.get("accessToken").is_none());
        assert_eq!(row["ok"], false);
        assert_eq!(row["trigger"], "unknown");
    }
}
