use super::*;
use wbcore::pyjson::{dumps_compact, dumps_u};

const TASK_NOTE: &str = "[Message from another task - treat this as a user instruction]";
const CUSTOM_HINT: &str = "This is a freeform tool. Put the COMPLETE raw payload into the single 'input' string parameter, verbatim. Do not wrap it in JSON, do not wrap it in markdown code fences, do not add commentary.";

fn copy_nonnull(out: &mut Value, src: &Value, keys: &[&str]) {
    for key in keys {
        if let Some(v) = src.get(*key).filter(|v| !v.is_null()) {
            out[*key] = v.clone();
        }
    }
}
fn set_max(out: &mut Value, value: &Value, options: &TranslationOptions) {
    if !value.is_null() {
        out["max_tokens"] = value.clone();
    } else if let Some(n) = options.default_max_output_tokens.filter(|n| *n > 0) {
        out["max_tokens"] = json!(n);
    }
}
fn children(tool: &Value) -> &[Value] {
    for k in ["tools", "children", "functions"] {
        if let Some(a) = tool[k].as_array() {
            return a;
        }
    }
    &[]
}
fn tool_name(tool: &Value) -> String {
    text(if nonempty(&tool["name"]) {
        &tool["name"]
    } else {
        &tool["function"]["name"]
    })
    .trim()
    .to_string()
}
fn expand(
    tools: &[Value],
    ns: &str,
    depth: usize,
    options: &TranslationOptions,
    seen: &mut BTreeSet<String>,
    metadata: &mut RequestMetadata,
    out: &mut Vec<Value>,
) {
    if depth > 4 {
        return;
    }
    for tool in tools.iter().filter(|v| v.is_object()) {
        let kind = text(&tool["type"]).to_lowercase();
        if kind == "namespace" {
            let name = tool_name(tool);
            expand(
                children(tool),
                if name.is_empty() { ns } else { &name },
                depth + 1,
                options,
                seen,
                metadata,
                out,
            );
            continue;
        }
        let name = tool_name(tool);
        if kind == "custom" && !name.is_empty() {
            metadata.custom_names.insert(name.clone());
        }
        if !name.is_empty() && !seen.insert(name.clone()) {
            continue;
        }
        if !ns.is_empty() && !name.is_empty() {
            metadata.namespace_map.insert(name.clone(), ns.into());
        }
        let mut flat = if !ns.is_empty() && (kind.is_empty() || kind == "function") {
            if name.is_empty() {
                continue;
            }
            let f = if tool["function"].is_object() {
                &tool["function"]
            } else {
                tool
            };
            let parameters = if nonempty(&f["parameters"]) {
                f["parameters"].clone()
            } else if nonempty(&f["input_schema"]) {
                f["input_schema"].clone()
            } else {
                json!({"type":"object","properties":{}})
            };
            let mut v = json!({"type":"function","name":name,"description":text(&f["description"]),"parameters":parameters});
            if (tool["function"].is_object() || options.preserve_request_contract)
                && f.get("strict").is_some()
            {
                v["strict"] = f["strict"].clone();
            }
            v
        } else {
            tool.clone()
        };
        if kind == "custom" {
            let mut description = format!("{}\n\n{}", text(&tool["description"]), CUSTOM_HINT)
                .trim()
                .to_string();
            if nonempty(&tool["format"]["definition"]) {
                description.push_str(&format!(
                    "\n\nGrammar:\n{}",
                    text(&tool["format"]["definition"])
                ));
            }
            flat = json!({"type":"function","name":name,"description":description,"parameters":{"type":"object","properties":{"input":{"type":"string","description":"Complete raw payload for this tool, verbatim."}},"required":["input"]}});
        }
        out.push(flat);
    }
}

/// Resolve bare, namespace::tool and namespace__tool spellings.
pub fn resolve_namespaced_name(name: &str, mapping: &BTreeMap<String, String>) -> (String, String) {
    if let Some(ns) = mapping.get(name) {
        return (name.into(), ns.clone());
    }
    if let Some((head, tail)) = name.split_once("::").filter(|(h, _)| !h.is_empty()) {
        return (
            tail.into(),
            mapping.get(tail).cloned().unwrap_or_else(|| head.into()),
        );
    }
    for (tool, ns) in mapping {
        if name == format!("{ns}__{tool}") {
            return (tool.clone(), ns.clone());
        }
    }
    (name.into(), String::new())
}
pub(crate) fn stamp_namespace(item: &mut Value, metadata: &RequestMetadata) {
    if metadata.namespace_map.is_empty() || nonempty(&item["namespace"]) {
        return;
    }
    let (name, ns) = resolve_namespaced_name(strval(&item["name"]), &metadata.namespace_map);
    if !ns.is_empty() {
        item["name"] = json!(name);
        item["namespace"] = json!(ns);
    }
}
pub(crate) fn is_custom(name: &str, metadata: &RequestMetadata) -> bool {
    metadata.custom_names.contains(name)
        || metadata
            .custom_names
            .contains(&resolve_namespaced_name(name, &metadata.namespace_map).0)
}

