use crate::pricing::Pricing;
use crate::{add_tokens, bump, number, outcome, scan, string_or, text, truth, Context};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use wbcore::pyval::py_round;

fn stat() -> Value {
    json!({"requests":0,"errors":0,"prompt_tokens":0,"completion_tokens":0,"reasoning_tokens":0,"cached_tokens":0,"total_tokens":0,
        "credit":0.0,"cost_cny":0.0,"ttft_sum":0.0,"ttft_n":0,"speed_sum":0.0,"speed_n":0,"elapsed_sum":0.0,"elapsed_n":0})
}
fn entry() -> Value {
    json!({"window":stat(),"all_time":stat(),"window_models":{},"all_models":{}})
}
fn feed(stat: &mut Value, row: &Value, cost: f64) {
    bump(
        stat,
        if outcome(row) == "completed" {
            "requests"
        } else {
            "errors"
        },
        1.0,
    );
    add_tokens(stat, row);
    bump(stat, "credit", number(row, "credit"));
    bump(stat, "cost_cny", cost);
    for (input, sum, n) in [
        ("ttft_ms", "ttft_sum", "ttft_n"),
        ("tokens_per_sec", "speed_sum", "speed_n"),
        ("elapsed_ms", "elapsed_sum", "elapsed_n"),
    ] {
        if truth(&row[input]) {
            bump(stat, sum, number(row, input));
            bump(stat, n, 1.0);
        }
    }
}
fn model_feed(map: &mut Value, row: &Value, cost: f64) {
    if outcome(row) != "completed" {
        return;
    }
    let model = string_or(&row["model"], "(unknown)");
    if map[&model].is_null() {
        map[&model] = json!({"requests":0,"tokens":0,"reasoning":0,"cost_cny":0.0});
    }
    let m = &mut map[&model];
    bump(m, "requests", 1.0);
    bump(m, "tokens", number(row, "total_tokens"));
    bump(m, "reasoning", number(row, "reasoning_tokens"));
    bump(m, "cost_cny", cost);
}
fn feed_entry(entry: &mut Value, row: &Value, cost: f64, window: bool, distribution: bool) {
    feed(&mut entry["all_time"], row, cost);
    if distribution {
        model_feed(&mut entry["all_models"], row, cost);
    }
    if window {
        feed(&mut entry["window"], row, cost);
        if distribution {
            model_feed(&mut entry["window_models"], row, cost);
        }
    }
}
fn finalize(stat: &mut Value) {
    let ratio = |a: &str, b: &str| {
        if number(stat, b) != 0.0 {
            py_round(number(stat, a) / number(stat, b) * 100.0, 1)
        } else {
            0.0
        }
    };
    let cache = ratio("cached_tokens", "prompt_tokens");
    let reasoning = ratio("reasoning_tokens", "completion_tokens");
    stat["cache_hit_pct"] = json!(cache);
    stat["reasoning_ratio"] = json!(reasoning);
    for (sum, n, out, precision) in [
        ("ttft_sum", "ttft_n", "ttft_ms_avg", 0),
        ("speed_sum", "speed_n", "speed_avg", 1),
        ("elapsed_sum", "elapsed_n", "elapsed_ms_avg", 0),
    ] {
        let value = if number(stat, n) == 0.0 {
            0.0
        } else {
            py_round(number(stat, sum) / number(stat, n), precision)
        };
        stat[out] = if precision == 0 {
            json!(value as i64)
        } else {
            json!(value)
        };
    }
}
fn finalize_entry(entry: &mut Value) {
    finalize(&mut entry["window"]);
    finalize(&mut entry["all_time"]);
}
fn rank(a: &Value, b: &Value) -> std::cmp::Ordering {
    number(&b["window"], "total_tokens")
        .total_cmp(&number(&a["window"], "total_tokens"))
        .then_with(|| {
            number(&b["all_time"], "total_tokens")
                .total_cmp(&number(&a["all_time"], "total_tokens"))
        })
}
fn key_entry() -> Value {
    let mut e = entry();
    e["realms"] = json!({});
    e["last_at"] = json!(0.0);
    e
}
fn sum_into(target: &mut Value, from: &Value) {
    if let Some(fields) = from.as_object() {
        for (key, n) in fields {
            bump(target, key, crate::num(n));
        }
    }
}
fn merge_unknown(target: &mut Value, from: &Value) {
    for k in ["window", "all_time", "realms"] {
        sum_into(&mut target[k], &from[k]);
    }
    target["last_at"] = json!(number(target, "last_at").max(number(from, "last_at")));
    for k in ["window_models", "all_models"] {
        if let Some(models) = from[k].as_object() {
            for (m, stat) in models {
                if target[k][m].is_null() {
                    target[k][m] = json!({"requests":0,"tokens":0,"reasoning":0});
                }
                for field in ["requests", "tokens", "reasoning"] {
                    bump(&mut target[k][m], field, number(stat, field));
                }
            }
        }
    }
}
fn decorate_key(
    mut e: Value,
    id: &str,
    name: &str,
    realm: &str,
    enabled: bool,
    source: &str,
) -> Value {
    e["key"] = json!(id);
    e["name"] = json!(name);
    e["realm"] = json!(realm);
    e["enabled"] = json!(enabled);
    e["source"] = json!(source);
    let mut models: Vec<Value> = e["window_models"].as_object().unwrap().iter().map(|(m,s)| json!({"model":m,"requests":s["requests"],"tokens":s["tokens"],"reasoning":s["reasoning"]})).collect();
    models.sort_by(|a, b| {
        number(b, "tokens")
            .total_cmp(&number(a, "tokens"))
            .then_with(|| number(b, "requests").total_cmp(&number(a, "requests")))
            .then_with(|| text(&a["model"]).cmp(&text(&b["model"])))
    });
    let other = if models.len() > 5 {
        let mut other = json!({"model":"(其他)","other":true,"models":models.len()-5,"requests":0,"tokens":0,"reasoning":0});
        for m in &models[5..] {
            for k in ["requests", "tokens", "reasoning"] {
                bump(&mut other, k, number(m, k));
            }
        }
        other
    } else {
        Value::Null
    };
    models.truncate(5);
    e["models"] = json!(models);
    e["models_other"] = other;
    e["cross_realm"] = json!(
        e["realms"]
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| !k.is_empty())
            .count()
            > 1
    );
    finalize_entry(&mut e);
    e
}

