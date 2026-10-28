use crate::{bundled_models, CatalogError, Realm};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use wbcore::{
    pyval::{py_int, py_str_or_empty, truthy},
    types::{Cred, Egress, HeaderCase, OutRequest},
};

/// Agent picker ids and metadata extracted from the same upstream document.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedCatalog {
    pub ids: Vec<String>,
    pub metadata: Map<String, Value>,
}

/// Virtual aliases, helper/completion models, and image-only models cannot be
/// directly served through the gateway's chat path.
pub fn is_chat_model(id: &str) -> bool {
    !id.is_empty()
        && !matches!(
            id,
            "lite"
                | "default-model"
                | "fast-model"
                | "balanced-model"
                | "primary-model"
                | "deep-model"
                | "auto"
        )
        && !["codewise-", "completion-"]
            .iter()
            .any(|prefix| id.starts_with(prefix))
        && !["-image-alpha", "-image-alpha-edit", "-taco-completion"]
            .iter()
            .any(|suffix| id.ends_with(suffix))
}

fn agent_list(name: &str, entry: &Value, cli: &mut Vec<Vec<String>>, other: &mut Vec<Vec<String>>) {
    let Some(models) = entry.get("models").and_then(Value::as_array) else {
        return;
    };
    // Object-valued metadata arrays are not picker lists.
    if !models.first().is_some_and(Value::is_string) {
        return;
    }
    let ids = models
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return;
    }
    if name.trim().eq_ignore_ascii_case("cli") {
        cli.push(ids);
    } else {
        other.push(ids);
    }
}

fn agent_lists(root: &Value, cli: &mut Vec<Vec<String>>, other: &mut Vec<Vec<String>>) {
    match root.get("agents") {
        Some(Value::Object(entries)) => {
            for (name, entry) in entries {
                agent_list(name, entry, cli, other);
            }
        }
        Some(Value::Array(entries)) => {
            for entry in entries {
                agent_list(
                    entry.get("name").and_then(Value::as_str).unwrap_or(""),
                    entry,
                    cli,
                    other,
                );
            }
        }
        _ => {}
    }
}

fn nested_agent_lists(root: &Value, cli: &mut Vec<Vec<String>>, other: &mut Vec<Vec<String>>) {
    match root {
        Value::Object(object) => {
            agent_lists(root, cli, other);
            for value in object.values() {
                nested_agent_lists(value, cli, other);
            }
        }
        Value::Array(items) => {
            for value in items {
                nested_agent_lists(value, cli, other);
            }
        }
        _ => {}
    }
}

fn walk_metadata(root: &Value, out: &mut Map<String, Value>) {
    match root {
        Value::Object(object) => {
            if let Some(models) = object.get("models").and_then(Value::as_array) {
                for item in models {
                    let Some(id) = item
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                    else {
                        continue;
                    };
                    out.entry(id).or_insert_with(|| item.clone());
                }
            }
            for value in object.values() {
                walk_metadata(value, out);
            }
        }
        Value::Array(items) => {
            for value in items {
                walk_metadata(value, out);
            }
        }
        _ => {}
    }
}

/// Parse v3 config: prefer a CLI agent over every other agent, then choose its
/// longest bare-string list (first list wins ties); deduplicate preserving order.
/// Accepts agents as arrays or name-keyed maps, with or without `data`. Metadata
/// is collected recursively from product/model arrays; first definition wins.
/// Nested agent envelopes are accepted when the normal root/data shape is absent.
/// A response with metadata but no usable agent ids is not a successful refresh.
pub fn parse_remote_catalog(payload: &Value) -> Result<ParsedCatalog, CatalogError> {
    validate_payload(payload)?;
    let (mut cli, mut other) = (Vec::new(), Vec::new());
    agent_lists(payload, &mut cli, &mut other);
    if let Some(data) = payload.get("data").filter(|data| data.is_object()) {
        agent_lists(data, &mut cli, &mut other);
    }
    if cli.is_empty() && other.is_empty() {
        nested_agent_lists(payload, &mut cli, &mut other);
    }
    let lists = if cli.is_empty() { &other } else { &cli };
    let mut best: Option<&Vec<String>> = None;
    for list in lists {
        if best.is_none_or(|current| list.len() > current.len()) {
            best = Some(list);
        }
    }
    let best = best.ok_or(CatalogError::EmptyCatalog)?;
    let mut seen = HashSet::new();
    let ids = best
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .cloned()
        .collect();
    let mut metadata = Map::new();
    walk_metadata(payload, &mut metadata);
    Ok(ParsedCatalog { ids, metadata })
}

fn validate_payload(payload: &Value) -> Result<(), CatalogError> {
    if !payload.is_object() {
        return Err(CatalogError::InvalidPayload);
    }
    if let Some(code) = payload.get("code").filter(|code| truthy(code)) {
        match py_int(code) {
            Some(0) => {}
            Some(code) => return Err(CatalogError::UpstreamCode(code.to_string())),
            None => return Err(CatalogError::InvalidPayload),
        }
    }
    Ok(())
}

