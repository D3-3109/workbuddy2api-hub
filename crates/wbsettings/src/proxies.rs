use crate::{invalid, text, Error, Result};
use serde_json::{json, Value};
use std::collections::HashSet;

fn clean(raw: &Value) -> Option<Value> {
    if !raw.is_object() || text(raw, "url").trim().is_empty() {
        return None;
    }
    let mut out = raw.clone();
    for key in [
        "id",
        "name",
        "url",
        "ip",
        "country",
        "country_code",
        "ip_type",
        "isp",
        "asn",
    ] {
        out[key] = json!(text(raw, key).trim());
    }
    out["country_code"] = json!(text(&out, "country_code").to_uppercase());
    out["ip_type"] = json!(text(&out, "ip_type").to_lowercase());
    out["enabled"] = json!(raw["enabled"] != false);
    out["probed_at"] = json!(raw["probed_at"]
        .as_u64()
        .or_else(|| raw["probed_at"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0));
    Some(out)
}
fn number(id: &str) -> u64 {
    id.strip_prefix("slot-")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

pub(crate) fn read_all(data: &Value) -> Vec<Value> {
    let Some(stored) = data["proxy_slots"].as_array() else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut ids = HashSet::new();
    let mut seq = 0;
    let mut out = Vec::new();
    for mut entry in stored.iter().filter_map(clean) {
        if !seen.insert(text(&entry, "url").to_owned()) {
            continue;
        }
        let mut id = text(&entry, "id").to_owned();
        if id.is_empty() || ids.contains(&id) {
            loop {
                seq += 1;
                id = format!("slot-{seq}");
                if !ids.contains(&id) {
                    break;
                }
            }
        }
        seq = seq.max(number(&id));
        ids.insert(id.clone());
        entry["id"] = json!(id);
        out.push(entry);
    }
    out
}
fn validate_url(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url).map_err(|_| invalid("proxy URL is invalid"))?;
    if !["http", "https", "socks5", "socks5h"].contains(&parsed.scheme())
        || parsed.host_str().is_none()
    {
        return Err(invalid(
            "proxy URL must use http, https, socks5 or socks5h and have a host",
        ));
    }
    if !parsed.path().is_empty() && parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(invalid("proxy URL cannot have a path, query, or fragment"));
    }
    Ok(())
}
pub(crate) fn save(data: &mut Value, slots: &Value) -> Result<Vec<Value>> {
    let slots = slots
        .as_array()
        .ok_or_else(|| invalid("slots must be a list"))?;
    let stored = read_all(data);
    let mut seq = data["proxy_slot_seq"]
        .as_u64()
        .unwrap_or(0)
        .max(stored.len() as u64);
    for entry in stored.iter().chain(slots.iter()) {
        seq = seq.max(number(text(entry, "id")));
    }
    let mut out = Vec::new();
    let mut urls = HashSet::new();
    let mut ids = HashSet::new();
    for raw in slots {
        let Some(mut entry) = clean(raw) else {
            continue;
        };
        validate_url(text(&entry, "url"))?;
        if raw.get("enabled").is_some_and(|v| !v.is_boolean()) {
            return Err(invalid("proxy enabled must be true or false"));
        }
        if !urls.insert(text(&entry, "url").to_owned()) {
            continue;
        }
        let mut id = text(&entry, "id").to_owned();
        if id.is_empty() || ids.contains(&id) {
            seq = seq
                .checked_add(1)
                .ok_or_else(|| invalid("proxy slot counter exhausted"))?;
            id = format!("slot-{seq}");
        }
        ids.insert(id.clone());
        entry["id"] = json!(id);
        // Retain probe metadata / extension fields omitted by a partial editor.
        if let Some(old) = stored
            .iter()
            .find(|old| old["id"] == entry["id"] && old["url"] == entry["url"])
        {
            let merged = crate::deep_merge(old, raw);
            entry = clean(&merged).unwrap();
            entry["id"] = json!(id);
        }
        out.push(entry);
    }
    data["proxy_slots"] = json!(out);
    data["proxy_slot_seq"] = json!(seq);
    Ok(out)
}
pub(crate) fn update(data: &mut Value, id: &str, fields: &Value) -> Result<Value> {
    let fields = fields
        .as_object()
        .ok_or_else(|| invalid("proxy fields must be an object"))?;
    let mut slots = read_all(data);
    let slot = slots
        .iter_mut()
        .find(|s| s["id"] == id)
        .ok_or_else(|| Error::NotFound("no such proxy slot".into()))?;
    for (field, value) in fields {
        if ![
            "ip",
            "country",
            "country_code",
            "ip_type",
            "isp",
            "asn",
            "probed_at",
        ]
        .contains(&field.as_str())
        {
            return Err(invalid(format!("unsupported proxy probe field: {field}")));
        }
        if field == "probed_at" {
            if !value.is_u64() {
                return Err(invalid("probed_at must be a non-negative integer"));
            }
        } else if !value.is_string() {
            return Err(invalid(format!("{field} must be a string")));
        }
        slot[field] = value.clone();
    }
    *slot = clean(slot).unwrap();
    let updated = slot.clone();
    data["proxy_slots"] = json!(slots);
    Ok(updated)
}
