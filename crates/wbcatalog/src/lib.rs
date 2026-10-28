//! Offline model catalogue and `/v1/models` rendering.
//!
//! [`Catalog::bundled`] is immediately usable without credentials or disk files.
//! The gateway executes [`config_requests`], then passes a successful JSON body
//! to [`Catalog::ingest`]. Failed/empty refreshes leave the last good catalogue
//! intact. [`Catalog::meta`] returns upstream camelCase metadata for chat request
//! normalization; [`Catalog::list`] returns the complete public models envelope.
//!
//! This crate never opens sockets, starts jobs, or reads environment variables.
//! The gateway owns refresh scheduling (300 s TTL / 30 s manual cooldown),
//! account/key availability filters, and synchronization around mutable ingests.
//! Optional models.dev index and output-probe documents can be supplied with
//! [`Catalog::set_limit_index`] and [`Catalog::set_output_probes`].

mod config;
mod entry;
mod tiers;

pub use config::{
    config_requests, curate_remote_catalog, is_chat_model, parse_remote_catalog, ParsedCatalog,
};
pub use entry::{model_entry, model_entry_with, ModelEntryOptions};
pub use wbcore::types::Realm;

use serde_json::{json, Map, Value};
use std::{collections::HashSet, fmt, sync::OnceLock};
use wbcore::pyval::truthy;
use wbcore::timeutil::now;

/// Refresh policy constants for a gateway scheduler; no timers run here.
pub const CACHE_TTL_SECONDS: f64 = 300.0;
pub const REFRESH_COOLDOWN_SECONDS: f64 = 30.0;

/// Ordered `(upstream id, raw upstream metadata)` catalogue.
pub type ModelCatalog = Vec<(String, Value)>;

pub const INTL_UI_ORDER: &[&str] = &[
    "hy4-preview-f",
    "hy3",
    "deepseek-v4.1-flash",
    "gpt-6-astra",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
    "grok-4.7",
    "gemini-3.5-flash",
    "glm-5.3-flash",
    "glm-5.3",
    "glm-5.2",
    "kimi-k3",
    "kimi-k2.6",
    "kimi-k2.8-preview",
];
pub const CN_UI_ORDER: &[&str] = &[
    "hy4-preview-f",
    "hy3",
    "deepseek-v4.1-flash",
    "glm-5.3",
    "glm-5.3-flash",
    "glm-5.2",
    "glm-5.1",
    "glm-5v-turbo",
    "minimax-m3",
    "kimi-k3-1",
    "kimi-k2.8-preview",
    "kimi-k2.7",
    "kimi-k2.6",
    "deepseek-v4-pro",
];

/// Invalid or unusable upstream config. Display text is safe to show to callers:
/// it does not include credentials or echo arbitrary upstream payloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogError {
    InvalidPayload,
    EmptyCatalog,
    UpstreamCode(String),
    HttpStatus(u16),
    InvalidJson,
}
impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPayload => f.write_str("catalogue payload must be a JSON object"),
            Self::EmptyCatalog => f.write_str("upstream returned no usable agent model list"),
            Self::UpstreamCode(code) => write!(f, "upstream catalogue error code {code}"),
            Self::HttpStatus(status) => write!(f, "upstream catalogue HTTP {status}"),
            Self::InvalidJson => f.write_str("upstream catalogue body is not valid JSON"),
        }
    }
}
impl std::error::Error for CatalogError {}

/// Full original snapshots (24 intl / 39 CN rows), including entries which are
/// deliberately not advertised. Extracted verbatim from `wb_catalog.py`.
pub fn bundled_models(realm: Realm) -> &'static [Value] {
    static SNAPSHOT: OnceLock<Value> = OnceLock::new();
    let snapshot = SNAPSHOT.get_or_init(|| {
        serde_json::from_str(include_str!("bundled.json")).expect("valid compiled-in catalogue")
    });
    snapshot[realm.as_str()]
        .as_array()
        .expect("bundled model array")
}

/// Merge reasoning field-by-field. A live bare effort is not a fixed pin when
/// the bundled table says the model has selectable supportedEfforts.
pub fn merge_reasoning(base: &Value, live: &Value) -> Value {
    let mut merged = base.as_object().cloned().unwrap_or_default();
    if let Some(live) = live.as_object() {
        merged.extend(live.clone());
    }
    if merged.get("supportedEfforts").is_some_and(truthy)
        && merged.get("effort").is_some_and(truthy)
    {
        let effort = merged["effort"].clone();
        merged.entry("defaultEffort").or_insert(effort);
        merged.remove("effort");
    }
    Value::Object(merged)
}

