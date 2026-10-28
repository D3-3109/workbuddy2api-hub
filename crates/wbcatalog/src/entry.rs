use serde_json::{json, Value};
use wbcore::{
    pyval::{py_int, truthy},
    timeutil::now,
};

/// Optional offline enrichment documents; no references are retained.
#[derive(Clone, Copy, Debug)]
pub struct ModelEntryOptions<'a> {
    pub created: i64,
    /// Decoded accounts/model.json: id -> [context, output] or {context, output}.
    pub limit_index: Option<&'a Value>,
    /// Decoded accounts/output_probes.json: id -> {clamped: integer, ...}.
    pub output_probes: Option<&'a Value>,
}
impl Default for ModelEntryOptions<'_> {
    fn default() -> Self {
        Self {
            created: now() as i64,
            limit_index: None,
            output_probes: None,
        }
    }
}

/// Render all OpenAI/OpenRouter/LobeChat-compatible fields from raw camelCase
/// metadata. Unknown contexts use 1M; unknown output limits are omitted.
pub fn model_entry(id: &str, meta: &Value) -> Value {
    model_entry_with(id, meta, ModelEntryOptions::default())
}

/// Render with an explicit clock and optional offline limits/probe enrichment.
/// Measured output clamps annotate the result without overriding the spec limit.
pub fn model_entry_with(id: &str, meta: &Value, options: ModelEntryOptions<'_>) -> Value {
    let mut item =
        json!({"id": id, "object": "model", "created": options.created, "owned_by": "workbuddy"});
    copy_truthy(&mut item, "name", &meta["name"]);
    let description = if truthy(&meta["descriptionEn"]) {
        &meta["descriptionEn"]
    } else {
        &meta["descriptionZh"]
    };
    copy_truthy(&mut item, "description", description);

    let mut vision = truthy(&meta["supportsImages"]) && !truthy(&meta["disabledMultimodal"]);
    if id == "glm-5.3-flash" {
        vision = false;
        item["vision_note"] = json!("上游宣告多模态但实际通道不支持读图，已修正为纯文本");
    }
    let tools = truthy(&meta["supportsToolCall"]);
    let reasoning = truthy(&meta["supportsReasoning"]);
    let inputs = if vision {
        json!(["text", "image"])
    } else {
        json!(["text"])
    };
    item["capabilities"] = json!({"vision": vision, "tool_calls": tools, "reasoning": reasoning});
    for key in ["supports_vision", "supports_images", "vision", "multimodal"] {
        item[key] = json!(vision);
    }
    item["supports_tool_calls"] = json!(tools);
    item["supports_reasoning"] = json!(reasoning);
    item["abilities"] = json!({"vision": vision, "functionCall": tools, "function_call": tools, "reasoning": reasoning});
    item["input_modalities"] = inputs.clone();
    item["output_modalities"] = json!(["text"]);
    item["modalities"] = json!({"input": inputs, "output": ["text"]});
    item["architecture"] = json!({
        "input_modalities": inputs,
        "output_modalities": ["text"],
        "modality": if vision { "text+image->text" } else { "text->text" },
    });

    let (context, output) = limits(id, meta, options.limit_index);
    item["context_length"] = json!(context);
    item["max_input_tokens"] = json!(context);
    if let Some(output) = output {
        item["max_output_tokens"] = json!(output);
        item["max_completion_tokens"] = json!(output);
    }
    if let Some(clamp) = options
        .output_probes
        .and_then(|probes| positive(&probes[id]["clamped"]))
    {
        item["output_clamp"] = json!(clamp);
        item["max_output_tokens_clamped"] = json!(clamp);
    }
    copy_truthy(
        &mut item,
        "context_windows",
        &meta["contextWindow"]["supportedLengths"],
    );
    for (raw, public) in [
        ("supportedEfforts", "reasoning_efforts"),
        ("effort", "reasoning_fixed_effort"),
        ("defaultEffort", "reasoning_default_effort"),
    ] {
        copy_truthy(&mut item, public, &meta["reasoning"][raw]);
    }
    let can_disable = &meta["reasoning"]["canDisableThinking"];
    if !can_disable.is_null() {
        item["reasoning_can_disable"] = can_disable.clone();
    }
    if !meta["onlyReasoning"].is_null() {
        item["always_reasoning"] = json!(truthy(&meta["onlyReasoning"]));
    }
    for key in ["credits", "vendor"] {
        copy_truthy(&mut item, key, &meta[key]);
    }
    for key in ["temperature", "top_p"] {
        if !meta[key].is_null() {
            item[key] = meta[key].clone();
        }
    }
    if truthy(&meta["isDefault"]) {
        item["is_default"] = json!(true);
    }
    if let Some(tags) = meta["tags"].as_array() {
        let tags: Vec<&str> = tags
            .iter()
            .filter_map(Value::as_str)
            .filter(|tag| !tag.starts_with("badge:"))
            .collect();
        if !tags.is_empty() {
            item["tags"] = json!(tags);
        }
    }
    item
}