pub(crate) fn query(path: &Path, ctx: &Context, pricing: &Pricing) -> io::Result<Value> {
    let realm = ctx.realm(true);
    let window = ctx.window();
    let mut summary = json!({"window":stat(),"all_time":stat()});
    let mut accounts = json!({});
    let mut models = json!({});
    let mut keys = json!({});
    scan::scan(path, |row| {
        if outcome(&row) == "client_aborted" || !ctx.matches(&row, &realm, (None, None)) {
            return;
        }
        let in_window = ctx.matches(&row, &None, window);
        let cost = number(&pricing.cost(&row), "cny");
        feed_entry(&mut summary, &row, cost, in_window, false);
        let uid = string_or(&row["account"], "(unattributed)");
        let model = string_or(&row["model"], "(unknown)");
        let key = if row.get("key").is_none() {
            "__before_keys__".to_string()
        } else {
            string_or(&row["key"], "__no_key__")
        };
        if accounts[&uid].is_null() {
            let mut e = entry();
            e["uid"] = json!(uid);
            e["nickname"] = json!(uid);
            e["domain"] = json!("");
            e["realm"] = json!(text(&row["realm"]));
            accounts[&uid] = e;
        }
        feed_entry(&mut accounts[&uid], &row, cost, in_window, true);
        if models[&model].is_null() {
            models[&model] = json!({"model":model,"window":stat(),"all_time":stat()});
        }
        feed_entry(&mut models[&model], &row, cost, in_window, false);
        if keys[&key].is_null() {
            keys[&key] = key_entry();
        }
        feed_entry(&mut keys[&key], &row, cost, in_window, true);
        bump(&mut keys[&key]["realms"], &ctx.row_realm(&row), 1.0);
        keys[&key]["last_at"] = json!(number(&keys[&key], "last_at").max(number(&row, "at")));
    })?;
    Ok(assemble(ctx, pricing, summary, accounts, models, keys))
}