/// Overlay live metadata on the snapshot, retaining the curated picker order.
/// `extras` admits live-only names with credits; use false for the legacy narrow
/// endpoint. The live CN paid `hy4-preview` replaces the absent free `-f` alias.
pub fn merge_catalog(primary: &[(String, Value)], realm: Realm, extras: bool) -> ModelCatalog {
    let primary_ids: HashSet<&str> = primary.iter().map(|(id, _)| id.as_str()).collect();
    let replace_hy4 = primary_ids.contains("hy4-preview") && !primary_ids.contains("hy4-preview-f");
    let mut merged = Map::new();
    for item in bundled_models(realm) {
        let Some(mid) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        if (mid == "hy4-preview-f" && replace_hy4) || !is_chat_model(mid) {
            continue;
        }
        merged.entry(mid).or_insert_with(|| item.clone());
    }
    for (mid, meta) in primary {
        if !is_chat_model(mid) {
            continue;
        }
        let base = merged.entry(mid).or_insert_with(|| json!({}));
        if let Some(live) = meta.as_object() {
            let base_reasoning = base.get("reasoning").cloned().unwrap_or(Value::Null);
            let object = base.as_object_mut().expect("catalogue metadata object");
            object.extend(live.clone());
            if meta.get("reasoning").is_some_and(Value::is_object) {
                object.insert(
                    "reasoning".into(),
                    merge_reasoning(&base_reasoning, &meta["reasoning"]),
                );
            }
        }
    }
    let order = if realm == Realm::Cn {
        CN_UI_ORDER
    } else {
        INTL_UI_ORDER
    };
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for &id in order {
        let id = if id == "hy4-preview-f" && replace_hy4 {
            "hy4-preview"
        } else {
            id
        };
        if let Some(meta) = merged.get(id) {
            if seen.insert(id.to_owned()) {
                result.push((id.to_owned(), meta.clone()));
            }
        }
    }
    if extras {
        for (id, _) in primary {
            if seen.contains(id) {
                continue;
            }
            if let Some(meta) = merged.get(id) {
                if meta.get("credits").is_some_and(truthy) {
                    seen.insert(id.clone());
                    result.push((id.clone(), meta.clone()));
                }
            }
        }
    }
    result
}

#[derive(Clone, Debug)]
struct RealmCatalog {
    models: ModelCatalog,
    source: &'static str,
    fetched_at: f64,
    tiers: tiers::Tiers,
}

/// In-memory catalogue, naturally Send + Sync. Mutations require `&mut self`,
/// allowing the gateway to use its existing lock rather than nested locks.
#[derive(Clone, Debug)]
pub struct Catalog {
    realms: [RealmCatalog; 2],
    limit_index: Value,
    output_probes: Value,
}

impl Default for Catalog {
    fn default() -> Self {
        Self::bundled()
    }
}

impl Catalog {
    /// Load curated bundled models; never reads files or the network.
    pub fn bundled() -> Self {
        let fetched_at = now();
        let make = |realm| RealmCatalog {
            models: merge_catalog(&[], realm, false),
            source: "bundled",
            fetched_at,
            tiers: tiers::Tiers::default(),
        };
        Self {
            realms: [make(Realm::Intl), make(Realm::Cn)],
            limit_index: json!({}),
            output_probes: json!({}),
        }
    }

    fn state(&self, realm: Realm) -> &RealmCatalog {
        &self.realms[realm_index(realm)]
    }
    fn state_mut(&mut self, realm: Realm) -> &mut RealmCatalog {
        &mut self.realms[realm_index(realm)]
    }

    /// Raw camelCase metadata, case-insensitive and whitespace-trimmed. Unknown
    /// ids return `{}`. Non-advertised bundled ids remain available to request
    /// normalization (without making those ids appear in `/v1/models`).
    pub fn meta(&self, realm: Realm, model: &str) -> Value {
        let model = model.trim();
        if let Some((_, meta)) = self
            .state(realm)
            .models
            .iter()
            .find(|(id, _)| id.eq_ignore_ascii_case(model))
        {
            return meta.clone();
        }
        bundled_models(realm)
            .iter()
            .find(|meta| {
                meta["id"]
                    .as_str()
                    .is_some_and(|id| id.eq_ignore_ascii_case(model))
            })
            .cloned()
            .unwrap_or_else(|| json!({}))
    }

    /// Ordered raw metadata used by listing and caller-side filtering.
    pub fn entries(&self, realm: Realm) -> &[(String, Value)] {
        &self.state(realm).models
    }

    /// Catalogue provenance. `file_mtime` stays null because desktop cache
    /// discovery is intentionally not part of the Rust port.
    pub fn provenance(&self, realm: Realm) -> Value {
        let state = self.state(realm);
        json!({"source": state.source, "fetched_at": state.fetched_at, "file_mtime": null})
    }

