//! Usage-block helpers shared by every output path (wb_proxy.py:619-663, 806-833).

use crate::pyval::{get, py_float, truthy};
use serde_json::{json, Map, Value};

/// First positive cache-hit value across every alias the upstream emits.
pub fn best_cached_tokens(usage: &Value) -> i64 {
    if !usage.is_object() {
        return 0;
    }
    let empty = Value::Object(Map::new());
    let pd = get(usage, "prompt_tokens_details")
        .filter(|v| truthy(v))
        .unwrap_or(&empty);
    let id = get(usage, "input_tokens_details")
        .filter(|v| truthy(v))
        .unwrap_or(&empty);
    let cd = get(usage, "completion_tokens_details")
        .filter(|v| truthy(v))
        .unwrap_or(&empty);
    let candidates = [
        get(pd, "cached_tokens"),
        get(usage, "prompt_cache_hit_tokens"),
        get(usage, "cache_read_input_tokens"),
        get(usage, "cached_tokens"),
        get(id, "cached_tokens"),
        get(cd, "cached_tokens"),
    ];
    for c in candidates {
        let n = match c {
            None => 0.0,
            Some(v) if !truthy(v) => 0.0,
            Some(v) => match py_float(v) {
                Some(f) => f,
                None => continue,
            },
        };
        if n > 0.0 {
            return n as i64;
        }
    }
    0
}

/// `normalize_usage_cache_aliases`: write the best cache value into every alias.
pub fn normalize_cache_aliases(usage: &mut Value) {
    let best = best_cached_tokens(usage);
    if best <= 0 {
        return;
    }
    let Some(m) = usage.as_object_mut() else {
        return;
    };
    m.insert("cache_read_input_tokens".into(), json!(best));
    m.insert("cached_tokens".into(), json!(best));
    m.insert("prompt_cache_hit_tokens".into(), json!(best));
    let mut pd = m
        .get("prompt_tokens_details")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    pd.insert("cached_tokens".into(), json!(best));
    m.insert("prompt_tokens_details".into(), Value::Object(pd));
    if let Some(Value::Object(idt)) = m.get("input_tokens_details").cloned() {
        let mut idt = idt;
        idt.insert("cached_tokens".into(), json!(best));
        m.insert("input_tokens_details".into(), Value::Object(idt));
    }
}

fn or_zero(v: Option<&Value>) -> Value {
    match v {
        Some(x) if truthy(x) => x.clone(),
        _ => json!(0),
    }
}

/// `_extract_usage`: the fields recorded in usage.jsonl.
pub fn extract_usage(usage: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(u) = usage.filter(|u| truthy(u)) else {
        return out;
    };
    let details = get(u, "completion_tokens_details")
        .cloned()
        .unwrap_or(json!({}));
    out.insert("prompt_tokens".into(), or_zero(get(u, "prompt_tokens")));
    out.insert(
        "completion_tokens".into(),
        or_zero(get(u, "completion_tokens")),
    );
    out.insert(
        "reasoning_tokens".into(),
        or_zero(get(&details, "reasoning_tokens")),
    );
    out.insert("cached_tokens".into(), json!(best_cached_tokens(u)));
    out.insert("total_tokens".into(), or_zero(get(u, "total_tokens")));
    out.insert("credit".into(), or_zero(get(u, "credit")));
    out
}

/// `total_tokens` of a usage block as f64 (0 when missing).
pub fn total_tokens(u: &Value) -> f64 {
    get(u, "total_tokens").and_then(py_float).unwrap_or(0.0)
}

/// Keep the usage block with the largest `total_tokens` (ties: later wins).
pub fn keep_best(slot: &mut Option<Value>, candidate: Value) {
    let replace = match slot {
        None => true,
        Some(prev) => total_tokens(&candidate) >= total_tokens(prev),
    };
    if replace {
        *slot = Some(candidate);
    }
}

/// `estimate_tokens`: CJK chars + max(1, other/3.6), 0 for empty.
pub fn estimate_tokens(text: &str) -> i64 {
    if text.is_empty() {
        return 0;
    }
    let mut cjk = 0i64;
    let mut other = 0i64;
    for c in text.chars() {
        let cp = c as u32;
        if (0x4e00..=0x9fff).contains(&cp) || (0x3400..=0x4dbf).contains(&cp) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + ((other as f64 / 3.6) as i64).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_aliases() {
        let mut u = json!({"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8,
                           "prompt_tokens_details": {"cached_tokens": 2}, "cache_read_input_tokens": 0});
        assert_eq!(best_cached_tokens(&u), 2);
        normalize_cache_aliases(&mut u);
        assert_eq!(
            crate::pyjson::dumps(&u),
            r#"{"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8, "prompt_tokens_details": {"cached_tokens": 2}, "cache_read_input_tokens": 2, "cached_tokens": 2, "prompt_cache_hit_tokens": 2}"#
        );
    }

    #[test]
    fn estimate() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("hi"), 1);
        assert_eq!(estimate_tokens("你好world"), 2 + 1);
        assert_eq!(estimate_tokens(&"a".repeat(36)), 10);
    }
}
