use crate::{invalid, text, Result};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

pub const LIMIT_KEYS: [&str; 5] = [
    "reserve_credits",
    "daily_token_limit",
    "daily_credit_limit",
    "model_daily_token_limit",
    "expiring_window_days",
];
const BOOLS: &[(&str, bool)] = &[
    ("auth_disabled", false),
    ("daily_chat_web", true),
    ("daily_chat_web_retry", true),
    ("daily_chat_web_fallback", false),
    ("daily_chat_web_random_prompt", false),
    ("affinity_stable_key_enabled", false),
    ("remaining_priority_enabled", false),
    ("egress_backoff_enabled", false),
    ("realm_fallback_enabled", false),
    ("auto_switch_product", false),
    ("free_model_credit_fallback", false),
    ("reasoning_effort_map", true),
    ("enable_anthropic_messages", false),
    ("local_web_tools", false),
    ("filter_models_by_key", false),
    ("models_only_available", false),
    ("window_probe", true),
    ("pricing_enabled", true),
    ("pricing_variant_inherit", true),
    ("log_db_enabled", false),
    ("accounts_collapsed", false),
    ("accounts_separate_tab", false),
    ("key_before_hidden", false),
    ("update_check_enabled", false),
];
const INTS: &[(&str, u64, u64, u64)] = &[
    ("daily_chat_web_attempts", 3, 1, 5),
    ("daily_chat_web_timeout", 120, 1, 3600),
    ("daily_chat_web_retry_backoff", 5, 0, 60),
    ("daily_chat_web_concurrency", 1, 1, 10),
    ("account_max_concurrent", 0, 0, 10),
    ("section_max_height", 640, 0, 4000),
    ("keep_backups", 1, 0, 3),
];

/// Recursively merge objects; a non-object patch replaces the corresponding value.
pub fn deep_merge(base: &Value, patch: &Value) -> Value {
    match (base.as_object(), patch.as_object()) {
        (Some(base), Some(patch)) => {
            let mut out = base.clone();
            for (key, value) in patch {
                out.insert(
                    key.clone(),
                    deep_merge(out.get(key).unwrap_or(&Value::Null), value),
                );
            }
            Value::Object(out)
        }
        _ => patch.clone(),
    }
}

pub fn defaults() -> Value {
    let mut v = json!({
        "limits": {}, "schedule": {"checkin":[9,21], "travel":[9,21], "keepalive":[22], "cat":[1]},
        "daily_chat_model":"", "daily_chat_web_fallback_models":[], "daily_chat_web_prompts":[],
        "auto_switch_products":["workbuddy","vscode","cli"], "model_aliases":{},
        "upstream":{"header_timeout_seconds":120,"idle_timeout_seconds":300,"device_token":"","device_token_file":""},
        "prompt":{"mode":"passthrough","file":"","text":""}, "identity":{"private_data":"auto"},
        "identity_versions":{}, "quota_scan_models":[], "pricing_refresh_minutes":5.0, "credits_refresh_hours":12.0,
        "log_db_max_rows":null,"log_db_max_days":7,"log_db_max_bytes":null,
        "usage":{"hour_keep_hours":48,"day_keep_days":32}, "ui_language":"zh", "hidden_pages":[],
        "proxy_slots":[],"proxy_slot_seq":0
    });
    for &(key, default) in BOOLS {
        v[key] = json!(default);
    }
    for &(key, default, _, _) in INTS {
        v[key] = json!(default);
    }
    for key in LIMIT_KEYS {
        v["limits"][key] = json!({"global":if key == "expiring_window_days" { 7 } else { 0 },"intl":null,"cn":null});
    }
    v
}

pub(crate) fn migrate(data: &mut Value) -> bool {
    let before = data.clone();
    if !data.is_object() {
        *data = json!({});
    }
    for key in LIMIT_KEYS {
        if let Some(legacy) = data.as_object_mut().unwrap().remove(key) {
            if !data["limits"].is_object() {
                data["limits"] = json!({});
            }
            if !data["limits"][key].is_object() {
                data["limits"][key] = json!({});
            }
            if data["limits"][key].get("global").is_none() {
                data["limits"][key]["global"] = json!(coerce_nonnegative(&legacy).unwrap_or(0));
            }
        }
    }
    if let Some(hours) = data
        .as_object_mut()
        .unwrap()
        .remove("pricing_refresh_hours")
    {
        if data.get("pricing_refresh_minutes").is_none() {
            data["pricing_refresh_minutes"] = json!(hours
                .as_f64()
                .map(|h| (h * 60.0).clamp(0.0, 43200.0))
                .unwrap_or(5.0));
        }
    }
    *data != before
}