    /// Complete `/v1/models` response, including all compatibility capability
    /// spellings, provenance, optional probe annotations, and PRO entitlements.
    /// Account/key filters belong in the gateway and can retain on `data`.
    pub fn list(&self, realm: Realm) -> Value {
        self.list_at(realm, now())
    }

    /// Deterministic-clock version of [`Self::list`], also useful to render an
    /// entire response with one consistent creation timestamp.
    pub fn list_at(&self, realm: Realm, timestamp: f64) -> Value {
        let state = self.state(realm);
        let data: Vec<Value> = state
            .models
            .iter()
            .map(|(id, meta)| {
                let mut item = model_entry_with(
                    id,
                    meta,
                    ModelEntryOptions {
                        created: timestamp as i64,
                        limit_index: Some(&self.limit_index),
                        output_probes: Some(&self.output_probes),
                    },
                );
                if state.tiers.requires_pro(id) {
                    item["entitlement"] =
                        json!({"source":"catalog", "required":"standard", "at": state.tiers.at});
                }
                item
            })
            .collect();
        json!({"object": "list", "data": data, "realm": realm.as_str(), "catalog": self.provenance(realm)})
    }

    /// Validate and publish one live `/v3/config` JSON response. Parsing must
    /// succeed before either model data, provenance, or tiers are replaced.
    pub fn ingest(&mut self, realm: Realm, payload: Value) -> Result<(), CatalogError> {
        self.ingest_at(realm, payload, now())
    }

    /// Deterministic-clock version of [`Self::ingest`]. Missing/malformed
    /// modelTiers preserves earlier tiers; an explicit empty array clears them.
    pub fn ingest_at(
        &mut self,
        realm: Realm,
        payload: Value,
        timestamp: f64,
    ) -> Result<(), CatalogError> {
        let parsed = parse_remote_catalog(&payload)?;
        let primary = curate_remote_catalog(realm, &parsed.ids, &parsed.metadata)
            .into_iter()
            .map(|id| {
                let meta = parsed
                    .metadata
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                (id, meta)
            })
            .collect::<Vec<_>>();
        let models = merge_catalog(&primary, realm, true);
        let state = self.state_mut(realm);
        state.tiers.ingest(&payload, timestamp);
        state.models = models;
        state.source = "remote";
        state.fetched_at = timestamp;
        Ok(())
    }

    /// Buffered transport response adapter. Errors preserve the last good state.
    pub fn ingest_response(
        &mut self,
        realm: Realm,
        response: &wbcore::types::InResponse,
    ) -> Result<(), CatalogError> {
        if !(200..300).contains(&response.status) {
            return Err(CatalogError::HttpStatus(response.status));
        }
        let payload = response.json().ok_or(CatalogError::InvalidJson)?;
        self.ingest(realm, payload)
    }

    /// Narrow endpoint fallback: only curated whitelist ids are advertised.
    /// Unions every agent's models (unlike v3's CLI preference); no live extras.
    pub fn ingest_endpoint(&mut self, realm: Realm, payload: Value) -> Result<(), CatalogError> {
        let primary = config::parse_endpoint_ids(&payload)?
            .into_iter()
            .map(|id| (id, json!({})))
            .collect::<Vec<_>>();
        let state = self.state_mut(realm);
        state.models = merge_catalog(&primary, realm, false);
        state.source = "endpoint";
        state.fetched_at = now();
        Ok(())
    }

    /// Explicit tier-only ingest for callers that already validated a response.
    pub fn ingest_tiers(&mut self, realm: Realm, payload: &Value) -> bool {
        self.state_mut(realm).tiers.ingest(payload, now())
    }

    /// Whether the last tier map marks this model as paid-only. Unknown models
    /// fail open; priority badges alone never create a gate. Realm-local only.
    pub fn requires_pro(&self, realm: Realm, model: &str) -> bool {
        self.state(realm).tiers.requires_pro(model)
    }

    /// Clone of `{model: "free" | "standard"}` for diagnostics.
    pub fn tiers(&self, realm: Realm) -> Value {
        self.state(realm).tiers.snapshot()
    }
    pub fn tiers_ingested_at(&self, realm: Realm) -> Option<f64> {
        self.state(realm).tiers.at
    }

    /// Supply the decoded `accounts/model.json` index; no disk/network reads.
    /// Values may be `[context, output]` or `{context, output}`.
    pub fn set_limit_index(&mut self, index: Value) {
        self.limit_index = index;
    }

    /// Supply the decoded `accounts/output_probes.json`; only `clamped` values
    /// are used, and are annotations, never overrides to model spec limits.
    pub fn set_output_probes(&mut self, probes: Value) {
        self.output_probes = probes;
    }
}

fn realm_index(realm: Realm) -> usize {
    match realm {
        Realm::Intl => 0,
        Realm::Cn => 1,
    }
}

#[cfg(test)]
mod tests;
