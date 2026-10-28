use serde_json::{json, Map, Value};
use std::collections::HashSet;
use wbcore::pyval::{py_int, py_str, truthy};

/// Validation error suitable for a gateway HTTP 400 response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatError {
    pub message: String,
    pub param: Option<String>,
}

impl ChatError {
    fn invalid(param: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            param: Some(param.into()),
        }
    }

    pub fn status_code(&self) -> u16 {
        400
    }

    pub fn as_value(&self) -> Value {
        json!({"error": {"message": self.message, "type": "invalid_request_error",
            "param": self.param, "code": 400}})
    }
}

impl std::fmt::Display for ChatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ChatError {}

#[derive(Clone, Debug)]
pub struct NormalizedRequest {
    /// Ready for upstream request encoding; upstream stream is always true.
    pub body: Value,
    /// Model after the single alias rewrite and background routing.
    pub model: String,
    /// Python-compatible truthiness of the original client's stream field.
    pub want_stream: bool,
    pub background_reason: Option<String>,
}

/// Normalize without mutating any input or reading process/file configuration.
///
/// Settings keys: `model_aliases`, `reasoning_effort_map` (default true),
/// `system_prompt`, `prompt: {mode, text}`, `background_model_names` (array or
/// comma-separated string), `background_model_target`, `default_reasoning_effort`
/// (high/catalog/off). The caller resolves env and prompt files into this Value.
/// Catalog accepts a single metadata object, an id-to-metadata map, an array of
/// entries with `id`, or a `/v1/models`-style `{data: [...]}` object. Pass metadata
/// for the effective model if using the single-object form. Missing model and
/// messages retain the baseline defaults; explicitly malformed messages error.
pub fn normalize_request(
    payload: &Value,
    settings: &Value,
    catalog: &Value,
) -> Result<NormalizedRequest, ChatError> {
    let mut body = payload.as_object().cloned().unwrap_or_default();
    let want_stream = body.get("stream").is_some_and(truthy);
    let model = resolve_model(payload, settings)?;
    let background_reason = background_request_reason(payload);
    let meta = catalog_meta(catalog, &model);
    if settings["reasoning_effort_map"] != false {
        map_effort(&mut body, meta);
    }
    let deepseek = model.to_lowercase().starts_with("deepseek");
    let thinking_type = body
        .get("thinking")
        .and_then(|v| v.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let effort_value = effort(&body).to_lowercase();
    let thinking_enabled = deepseek
        && (thinking_type == "enabled" || (thinking_type != "disabled" && effort_value != "none"));
    let mut messages = normalize_messages(body.get("messages"))?;
    apply_prompt_mode(&mut messages, settings);
    let has_trace = messages.iter().any(|m| {
        m.get("reasoning_content").is_some()
            || m["reasoning"].as_str().is_some_and(|s| !s.is_empty())
    });
    if deepseek && (thinking_enabled || has_trace) {
        for m in &mut messages {
            if m["role"] != "assistant" {
                continue;
            }
            if !m["reasoning_content"].is_string() {
                m["reasoning_content"] = json!(m["reasoning"].as_str().unwrap_or(""));
            }
            if !m["reasoning"].as_str().is_some_and(|s| !s.is_empty()) {
                let rc = m["reasoning_content"].as_str().unwrap_or("");
                m["reasoning"] = json!(if rc.is_empty() { " " } else { rc });
            }
        }
    }
    if messages.first().is_none_or(|m| m["role"] != "system") {
        messages.insert(0, json!({"role":"system", "content":settings["system_prompt"].as_str().unwrap_or("You are a helpful assistant.")}));
    }
    body.retain(|k, _| !k.starts_with('_'));
    body.insert("model".into(), json!(model));
    body.insert(
        "messages".into(),
        Value::Array(repair_tool_pairing(messages)),
    );
    if let Some(value) = body.remove("max_completion_tokens") {
        if !body.contains_key("max_tokens") {
            if let Some(n) = py_int(&value).filter(|n| *n > 0) {
                body.insert("max_tokens".into(), json!(n));
            }
        }
    }
    if model.to_lowercase().starts_with("gpt-")
        && body
            .get("max_tokens")
            .and_then(py_int)
            .is_some_and(|n| n < 16)
    {
        body.insert("max_tokens".into(), json!(16));
    }
    normalize_tool_choice(&mut body);
    normalize_tools(&mut body);
    normalize_stop(&mut body);
    if !body.contains_key("max_tokens") {
        let n = meta
            .get("maxOutputTokens")
            .or_else(|| meta.get("max_output_tokens"))
            .or_else(|| meta.get("max_completion_tokens"));
        if let Some(n) = n.and_then(py_int).filter(|n| *n > 0) {
            body.insert("max_tokens".into(), json!(n));
        }
    }
    let disabled = body
        .get("thinking")
        .and_then(|v| v.get("type"))
        .and_then(Value::as_str)
        == Some("disabled");
    if deepseek && !disabled && effort(&body).to_lowercase() != "none" {
        body.entry("thinking")
            .or_insert_with(|| json!({"type":"enabled"}));
        let mode = settings["default_reasoning_effort"]
            .as_str()
            .unwrap_or("high")
            .trim()
            .to_lowercase();
        if effort(&body).is_empty() && !["off", "none", "0", "false", "no"].contains(&mode.as_str())
        {
            let fallback = if ["catalog", "model", "default"].contains(&mode.as_str()) {
                ""
            } else {
                "high"
            };
            let chosen = default_effort(meta).unwrap_or(fallback);
            if !chosen.is_empty() {
                body.insert("reasoning_effort".into(), json!(chosen));
            }
        }
    }
    body.insert("stream".into(), json!(true));
    body.entry("stream_options")
        .or_insert_with(|| json!({"include_usage":true}));
    Ok(NormalizedRequest {
        body: Value::Object(body),
        model,
        want_stream,
        background_reason,
    })
}

pub fn build_upstream_body(
    payload: &Value,
    settings: &Value,
    catalog: &Value,
) -> Result<Value, ChatError> {
    normalize_request(payload, settings, catalog).map(|r| r.body)
}

/// Resolve the model for realm/catalog selection, using exactly the same
/// single alias pass and background routing as `normalize_request`. Keep the
/// original payload unchanged when subsequently normalizing the request.
pub fn resolve_model(payload: &Value, settings: &Value) -> Result<String, ChatError> {
    let mut model = match payload.get("model") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        _ => return Err(ChatError::invalid("model", "model must be a string")),
    };
    apply_model_alias(&mut model, &settings["model_aliases"]);
    let names: Vec<String> = match settings.get("background_model_names") {
        Some(Value::String(s)) => s.split(',').map(|s| s.trim().to_lowercase()).collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(|s| s.trim().to_lowercase())
            .collect(),
        _ => vec![
            "gpt-5.6-terra".into(),
            "gpt-5.6-luna".into(),
            "gpt-5.3-codex".into(),
        ],
    };
    if background_request_reason(payload).is_some() && names.contains(&model.trim().to_lowercase())
    {
        model = settings["background_model_target"]
            .as_str()
            .unwrap_or("deepseek-v4.1-flash")
            .to_string();
    }
    Ok(model)
}

