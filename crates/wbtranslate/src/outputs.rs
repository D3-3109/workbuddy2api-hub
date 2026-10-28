use super::*;
use crate::requests::{content_text, is_custom, stamp_namespace};

pub fn responses_usage(u: &Value) -> Option<Value> {
    if !nonempty(u) {
        return None;
    }
    // Use the shared cache alias resolver, including aliases missed by Python.
    Some(
        json!({"input_tokens":int(&u["prompt_tokens"]),"input_tokens_details":{"cached_tokens":wbcore::usage::best_cached_tokens(u)},"output_tokens":int(&u["completion_tokens"]),"output_tokens_details":{"reasoning_tokens":int(&u["completion_tokens_details"]["reasoning_tokens"])},"total_tokens":int(&u["total_tokens"])}),
    )
}
pub fn anthropic_usage(u: &Value) -> Value {
    let read = if !u["prompt_cache_hit_tokens"].is_null() {
        int(&u["prompt_cache_hit_tokens"])
    } else {
        wbcore::usage::best_cached_tokens(u)
    };
    let creation = int(&u["prompt_cache_write_tokens"]);
    let mut v = json!({"input_tokens":(int(&u["prompt_tokens"])-read-creation).max(0),"cache_creation_input_tokens":creation,"cache_read_input_tokens":read,"output_tokens":int(&u["completion_tokens"])});
    if nonempty(&u["service_tier"]) {
        v["service_tier"] = u["service_tier"].clone();
    }
    v
}
pub(crate) fn unwrap_custom_input(args: &Value) -> String {
    let raw = if args.is_string() {
        text(args)
    } else if nonempty(args) {
        wbcore::pyjson::dumps_u(args)
    } else {
        wbcore::pyjson::dumps_u(&Value::String(String::new()))
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::String(s)) => s,
        Ok(v) if v.is_object() && !v["input"].is_null() => {
            if v["input"].is_string() {
                text(&v["input"])
            } else {
                wbcore::pyjson::dumps_u(&v["input"])
            }
        }
        _ => raw,
    }
}
pub(crate) fn tool_input(args: &Value) -> Value {
    if args.is_null() || args == "" {
        return json!({});
    }
    if args.is_object() {
        return args.clone();
    }
    if let Some(s) = args.as_str() {
        return match serde_json::from_str::<Value>(s) {
            Ok(v) if v.is_object() => v,
            Ok(v) => json!({"value":v}),
            Err(_) => json!({"raw":s}),
        };
    }
    json!({"value":args})
}
pub(crate) fn stop_reason(finish: &str) -> &str {
    match finish {
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        "content_filter" | "refusal" => "refusal",
        _ => "end_turn",
    }
}
pub(crate) fn anthropic_id(value: &Value) -> String {
    let v = text(value);
    if v.is_empty() {
        id("msg_")
    } else if v.starts_with("msg_") {
        v
    } else {
        format!("msg_{v}")
    }
}
pub(crate) fn reason_item(item_id: &str, status: &str, reasoning: &str) -> Value {
    json!({"id":item_id,"type":"reasoning","status":status,"summary":[{"type":"summary_text","text":reasoning}]})
}
pub(crate) fn text_part(text: &str) -> Value {
    json!({"type":"output_text","text":text,"annotations":[]})
}
pub(crate) fn message_item(item_id: &str, status: &str, text: &str) -> Value {
    json!({"id":item_id,"type":"message","status":status,"role":"assistant","content":if text.is_empty() { vec![] } else { vec![text_part(text)] }})
}
pub(crate) fn response_base(
    options: &ResponseOptions,
    rid: &str,
    created: i64,
    status: &str,
    output: Vec<Value>,
    text: &str,
    stream: bool,
) -> Value {
    let mut v = json!({"id":rid,"object":"response","created_at":created,"status":status,"model":options.model,"output":output,"output_text":text});
    if !stream {
        v["metadata"] = json!({});
    }
    v["parallel_tool_calls"] = options.metadata.parallel_tool_calls.clone();
    v["tool_choice"] = options.metadata.tool_choice.clone();
    v["tools"] = if nonempty(&options.metadata.tools) {
        options.metadata.tools.clone()
    } else {
        json!([])
    };
    if stream {
        v["metadata"] = json!({});
    }
    v
}