pub(crate) fn resolve(raw: &Value) -> Value {
    let default = defaults();
    let mut out = deep_merge(&default, raw);
    for &(key, fallback) in BOOLS {
        out[key] = json!(if key == "reasoning_effort_map" {
            raw[key] != false
        } else {
            raw.get(key).map(|v| v == true).unwrap_or(fallback)
        });
    }
    for &(key, fallback, min, max) in INTS {
        let number = raw[key].as_u64();
        out[key] = json!(if key == "section_max_height" || key == "keep_backups" {
            number.map(|n| n.min(max)).unwrap_or(fallback)
        } else {
            number
                .filter(|n| *n >= min && *n <= max)
                .unwrap_or(fallback)
        });
    }
    for group in [
        "limits",
        "schedule",
        "upstream",
        "usage",
        "prompt",
        "identity",
        "identity_versions",
    ] {
        if !out[group].is_object() {
            out[group] = default[group].clone();
        }
    }
    for key in LIMIT_KEYS {
        let entry = &raw["limits"][key];
        let mut normalized = if entry.is_object() {
            entry.clone()
        } else {
            json!({})
        };
        normalized["global"] = json!(if entry["global"].is_null() {
            default["limits"][key]["global"].as_u64().unwrap()
        } else {
            coerce_nonnegative(&entry["global"]).unwrap_or(0)
        });
        for realm in ["intl", "cn"] {
            normalized[realm] = coerce_nonnegative(&entry[realm])
                .map(|n| json!(n))
                .unwrap_or(Value::Null);
        }
        out["limits"][key] = normalized;
    }
    for (group, key, fallback, min, max) in [
        ("upstream", "header_timeout_seconds", 120, 1, u64::MAX),
        ("upstream", "idle_timeout_seconds", 300, 1, u64::MAX),
        ("usage", "hour_keep_hours", 48, 1, 336),
        ("usage", "day_keep_days", 32, 1, 366),
    ] {
        out[group][key] = json!(raw[group][key]
            .as_u64()
            .filter(|n| *n >= min && *n <= max)
            .unwrap_or(fallback));
    }
    for key in ["device_token", "device_token_file"] {
        out["upstream"][key] = json!(text(&raw["upstream"], key).trim());
    }
    for key in ["checkin", "travel", "keepalive", "cat"] {
        out["schedule"][key] = normalize_hours(&raw["schedule"][key])
            .filter(|v| !v.as_array().unwrap().is_empty())
            .unwrap_or_else(|| default["schedule"][key].clone());
    }
    let daily = normalize_times(&raw["schedule"]["daily_chat"]).unwrap_or_else(|| json!([]));
    out["schedule"]["daily_chat"] = if daily.as_array().unwrap().is_empty() {
        Value::Array(
            out["schedule"]["checkin"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| json!(format!("{:02}:00", v.as_u64().unwrap_or(0))))
                .collect(),
        )
    } else {
        daily
    };
    out["ui_language"] = json!(
        if ["zh", "zh-Hant", "en"].contains(&text(raw, "ui_language")) {
            text(raw, "ui_language")
        } else {
            "zh"
        }
    );
    out["daily_chat_model"] = json!(text(raw, "daily_chat_model").trim());
    out["hidden_pages"] = hidden_pages(&raw["hidden_pages"]);
    out["daily_chat_web_fallback_models"] = strings(
        &raw["daily_chat_web_fallback_models"],
        20,
        128,
        false,
        false,
    );
    out["daily_chat_web_prompts"] = strings(&raw["daily_chat_web_prompts"], 500, 200, false, true);
    out["quota_scan_models"] = strings(&raw["quota_scan_models"], 200, usize::MAX, false, false);
    if let Some(products) = raw["auto_switch_products"].as_array() {
        out["auto_switch_products"] = json!(products
            .iter()
            .filter_map(Value::as_str)
            .filter(|p| ["workbuddy", "vscode", "cli"].contains(p))
            .collect::<Vec<_>>());
    }
    if !raw["auto_switch_products"].is_array() {
        out["auto_switch_products"] = default["auto_switch_products"].clone();
    }
    for (key, fallback, cap) in [
        ("pricing_refresh_minutes", 5.0, 43200.0),
        ("credits_refresh_hours", 12.0, 720.0),
    ] {
        out[key] = json!(raw[key]
            .as_f64()
            .filter(|v| v.is_finite())
            .map(|v| {
                if v < 0.0 && key == "credits_refresh_hours" {
                    fallback
                } else {
                    v.clamp(0.0, cap)
                }
            })
            .unwrap_or(fallback));
    }
    for (key, cap) in [
        ("log_db_max_rows", 100_000_000),
        ("log_db_max_days", 36500),
        ("log_db_max_bytes", 100 * 1024 * 1024 * 1024),
    ] {
        out[key] = if raw.get(key).is_none() {
            default[key].clone()
        } else {
            retention(&raw[key], cap)
        };
    }
    let mode = text(&raw["prompt"], "mode").trim().to_lowercase();
    out["prompt"]["mode"] = json!(
        if ["passthrough", "custom", "append"].contains(&mode.as_str()) {
            mode.as_str()
        } else {
            "passthrough"
        }
    );
    for key in ["file", "text"] {
        out["prompt"][key] = json!(text(&raw["prompt"], key));
    }
    let private = text(&raw["identity"], "private_data").to_lowercase();
    out["identity"]["private_data"] =
        json!(if ["auto", "true", "false"].contains(&private.as_str()) {
            private.as_str()
        } else {
            "auto"
        });
    if validate_versions(&raw["identity_versions"]).is_err() {
        out["identity_versions"] = json!({});
    }
    let mut aliases = json!({});
    if let Some(map) = raw["model_aliases"].as_object() {
        for (from, to) in map {
            if let Some(to) = to.as_str() {
                let from = from.trim().to_lowercase();
                if !from.is_empty()
                    && !to.trim().is_empty()
                    && !from.eq_ignore_ascii_case(to.trim())
                {
                    aliases[&from] = json!(to.trim());
                }
            }
        }
    } else if let Some(rows) = raw["model_aliases"].as_array() {
        for row in rows {
            let from = text(row, "from").trim().to_lowercase();
            let to = text(row, "to").trim();
            if !from.is_empty() && !to.is_empty() && !from.eq_ignore_ascii_case(to) {
                aliases[&from] = json!(to);
            }
        }
    }
    out["model_aliases"] = aliases;
    out
}