fn apply_model_alias(model: &mut String, aliases: &Value) {
    let key = model.trim().to_lowercase();
    let mut target = None;
    let mut consider = |from: &str, to: &str| {
        let from = from.trim().to_lowercase();
        let to = to.trim();
        if from == key && !from.is_empty() && !to.is_empty() && from != to.to_lowercase() {
            target = Some(to.to_string());
        }
    };
    match aliases {
        Value::Object(m) => {
            for (from, to) in m {
                if let Some(to) = to.as_str() {
                    consider(from, to);
                }
            }
        }
        Value::Array(a) => {
            for item in a {
                let from = ["from", "alias", "name"].iter().find_map(|k| {
                    item.get(*k)
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                });
                let to = ["to", "target", "model"].iter().find_map(|k| {
                    item.get(*k)
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                });
                if let (Some(from), Some(to)) = (from, to) {
                    consider(from, to);
                }
            }
        }
        _ => {}
    }
    if let Some(target) = target {
        *model = target;
    }
}

/// Classify client metadata only; this never inspects or rewrites user content.
pub fn background_request_reason(payload: &Value) -> Option<String> {
    let metadata = payload.get("client_metadata")?.as_object()?;
    let mut fields = Map::new();
    for v in metadata.values() {
        if let Some(s) = v.as_str().filter(|s| s.starts_with('{')) {
            if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(s) {
                for key in [
                    "request_kind",
                    "turn_trigger",
                    "thread_source",
                    "kind",
                    "trigger",
                    "source",
                ] {
                    if let Some(v) = m.get(key).filter(|v| truthy(v)) {
                        fields.insert(key.into(), v.clone());
                    }
                }
            }
        }
    }
    for key in ["request_kind", "turn_trigger", "thread_source"] {
        if let Some(v) = metadata.get(key).filter(|v| truthy(v)) {
            fields.insert(key.into(), v.clone());
        }
    }
    let request_kind = fields
        .get("request_kind")
        .map(py_str)
        .unwrap_or_default()
        .to_lowercase();
    let source = fields
        .get("thread_source")
        .map(py_str)
        .unwrap_or_default()
        .to_lowercase();
    if request_kind.contains("compact")
        && ![
            "memory_consolidation",
            "memory",
            "ambient",
            "suggestion",
            "auto_review",
            "autoreview",
            "title",
        ]
        .contains(&source.as_str())
    {
        return None;
    }
    let joined = fields
        .values()
        .map(py_str)
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    [
        "memory_consolidation",
        "memory",
        "memory-write",
        "memory_write",
        "memorywriting",
        "ambient",
        "suggestion",
        "auto_review",
        "auto-review",
        "autoreview",
        "title",
        "compaction",
        "compact",
        "summariz",
    ]
    .into_iter()
    .find(|keyword| joined.contains(keyword))
    .map(str::to_string)
}

