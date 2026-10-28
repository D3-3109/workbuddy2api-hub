use crate::{invalid, schema::strings, security::constant_time_eq, text, Error, Result};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use wbcore::ids;

fn timestamp() -> String {
    chrono::Local::now().format("%Y/%m/%d %H:%M").to_string()
}
fn unique_id(candidate: &str, used: &HashSet<String>) -> String {
    if !candidate.is_empty() && !used.contains(candidate) {
        return candidate.to_owned();
    }
    if !candidate.is_empty() {
        for suffix in 2.. {
            let id = format!("{candidate}-{suffix}");
            if !used.contains(&id) {
                return id;
            }
        }
    }
    loop {
        let id = ids::token_hex(6);
        if !used.contains(&id) {
            return id;
        }
    }
}
fn clean(entry: &Value) -> Option<Value> {
    if !entry.is_object() {
        return None;
    }
    let key = text(entry, "key").trim();
    let deleted = text(entry, "deleted_at").trim();
    if key.is_empty() && deleted.is_empty() {
        return None;
    }
    let mut out = entry.clone();
    let id = text(entry, "id").trim();
    // Stable across file reloads, so a view's generated id can be revealed/saved.
    out["id"] = json!(if id.is_empty() {
        ids::sha256_hex(&format!("{key}|{deleted}"))[..12].to_owned()
    } else {
        id.to_owned()
    });
    out["name"] = json!(if text(entry, "name").trim().is_empty() {
        "未命名"
    } else {
        text(entry, "name").trim()
    });
    out["key"] = json!(if deleted.is_empty() { key } else { "" });
    let realm = text(entry, "realm").trim().to_lowercase();
    out["realm"] = json!(if ["intl", "cn"].contains(&realm.as_str()) {
        realm.as_str()
    } else {
        ""
    });
    out["models"] = strings(&entry["models"], usize::MAX, usize::MAX, true, false);
    out["accounts"] = strings(&entry["accounts"], 200, 128, false, false);
    out["concurrency_limit"] = json!(entry
        .get("concurrency_limit")
        .unwrap_or(&entry["max_concurrency"])
        .as_u64()
        .or_else(|| {
            entry
                .get("concurrency_limit")
                .unwrap_or(&entry["max_concurrency"])
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
        })
        .unwrap_or(0));
    out["enabled"] = json!(deleted.is_empty() && entry["enabled"] != false);
    out["created_at"] = json!(text(entry, "created_at"));
    out["deleted_at"] = json!(deleted);
    out.as_object_mut().unwrap().remove("max_concurrency");
    Some(out)
}

pub(crate) fn read_all(data: &Value) -> Vec<Value> {
    if let Some(stored) = data["api_keys"].as_array() {
        let mut ids = HashSet::new();
        let mut secrets = HashSet::new();
        stored
            .iter()
            .filter_map(clean)
            .filter_map(|mut entry| {
                let secret = text(&entry, "key");
                if !secret.is_empty() && !secrets.insert(secret.to_owned()) {
                    return None;
                }
                let id = unique_id(text(&entry, "id"), &ids);
                ids.insert(id.clone());
                entry["id"] = json!(id);
                Some(entry)
            })
            .collect()
    } else if data["api_key_set"] == true && !text(data, "api_key").trim().is_empty() {
        vec![clean(&json!({"id":"legacy","name":"默认（跟随面板切换）","key":text(data,"api_key").trim(),"enabled":true})).unwrap()]
    } else {
        Vec::new()
    }
}
fn retire(entry: &Value) -> Value {
    // Only known metadata survives retirement: extension fields must not retain
    // another copy of a revoked credential.
    let mut out = json!({});
    for key in [
        "id",
        "name",
        "realm",
        "models",
        "accounts",
        "concurrency_limit",
        "created_at",
    ] {
        out[key] = entry[key].clone();
    }
    out["key"] = json!("");
    out["enabled"] = json!(false);
    out["deleted_at"] = if text(entry, "deleted_at").is_empty() {
        json!(timestamp())
    } else {
        entry["deleted_at"].clone()
    };
    out
}