/// True for a completed reasoning-only result. Custom tools count as real output.
pub fn reasoning_only_response(response: &Value) -> bool {
    if response["status"] == "incomplete" || !strval(&response["output_text"]).trim().is_empty() {
        return false;
    }
    let mut reasoning = false;
    for item in array(&response["output"]) {
        if item["type"] == "reasoning" {
            reasoning = true;
        }
        if matches!(
            strval(&item["type"]),
            "function_call" | "custom_tool_call" | "web_search_call"
        ) {
            return false;
        }
    }
    reasoning
}

/// Translate an aggregated chat completion. In-band errors and reasoning-only
/// completions return errors; callers must not run an error-swallowing aggregator first.
pub fn chat_to_response(
    chat: &Value,
    options: &ResponseOptions,
) -> Result<Value, TranslationError> {
    if let Some(e) = upstream_error(chat) {
        return Err(e);
    }
    let choice = &chat["choices"][0];
    let message = &choice["message"];
    let mut text = content_text(&message["content"]);
    let reasoning = content_text(&message["reasoning_content"]);
    let mut output = Vec::new();
    if !reasoning.is_empty() {
        output.push(reason_item(&id("rs_"), "completed", &reasoning));
    }
    for tc in array(&message["tool_calls"]) {
        let name = text_of_name(tc);
        if name.is_empty() {
            continue;
        }
        let cid = if nonempty(&tc["id"]) {
            crate::text(&tc["id"])
        } else {
            id("call_")
        };
        let args = &tc["function"]["arguments"];
        let mut item = if is_custom(&name, &options.metadata) {
            json!({"id":id("ctc_"),"type":"custom_tool_call","status":"completed","call_id":cid,"name":name,"input":unwrap_custom_input(args)})
        } else {
            json!({"id":id("fc_"),"type":"function_call","status":"completed","call_id":cid,"name":name,"arguments":if nonempty(args) { args.clone() } else { json!("{}") }})
        };
        stamp_namespace(&mut item, &options.metadata);
        output.push(item);
    }
    if array(&message["tool_calls"]).is_empty() {
        let (calls, clean) = wbcore::toolcalls::parse_dsml_tool_calls(&text);
        if let Some(calls) = calls {
            text = clean;
            for call in calls {
                let mut item = json!({"id":id("fc_"),"type":"function_call","status":"completed","call_id":call.id,"name":call.name,"arguments":call.arguments});
                stamp_namespace(&mut item, &options.metadata);
                output.push(item);
            }
        }
    }
    if !text.is_empty() || output.is_empty() {
        output.push(message_item(&id("msg_"), "completed", &text));
    }
    let length = choice["finish_reason"] == "length";
    let mut response = response_base(
        options,
        &options.response_id.clone().unwrap_or_else(|| id("resp_")),
        created(options),
        if length { "incomplete" } else { "completed" },
        output,
        &text,
        false,
    );
    if let Some(usage) = responses_usage(&chat["usage"]) {
        response["usage"] = usage;
    }
    if length {
        response["incomplete_details"] = json!({"reason":"max_output_tokens"});
    }
    if reasoning_only_response(&response) {
        return Err(TranslationError {
            status: 502,
            code: "empty_reasoning_only".into(),
            message: "upstream returned reasoning without output text; retry".into(),
        });
    }
    Ok(response)
}
fn text_of_name(tc: &Value) -> String {
    text(&tc["function"]["name"])
}

pub fn chat_to_messages(chat: &Value) -> Result<Value, TranslationError> {
    if let Some(e) = upstream_error(chat) {
        return Err(e);
    }
    let choice = &chat["choices"][0];
    let message = &choice["message"];
    let mut content = Vec::new();
    let reasoning = content_text(&message["reasoning_content"]);
    let text = content_text(&message["content"]);
    if !reasoning.is_empty() {
        content.push(json!({"type":"thinking","thinking":reasoning}));
    }
    if !text.is_empty() {
        content.push(json!({"type":"text","text":text}));
    }
    for tc in array(&message["tool_calls"]) {
        content.push(json!({"type":"tool_use","id":if nonempty(&tc["id"]) { crate::text(&tc["id"]) } else { id("toolu_") },"name":text_of_name(tc),"input":tool_input(&tc["function"]["arguments"])}));
    }
    Ok(
        json!({"id":anthropic_id(&chat["id"]),"type":"message","role":"assistant","model":crate::text(&chat["model"]),"content":content,"stop_reason":stop_reason(strval(&choice["finish_reason"])),"stop_sequence":null,"usage":anthropic_usage(&chat["usage"])}),
    )
}
