//! `aggregate_stream`: fold an upstream chat SSE stream into one
//! `chat.completion` object (wb_proxy.py:15412-15558).

use crate::pyval::{get, py_int, truthy};
use crate::sse::strip_data_prefix;
use crate::toolcalls::{is_truncated_arguments, new_id, repair_leaked_tool_calls};
use crate::usage::{estimate_tokens, keep_best, normalize_cache_aliases, total_tokens};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

fn s_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(x) if truthy(x) => crate::pyval::py_str(x),
        _ => String::new(),
    }
}

#[derive(Default)]
pub struct Aggregator {
    content: String,
    reasoning: String,
    has_reasoning: bool,
    finish: Option<Value>,
    tool_calls: BTreeMap<i64, Value>,
    usage: Option<Value>,
    resp_id: Option<String>,
    model: Option<String>,
    pub first_chunk_at: Option<f64>,
    pub saw_done: bool,
}

/// Result of [`Aggregator::finish`].
pub struct Aggregated {
    /// The `chat.completion` object (without the internal timing fields).
    pub obj: Value,
    pub first_chunk_at: Option<f64>,
    pub saw_done: bool,
}

impl Aggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one upstream line (already split on `\n`).
    pub fn push_line(&mut self, line: &str) {
        let data = strip_data_prefix(line);
        if data.is_empty() {
            return;
        }
        if data == "[DONE]" {
            self.saw_done = true;
            return;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return;
        };
        self.push_chunk(&chunk);
    }

    pub fn push_chunk(&mut self, chunk: &Value) {
        if !chunk.is_object() {
            return;
        }
        if self.first_chunk_at.is_none() {
            self.first_chunk_at = Some(crate::timeutil::now());
        }
        if let Some(id) = get(chunk, "id").filter(|v| truthy(v)) {
            self.resp_id = Some(crate::pyval::py_str(id));
        }
        if let Some(m) = get(chunk, "model").filter(|v| truthy(v)) {
            self.model = Some(crate::pyval::py_str(m));
        }
        if let Some(u) = get(chunk, "usage").filter(|v| truthy(v)) {
            keep_best(&mut self.usage, u.clone());
        }
        let Some(choices) = get(chunk, "choices").and_then(|c| c.as_array()) else {
            return;
        };
        for choice in choices {
            let delta = get(choice, "delta")
                .filter(|d| truthy(d))
                .cloned()
                .unwrap_or(json!({}));
            if let Some(Value::String(c)) = get(&delta, "content") {
                self.content.push_str(c);
            }
            if let Some(Value::String(r)) = get(&delta, "reasoning_content") {
                if !r.is_empty() {
                    self.reasoning.push_str(r);
                    self.has_reasoning = true;
                }
            }
            if let Some(Value::Array(tcs)) = get(&delta, "tool_calls") {
                for tc in tcs {
                    let idx = get(tc, "index")
                        .and_then(py_int)
                        .unwrap_or(self.tool_calls.len() as i64);
                    let empty = json!({});
                    let f = get(tc, "function").filter(|v| truthy(v)).unwrap_or(&empty);
                    let call_id = get(tc, "id")
                        .filter(|v| truthy(v))
                        .map(crate::pyval::py_str);
                    let name = s_of(get(f, "name"));
                    let args = s_of(get(f, "arguments"));
                    match self.tool_calls.get_mut(&idx) {
                        None => {
                            let ty = get(tc, "type")
                                .filter(|v| truthy(v))
                                .cloned()
                                .unwrap_or(json!("function"));
                            self.tool_calls.insert(
                                idx,
                                json!({"id": call_id.unwrap_or_else(|| new_id("call_")), "type": ty,
                                       "function": {"name": name, "arguments": args}}),
                            );
                        }
                        Some(e) => {
                            if let Some(cid) = call_id {
                                e["id"] = json!(cid);
                            }
                            if !name.is_empty() {
                                let n = format!(
                                    "{}{}",
                                    e["function"]["name"].as_str().unwrap_or(""),
                                    name
                                );
                                e["function"]["name"] = json!(n);
                            }
                            if !args.is_empty() {
                                let a = format!(
                                    "{}{}",
                                    e["function"]["arguments"].as_str().unwrap_or(""),
                                    args
                                );
                                e["function"]["arguments"] = json!(a);
                            }
                        }
                    }
                }
            }
            if let Some(fc) = get(&delta, "function_call") {
                if fc.is_object() && get(fc, "name").map(truthy).unwrap_or(false) {
                    let name = s_of(get(fc, "name"));
                    let args = s_of(get(fc, "arguments"));
                    match self.tool_calls.get_mut(&0) {
                        None => {
                            self.tool_calls.insert(
                                0,
                                json!({"id": new_id("call_"), "type": "function",
                                       "function": {"name": name, "arguments": args}}),
                            );
                        }
                        Some(e) => {
                            if e["function"]["name"].as_str().unwrap_or("").is_empty() {
                                e["function"]["name"] = json!(name);
                            }
                            if !args.is_empty() {
                                let a = format!(
                                    "{}{}",
                                    e["function"]["arguments"].as_str().unwrap_or(""),
                                    args
                                );
                                e["function"]["arguments"] = json!(a);
                            }
                        }
                    }
                }
            }
            if let Some(fr) = get(choice, "finish_reason").filter(|v| truthy(v)) {
                self.finish = Some(fr.clone());
            }
        }
    }

    /// Build the final object. `model` / `resp_id` are the fallbacks used when
    /// the upstream never sent its own.
    pub fn finish(self, model: &str, resp_id: Option<&str>) -> Aggregated {
        let mut finish = self.finish.unwrap_or(json!("stop"));
        let mut message = Map::new();
        message.insert("role".into(), json!("assistant"));
        message.insert("content".into(), json!(self.content));
        if self.has_reasoning {
            message.insert("reasoning_content".into(), json!(self.reasoning));
        }
        let mut message = Value::Object(message);
        let mut calls = self.tool_calls;
        if calls.is_empty() && repair_leaked_tool_calls(&mut message) {
            finish = json!("tool_calls");
        }
        calls.retain(|_, v| {
            v["function"]["name"]
                .as_str()
                .map(|s| !s.is_empty())
                .unwrap_or(false)
        });
        if !calls.is_empty() && (finish == json!("length") || !self.saw_done) {
            calls.retain(|_, v| !is_truncated_arguments(v["function"].get("arguments")));
        }
        if !calls.is_empty() {
            message["tool_calls"] = Value::Array(calls.into_values().collect());
            if finish == json!("stop") || finish.is_null() {
                finish = json!("tool_calls");
            }
        } else if message.get("tool_calls").is_some() {
            finish = json!("tool_calls");
        } else if finish == json!("tool_calls") {
            finish = json!("stop");
        }
        let mut usage = self.usage;
        if usage
            .as_ref()
            .map(|u| total_tokens(u) == 0.0)
            .unwrap_or(true)
            && (!self.content.is_empty() || !self.reasoning.is_empty())
        {
            let comp = estimate_tokens(&self.content) + estimate_tokens(&self.reasoning);
            let prompt = (comp / 2).max(1);
            usage = Some(json!({
                "prompt_tokens": prompt,
                "completion_tokens": comp,
                "total_tokens": prompt + comp,
                "completion_tokens_details": {"reasoning_tokens": estimate_tokens(&self.reasoning)},
                "prompt_tokens_details": {"cached_tokens": 0},
            }));
        }
        let id = self
            .resp_id
            .or_else(|| resp_id.map(str::to_string))
            .unwrap_or_else(|| format!("chatcmpl-wb-{}", &crate::ids::uuid4_hex()[..16]));
        let mut out = Map::new();
        out.insert("id".into(), json!(id));
        out.insert("object".into(), json!("chat.completion"));
        out.insert("created".into(), json!(crate::timeutil::now() as i64));
        out.insert(
            "model".into(),
            json!(self.model.unwrap_or_else(|| model.to_string())),
        );
        out.insert(
            "choices".into(),
            json!([{"index": 0, "message": message, "finish_reason": finish}]),
        );
        if let Some(mut u) = usage.filter(truthy) {
            normalize_cache_aliases(&mut u);
            out.insert("usage".into(), u);
        }
        Aggregated {
            obj: Value::Object(out),
            first_chunk_at: self.first_chunk_at,
            saw_done: self.saw_done,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_tool_calls() {
        let mut a = Aggregator::new();
        for l in [
            r#"data: {"id":"up-9","model":"deepseek-v4.1-flash","choices":[{"index":0,"delta":{"reasoning_content":"hmm"}}]}"#,
            r#"data: {"id":"up-9","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"ls","arguments":"{\"p\""}}]}}]}"#,
            r#"data: {"id":"up-9","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":":1}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14}}"#,
            "data: [DONE]",
        ] {
            a.push_line(l);
        }
        let r = a.finish("x", None);
        let o = r.obj;
        assert_eq!(o["id"], "up-9");
        assert_eq!(o["model"], "deepseek-v4.1-flash");
        assert_eq!(o["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(o["choices"][0]["message"]["content"], "");
        assert_eq!(o["choices"][0]["message"]["reasoning_content"], "hmm");
        assert_eq!(
            o["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"p\":1}"
        );
        assert_eq!(o["usage"]["total_tokens"], 14);
    }

    #[test]
    fn drops_truncated_and_downgrades() {
        let mut a = Aggregator::new();
        a.push_line(r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"f","arguments":"{\"a\":"}}]},"finish_reason":"tool_calls"}]}"#);
        let o = a.finish("m", None).obj;
        assert_eq!(o["choices"][0]["finish_reason"], "stop");
        assert!(o["choices"][0]["message"].get("tool_calls").is_none());
        assert!(o.get("usage").is_none());
    }

    #[test]
    fn estimates_usage() {
        let mut a = Aggregator::new();
        a.push_line(r#"data: {"choices":[{"delta":{"content":"hello world"}}]}"#);
        a.push_line("data: [DONE]");
        let o = a.finish("m", None).obj;
        assert_eq!(o["usage"]["completion_tokens"], 3);
        assert_eq!(o["usage"]["prompt_tokens"], 1);
        assert!(o["id"].as_str().unwrap().starts_with("chatcmpl-wb-"));
    }
}