pub(crate) fn apply_patch(data: &mut Value, patch: &Map<String, Value>) -> Result<()> {
    let empty = Vec::new();
    let incoming = patch
        .get("api_keys")
        .map(|v| {
            v.as_array()
                .ok_or_else(|| invalid("api_keys must be a list"))
        })
        .transpose()?
        .unwrap_or(&empty);
    let upsert = patch.contains_key("deleted_api_key_ids");
    let drop: HashSet<String> = if let Some(v) = patch.get("deleted_api_key_ids") {
        v.as_array()
            .ok_or_else(|| invalid("deleted_api_key_ids must be a list of strings"))?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("deleted_api_key_ids must be a list of strings"))
            })
            .collect::<Result<_>>()?
    } else {
        HashSet::new()
    };
    let previous = read_all(data);
    let mut out = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut seen_secrets = HashSet::new();
    for raw in incoming {
        let fields = raw
            .as_object()
            .ok_or_else(|| invalid("each API key must be an object"))?;
        let raw_id = fields
            .get("id")
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| invalid("API key id must be a string"))
            })
            .transpose()?
            .unwrap_or("")
            .trim();
        let given_secret = fields
            .get("key")
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| invalid("API key secret must be a string"))
            })
            .transpose()?
            .unwrap_or("")
            .trim();
        let old = previous.iter().find(|e| e["id"] == raw_id).or_else(|| {
            if upsert && !given_secret.is_empty() {
                previous.iter().find(|e| {
                    e["deleted_at"] == ""
                        && constant_time_eq(text(e, "key").as_bytes(), given_secret.as_bytes())
                })
            } else {
                None
            }
        });
        if let Some(old) = old {
            if old["deleted_at"] != "" {
                // A stale browser cannot revive or mutate deleted history.
                if seen_ids.insert(text(old, "id").to_owned()) {
                    out.push(retire(old));
                }
                continue;
            }
        }
        let mut entry = old.cloned().unwrap_or_else(|| json!({"id":if raw_id.is_empty() {ids::token_hex(6)} else {raw_id.to_owned()},"created_at":timestamp()}));
        for key in [
            "name",
            "realm",
            "models",
            "accounts",
            "enabled",
            "concurrency_limit",
            "max_concurrency",
        ] {
            if let Some(value) = fields.get(key) {
                entry[key] = value.clone();
            }
        }
        if !given_secret.is_empty() {
            entry["key"] = json!(given_secret);
        }
        let secret = text(&entry, "key").trim();
        if secret.chars().count() < 4 {
            return Err(invalid("API key secret must contain at least 4 characters (blank only preserves an existing secret)"));
        }
        for key in ["name", "realm"] {
            if fields.contains_key(key) && !fields[key].is_string() {
                return Err(invalid(format!("API key {key} must be a string")));
            }
        }
        let realm = text(&entry, "realm").trim().to_lowercase();
        if !["", "intl", "cn"].contains(&realm.as_str()) {
            return Err(invalid("API key realm must be empty, intl or cn"));
        }
        for key in ["models", "accounts"] {
            if let Some(v) = fields.get(key) {
                if !v.is_string() && !v.is_array() && !v.is_null() {
                    return Err(invalid(format!("API key {key} must be a list or string")));
                }
                if v.as_array()
                    .is_some_and(|a| a.iter().any(|i| !i.is_string()))
                {
                    return Err(invalid(format!("API key {key} must contain strings")));
                }
            }
        }
        if fields.get("enabled").is_some_and(|v| !v.is_boolean()) {
            return Err(invalid("API key enabled must be true or false"));
        }
        if let Some(v) = fields
            .get("concurrency_limit")
            .or_else(|| fields.get("max_concurrency"))
        {
            if !v.is_u64() {
                return Err(invalid(
                    "API key concurrency_limit must be a non-negative integer",
                ));
            }
            entry["concurrency_limit"] = v.clone();
        }
        let mut entry = clean(&entry).unwrap();
        let secret = text(&entry, "key").to_owned();
        if !seen_secrets.insert(secret) {
            continue;
        }
        let id = unique_id(text(&entry, "id"), &seen_ids);
        seen_ids.insert(id.clone());
        entry["id"] = json!(id);
        out.push(if drop.contains(&id) {
            retire(&entry)
        } else {
            entry
        });
    }
    for old in previous {
        let id = text(&old, "id");
        if seen_ids.contains(id) {
            continue;
        }
        let keep = upsert
            && !drop.contains(id)
            && (text(&old, "key").is_empty() || !seen_secrets.contains(text(&old, "key")));
        out.push(if keep { old } else { retire(&old) });
    }
    data["api_keys"] = json!(out);
    data.as_object_mut().unwrap().remove("api_key");
    data.as_object_mut().unwrap().remove("api_key_set");
    Ok(())
}