// The legacy endpoint unions every agent, unlike v3's CLI/longest selection.
pub(crate) fn parse_endpoint_ids(payload: &Value) -> Result<Vec<String>, CatalogError> {
    validate_payload(payload)?;
    let root = payload
        .get("data")
        .filter(|data| data.is_object())
        .unwrap_or(payload);
    let agents: Vec<&Value> = match root.get("agents") {
        Some(Value::Array(agents)) => agents.iter().collect(),
        Some(Value::Object(agents)) => agents.values().collect(),
        _ => Vec::new(),
    };
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    for agent in agents {
        if let Some(models) = agent.get("models").and_then(Value::as_array) {
            for id in models.iter().filter_map(Value::as_str).map(str::trim) {
                if !id.is_empty() && seen.insert(id) {
                    ids.push(id.to_owned());
                }
            }
        }
    }
    if ids.is_empty() {
        Err(CatalogError::EmptyCatalog)
    } else {
        Ok(ids)
    }
}

/// Drop virtual/non-chat ids, `-sg` / `-x` builds, and paid siblings when the
/// actual picker lists a free counterpart. Snapshot prices are only fallback:
/// live metadata (including omitted/empty credits) is authoritative.
pub fn curate_remote_catalog(
    _realm: Realm,
    ids: &[String],
    metadata: &Map<String, Value>,
) -> Vec<String> {
    let mut credits = HashMap::new();
    for realm in Realm::ALL {
        for item in bundled_models(realm) {
            if let Some(id) = item["id"].as_str() {
                credits
                    .entry(id.to_owned())
                    .or_insert_with(|| py_str_or_empty(&item["credits"]).trim().to_lowercase());
            }
        }
    }
    for (id, meta) in metadata {
        if meta.is_object() {
            credits.insert(
                id.clone(),
                py_str_or_empty(&meta["credits"]).trim().to_lowercase(),
            );
        }
    }
    let free = |id: &str| {
        credits
            .get(id)
            .is_some_and(|credit| matches!(credit.as_str(), "x0.00" | "x0" | "0" | "0.00"))
    };
    let known: HashSet<&str> = ids.iter().map(String::as_str).collect();
    ids.iter()
        .filter(|id| {
            if !is_chat_model(id) || id.ends_with("-sg") || id.ends_with("-x") {
                return false;
            }
            if let Some(base) = id.strip_suffix("-f") {
                if known.contains(base) && free(base) && !free(id) {
                    return false;
                }
            } else {
                let sibling = format!("{id}-f");
                if known.contains(sibling.as_str()) && free(&sibling) && !free(id) {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect()
}

/// Two ordered, sans-IO GET requests for the gateway to attempt: fixed desktop
/// UA first, fixed billing UA second. Account product/version overrides must not
/// affect config discovery. Uses title-case transport semantics and 10 s timeout.
/// `cn_chat_base` is an optional already-validated HTTP(S) base override; invalid
/// or empty bases fall back to `https://copilot.tencent.com`.
pub fn config_requests(cred: &Cred, egress: Egress, cn_chat_base: Option<&str>) -> Vec<OutRequest> {
    let (base, origin, desktop_ua, billing_ua) = match cred.realm {
        Realm::Intl => (
            "https://www.workbuddy.ai",
            "https://www.workbuddy.ai",
            "WorkBuddy/5.8.4 workbuddy-ai/5.8.4 CLI/2.161.0",
            "WorkBuddy/5.8.4",
        ),
        Realm::Cn => (
            cn_chat_base
                .map(str::trim)
                .map(|base| base.trim_end_matches('/'))
                .filter(|base| valid_base(base))
                .unwrap_or("https://copilot.tencent.com"),
            "https://www.codebuddy.cn",
            "WorkBuddy/5.8.3 WorkBuddy/5.8.3 CLI/2.161.0",
            "WorkBuddy/5.8.3",
        ),
    };
    [desktop_ua, billing_ua]
        .into_iter()
        .map(|ua| {
            let mut headers = vec![
                ("Accept".into(), "application/json, text/plain, */*".into()),
                ("User-Agent".into(), ua.into()),
                ("Origin".into(), origin.into()),
                ("Referer".into(), format!("{origin}/")),
                (
                    "Authorization".into(),
                    format!("Bearer {}", cred.access_token),
                ),
                ("X-User-Id".into(), cred.uid.clone()),
            ];
            if cred.is_api_key() {
                headers.push(("X-API-Key".into(), cred.access_token.clone()));
            }
            OutRequest {
                method: "GET".into(),
                url: format!("{base}/v3/config"),
                headers,
                body: None,
                case: HeaderCase::Title,
                timeout_secs: 10.0,
                egress: egress.clone(),
            }
        })
        .collect()
}

fn valid_base(base: &str) -> bool {
    let Some(rest) = base
        .strip_prefix("https://")
        .or_else(|| base.strip_prefix("http://"))
    else {
        return false;
    };
    !rest.split('/').next().unwrap_or("").is_empty()
        && !base.chars().any(char::is_whitespace)
        && !base.contains(['?', '#'])
}
