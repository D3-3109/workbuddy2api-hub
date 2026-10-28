use crate::pricing::Pricing;
use crate::{add_tokens, bump, number, outcome, scan, string_or, text, truth, Context, TOKENS};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::Path;
use wbcore::{pyval::py_round, timeutil};

fn totals_bucket() -> Value {
    json!({"requests":0,"accounts":{},"cost_cny":0.0})
}
fn feed_bucket(bucket: &mut Value, row: &Value, cost: &Value) {
    bump(bucket, "requests", 1.0);
    for key in TOKENS.into_iter().chain(["credit"]) {
        if row.get(key).is_some() {
            bump(bucket, key, number(row, key));
        }
    }
    let account = text(&row["account"]);
    if !account.is_empty() {
        bump(&mut bucket["accounts"], &account, 1.0);
    }
    if truth(&cost["known"]) {
        bump(bucket, "cost_cny", number(cost, "cny"));
    }
}
pub(crate) fn totals(
    path: &Path,
    ctx: &Context,
    pricing: &Pricing,
    started: f64,
) -> io::Result<Value> {
    let realm = ctx.realm(false);
    let window = ctx.window();
    let mut out = json!({"requests":0,"errors":0,"prompt_tokens":0,"completion_tokens":0,"reasoning_tokens":0,"cached_tokens":0,"total_tokens":0,
        "credit":0.0,"cost_cny":0.0,"cost_missing":{},"started":started,"by_model":{},"by_model_realm":{},"by_model_acct":{},
        "ttft_ms_sum":0,"ttft_samples":0,"gen_ms_sum":0,"gen_samples":0,"wall_ms_sum":0,"wall_samples":0,
        "since":timeutil::local_stamp(started),"log_file":path,"realm":realm.as_deref().unwrap_or("all"),"usd_cny":pricing.fx(),"accounts_map":{},
        "account":{"uid":"","domain":"","issuer":"","credential_file":"","expires_at":0,"accounts":0,"accounts_ready":0}});
    scan::scan(path, |row| {
        if !ctx.matches(&row, &realm, window) {
            return;
        }
        let result = outcome(&row);
        let completed = result == "completed";
        bump(&mut out, if completed { "requests" } else { "errors" }, 1.0);
        if result == "client_aborted" {
            return;
        }
        let model = string_or(&row["model"], "unknown");
        let cost = pricing.cost(&row);
        bump(&mut out, "credit", number(&row, "credit"));
        if truth(&cost["known"]) {
            bump(&mut out, "cost_cny", number(&cost, "cny"));
        } else if !truth(&cost["disabled"]) {
            bump(&mut out["cost_missing"], &model, 1.0);
        }
        if !completed {
            return;
        }
        add_tokens(&mut out, &row);
        let r = ctx.row_realm(&row);
        let account = string_or(&row["account"], "(unattributed)");
        if out["by_model"][&model].is_null() {
            out["by_model"][&model] = totals_bucket();
        }
        feed_bucket(&mut out["by_model"][&model], &row, &cost);
        if out["by_model_realm"][&model][&r].is_null() {
            out["by_model_realm"][&model][&r] = totals_bucket();
        }
        feed_bucket(&mut out["by_model_realm"][&model][&r], &row, &cost);
        if out["by_model_acct"][&model][&r][&account].is_null() {
            out["by_model_acct"][&model][&r][&account] = totals_bucket();
        }
        feed_bucket(&mut out["by_model_acct"][&model][&r][&account], &row, &cost);
    })?;
    for uid in &ctx.account_order {
        let acc = &ctx.accounts[uid];
        out["accounts_map"][uid] = json!({"nickname":string_or(&acc["nickname"],uid),"realm":string_or(&acc["realm"],"intl")});
    }
    if ctx.metadata["account"].is_object() {
        out["account"] = ctx.metadata["account"].clone();
    } else {
        out["account"]["accounts"] = json!(ctx.accounts.len());
    }
    Ok(out)
}