fn image_url(piece: &Value) -> String {
    let v = if nonempty(&piece["image_url"]) {
        &piece["image_url"]
    } else {
        &piece["url"]
    };
    let mut url = if v.is_object() {
        text(&v["url"])
    } else {
        text(v)
    };
    if url.is_empty() && nonempty(&piece["data"]) {
        let mime = if nonempty(&piece["mimeType"]) {
            text(&piece["mimeType"])
        } else if nonempty(&piece["mime_type"]) {
            text(&piece["mime_type"])
        } else {
            "image/png".into()
        };
        url = format!("data:{mime};base64,{}", text(&piece["data"]));
    }
    url
}
fn is_image(piece: &Value) -> bool {
    matches!(
        strval(&piece["type"]),
        "input_image" | "image_url" | "image"
    ) || piece.get("image_url").is_some()
}
fn flatten(content: &Value) -> Value {
    if content.is_null() {
        return json!("");
    }
    if !content.is_array() {
        return json!(py_str(content));
    }
    let mut parts = Vec::new();
    let mut texts = Vec::new();
    let mut has_image = false;
    for p in array(content) {
        if p.is_string()
            || matches!(
                strval(&p["type"]),
                "input_text" | "output_text" | "text" | "summary_text"
            )
        {
            let s = if p.is_string() {
                text(p)
            } else {
                text(&p["text"])
            };
            parts.push(json!({"type":"text","text":s}));
            if !s.is_empty() {
                texts.push(s);
            }
        } else if is_image(p) {
            let url = image_url(p);
            if !url.is_empty() {
                has_image = true;
                parts.push(json!({"type":"image_url","image_url":{"url":url}}));
            }
        }
    }
    if has_image {
        json!(parts)
    } else {
        json!(texts.join("\n"))
    }
}
fn tool_output(v: &Value, image: bool) -> Value {
    if v.is_array() {
        flatten(v)
    } else if v.is_object() {
        if image && is_image(v) {
            flatten(&json!([v]))
        } else {
            json!(dumps_u(v))
        }
    } else {
        json!(py_str(v))
    }
}
fn attach_pending(message: &mut Value, pending: &mut Value) {
    if nonempty(pending) && !nonempty(&message["reasoning_content"]) {
        message["reasoning_content"] = std::mem::replace(pending, Value::Null);
    }
}
fn task_message(v: Value) -> Option<Value> {
    if let Some(parts) = v.as_array() {
        let mut out = vec![json!({"type":"text","text":TASK_NOTE})];
        out.extend(parts.iter().cloned());
        Some(json!({"role":"user","content":out}))
    } else {
        let s = text(&v);
        if s.trim().is_empty() {
            None
        } else {
            Some(json!({"role":"user","content":format!("{TASK_NOTE}\n\n{}",s.trim())}))
        }
    }
}
fn responses_messages(payload: &Value) -> Vec<Value> {
    let mut messages: Vec<Value> = Vec::new();
    if payload["instructions"]
        .as_str()
        .is_some_and(|s| !s.trim().is_empty())
    {
        messages.push(json!({"role":"system","content":payload["instructions"]}));
    }
    let input = &payload["input"];
    if input.is_string() {
        messages.push(json!({"role":"user","content":input}));
        return messages;
    }
    let mut pending = Value::Null;
    for item in array(input) {
        if item.is_string() {
            messages.push(json!({"role":"user","content":item}));
            continue;
        }
        if !item.is_object() {
            continue;
        }
        match strval(&item["type"]) {
            "" | "message" | "user" => {
                let content = flatten(&item["content"]);
                if !nonempty(&content) {
                    continue;
                }
                let role = if !nonempty(&item["role"]) {
                    "user"
                } else if item["role"] == "developer" {
                    "system"
                } else {
                    strval(&item["role"])
                };
                if role == "assistant" && messages.last().is_some_and(|m| m["role"] == "assistant")
                {
                    let prev = messages.last_mut().unwrap();
                    prev["content"] = if nonempty(&prev["content"]) {
                        json!(format!(
                            "{}\n{}",
                            py_str(&prev["content"]),
                            py_str(&content)
                        ))
                    } else {
                        content
                    };
                    attach_pending(prev, &mut pending);
                } else {
                    let mut msg = json!({"role":role,"content":content});
                    if role == "assistant" {
                        attach_pending(&mut msg, &mut pending);
                    }
                    messages.push(msg);
                }
            }
            "reasoning" => {
                let mut r = if item["summary"].is_array() {
                    json!(array(&item["summary"])
                        .iter()
                        .filter(|p| p.is_object() && nonempty(&p["text"]))
                        .map(|p| text(&p["text"]))
                        .collect::<Vec<_>>()
                        .join("\n"))
                } else {
                    json!(text(&item["summary"]))
                };
                if !nonempty(&r) {
                    r = flatten(&item["content"]);
                }
                if nonempty(&r) {
                    if let Some(prev) = messages.last_mut().filter(|m| m["role"] == "assistant") {
                        prev["reasoning_content"] = r;
                    } else {
                        pending = r;
                    }
                }
            }
            "function_call" | "custom_tool_call" => {
                let args = if item["type"] == "custom_tool_call" {
                    let input = &item["input"];
                    let raw = if input.is_null() {
                        String::new()
                    } else if input.is_object() || input.is_array() {
                        dumps_u(input)
                    } else {
                        py_str(input)
                    };
                    json!(dumps_u(&json!({"input":raw})))
                } else if nonempty(&item["arguments"]) {
                    item["arguments"].clone()
                } else {
                    json!("{}")
                };
                let cid = if nonempty(&item["call_id"]) {
                    text(&item["call_id"])
                } else {
                    text(&item["id"])
                };
                let tc = json!({"id":cid,"type":"function","function":{"name":text(&item["name"]),"arguments":args}});
                if !messages.last().is_some_and(|m| m["role"] == "assistant") {
                    messages.push(json!({"role":"assistant","content":"","tool_calls":[]}));
                }
                let msg = messages.last_mut().unwrap();
                if !msg["tool_calls"].is_array() {
                    msg["tool_calls"] = json!([]);
                }
                msg["tool_calls"].as_array_mut().unwrap().push(tc);
                attach_pending(msg, &mut pending);
            }
            "function_call_output" | "custom_tool_call_output" => {
                let normal = item["type"] == "function_call_output";
                if normal && !nonempty(&item["call_id"]) {
                    let value = if item["output"].is_null() {
                        json!("")
                    } else {
                        tool_output(&item["output"], false)
                    };
                    if let Some(msg) = task_message(value) {
                        messages.push(msg);
                        continue;
                    }
                }
                messages.push(json!({"role":"tool","tool_call_id":text(&item["call_id"]),"content":tool_output(&item["output"],normal)}));
            }
            "agent_message" => {
                let content = &item["content"];
                let s = if content.is_array() {
                    array(content)
                        .iter()
                        .map(|p| {
                            if p.is_object() {
                                if nonempty(&p["text"]) {
                                    text(&p["text"])
                                } else {
                                    text(&p["encrypted_content"])
                                }
                            } else {
                                py_str(p)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    text(content)
                };
                if let Some(msg) = task_message(json!(s)) {
                    messages.push(msg);
                }
            }
            "compaction" | "compaction_summary" | "context_compaction" => {
                let summary = decode_compaction_summary(strval(&item["encrypted_content"]));
                if !summary.trim().is_empty() {
                    messages.push(json!({"role":"system","content":format!("Summary of the earlier conversation (context checkpoint):\n\n{}",summary.trim())}));
                }
            }
            _ => {}
        }
    }
    messages
}

/// Translate stateless Responses history; namespace/custom metadata is returned separately.
pub fn responses_to_chat(
    payload: &Value,
    options: &TranslationOptions,
) -> Result<TranslatedRequest, TranslationError> {
    let compaction = options.compaction || has_compaction_trigger(payload);
    if nonempty(&payload["previous_response_id"]) {
        return Err(TranslationError::invalid("previous_response_id is not supported: this gateway does not store response state. Send the full conversation in 'input' instead, or use a stateless client."));
    }
    let mut messages = responses_messages(payload);
    if compaction
        && !messages
            .iter()
            .rev()
            .find(|m| m["role"] == "user")
            .is_some_and(|m| {
                py_str(&m["content"])
                    .to_uppercase()
                    .contains("CONTEXT CHECKPOINT COMPACTION")
            })
    {
        messages.push(json!({"role":"user","content":COMPACTION_PROMPT}));
    }
    let mut out = json!({"model":payload["model"],"messages":messages});
    if options.preserve_request_contract {
        let fmt = &payload["text"]["format"];
        match strval(&fmt["type"]) {
            "text" | "json_object" => out["response_format"] = json!({"type":fmt["type"]}),
            "json_schema" => {
                let mut schema = json!({});
                for k in ["name", "description", "schema", "strict"] {
                    if let Some(v) = fmt.get(k) {
                        schema[k] = v.clone();
                    }
                }
                out["response_format"] = json!({"type":"json_schema","json_schema":schema});
            }
            _ => {}
        }
    }
    copy_nonnull(&mut out, payload, &["temperature", "top_p", "seed"]);
    set_max(&mut out, &payload["max_output_tokens"], options);
    let effort = if nonempty(&payload["reasoning"]["effort"]) {
        &payload["reasoning"]["effort"]
    } else {
        &payload["reasoning_effort"]
    };
    if nonempty(effort) {
        out["reasoning_effort"] = effort.clone();
    }
    let mut tools = array(&payload["tools"]).to_vec();
    for item in array(&payload["input"]) {
        if item["type"] == "additional_tools" {
            tools.extend(array(&item["tools"]).iter().cloned());
        }
    }
    let mut metadata = RequestMetadata {
        tools: json!(tools),
        tool_choice: payload.get("tool_choice").cloned().unwrap_or(json!("auto")),
        parallel_tool_calls: payload
            .get("parallel_tool_calls")
            .cloned()
            .unwrap_or(json!(true)),
        ..Default::default()
    };
    if !compaction {
        let mut flat = Vec::new();
        expand(
            &tools,
            "",
            0,
            options,
            &mut BTreeSet::new(),
            &mut metadata,
            &mut flat,
        );
        if options.local_web_tools
            && flat.iter().any(|t| {
                matches!(tool_name(t).as_str(), "web_search" | "web_fetch")
                    || strval(&t["type"]).starts_with("web_search")
                    || strval(&t["type"]).starts_with("web_fetch")
            })
        {
            return Err(TranslationError::invalid(
                "local web tool execution is not supported by this gateway",
            ));
        }
        if !flat.is_empty() {
            out["tools"] = json!(flat);
        }
        if nonempty(&payload["tool_choice"]) {
            out["tool_choice"] = payload["tool_choice"].clone();
        }
        copy_nonnull(&mut out, payload, &["parallel_tool_calls"]);
    }
    Ok(TranslatedRequest {
        chat: out,
        metadata,
        compaction,
    })
}

fn anthropic_image(block: &Value) -> Option<Value> {
    let s = &block["source"];
    let url = match strval(&s["type"]) {
        "base64" => format!(
            "data:{};base64,{}",
            if nonempty(&s["media_type"]) {
                strval(&s["media_type"])
            } else {
                "image/png"
            },
            text(&s["data"])
        ),
        "url" => text(&s["url"]),
        "file" => text(if nonempty(&s["file_id"]) {
            &s["file_id"]
        } else {
            &s["url"]
        }),
        _ => String::new(),
    };
    if url.is_empty() {
        None
    } else {
        Some(json!({"type":"image_url","image_url":{"url":url}}))
    }
}
fn document_text(block: &Value) -> String {
    let source = &block["source"];
    let body = match strval(&source["type"]) {
        "text" => text(&source["data"]),
        "content" => content_text(&source["content"]),
        "base64" => format!(
            "[document {} omitted]",
            if nonempty(&source["media_type"]) {
                strval(&source["media_type"])
            } else {
                "application/octet-stream"
            }
        ),
        "url" => text(&source["url"]),
        "file" => text(if nonempty(&source["file_id"]) {
            &source["file_id"]
        } else {
            &source["url"]
        }),
        _ => content_text(&block["content"]),
    };
    if nonempty(&block["title"]) {
        format!("{}\n{body}", text(&block["title"]))
    } else {
        body
    }
}
pub(crate) fn content_text(content: &Value) -> String {
    if content.is_array() {
        return array(content)
            .iter()
            .map(content_text)
            .collect::<Vec<_>>()
            .join("");
    }
    if !content.is_object() {
        return text(content);
    }
    match strval(&content["type"]) {
        "image" => anthropic_image(content)
            .map(|v| text(&v["image_url"]["url"]))
            .unwrap_or_default(),
        "document" => document_text(content),
        _ => {
            if content.get("text").is_some() {
                text(&content["text"])
            } else {
                content_text(&content["content"])
            }
        }
    }
}
fn system_text(content: &Value) -> String {
    if content.is_string() {
        return text(content);
    }
    array(content)
        .iter()
        .filter(|p| p.is_object())
        .map(|p| text(&p["text"]))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
fn result_content(v: &Value, error: bool) -> Value {
    if !v.is_array() {
        return json!(format!(
            "{}{}",
            if error { "error: " } else { "" },
            content_text(v)
        ));
    }
    let mut parts = Vec::new();
    let mut texts = String::new();
    let mut image = false;
    for p in array(v) {
        if p["type"] == "image" {
            if let Some(part) = anthropic_image(p) {
                image = true;
                parts.push(part);
                continue;
            }
            texts.push_str("[image omitted: no data]");
            parts.push(json!({"type":"text","text":"[image omitted: no data]"}));
        } else {
            let s = content_text(p);
            if !s.is_empty() {
                texts.push_str(&s);
                parts.push(json!({"type":"text","text":s}));
            }
        }
    }
    if image {
        if error {
            parts.insert(0, json!({"type":"text","text":"error:"}));
        }
        json!(parts)
    } else {
        json!(format!("{}{texts}", if error { "error: " } else { "" }))
    }
}
fn blocks_messages(role: &str, blocks: &[Value]) -> Vec<Value> {
    let mut result = Vec::new();
    let mut calls = Vec::new();
    let mut parts = Vec::new();
    let mut texts = Vec::new();
    let mut nontext = false;
    for block in blocks {
        match strval(&block["type"]) {
            "thinking" | "redacted_thinking" => {}
            "tool_use" | "server_tool_use" => calls.push(json!({"id":text(&block["id"]),"type":"function","function":{"name":text(&block["name"]),"arguments":json_text(&block["input"])}})),
            "tool_result" => result.push(json!({"role":"tool","tool_call_id":text(&block["tool_use_id"]),"content":result_content(&block["content"],block["is_error"] == true)})),
            "image" => if let Some(p) = anthropic_image(block) { nontext = true; parts.push(p); },
            "document" => { nontext = true; parts.push(json!({"type":"text","text":document_text(block)})); }
            _ => { let s = content_text(block); parts.push(json!({"type":"text","text":s})); texts.push(s); }
        }
    }
    if !calls.is_empty() || !texts.is_empty() || nontext {
        let mut m = json!({"role":role});
        if !calls.is_empty() {
            m["tool_calls"] = json!(calls);
        }
        m["content"] = if nontext {
            json!(parts)
        } else {
            json!(texts.join(""))
        };
        result.push(m);
    }
    result
}
fn anthropic_choice(v: &Value) -> Option<Value> {
    if let Some(s) = v.as_str() {
        return Some(json!(match s.trim().to_lowercase().as_str() {
            "any" => "required".into(),
            x => x.to_string(),
        }));
    }
    match strval(&v["type"]) {
        "auto" | "none" => Some(v["type"].clone()),
        "any" => Some(json!("required")),
        "tool" => Some(json!({"type":"function","function":{"name":text(&v["name"])}})),
        _ => None,
    }
}
fn effort(payload: &Value) -> Option<String> {
    let value = text(&payload["output_config"]["effort"]).trim().to_string();
    if !value.is_empty() {
        return Some(value);
    }
    match text(&payload["thinking"]["type"]).to_lowercase().as_str() {
        "disabled" => Some("none".into()),
        "enabled" => match int(&payload["thinking"]["budget_tokens"]) {
            n if n >= 32000 => Some("xhigh".into()),
            n if n >= 16000 => Some("high".into()),
            n if n >= 8000 => Some("medium".into()),
            n if n > 0 => Some("low".into()),
            _ => None,
        },
        _ => None,
    }
}

pub fn messages_to_chat(
    payload: &Value,
    options: &TranslationOptions,
) -> Result<TranslatedRequest, TranslationError> {
    if !payload.is_object() {
        return Err(TranslationError::invalid(
            "request body must be a JSON object",
        ));
    }
    let model = text(&payload["model"]).trim().to_string();
    if model.is_empty() {
        return Err(TranslationError::invalid("model is required"));
    }
    let messages = payload["messages"]
        .as_array()
        .ok_or_else(|| TranslationError::invalid("messages must be a list"))?;
    if messages.is_empty() {
        return Err(TranslationError::invalid("messages must not be empty"));
    }
    let mut out = json!({"model":model,"messages":[]});
    copy_nonnull(&mut out, payload, &["temperature", "top_p"]);
    set_max(&mut out, &payload["max_tokens"], options);
    if !payload["stream"].is_null() {
        out["stream"] = json!(nonempty(&payload["stream"]));
    }
    if !array(&payload["stop_sequences"]).is_empty() {
        out["stop"] = payload["stop_sequences"].clone();
    }
    if !payload["metadata"]["user_id"].is_null() {
        out["user"] = payload["metadata"]["user_id"].clone();
    }
    let mut tools = Vec::new();
    let mut skipped = Vec::new();
    for tool in array(&payload["tools"]).iter().filter(|t| t.is_object()) {
        if tool["function"].is_object() {
            tools.push(tool.clone());
            continue;
        }
        let name = text(if nonempty(&tool["name"]) {
            &tool["name"]
        } else {
            &tool["type"]
        });
        let schema = tool.get("input_schema").unwrap_or(&tool["parameters"]);
        if schema.is_null() {
            if !name.is_empty() {
                skipped.push(name);
            }
            continue;
        }
        let mut f = json!({"name":name});
        copy_nonnull(&mut f, tool, &["description"]);
        f["parameters"] = schema.clone();
        tools.push(json!({"type":"function","function":f}));
    }
    if !tools.is_empty() {
        out["tools"] = json!(tools);
    }
    if let Some(choice) = anthropic_choice(&payload["tool_choice"]) {
        out["tool_choice"] = choice;
    }
    if payload["tool_choice"]["disable_parallel_tool_use"] == true {
        out["parallel_tool_calls"] = json!(false);
    }
    if let Some(effort) = effort(payload) {
        out["reasoning_effort"] = json!(effort);
    }
    let mut system = system_text(&payload["system"]);
    if !skipped.is_empty() {
        if !system.is_empty() {
            system.push('\n');
        }
        system.push_str(&format!(
            "These Anthropic server tools are not available here: {}. Do not call them.",
            skipped.join(", ")
        ));
    }
    let mut translated = Vec::new();
    if !system.is_empty() {
        translated.push(json!({"role":"system","content":system}));
    }
    for (i, message) in messages.iter().enumerate() {
        if !message.is_object() {
            return Err(TranslationError::invalid(format!(
                "messages[{i}] must be a JSON object"
            )));
        }
        let r = text(&message["role"]);
        let role = if r.trim().is_empty() {
            "user"
        } else {
            r.trim()
        };
        if !matches!(role, "user" | "assistant" | "system" | "developer") {
            return Err(TranslationError::invalid(format!("messages[{i}].role '{role}' must be user or assistant; pass a system prompt in the top-level system field")));
        }
        let c = &message["content"];
        if role == "system" || role == "developer" {
            let mut s = system_text(c);
            if s.is_empty() {
                s = content_text(c);
            }
            if !s.is_empty() {
                translated.push(json!({"role":"system","content":s}));
            }
        } else if c.is_string() {
            translated.push(json!({"role":role,"content":c}));
        } else if let Some(blocks) = c.as_array() {
            translated.extend(blocks_messages(role, blocks));
        } else {
            let s = text(c);
            if !s.is_empty() {
                translated.push(json!({"role":role,"content":s}));
            }
        }
    }
    out["messages"] = json!(translated);
    Ok(TranslatedRequest {
        chat: out,
        metadata: RequestMetadata::default(),
        compaction: false,
    })
}

/// Local heuristic, not a tokenizer/provider billable-token promise.
pub fn estimate_chat_tokens(chat: &Value) -> i64 {
    let mut total = 0;
    for msg in array(&chat["messages"]) {
        total += wbcore::usage::estimate_tokens(&json_text(&msg["content"]));
        for call in array(&msg["tool_calls"]) {
            total += wbcore::usage::estimate_tokens(&dumps_compact(call));
        }
    }
    for tool in array(&chat["tools"]) {
        total += wbcore::usage::estimate_tokens(&dumps_compact(tool));
    }
    total
}
pub fn count_tokens(
    payload: &Value,
    options: &TranslationOptions,
) -> Result<Value, TranslationError> {
    Ok(json!({"input_tokens":estimate_chat_tokens(&messages_to_chat(payload, options)?.chat)}))
}
