use crate::{append_json, invalid, num, number, scan, string_or, text, truth};
use chrono::{Datelike, TimeZone, Timelike, Utc};
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use wbcore::{pyjson, pyval::py_round, timeutil};

const BUILTIN: &str = include_str!("../../../assets/pricing.json");
const DETAILS: [&str; 12] = [
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
    "markup",
    "markup_from",
];

/// Python-compatible SHA-1 policy identifier. Audit/display fields deliberately
/// do not enter this hash; historical usage rows depend on this exact definition.
pub fn policy_id(model: &str, flat: &Value, bands: &Value, usd_cny: f64) -> String {
    let payload = json!({"model":model,"flat":flat,"bands":if truth(bands) { bands.clone() } else { Value::Null },"usd_cny":py_round(usd_cny,6)});
    hex::encode(Sha1::digest(pyjson::dumps_sorted(&payload).as_bytes()))[..12].to_string()
}

pub(crate) struct Pricing {
    snapshot: Value,
    policies: BTreeMap<String, Value>,
    timeline: Vec<Value>,
    overrides: Value,
    markup: Value,
    enabled: bool,
}
fn read_json(path: &Path) -> Option<Value> {
    let meta = fs::metadata(path).ok()?;
    if meta.len() > 32 * 1024 * 1024 {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}
impl Pricing {
    pub fn load(dir: &Path, snapshot_file: Option<&Path>, enabled: bool) -> io::Result<Self> {
        let snapshot = snapshot_file
            .and_then(read_json)
            .filter(|v| v["models"].is_object())
            .unwrap_or_else(|| serde_json::from_str(BUILTIN).expect("bundled pricing snapshot"));
        let mut policies = BTreeMap::new();
        scan::scan(&dir.join("pricing-policies.jsonl"), |r| {
            let id = text(&r["id"]);
            if !id.is_empty() {
                policies.insert(id, r);
            }
        })?;
        let mut timeline = Vec::new();
        scan::scan(&dir.join("pricing-timeline.jsonl"), |r| {
            if r["policies"].is_object() {
                timeline.push(r);
            }
        })?;
        timeline.sort_by(|a, b| number(a, "at").total_cmp(&number(b, "at")));
        Ok(Self {
            snapshot,
            policies,
            timeline,
            overrides: read_json(&dir.join("pricing-overrides.json")).unwrap_or(json!({})),
            markup: read_json(&dir.join("pricing-markup.json")).unwrap_or(json!({})),
            enabled,
        })
    }
    pub fn fx(&self) -> f64 {
        let n = number(&self.snapshot["meta"], "usd_cny");
        if n == 0.0 {
            1.0
        } else {
            n
        }
    }
    fn assignment(&self) -> Value {
        self.timeline
            .last()
            .map(|t| t["policies"].clone())
            .unwrap_or(json!({}))
    }
    pub fn current_id(&self, model: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }
        self.assignment()[model]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
    fn policy_ref_at(&self, at: f64, model: &str) -> (Option<String>, bool) {
        if self.timeline.is_empty() {
            return (None, false);
        }
        let idx = self
            .timeline
            .partition_point(|t| number(t, "at") <= at)
            .saturating_sub(1);
        let reference = |i: usize| {
            self.timeline[i]["policies"][model]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        if let Some(pid) = reference(idx) {
            return (Some(pid), false);
        }
        for (i, t) in self.timeline.iter().enumerate() {
            if number(t, "at") >= at {
                if let Some(pid) = reference(i) {
                    return (Some(pid), true);
                }
            }
        }
        for i in (0..=idx).rev() {
            if let Some(pid) = reference(i) {
                return (Some(pid), true);
            }
        }
        (None, false)
    }
    pub fn cost(&self, row: &Value) -> Value {
        let mut result = json!({"cny":0.0,"known":false,"band":null,"source":"builtin","source_at":null,"backfilled":false});
        for key in DETAILS {
            result[key] = Value::Null;
        }
        if !self.enabled || !truth(row) {
            result["disabled"] = json!(true);
            return result;
        }
        let model = text(&row["model"]);
        let (pid, backfilled) = match row["cost_policy"].as_str().filter(|s| !s.is_empty()) {
            Some(s) => (Some(s.to_string()), false),
            None => self.policy_ref_at(number(row, "at"), &model),
        };
        let policy = pid.as_ref().and_then(|p| self.policies.get(p));
        let (entry, fx) = if let Some(p) = policy {
            result["source"] = json!(pid.unwrap());
            result["source_at"] = p["first_seen"].clone();
            result["backfilled"] = json!(backfilled);
            (
                p,
                if number(p, "usd_cny") == 0.0 {
                    7.1
                } else {
                    number(p, "usd_cny")
                },
            )
        } else {
            (&self.snapshot["models"][&model], self.fx())
        };
        if !entry.is_object() {
            return result;
        }
        let (rates, band) = band_for(entry, number(row, "at"), number(row, "prompt_tokens"));
        if !rates.is_object() {
            return result;
        }
        let prompt = number(row, "prompt_tokens");
        let cached = number(row, "cached_tokens").min(prompt);
        let miss = number(rates, "input_cache_miss");
        let hit = if rates["input_cache_hit"].is_null() {
            miss
        } else {
            number(rates, "input_cache_hit")
        };
        let output = number(rates, "output");
        let unit = if number(entry, "unit") == 0.0 {
            1e6
        } else {
            number(entry, "unit")
        };
        let currency = string_or(&entry["currency"], "USD");
        let factor = if currency.eq_ignore_ascii_case("USD") {
            fx
        } else {
            1.0
        };
        let markup = num(&self.markup[&model]);
        let markup = if markup > 0.0 { markup } else { 1.0 };
        result["cny"] = json!(
            ((prompt - cached) * miss + cached * hit + number(row, "completion_tokens") * output)
                / unit
                * factor
                * markup
        );
        result["known"] = json!(true);
        result["band"] = json!(band);
        result["rates"] = rates.clone();
        result["unit"] = json!(unit);
        result["currency"] = json!(currency);
        result["usd_cny"] = json!(fx);
        result["or_id"] = entry["or_id"].clone();
        let recorded = text(&entry["via"]);
        if ["direct", "variant", "override"].contains(&recorded.as_str()) {
            result["via"] = json!(recorded);
            result["inherited_from"] = entry["inherited_from"].clone();
            result["override_from"] = entry["override_from"].clone();
            result["via_derived"] = json!(false);
        } else {
            let target = string_or(
                &self.overrides[&model],
                source_override(&model).unwrap_or(""),
            );
            let overridden = !target.is_empty() && target == text(&entry["or_id"]);
            result["via"] = json!(if overridden { "override" } else { "direct" });
            result["via_derived"] = json!(overridden);
            if overridden {
                result["override_from"] = json!(model);
            }
        }
        result["band_note"] = band
            .map(|i| band_note(&entry["bands"][i]))
            .unwrap_or(Value::Null);
        result["markup"] = json!(markup);
        if markup != 1.0 {
            result["markup_from"] = json!("pricing-markup.json");
        }
        result
    }
    pub fn record(mut self, dir: &Path, doc: &Value, at: f64, merge: bool) -> io::Result<Value> {
        if !at.is_finite() {
            return Err(invalid("policy timestamp must be finite"));
        }
        let models = doc["models"]
            .as_object()
            .ok_or_else(|| invalid("pricing snapshot requires a models object"))?;
        fs::create_dir_all(dir)?;
        let old = self.assignment();
        let mut assignment = if merge { old.clone() } else { json!({}) };
        let fx = if number(&doc["meta"], "usd_cny") == 0.0 {
            7.1
        } else {
            number(&doc["meta"], "usd_cny")
        };
        let mut added = 0;
        for (model, entry) in models {
            if !entry.is_object() {
                return Err(invalid(format!("pricing model {model} must be an object")));
            }
            let pid = policy_id(model, &entry["flat"], &entry["bands"], fx);
            if !self.policies.contains_key(&pid) {
                let policy = json!({"id":pid,"model":model,"or_id":entry["or_id"],"currency":string_or(&entry["currency"],"USD"),
                    "unit":entry.get("unit").cloned().unwrap_or(json!(1000000)),"usd_cny":fx,"flat":entry["flat"],
                    "bands":if truth(&entry["bands"]) {entry["bands"].clone()} else {Value::Null},
                    "via":string_or(&entry["via"],"direct"),"inherited_from":entry["inherited_from"],"override_from":entry["override_from"],"first_seen":at});
                append_json(&dir.join("pricing-policies.jsonl"), &policy)?;
                self.policies.insert(pid.clone(), policy);
                added += 1;
            }
            assignment[model] = json!(pid);
        }
        let changed = assignment != old;
        if changed {
            append_json(
                &dir.join("pricing-timeline.jsonl"),
                &json!({"at":at,"policies":assignment}),
            )?;
        }
        Ok(json!({"new_policies":added,"timeline_changed":changed,"assignment":assignment}))
    }
    pub fn status(&self, dir: &Path) -> Value {
        let assignment = self.assignment();
        let mut current = Map::new();
        let mut models: Vec<_> = assignment
            .as_object()
            .into_iter()
            .flat_map(|m| m.keys())
            .collect();
        models.sort();
        for model in models {
            let pid = text(&assignment[model]);
            let entry = self.policies.get(&pid).unwrap_or(&Value::Null);
            current.insert(model.clone(), json!({"id":pid,"label":label(&entry["first_seen"]),"via":string_or(&entry["via"],"direct"),"inherited_from":entry["inherited_from"]}));
        }
        let at = self
            .timeline
            .last()
            .map(|t| t["at"].clone())
            .unwrap_or(Value::Null);
        json!({"interval_minutes":0.0,"enabled":false,"master_enabled":self.enabled,"running":false,
            "last_run":null,"last_run_label":null,"next_run":null,"next_run_label":null,"last_error":null,
            "last_models":0,"last_extra":0,"policies":self.policies.len(),"models":current.len(),"current_at":at,"current_at_label":label(&at),
            "current":current,"gaps":[],"gap_summary":{"total":0,"or_missing":0,"variant_unmatched":0,"aliases":0,"variants_enabled":true},
            "variant_inherit":true,"overrides_file":dir.join("pricing-overrides.json"),"policies_file":dir.join("pricing-policies.jsonl"),
            "timeline":dir.join("pricing-timeline.jsonl"),"logs":[],"msg":"Remote pricing refresh is unsupported; file-only pricing is active","refresh_supported":false})
    }
}
fn label(at: &Value) -> Value {
    if at.is_null() {
        Value::Null
    } else {
        json!(timeutil::fmt_local(num(at), "%Y-%m-%d %H:%M"))
    }
}
fn band_for(entry: &Value, at: f64, prompt: f64) -> (&Value, Option<usize>) {
    let mut selected = (&entry["flat"], None);
    if let Some(bands) = entry["bands"].as_array() {
        let dt = Utc.timestamp_opt(at as i64, 0).single();
        for (i, band) in bands.iter().enumerate() {
            if !band["flat"].is_object() {
                continue;
            }
            let applies = if band.get("min_prompt_tokens").is_some() {
                prompt >= number(band, "min_prompt_tokens")
            } else if let Some(dt) = dt {
                let hhmm = dt.hour() * 100 + dt.minute();
                let day = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
                    [dt.weekday().num_days_from_monday() as usize];
                let days_ok = band["days"]
                    .as_array()
                    .filter(|a| !a.is_empty())
                    .map(|a| a.iter().any(|d| d.as_str() == Some(day)))
                    .unwrap_or(true);
                let end = if number(band, "end") == 0.0 {
                    2400.0
                } else {
                    number(band, "end")
                };
                days_ok && hhmm as f64 >= number(band, "start") && (hhmm as f64) < end
            } else {
                false
            };
            if applies {
                selected = (&band["flat"], Some(i));
            }
        }
    }
    selected
}
fn comma(n: i64) -> String {
    let digits = n.to_string();
    let len = digits.len();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}
fn band_note(band: &Value) -> Value {
    if band.get("min_prompt_tokens").is_some() {
        return json!(format!(
            "输入长度 ≥ {} tokens",
            comma(number(band, "min_prompt_tokens") as i64)
        ));
    }
    let names = |day: &str| {
        match day {
            "mon" => "周一",
            "tue" => "周二",
            "wed" => "周三",
            "thu" => "周四",
            "fri" => "周五",
            "sat" => "周六",
            "sun" => "周日",
            _ => day,
        }
        .to_string()
    };
    let prefix = band["days"]
        .as_array()
        .filter(|a| !a.is_empty())
        .map(|a| {
            format!(
                "每周 {}",
                a.iter()
                    .map(|v| names(v.as_str().unwrap_or("")))
                    .collect::<Vec<_>>()
                    .join("/")
            )
        })
        .unwrap_or("每天".into());
    let start = number(band, "start") as i64;
    let end = if number(band, "end") == 0.0 {
        2400
    } else {
        number(band, "end") as i64
    };
    json!(format!(
        "{prefix} {:02}:{:02}–{:02}:{:02} UTC",
        start / 100,
        start % 100,
        end / 100,
        end % 100
    ))
}
fn source_override(model: &str) -> Option<&'static str> {
    Some(match model {
        "hy4-preview-f" | "hy4-preview-dev" | "hy4-preview-x" => "tencent/hy4-preview",
        "hy3-x" => "tencent/hy3",
        "kimi-k3-1" => "moonshotai/kimi-k3",
        "kimi-k2.7" => "moonshotai/kimi-k2.7-code",
        "glm-5.0-turbo" => "z-ai/glm-5-turbo",
        "deepseek-v3-1" | "deepseek-v3-1-volc" => "deepseek/deepseek-chat-v3.1",
        "deepseek-v3-2-volc" => "deepseek/deepseek-v3.2",
        "deepseek-v3-0324" => "deepseek/deepseek-chat-v3-0324",
        "hunyuan-2.0-instruct" | "hunyuan-chat" => "tencent/hunyuan-a13b-instruct",
        _ => return None,
    })
}