pub(crate) fn recent(path: &Path, ctx: &Context, pricing: &Pricing) -> io::Result<Value> {
    let realm = ctx.realm(false);
    let limit = ctx.integer("limit", 100).clamp(1, 1000) as u64;
    let requested = ctx.integer("page", 1).max(1) as u64;
    let mut total = 0u64;
    let mut valid = 0u64;
    scan::scan_from(path, 0, |row| match row {
        Some(row) if ctx.matches(&row, &realm, (None, None)) => {
            total += 1;
            valid += 1;
        }
        None if realm.is_none() => total += 1,
        _ => (),
    })?;
    let pages = total.div_ceil(limit).max(1);
    let page = requested.min(pages);
    let skip = (page - 1).saturating_mul(limit);
    let end = valid.saturating_sub(skip);
    let start = end.saturating_sub(limit);
    let mut rows = Vec::new();
    let mut i = 0u64;
    scan::scan(path, |mut row| {
        if !ctx.matches(&row, &realm, (None, None)) {
            return;
        }
        if i >= start && i < end {
            let cost = pricing.cost(&row);
            let known = truth(&cost["known"]);
            row["key_name"] = json!(ctx.key_name(&row));
            row["cost_cny"] = if known {
                json!(py_round(number(&cost, "cny"), 6))
            } else {
                Value::Null
            };
            for key in [
                "band",
                "source",
                "source_at",
                "backfilled",
                "rates",
                "unit",
                "currency",
                "usd_cny",
                "or_id",
                "via",
                "inherited_from",
                "override_from",
                "band_note",
                "via_derived",
            ] {
                row[format!("cost_{key}")] = if known {
                    cost[key].clone()
                } else if key == "backfilled" {
                    json!(false)
                } else {
                    Value::Null
                };
            }
            rows.push(row);
        }
        i += 1;
    })?;
    rows.reverse();
    Ok(
        json!({"total":total,"page":page,"limit":limit,"total_pages":pages,"usd_cny":pricing.fx(),"rows":rows}),
    )
}

#[derive(Default)]
struct Perf {
    total: u64,
    success: u64,
    errors: u64,
    aborted: u64,
    metrics: BTreeMap<&'static str, Vec<Value>>,
}
impl Perf {
    fn feed(&mut self, row: &Value) {
        self.total += 1;
        let result = outcome(row);
        if result == "completed" {
            self.success += 1;
            for (input, output) in [
                ("ttft_ms", "ttft_ms"),
                ("gen_ms", "generation_ms"),
                ("elapsed_ms", "wall_ms"),
                ("tokens_per_sec", "tokens_per_sec"),
                ("cache_hit_pct", "cache_hit_pct"),
            ] {
                if !row[input].is_null() && (input != "tokens_per_sec" || truth(&row[input])) {
                    self.metrics
                        .entry(output)
                        .or_default()
                        .push(row[input].clone());
                }
            }
        } else {
            if result == "client_aborted" {
                self.aborted += 1
            } else {
                self.errors += 1
            }
            if truth(&row["elapsed_ms"]) {
                self.metrics
                    .entry("wall_ms")
                    .or_default()
                    .push(row["elapsed_ms"].clone());
            }
        }
    }
    fn finish(mut self, top: bool) -> Value {
        let mut out = json!({"errors":self.errors,"client_aborted":self.aborted,"success_rate_pct":if self.success+self.errors == 0 { Value::Null } else { json!(py_round(self.success as f64*100.0/(self.success+self.errors) as f64,1)) }});
        if top {
            out["sampled"] = json!(self.total);
            out["success"] = json!(self.success);
        } else {
            out["requests"] = json!(self.total);
        }
        for key in [
            "ttft_ms",
            "generation_ms",
            "wall_ms",
            "tokens_per_sec",
            "cache_hit_pct",
        ] {
            let mut samples = self.metrics.remove(key).unwrap_or_default();
            if samples.is_empty() {
                out[key] = Value::Null;
                continue;
            }
            samples.sort_by(|a, b| crate::num(a).total_cmp(&crate::num(b)));
            let count = samples.len();
            let avg = py_round(
                samples.iter().map(crate::num).sum::<f64>() / count as f64,
                1,
            );
            let percentile = |p: f64| {
                samples[(py_round(p * (count - 1) as f64, 0) as usize).min(count - 1)].clone()
            };
            out[key] = json!({"avg":avg,"p50":percentile(0.5),"p90":percentile(0.9),"p99":percentile(0.99),"max":samples[count-1],"samples":count});
        }
        out
    }
}
pub(crate) fn perf(path: &Path, ctx: &Context) -> io::Result<Value> {
    let sample = ctx.integer("sample", 5000).clamp(10, 20000) as usize;
    let mut tail = VecDeque::new();
    scan::scan_from(path, 0, |row| {
        if tail.len() == sample {
            tail.pop_front();
        }
        // Perf needs no messages/fingerprints or other unbounded raw fields.
        // Retain only its small metric projection, even for a 20,000-row sample.
        tail.push_back(row.map(|r| {
            let mut projected = json!({});
            for k in [
                "at",
                "model",
                "realm",
                "account",
                "outcome",
                "error",
                "ttft_ms",
                "gen_ms",
                "elapsed_ms",
                "tokens_per_sec",
                "cache_hit_pct",
            ] {
                if let Some(v) = r.get(k) {
                    projected[k] = v.clone();
                }
            }
            projected
        }));
    })?;
    let capped = tail.len() >= sample;
    let from = tail
        .iter()
        .flatten()
        .next()
        .map(|r| r["at"].clone())
        .unwrap_or(Value::Null);
    let mut overall = Perf::default();
    let mut models: BTreeMap<String, Perf> = BTreeMap::new();
    let mut realms: BTreeMap<(String, String), Perf> = BTreeMap::new();
    let mut accounts: BTreeMap<(String, String, String), Perf> = BTreeMap::new();
    let realm = ctx.realm(false);
    let window = ctx.window();
    for row in tail.into_iter().flatten() {
        if !ctx.matches(&row, &realm, window) {
            continue;
        }
        let model = string_or(&row["model"], "unknown");
        let r = ctx.row_realm(&row);
        let account = string_or(&row["account"], "(unattributed)");
        overall.feed(&row);
        models.entry(model.clone()).or_default().feed(&row);
        realms
            .entry((model.clone(), r.clone()))
            .or_default()
            .feed(&row);
        accounts.entry((model, r, account)).or_default().feed(&row);
    }
    let mut out = overall.finish(true);
    out["sample_from"] = from;
    out["sample_capped"] = json!(capped);
    out["by_model"] = json!({});
    out["by_model_realm"] = json!({});
    out["by_model_acct"] = json!({});
    for (m, p) in models {
        out["by_model"][m] = p.finish(false);
    }
    for ((m, r), p) in realms {
        out["by_model_realm"][m][r] = p.finish(false);
    }
    for ((m, r, a), p) in accounts {
        out["by_model_acct"][m][r][a] = p.finish(false);
    }
    Ok(out)
}