fn catalog_meta<'a>(catalog: &'a Value, model: &str) -> &'a Value {
    if let Some(entries) = catalog
        .as_array()
        .or_else(|| catalog.get("data").and_then(Value::as_array))
    {
        return entries
            .iter()
            .find(|v| {
                v["id"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case(model))
            })
            .unwrap_or(&Value::Null);
    }
    if let Some(map) = catalog.as_object() {
        if let Some((_, v)) = map.iter().find(|(k, _)| k.eq_ignore_ascii_case(model)) {
            return v;
        }
        if map.contains_key("reasoning")
            || map.contains_key("maxOutputTokens")
            || map.contains_key("reasoning_efforts")
            || map.contains_key("max_output_tokens")
            || map.contains_key("id")
        {
            return catalog;
        }
    }
    &Value::Null
}

fn default_effort(meta: &Value) -> Option<&str> {
    meta["reasoning"]["defaultEffort"]
        .as_str()
        .or_else(|| meta["reasoning_default_effort"].as_str())
        .filter(|s| !s.is_empty())
}
fn effort(body: &Map<String, Value>) -> &str {
    body.get("reasoning_effort")
        .filter(|v| truthy(v))
        .or_else(|| body.get("reasoningEffort"))
        .and_then(Value::as_str)
        .unwrap_or("")
}
fn map_effort(body: &mut Map<String, Value>, meta: &Value) {
    let key = if body.contains_key("reasoning_effort") {
        "reasoning_effort"
    } else {
        "reasoningEffort"
    };
    let Some(requested) = body
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    let r = &meta["reasoning"];
    let supported: Vec<&str> = r["supportedEfforts"]
        .as_array()
        .or_else(|| meta["reasoning_efforts"].as_array())
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let pinned = r["effort"]
        .as_str()
        .or_else(|| meta["reasoning_fixed_effort"].as_str())
        .filter(|s| !s.is_empty());
    let fallback = default_effort(meta).or_else(|| {
        if supported.is_empty() {
            pinned
        } else {
            supported.first().copied()
        }
    });
    let disable =
        ["off", "none", "disabled", "disable"].contains(&requested.to_lowercase().as_str());
    let replace = if disable {
        r["canDisableThinking"] == false || meta["reasoning_can_disable"] == false
    } else if !supported.is_empty() {
        !supported.iter().any(|s| s.eq_ignore_ascii_case(requested))
    } else {
        pinned.is_some_and(|s| !s.eq_ignore_ascii_case(requested))
    };
    if replace {
        if let Some(fallback) = fallback {
            body.insert(key.into(), json!(fallback));
        }
    }
}