/// Everything after the per-row fold: metadata accounts, ranking and key rows.
pub(crate) fn assemble(
    ctx: &Context,
    pricing: &Pricing,
    mut summary: Value,
    mut accounts: Value,
    models: Value,
    keys: Value,
) -> Value {
    let realm = ctx.realm(true);
    let window = ctx.window();
    for uid in &ctx.account_order {
        let acc = &ctx.accounts[uid];
        let r = string_or(&acc["realm"], "intl");
        if realm.as_ref().map(|filter| *filter != r).unwrap_or(false) {
            continue;
        }
        if accounts[uid].is_null() {
            let mut e = entry();
            e["uid"] = json!(uid);
            accounts[uid] = e;
        }
        accounts[uid]["nickname"] = json!(string_or(&acc["nickname"], uid));
        accounts[uid]["realm"] = json!(r);
        accounts[uid]["domain"] = json!(text(&acc["domain"]));
        accounts[uid]["credits"] = acc
            .get("credits")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(json!({}));
    }
    let mut account_rows: Vec<Value> = accounts.as_object().unwrap().values().cloned().collect();
    let mut model_rows: Vec<Value> = models.as_object().unwrap().values().cloned().collect();
    for row in account_rows.iter_mut().chain(model_rows.iter_mut()) {
        finalize_entry(row);
    }
    account_rows.sort_by(rank);
    model_rows.sort_by(rank);
    finalize_entry(&mut summary);
    let mut key_rows = Vec::new();
    let mut known = BTreeSet::new();
    for configured in ctx.keys() {
        let id = text(&configured["id"]);
        if id.is_empty() {
            continue;
        }
        let bound = text(&configured["realm"]);
        if !bound.is_empty() && realm.as_ref().map(|r| *r != bound).unwrap_or(false) {
            continue;
        }
        known.insert(id.clone());
        let enabled = configured["enabled"] != json!(false);
        let e = keys.get(&id).cloned().unwrap_or_else(key_entry);
        if !enabled
            && number(&e["window"], "requests") == 0.0
            && number(&e["window"], "errors") == 0.0
        {
            continue;
        }
        key_rows.push(decorate_key(
            e,
            &id,
            &string_or(&configured["name"], &id),
            &bound,
            enabled,
            "panel",
        ));
    }
    for (id, name, source, enabled) in [
        ("launcher", "启动参数", "launcher", true),
        ("__before_keys__", "(切换前)", "bucket", false),
        ("__no_key__", "(无 key)", "bucket", false),
    ] {
        if known.contains(id) {
            continue;
        }
        known.insert(id.into());
        if let Some(e) = keys.get(id) {
            key_rows.push(decorate_key(e.clone(), id, name, "", enabled, source));
        }
    }
    let mut unknown = key_entry();
    let mut has_unknown = false;
    for (id, e) in keys.as_object().unwrap() {
        if !known.contains(id) {
            merge_unknown(&mut unknown, e);
            has_unknown = true;
        }
    }
    if has_unknown {
        key_rows.push(decorate_key(
            unknown,
            "__unknown_key__",
            "(未知 key)",
            "",
            false,
            "bucket",
        ));
    }
    key_rows.sort_by(|a, b| rank(a, b).then_with(|| text(&a["name"]).cmp(&text(&b["name"]))));
    json!({"window":{"since":window.0,"until":window.1},"realm":realm.as_deref().unwrap_or("all"),"usd_cny":pricing.fx(),
        "summary":summary,"accounts":account_rows,"models":model_rows,"keys":key_rows})
}