pub(crate) fn timeseries(path: &Path, ctx: &Context) -> io::Result<Value> {
    let realm = ctx.realm(false);
    let (lo, hi) = ctx.window();
    let hi = hi.unwrap_or_else(timeutil::now);
    let lo = lo.unwrap_or(hi - 86400.0);
    let bucket = if !ctx.param("bucket").is_null() {
        ctx.integer("bucket", 0)
    } else {
        ctx.integer("bucket_seconds", 0)
    };
    let step = if bucket != 0 {
        bucket.max(60)
    } else if hi - lo <= 6.0 * 3600.0 {
        60
    } else if hi - lo <= 14.0 * 86400.0 {
        3600
    } else {
        86400
    };
    let origin = match step {
        86400 => timeutil::local_midnight(lo, 0),
        3600 => timeutil::local_hour_start(lo),
        60 => (lo / 60.0).floor() * 60.0,
        _ => lo,
    };
    let mut series: BTreeMap<i64, Value> = BTreeMap::new();
    let mut credits = Vec::new();
    scan::scan(path, |row| {
        if !ctx.matches(&row, &realm, (Some(lo), Some(hi))) {
            return;
        }
        let at = number(&row, "at");
        let k = ((at - origin) / step as f64).floor() as i64;
        let b = series.entry(k).or_insert_with(|| json!({"at":origin+k as f64*step as f64,"requests":0,"errors":0,"prompt_tokens":0,"completion_tokens":0,"reasoning_tokens":0,"cached_tokens":0,"total_tokens":0,"credit":0.0}));
        let result = outcome(&row);
        if result == "completed" {
            bump(b, "requests", 1.0);
            add_tokens(b, &row);
        } else {
            bump(b, "errors", 1.0);
        }
        if result != "client_aborted" {
            bump(b, "credit", number(&row, "credit"));
        }
        if number(&row, "credit") > 0.0 {
            credits.push(json!({"at":row["at"],"iso":row["iso"],"model":row["model"],"account":row["account"],"credit":row["credit"],"total_tokens":row["total_tokens"]}));
            credits.sort_by(|a, b| number(b, "at").total_cmp(&number(a, "at")));
            credits.truncate(50);
        }
    })?;
    Ok(
        json!({"ok":true,"realm":realm.as_deref().unwrap_or("all"),"bucket_seconds":step,"since":lo,"until":hi,"series":series.into_values().collect::<Vec<_>>(),"credits":credits}),
    )
}

pub(crate) fn by_account(path: &Path) -> io::Result<Value> {
    let mut accounts = json!({});
    scan::scan(path, |row| {
        if truth(&row["error"]) {
            return;
        }
        let uid = string_or(&row["account"], "(unattributed)");
        let model = string_or(&row["model"], "?");
        if accounts[&uid].is_null() {
            accounts[&uid] = json!({"account":uid,"requests":0,"prompt_tokens":0,"completion_tokens":0,"reasoning_tokens":0,"cached_tokens":0,"total_tokens":0,"models":{}});
        }
        let b = &mut accounts[&uid];
        bump(b, "requests", 1.0);
        add_tokens(b, &row);
        bump(&mut b["models"], &model, 1.0);
    })?;
    let mut rows: Vec<Value> = accounts.as_object().unwrap().values().cloned().collect();
    for row in &mut rows {
        let mut models: Vec<Value> = row["models"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(m, n)| json!([m, n]))
            .collect();
        models.sort_by(|a, b| crate::num(&b[1]).total_cmp(&crate::num(&a[1])));
        models.truncate(5);
        row["models"] = json!(models);
    }
    rows.sort_by(|a, b| number(b, "total_tokens").total_cmp(&number(a, "total_tokens")));
    Ok(json!({"accounts":rows}))
}