fn normalize_messages(value: Option<&Value>) -> Result<Vec<Value>, ChatError> {
    let mut messages = match value {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        _ => return Err(ChatError::invalid("messages", "messages must be an array")),
    };
    for (i, m) in messages.iter_mut().enumerate() {
        let param = format!("messages[{i}]");
        let Some(obj) = m.as_object_mut() else {
            return Err(ChatError::invalid(param, "each message must be an object"));
        };
        let Some(role) = obj.get("role").and_then(Value::as_str) else {
            return Err(ChatError::invalid(
                format!("{param}.role"),
                "message role must be a string",
            ));
        };
        if ![
            "system",
            "developer",
            "user",
            "assistant",
            "tool",
            "function",
        ]
        .contains(&role)
        {
            return Err(ChatError::invalid(
                format!("{param}.role"),
                "unsupported message role",
            ));
        }
        if role == "developer" {
            obj.insert("role".into(), json!("system"));
        }
        match obj.get("content") {
            None | Some(Value::Null | Value::String(_)) => {}
            Some(Value::Array(a)) if a.iter().all(|p| p.is_object()) => {}
            _ => {
                return Err(ChatError::invalid(
                    format!("{param}.content"),
                    "message content must be a string, content-part array, or null",
                ))
            }
        }
        if let Some(calls) = obj.get_mut("tool_calls") {
            let Some(calls) = calls.as_array_mut() else {
                return Err(ChatError::invalid(
                    format!("{param}.tool_calls"),
                    "tool_calls must be an array",
                ));
            };
            for call in calls {
                let Some(call) = call.as_object_mut() else {
                    return Err(ChatError::invalid(
                        format!("{param}.tool_calls"),
                        "tool calls must be objects",
                    ));
                };
                let Some(f) = call.get_mut("function").and_then(Value::as_object_mut) else {
                    return Err(ChatError::invalid(
                        format!("{param}.tool_calls"),
                        "tool calls must contain a function object",
                    ));
                };
                if !f.get("name").is_some_and(Value::is_string) {
                    return Err(ChatError::invalid(
                        format!("{param}.tool_calls"),
                        "tool function name must be a string",
                    ));
                }
                let args = match f.get("arguments") {
                    Some(Value::String(s)) => s.clone(),
                    None | Some(Value::Null) => "{}".into(),
                    Some(v) => wbcore::pyjson::dumps_u(v),
                };
                f.insert("arguments".into(), json!(args));
            }
        }
    }
    Ok(messages)
}

