//! Sans-IO Responses and Anthropic Messages translation.
//!
//! Translate requests with [`responses_to_chat`] / [`messages_to_chat`], passing
//! catalog defaults and feature flags in [`TranslationOptions`]. Send `.chat` to
//! the chat pipeline; keep `.metadata` for Responses output. No private markers
//! are inserted in the chat body. All network execution belongs to the caller.
//!
//! For streaming, feed each chat SSE line to [`ResponsesStream::push_line`] or
//! [`MessagesStream::push_line`] and write each returned [`SseEvent::to_sse`]
//! immediately. Call `finish` at EOF or `abort` on an upstream read failure.
//! Finishing/aborting is idempotent; an error can never become a success.
//! Responses `start()` optionally emits lifecycle frames before reading upstream.
//! Adapters accumulate final item state, not the original stream; ordinary text
//! and argument deltas are emitted during `push_line`, not replayed at EOF.
//!
//! Catalog lookups, HTTP/auth, heartbeat scheduling, local web execution and
//! persistent response state are deliberately outside this crate.

use serde_json::json;
pub use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use wbcore::pyval::{py_int, py_str, truthy};

mod aggregate;
pub use aggregate::CheckedAggregator;
mod compaction;
mod outputs;
mod requests;
mod streams;
pub use compaction::{
    chat_to_compaction, compaction_events, decode_compaction_summary, encode_compaction_summary,
    has_compaction_trigger, COMPACTION_PROMPT,
};
pub use outputs::{
    anthropic_usage, chat_to_messages, chat_to_response, reasoning_only_response, responses_usage,
};
pub use requests::{
    count_tokens, estimate_chat_tokens, messages_to_chat, resolve_namespaced_name,
    responses_to_chat,
};
pub use streams::{MessagesStream, ResponsesStream, SseEvent};

#[derive(Debug, Clone, Default)]
pub struct TranslationOptions {
    pub preserve_request_contract: bool,
    /// Supply the catalog's positive maxOutputTokens; no catalog dependency.
    pub default_max_output_tokens: Option<i64>,
    /// Force compaction (the /responses/compact route). Input triggers also work.
    pub compaction: bool,
    /// Unsupported when web execution is requested; never silently enabled.
    pub local_web_tools: bool,
}

#[derive(Debug, Clone)]
pub struct RequestMetadata {
    /// Original tools, including additional_tools; echoed to Responses clients.
    pub tools: Value,
    pub tool_choice: Value,
    pub parallel_tool_calls: Value,
    pub custom_names: BTreeSet<String>,
    pub namespace_map: BTreeMap<String, String>,
}
impl Default for RequestMetadata {
    fn default() -> Self {
        Self {
            tools: json!([]),
            tool_choice: json!("auto"),
            parallel_tool_calls: json!(true),
            custom_names: BTreeSet::new(),
            namespace_map: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TranslatedRequest {
    pub chat: Value,
    pub metadata: RequestMetadata,
    pub compaction: bool,
}

#[derive(Debug, Clone)]
pub struct ResponseOptions {
    /// Requested (not upstream-reported) model for Responses objects.
    pub model: String,
    pub metadata: RequestMetadata,
    /// Optional stable response id / clock, useful for gateway correlation/tests.
    pub response_id: Option<String>,
    pub created_at: Option<i64>,
}
impl Default for ResponseOptions {
    fn default() -> Self {
        Self {
            model: "deepseek-v4.1-flash".into(),
            metadata: RequestMetadata::default(),
            response_id: None,
            created_at: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationError {
    pub status: u16,
    pub code: String,
    pub message: String,
}
impl TranslationError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            code: "invalid_request_error".into(),
            message: message.into(),
        }
    }
    pub fn upstream(message: impl Into<String>) -> Self {
        Self {
            status: 502,
            code: "upstream_stream_error".into(),
            message: message.into(),
        }
    }
    pub fn openai(&self) -> Value {
        json!({"error":{"message":self.message,"type":if self.status < 500 {"invalid_request_error"} else {"server_error"},"param":null,"code":self.code}})
    }
    pub fn anthropic(&self) -> Value {
        json!({"type":"error","error":{"type":if self.status < 500 {"invalid_request_error"} else {"api_error"},"message":self.message}})
    }
}
impl std::fmt::Display for TranslationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for TranslationError {}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(_) | Value::Bool(_) => py_str(v),
        _ => String::new(),
    }
}
fn strval(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}
fn nonempty(v: &Value) -> bool {
    truthy(v)
}
fn int(v: &Value) -> i64 {
    py_int(v).unwrap_or(0)
}
fn array(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn id(prefix: &str) -> String {
    wbcore::toolcalls::new_id(prefix)
}
fn created(options: &ResponseOptions) -> i64 {
    options
        .created_at
        .unwrap_or_else(|| wbcore::timeutil::now() as i64)
}
fn json_text(v: &Value) -> String {
    match v {
        Value::Null => "{}".into(),
        Value::String(s) => s.clone(),
        _ => wbcore::pyjson::dumps_compact(v),
    }
}
fn upstream_error(v: &Value) -> Option<TranslationError> {
    v.get("error")
        .filter(|e| e.is_object() || nonempty(e))
        .map(|e| {
            let message = if nonempty(&e["message"]) {
                text(&e["message"])
            } else if e.is_string() {
                text(e)
            } else {
                "upstream error".into()
            };
            TranslationError::upstream(message)
        })
}

#[cfg(test)]
mod oracle_tests;
#[cfg(test)]
mod tests;
