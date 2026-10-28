use super::*;
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
fn encode64(bytes: &[u8]) -> String {
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let bits = ((c[0] as u32) << 16)
            | ((c.get(1).copied().unwrap_or(0) as u32) << 8)
            | c.get(2).copied().unwrap_or(0) as u32;
        out.push(BASE64[((bits >> 18) & 63) as usize] as char);
        out.push(BASE64[((bits >> 12) & 63) as usize] as char);
        out.push(if c.len() > 1 {
            BASE64[((bits >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            BASE64[(bits & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
fn decode64(raw: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut count = 0;
    let mut symbols = 0;
    let mut out = Vec::new();
    for b in raw.bytes() {
        if b == b'=' {
            break;
        }
        let Some(v) = BASE64.iter().position(|c| *c == b) else {
            continue;
        };
        bits = (bits << 6) | v as u32;
        count += 6;
        symbols += 1;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    if symbols % 4 == 1 {
        None
    } else {
        Some(out)
    }
}

pub const COMPACTION_PROMPT: &str = "You are performing a CONTEXT CHECKPOINT COMPACTION. Create a handoff summary for another LLM that will resume the task.\n\nInclude:\n- Current progress and key decisions made\n- Important context, constraints, or user preferences\n- What remains to be done (clear next steps)\n- Any critical data, examples, or references needed to continue\n\nBe concise, structured, and focused on helping the next LLM seamlessly continue the work. Return only the summary.";

pub fn has_compaction_trigger(payload: &Value) -> bool {
    strval(&payload["input"]).trim() == "compaction_trigger"
        || array(&payload["input"])
            .iter()
            .any(|p| p["type"] == "compaction_trigger")
}
/// Opaque, portable envelope; this is base64 encoding, NOT encryption.
pub fn encode_compaction_summary(summary: &str) -> String {
    encode64(wbcore::pyjson::dumps_compact(&json!({"v":1,"summary":summary})).as_bytes())
}
pub fn decode_compaction_summary(blob: &str) -> String {
    let raw = blob.trim();
    // Python's non-strict decoder discards non-alphabet characters and accepts
    // missing padding. A failed decode/JSON parse falls back to plaintext.
    if let Some(bytes) = decode64(raw) {
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(s) = v["summary"].as_str() {
                return s.into();
            }
        }
    }
    raw.into()
}
/// Compaction deliberately needs the complete summary. This is a non-stream
/// conversion; `compaction_events` is its four-frame protocol envelope, not an
/// incremental text stream. Ordinary generation uses ResponsesStream instead.
pub fn chat_to_compaction(
    chat: &Value,
    options: &ResponseOptions,
) -> Result<Value, TranslationError> {
    if let Some(e) = upstream_error(chat) {
        return Err(e);
    }
    let summary = strval(&chat["choices"][0]["message"]["content"]).trim();
    if summary.is_empty() {
        return Err(TranslationError::upstream(
            "compaction produced no summary (the model returned no text); retry",
        ));
    }
    let item = json!({"id":id("cmp_"),"type":"compaction","encrypted_content":encode_compaction_summary(summary)});
    Ok(
        json!({"id":options.response_id.clone().unwrap_or_else(|| id("resp_")),"object":"response","created_at":created(options),"status":"completed","model":options.model,"output":[item],"output_text":"","metadata":{},"usage":responses_usage(&chat["usage"]).unwrap_or(json!({}))}),
    )
}
/// Encode an already-aggregated compaction result in the documented four-frame
/// format. Never call this for ordinary incremental generation.
pub fn compaction_events(
    chat: &Value,
    options: &ResponseOptions,
) -> Result<Vec<SseEvent>, TranslationError> {
    let result = chat_to_compaction(chat, options)?;
    let mut base = json!({"id":result["id"],"object":"response","created_at":result["created_at"],"model":result["model"],"output":[],"output_text":"","metadata":{},"status":"in_progress"});
    let mut events = vec![
        SseEvent::response("response.created", 1, json!({"response":base})),
        SseEvent::response("response.in_progress", 2, json!({"response":base})),
        SseEvent::response(
            "response.output_item.done",
            3,
            json!({"output_index":0,"item":result["output"][0]}),
        ),
    ];
    base["status"] = json!("completed");
    base["output"] = result["output"].clone();
    base["usage"] = if nonempty(&result["usage"]) {
        result["usage"].clone()
    } else {
        json!({"input_tokens":0,"output_tokens":0})
    };
    events.push(SseEvent::response(
        "response.completed",
        4,
        json!({"response":base}),
    ));
    Ok(events)
}