fn apply_prompt_mode(messages: &mut Vec<Value>, settings: &Value) {
    let mode = settings["prompt"]["mode"].as_str().unwrap_or("passthrough");
    let Some(text) = settings["prompt"]["text"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
    else {
        return;
    };
    let prompt = json!({"role":"system", "content":text});
    match mode {
        "custom" => {
            messages.retain(|m| m["role"] != "system");
            messages.insert(0, prompt);
        }
        "append" => {
            let i = messages
                .iter()
                .take_while(|m| m["role"] == "system")
                .count();
            messages.insert(i, prompt);
        }
        _ => {}
    }
}

fn calls(m: &Value) -> &[Value] {
    m["tool_calls"].as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn empty_content(m: &Value) -> bool {
    m.get("content").is_none_or(|v| {
        v.is_null() || v.as_str() == Some("") || v.as_array().is_some_and(Vec::is_empty)
    })
}
fn fold_reasoning(previous: &mut Value, current: &Value) {
    if let Some(r) = current["reasoning_content"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        let prev = previous["reasoning_content"].as_str().unwrap_or("");
        previous["reasoning_content"] = json!(if prev.is_empty() {
            r.to_string()
        } else {
            format!("{prev}\n{r}")
        });
    }
}
fn repair_tool_pairing(messages: Vec<Value>) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();
    for m in messages {
        if m["role"] == "assistant" {
            if let Some(prev) = merged
                .last_mut()
                .filter(|p| p["role"] == "assistant" && !calls(p).is_empty())
            {
                if !calls(&m).is_empty() && empty_content(&m) {
                    let more = calls(&m).to_vec();
                    prev["tool_calls"].as_array_mut().unwrap().extend(more);
                    fold_reasoning(prev, &m);
                    continue;
                }
                if m.get("tool_calls").is_none()
                    && m["content"].as_str().is_some_and(|s| !s.is_empty())
                    && empty_content(prev)
                {
                    prev["content"] = m["content"].clone();
                    fold_reasoning(prev, &m);
                    continue;
                }
            }
        }
        merged.push(m);
    }
    let mut repacked = Vec::new();
    let mut i = 0;
    while i < merged.len() {
        let m = &merged[i];
        repacked.push(m.clone());
        i += 1;
        if m["role"] != "assistant" || calls(m).is_empty() {
            continue;
        }
        let ids: HashSet<&str> = calls(m).iter().filter_map(|c| c["id"].as_str()).collect();
        let mut end = i;
        let mut results = Vec::new();
        let mut intruders = Vec::new();
        while end < merged.len() {
            let next = &merged[end];
            if next["role"] == "assistant" {
                break;
            }
            if next["role"] == "tool" {
                if !next["tool_call_id"]
                    .as_str()
                    .is_some_and(|id| ids.contains(id))
                {
                    break;
                }
                results.push(next.clone());
            } else {
                intruders.push(next.clone());
            }
            end += 1;
        }
        if !results.is_empty() && !intruders.is_empty() {
            repacked.extend(results);
            repacked.extend(intruders);
            i = end;
        }
    }
    let declared: HashSet<String> = repacked
        .iter()
        .filter(|m| m["role"] == "assistant")
        .flat_map(calls)
        .filter_map(|c| c["id"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let answered: HashSet<String> = repacked
        .iter()
        .filter(|m| m["role"] == "tool")
        .filter_map(|m| m["tool_call_id"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let keep: HashSet<_> = declared.intersection(&answered).collect();
    repacked.retain_mut(|m| {
        if m["role"] == "tool" {
            return m["tool_call_id"]
                .as_str()
                .is_some_and(|id| keep.iter().any(|k| k.as_str() == id));
        }
        if m["role"] == "assistant" {
            if let Some(a) = m.get_mut("tool_calls").and_then(Value::as_array_mut) {
                a.retain(|c| {
                    c["id"]
                        .as_str()
                        .is_some_and(|id| keep.iter().any(|k| k.as_str() == id))
                });
                if a.is_empty() {
                    m.as_object_mut().unwrap().remove("tool_calls");
                }
            }
        }
        true
    });
    repacked
}

fn normalize_tool_choice(body: &mut Map<String, Value>) {
    let Some(choice) = body.remove("tool_choice") else {
        return;
    };
    let normalized = match choice {
        Value::String(s) => Some(json!(if s.trim().eq_ignore_ascii_case("none") {
            "none".into()
        } else {
            s
        })),
        Value::Object(o) => match o
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase()
            .as_str()
        {
            "none" => Some(json!("none")),
            "auto" => Some(json!("auto")),
            "required" => Some(json!("required")),
            "function" => {
                let name = o
                    .get("function")
                    .and_then(|v| v.get("name"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .or_else(|| o.get("name").and_then(Value::as_str))
                    .unwrap_or("")
                    .trim();
                Some(json!(if name.is_empty() { "auto" } else { name }))
            }
            _ => None,
        },
        _ => None,
    };
    if let Some(v) = normalized {
        body.insert("tool_choice".into(), v);
    }
}
fn normalize_tools(body: &mut Map<String, Value>) {
    if !body.contains_key("tools") {
        if let Some(Value::Array(functions)) = body.remove("functions") {
            body.insert(
                "tools".into(),
                Value::Array(
                    functions
                        .into_iter()
                        .filter(Value::is_object)
                        .map(|f| json!({"type":"function", "function":f}))
                        .collect(),
                ),
            );
        }
    }
    let Some(Value::Array(tools)) = body.remove("tools") else {
        return;
    };
    let mut normalized = Vec::new();
    for tool in tools {
        let Some(mut tool) = tool.as_object().cloned() else {
            continue;
        };
        if tool.contains_key("name") && !tool.contains_key("function") {
            let mut function = Map::new();
            for (key, fallback) in [
                ("name", json!("")),
                ("description", json!("")),
                ("parameters", json!({})),
            ] {
                function.insert(
                    key.into(),
                    tool.get(key)
                        .filter(|v| truthy(v))
                        .cloned()
                        .unwrap_or(fallback),
                );
            }
            if let Some(strict) = tool.get("strict") {
                function.insert("strict".into(), strict.clone());
            }
            tool = Map::new();
            tool.insert("function".into(), Value::Object(function));
        }
        tool.insert("type".into(), json!("function"));
        if let Some(f) = tool.get_mut("function").and_then(Value::as_object_mut) {
            let mut parameters = f
                .get("parameters")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            parameters.insert("type".into(), json!("object"));
            if !parameters.get("properties").is_some_and(Value::is_object) {
                parameters.insert("properties".into(), json!({}));
            }
            let mut parameters = Value::Object(parameters);
            normalize_schema(&mut parameters);
            f.insert("parameters".into(), parameters);
        }
        normalized.push(Value::Object(tool));
    }
    if !normalized.is_empty() {
        body.insert("tools".into(), Value::Array(normalized));
    }
}
fn schema_type(s: &str) -> String {
    match s.trim().to_lowercase().as_str() {
        "str" | "text" => "string".into(),
        "int" => "integer".into(),
        "bool" => "boolean".into(),
        "dict" => "object".into(),
        "list" => "array".into(),
        "float" => "number".into(),
        other => other.into(),
    }
}
fn normalize_schema(schema: &mut Value) {
    let Some(s) = schema.as_object_mut() else {
        return;
    };
    for k in [
        "description",
        "title",
        "format",
        "pattern",
        "items",
        "default",
    ] {
        if s.get(k).is_some_and(Value::is_null) {
            s.remove(k);
        }
    }
    if let Some(ty) = s.remove("type") {
        match ty {
            Value::String(t) => {
                let t = schema_type(&t);
                if [
                    "string", "integer", "boolean", "object", "array", "number", "null",
                ]
                .contains(&t.as_str())
                {
                    s.insert("type".into(), json!(t));
                } else if s.contains_key("properties") {
                    s.insert("type".into(), json!("object"));
                } else if s.contains_key("items") {
                    s.insert("type".into(), json!("array"));
                }
            }
            Value::Array(types) => {
                s.insert(
                    "type".into(),
                    Value::Array(
                        types
                            .iter()
                            .filter(|v| truthy(v))
                            .map(|v| json!(schema_type(&py_str(v))))
                            .collect(),
                    ),
                );
            }
            other => {
                s.insert("type".into(), other);
            }
        }
    }
    if s.get("type").and_then(Value::as_str) == Some("object")
        || (s.contains_key("properties") && !s.contains_key("type"))
    {
        s.insert("type".into(), json!("object"));
        if !s.get("properties").is_some_and(Value::is_object) {
            s.insert("properties".into(), json!({}));
        }
    }
    if let Some(required) = s.remove("required") {
        let values: Vec<Value> = match required {
            Value::Array(a) => a
                .iter()
                .filter(|v| !v.is_null())
                .map(py_str)
                .filter(|s| !s.trim().is_empty())
                .map(Value::String)
                .collect(),
            Value::String(v) if !v.trim().is_empty() => vec![json!(v.trim())],
            _ => Vec::new(),
        };
        if !values.is_empty() {
            s.insert("required".into(), Value::Array(values));
        }
    }
    if let Some(items) = s.get_mut("items") {
        if let Value::Array(a) = items {
            *items = a
                .iter()
                .find(|v| v.is_object())
                .cloned()
                .unwrap_or_else(|| json!({"type":"string"}));
        }
        normalize_schema(items);
    }
    for key in ["properties", "$defs", "definitions"] {
        if let Some(m) = s.get_mut(key).and_then(Value::as_object_mut) {
            for v in m.values_mut() {
                normalize_schema(v);
            }
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(a) = s.get_mut(key).and_then(Value::as_array_mut) {
            for v in a {
                normalize_schema(v);
            }
        }
    }
}
fn normalize_stop(body: &mut Map<String, Value>) {
    let Some(stop) = body.remove("stop") else {
        return;
    };
    let stops = match stop {
        Value::String(s) if !s.is_empty() => vec![json!(s)],
        Value::Array(a) => a
            .into_iter()
            .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
            .collect(),
        _ => Vec::new(),
    };
    if !stops.is_empty() {
        body.insert("stop".into(), Value::Array(stops));
    }
}
