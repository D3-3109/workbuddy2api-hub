use serde_json::{Map, Value};
use wbcore::pyval::{py_str_or_empty, truthy};

#[derive(Clone, Debug, Default)]
pub(crate) struct Tiers {
    map: Map<String, Value>,
    pub at: Option<f64>,
}

impl Tiers {
    pub fn ingest(&mut self, payload: &Value, timestamp: f64) -> bool {
        if !payload.is_object() {
            return false;
        }
        let root = payload
            .get("data")
            .filter(|data| data.is_object())
            .unwrap_or(payload);
        let Some(entries) = root.get("modelTiers").and_then(Value::as_array) else {
            return false;
        };
        let mut map = Map::new();
        for entry in entries {
            if !entry.is_object()
                || entry.get("enabled") == Some(&Value::Bool(false))
                || priority_only(entry)
            {
                continue;
            }
            let raw = entry
                .get("requiredUserType")
                .filter(|value| truthy(value))
                .or_else(|| entry.get("tier"))
                .unwrap_or(&Value::Null);
            let raw = py_str_or_empty(raw).trim().to_lowercase();
            let tier = match raw.as_str() {
                "" => continue,
                "free" | "personal_free_user" | "personal-free-user" => "free",
                _ => "standard",
            };
            let Some(ids) = entry.get("modelIds").and_then(Value::as_array) else {
                continue;
            };
            for id in ids {
                let id = py_str_or_empty(id).trim().to_owned();
                if !id.is_empty() {
                    map.insert(id, Value::String(tier.into()));
                }
            }
        }
        self.map = map;
        self.at = Some(timestamp);
        true
    }

    pub fn requires_pro(&self, model: &str) -> bool {
        self.map.get(model.trim()).and_then(Value::as_str) == Some("standard")
    }

    pub fn snapshot(&self) -> Value {
        Value::Object(self.map.clone())
    }
}

fn priority_only(entry: &Value) -> bool {
    let access = py_str_or_empty(&entry["accessMode"]);
    if access.trim().eq_ignore_ascii_case("exclusive") {
        return false;
    }
    let badge = &entry["badge"];
    let labels = if badge.is_object() {
        vec![&badge["label"], &badge["labelEn"]]
    } else if badge.is_string() {
        vec![badge]
    } else {
        Vec::new()
    };
    labels.into_iter().any(|label| {
        let text = py_str_or_empty(label);
        let text = text.trim();
        text.contains("订阅优先")
            || text.eq_ignore_ascii_case("priority")
            || text.eq_ignore_ascii_case("subscription priority")
    })
}