/// Select defaults' fields recursively so unknown nested secret-bearing fields
/// can never make their way into the dashboard response.
pub(crate) fn public_config(snapshot: &Value) -> Value {
    fn select(template: &Value, source: &Value) -> Value {
        if let Some(map) = template.as_object() {
            let mut result = json!({});
            for (key, value) in map {
                result[key] = select(value, &source[key]);
            }
            result
        } else {
            source.clone()
        }
    }
    let mut out = select(&defaults(), snapshot);
    out.as_object_mut().unwrap().remove("proxy_slots");
    out.as_object_mut().unwrap().remove("proxy_slot_seq");
    out["upstream"]
        .as_object_mut()
        .unwrap()
        .remove("device_token");
    out["upstream"]
        .as_object_mut()
        .unwrap()
        .remove("device_token_file");
    out["upstream"]["device_token_set"] =
        json!(!text(&snapshot["upstream"], "device_token").is_empty());
    out["schedule"]["daily_chat"] = snapshot["schedule"]["daily_chat"].clone();
    out["identity_versions"] = snapshot["identity_versions"].clone();
    out["model_aliases"] = snapshot["model_aliases"].clone();
    out
}

pub(crate) fn apply_patch(data: &mut Value, patch: &Map<String, Value>) -> Result<()> {
    for (key, value) in patch {
        let key = key.as_str();
        if BOOLS.iter().any(|(k, _)| *k == key) {
            if key == "auth_disabled" {
                data[key] = json!(wbcore::pyval::truthy(value));
            } else {
                if !value.is_boolean() {
                    return Err(invalid(format!("{key} must be true or false")));
                }
                data[key] = value.clone();
            }
            continue;
        }
        if let Some(&(_, _, min, max)) = INTS.iter().find(|(k, _, _, _)| *k == key) {
            let n = if key == "section_max_height" {
                value.as_u64().or_else(|| {
                    value
                        .as_f64()
                        .filter(|f| f.is_finite() && *f >= 0.0 && f.fract() == 0.0)
                        .map(|f| f as u64)
                })
            } else {
                value.as_u64()
            }
            .ok_or_else(|| invalid(format!("{key} must be a whole number")))?;
            if n < min || (n > max && key != "section_max_height" && key != "keep_backups") {
                return Err(invalid(format!("{key} must be between {min} and {max}")));
            }
            data[key] = json!(n.min(max));
            continue;
        }
        if LIMIT_KEYS.contains(&key) {
            validate_integer(value, key, 0, u64::MAX)?;
            merge_group(data, "limits", &json!({key:{"global":value}}));
            continue;
        }
        match key {
            "api_key" => {
                let s = value
                    .as_str()
                    .ok_or_else(|| invalid("api_key must be a string"))?
                    .trim();
                if !s.is_empty() && s.chars().count() < 4 {
                    return Err(invalid("api_key must contain at least 4 characters"));
                }
                data["api_key"] = json!(s);
                data["api_key_set"] = json!(true);
            }
            "limits" => {
                let map = object(value, key)?;
                let mut normalized = json!({});
                for (guard, entry) in map {
                    if !LIMIT_KEYS.contains(&guard.as_str()) {
                        return Err(invalid(format!("unknown limit: {guard}")));
                    }
                    if let Some(scopes) = entry.as_object() {
                        normalized[guard] = json!({});
                        for (scope, n) in scopes {
                            if !["global", "intl", "cn"].contains(&scope.as_str()) {
                                return Err(invalid(format!("unknown limit scope: {scope}")));
                            }
                            if scope != "global" && (n.is_null() || n == "") {
                                normalized[guard][scope] = Value::Null;
                            } else {
                                validate_integer(n, guard, 0, u64::MAX)?;
                                normalized[guard][scope] = n.clone();
                            }
                        }
                    } else {
                        validate_integer(entry, guard, 0, u64::MAX)?;
                        normalized[guard] = json!({"global":entry});
                    }
                }
                merge_group(data, key, &normalized);
            }
            "upstream" | "usage" => {
                let map = object(value, key)?;
                for (field, v) in map {
                    match (key, field.as_str()) {
                        ("upstream", "header_timeout_seconds" | "idle_timeout_seconds") => {
                            validate_integer(v, field, 1, u64::MAX)?
                        }
                        ("upstream", "device_token" | "device_token_file") => {
                            if !v.is_string() {
                                return Err(invalid(format!("{field} must be a string")));
                            }
                        }
                        ("usage", "hour_keep_hours") => validate_integer(v, field, 1, 336)?,
                        ("usage", "day_keep_days") => validate_integer(v, field, 1, 366)?,
                        _ => return Err(invalid(format!("unknown {key} setting: {field}"))),
                    }
                }
                merge_group(data, key, value);
            }
            "schedule" => {
                let mut normalized = json!({});
                for (field, v) in object(value, key)? {
                    if field == "daily_chat" {
                        normalized[field] = normalize_times(v)
                            .ok_or_else(|| invalid("invalid daily_chat schedule"))?;
                    } else if ["checkin", "travel", "keepalive", "cat"].contains(&field.as_str()) {
                        normalized[field] = normalize_hours(v)
                            .filter(|x| !x.as_array().unwrap().is_empty())
                            .ok_or_else(|| {
                                invalid(format!("schedule.{field} needs a valid hour"))
                            })?;
                    } else {
                        return Err(invalid(format!("unknown schedule: {field}")));
                    }
                }
                merge_group(data, key, &normalized);
            }
            "prompt" => {
                for (field, v) in object(value, key)? {
                    let s = v
                        .as_str()
                        .ok_or_else(|| invalid(format!("prompt.{field} must be a string")))?;
                    match field.as_str() {
                        "mode"
                            if ["passthrough", "custom", "append"]
                                .contains(&s.trim().to_lowercase().as_str()) => {}
                        "file" => (),
                        "text" if s.len() <= 65536 => (),
                        _ => return Err(invalid(format!("invalid prompt.{field}"))),
                    }
                }
                merge_group(data, key, value);
            }
            "identity" => {
                for (field, v) in object(value, key)? {
                    if field != "private_data"
                        || !v
                            .as_str()
                            .is_some_and(|s| ["auto", "true", "false"].contains(&s))
                    {
                        return Err(invalid("identity.private_data must be auto, true or false"));
                    }
                }
                merge_group(data, key, value);
            }
            "identity_versions" => {
                validate_versions(value)?;
                if value.as_object().unwrap().is_empty() {
                    data.as_object_mut().unwrap().remove(key);
                } else {
                    data[key] = value.clone();
                }
            }
            "ui_language" => {
                if !value
                    .as_str()
                    .is_some_and(|s| ["zh", "zh-Hant", "en"].contains(&s))
                {
                    return Err(invalid("ui_language must be zh, zh-Hant or en"));
                }
                data[key] = value.clone();
            }
            "daily_chat_model" => {
                let s = value
                    .as_str()
                    .ok_or_else(|| invalid("daily_chat_model must be a string"))?
                    .trim();
                if s.len() > 64
                    || !s
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c))
                {
                    return Err(invalid("invalid daily_chat_model"));
                }
                data[key] = json!(s);
            }
            "auto_switch_products" => {
                let values = string_array(value, key)?;
                if values
                    .iter()
                    .any(|s| !["workbuddy", "vscode", "cli"].contains(&s.as_str()))
                {
                    return Err(invalid("unknown auto_switch_products entry"));
                }
                data[key] = json!(values);
            }
            "hidden_pages" => {
                string_array(value, key)?;
                data[key] = hidden_pages(value);
            }
            "quota_scan_models" => {
                if !value.is_null() {
                    let list = string_array(value, key)?;
                    if list.len() > 200 {
                        return Err(invalid("quota_scan_models allows at most 200 entries"));
                    }
                }
                data[key] = strings(value, 200, usize::MAX, false, false);
            }
            "daily_chat_web_fallback_models" | "daily_chat_web_prompts" => {
                if !value.is_string() && !value.is_array() {
                    return Err(invalid(format!("{key} must be a list or string")));
                }
                data[key] = if key.ends_with("prompts") {
                    strings(value, 500, 200, false, true)
                } else {
                    strings(value, 20, 128, false, false)
                };
            }
            "pricing_refresh_minutes" | "pricing_refresh_hours" | "credits_refresh_hours" => {
                let n = value
                    .as_f64()
                    .filter(|n| *n >= 0.0 && n.is_finite())
                    .ok_or_else(|| invalid(format!("{key} must be a non-negative number")))?;
                if key == "pricing_refresh_hours" {
                    if !patch.contains_key("pricing_refresh_minutes") {
                        data["pricing_refresh_minutes"] = json!((n * 60.0).min(43200.0));
                    }
                } else {
                    data[key] = json!(n.min(if key == "credits_refresh_hours" {
                        720.0
                    } else {
                        43200.0
                    }));
                }
                data.as_object_mut()
                    .unwrap()
                    .remove("pricing_refresh_hours");
            }
            "log_db_max_rows" => data[key] = retention(value, 100_000_000),
            "log_db_max_days" => data[key] = retention(value, 36500),
            "log_db_max_bytes" => data[key] = retention(value, 100 * 1024 * 1024 * 1024),
            // HTTP-only flags and unknown keys are never persisted. Dedicated
            // methods own password hashes, launcher credentials, and proxy slots.
            _ => (),
        }
    }
    Ok(())
}