pub fn mask_key(secret: &str) -> String {
    let chars: Vec<_> = secret.chars().collect();
    if chars.len() <= 8 {
        return "*".repeat(chars.len());
    }
    format!(
        "{}******{}",
        chars[..4].iter().collect::<String>(),
        chars[chars.len() - 4..].iter().collect::<String>()
    )
}

/// Exact account references, never wildcard-expanded.
pub fn key_allows_account(entry: &Value, uid: &str) -> bool {
    let accounts = strings(&entry["accounts"], 200, 128, false, false);
    let accounts = accounts.as_array().unwrap();
    accounts.is_empty() || accounts.iter().any(|v| v == uid.trim())
}
/// Python fnmatch-style case-insensitive model allowlist (*, ?, [abc], [!abc]).
pub fn key_allows_model(entry: &Value, model: &str) -> bool {
    let patterns = strings(&entry["models"], usize::MAX, usize::MAX, true, false);
    let patterns = patterns.as_array().unwrap();
    if patterns.is_empty() {
        return true;
    }
    let model = model.trim().to_lowercase();
    !model.is_empty()
        && patterns
            .iter()
            .filter_map(Value::as_str)
            .any(|p| glob_match(p, &model))
}
fn glob_match(pattern: &str, value: &str) -> bool {
    let chars: Vec<_> = pattern.chars().collect();
    let mut expression = String::from("(?s)\\A");
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => expression.push_str(".*"),
            '?' => expression.push('.'),
            '[' => {
                let mut end = i + 1;
                if end < chars.len() && chars[end] == '!' {
                    end += 1;
                }
                if end < chars.len() && chars[end] == ']' {
                    end += 1;
                }
                while end < chars.len() && chars[end] != ']' {
                    end += 1;
                }
                if end == chars.len() {
                    expression.push_str("\\[");
                } else {
                    expression.push('[');
                    let mut start = i + 1;
                    if chars[start] == '!' {
                        expression.push('^');
                        start += 1;
                    }
                    for &c in &chars[start..end] {
                        if c == '-' {
                            expression.push(c);
                        } else {
                            expression.push_str(&regex::escape(&c.to_string()));
                        }
                    }
                    expression.push(']');
                    i = end;
                }
            }
            c => expression.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    expression.push_str("\\z");
    regex::Regex::new(&expression).is_ok_and(|r| r.is_match(value))
}

/// Non-cloneable request permit. Dropping never re-reads a changed key limit.
#[must_use = "keep the permit alive until the response stream ends"]
pub struct KeySlot {
    counts: Arc<Mutex<HashMap<String, u64>>>,
    id: String,
}
impl Drop for KeySlot {
    fn drop(&mut self) {
        let mut counts = self.counts.lock();
        if let Some(count) = counts.get_mut(&self.id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.id);
            }
        }
    }
}
pub(crate) fn acquire(counts: Arc<Mutex<HashMap<String, u64>>>, entry: &Value) -> Result<KeySlot> {
    let id = if text(entry, "id").is_empty() {
        ids::sha256_hex(text(entry, "key"))
    } else {
        text(entry, "id").to_owned()
    };
    let limit = entry["concurrency_limit"].as_u64().unwrap_or(0);
    {
        let mut current = counts.lock();
        let count = current.entry(id.clone()).or_default();
        if limit > 0 && *count >= limit {
            return Err(Error::RateLimited {
                key_id: id,
                limit,
                in_flight: *count,
            });
        }
        *count += 1;
    }
    Ok(KeySlot { counts, id })
}