fn copy_truthy(item: &mut Value, key: &str, value: &Value) {
    if truthy(value) {
        item[key] = value.clone();
    }
}
fn positive(value: &Value) -> Option<i64> {
    py_int(value).filter(|value| *value > 0)
}
fn bare_id(id: &str) -> String {
    id.trim().rsplit('/').next().unwrap_or("").to_lowercase()
}

fn limits(id: &str, meta: &Value, index: Option<&Value>) -> (i64, Option<i64>) {
    let key = bare_id(id);
    let table = fallback(&key);
    let cached = index.and_then(Value::as_object).and_then(|index| {
        index.get(&key).or_else(|| {
            index
                .iter()
                .find(|(id, _)| bare_id(id) == key)
                .map(|(_, value)| value)
        })
    });
    let cached = match cached {
        Some(Value::Array(pair)) if pair.len() == 2 => (positive(&pair[0]), positive(&pair[1])),
        Some(value) if value.is_object() => {
            (positive(&value["context"]), positive(&value["output"]))
        }
        _ => (None, None),
    };
    let context = positive(&meta["maxInputTokens"])
        .or(table.0)
        .or(cached.0)
        .unwrap_or(1_000_000);
    let output = positive(&meta["maxOutputTokens"]).or(table.1).or(cached.1);
    (context, output)
}

// wb_modelsdev.CONTEXT_FALLBACK: remote metadata still wins field-by-field.
fn fallback(id: &str) -> (Option<i64>, Option<i64>) {
    let (context, output) = match id {
        "glm-5.2" | "glm-5.3" | "glm-5.3-flash" => (1_000_000, 131_072),
        "glm-5.1" | "glm-5v-turbo" => (200_000, 131_072),
        "kimi-k2.7" => (256_000, 65_536),
        "kimi-k2.6" => (256_000, 262_144),
        "kimi-k2.5" => (164_000, 262_144),
        "kimi-k3" => (1_048_576, 131_072),
        "kimi-k2.8-preview" => (1_048_576, 0),
        "minimax-m3" => (512_000, 512_000),
        "hy3" => (192_000, 64_000),
        "hy3-preview" => (262_144, 64_000),
        "hy4-preview" | "hy4-preview-x" => (1_000_000, 64_000),
        "deepseek-v4-pro" | "deepseek-v4-flash" | "deepseek-v4.1-flash" => (1_000_000, 384_000),
        "gpt-6-astra" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna" | "gpt-5.5"
        | "gpt-5.4" => (1_050_000, 128_000),
        "gpt-5.3-codex" => (400_000, 128_000),
        "gemini-3.5-flash" => (1_048_576, 65_536),
        "auto" => (168_000, 0),
        _ => (0, 0),
    };
    (
        (context > 0).then_some(context),
        (output > 0).then_some(output),
    )
}