fn merge_group(data: &mut Value, key: &str, patch: &Value) {
    data[key] = deep_merge(&data[key], patch);
}
fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{name} must be an object")))
}
fn validate_integer(v: &Value, name: &str, min: u64, max: u64) -> Result<()> {
    if !v.as_u64().is_some_and(|n| n >= min && n <= max) {
        return Err(invalid(format!(
            "{name} must be an integer between {min} and {max}"
        )));
    }
    Ok(())
}
fn coerce_nonnegative(v: &Value) -> Option<u64> {
    if v.is_null() || v == "" {
        return None;
    }
    v.as_u64()
        .or_else(|| v.as_i64().map(|n| n.max(0) as u64))
        .or_else(|| {
            v.as_str()
                .and_then(|s| s.trim().parse::<i64>().ok())
                .map(|n| n.max(0) as u64)
        })
}
fn retention(value: &Value, cap: u64) -> Value {
    coerce_nonnegative(value)
        .filter(|n| *n > 0)
        .map(|n| json!(n.min(cap)))
        .unwrap_or(Value::Null)
}
fn string_array(value: &Value, name: &str) -> Result<Vec<String>> {
    let array = value
        .as_array()
        .ok_or_else(|| invalid(format!("{name} must be a list of strings")))?;
    array
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid(format!("{name} must be a list of strings")))
        })
        .collect()
}
pub(crate) fn strings(
    value: &Value,
    max: usize,
    max_bytes: usize,
    lower: bool,
    lines_only: bool,
) -> Value {
    let raw: Vec<String> = match value {
        Value::String(s) => s
            .split(|c| c == '\n' || (!lines_only && (c == ',' || c == ';')))
            .map(str::to_owned)
            .collect(),
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for item in raw {
        let item = if lower {
            item.trim().to_lowercase()
        } else {
            item.trim().to_owned()
        };
        if item.is_empty() || item.len() > max_bytes || !seen.insert(item.clone()) {
            continue;
        }
        out.push(item);
        if out.len() == max {
            break;
        }
    }
    json!(out)
}
fn hidden_pages(value: &Value) -> Value {
    let array = strings(value, usize::MAX, 32, false, false);
    json!(array
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .filter(|s| {
            *s != "settings"
                && s.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                && s.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
        })
        .take(32)
        .collect::<Vec<_>>())
}
fn normalize_hours(value: &Value) -> Option<Value> {
    let raw: Vec<Value> = if let Some(s) = value.as_str() {
        s.split(',').map(|s| json!(s.trim())).collect()
    } else {
        value.as_array()?.clone()
    };
    let mut hours: Vec<u64> = raw
        .iter()
        .filter_map(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .filter(|n| *n <= 23)
        .collect();
    hours.sort_unstable();
    hours.dedup();
    Some(json!(hours))
}
fn normalize_times(value: &Value) -> Option<Value> {
    if value.is_null() {
        return Some(json!([]));
    }
    let parts: Vec<String> = if let Some(s) = value.as_str() {
        s.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        value
            .as_array()?
            .iter()
            .map(|v| v.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?
    };
    let mut out = Vec::new();
    for part in parts {
        let s = part.replace('：', ":");
        let (h, m) = if let Some((h, m)) = s.split_once(':') {
            (h.trim().parse::<u8>().ok()?, m.trim().parse::<u8>().ok()?)
        } else {
            (s.trim().parse::<u8>().ok()?, 0)
        };
        if h > 23 || m > 59 {
            return None;
        }
        let normalized = format!("{h:02}:{m:02}");
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    out.sort();
    out.truncate(24);
    Some(json!(out))
}
fn validate_versions(value: &Value) -> Result<()> {
    for (key, value) in object(value, "identity_versions")? {
        if ![
            "desktop_intl",
            "desktop_cn",
            "desktop_cli",
            "vscode_product",
            "vscode_ide",
            "cli",
            "sdk",
            "node",
        ]
        .contains(&key.as_str())
        {
            return Err(invalid(format!("unknown identity version: {key}")));
        }
        let s = value
            .as_str()
            .ok_or_else(|| invalid("identity version must be a string"))?;
        if s.is_empty()
            || s.len() > 64
            || !s.as_bytes()[0].is_ascii_alphanumeric()
            || !s
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        {
            return Err(invalid(format!("invalid identity version: {key}")));
        }
    }
    Ok(())
}
