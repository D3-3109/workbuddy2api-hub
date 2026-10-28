use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use wbcore::aggregate::Aggregator;
use wbcore::pyval::{py_int, truthy};
use wbcore::sse::{chunk_error, clean_chunk_value, strip_data_prefix, CleanResult};
use wbcore::toolcalls::{call_to_tool_call, new_id, LeakFilter, LeakOut};
use wbcore::usage::{
    estimate_tokens, extract_usage, keep_best, normalize_cache_aliases, total_tokens,
};

/// Read-only through `ChatStream::accounting`; never serialized into completions.
#[derive(Clone, Debug)]
pub struct StreamAccounting {
    /// Best upstream usage while running; success may replace missing/zero
    /// usage with an estimate for logging only. Never estimated on abort.
    pub usage: Option<Value>,
    pub upstream_usage: Option<Value>,
    pub usage_estimated: bool,
    /// Error object (without its outer `error` envelope).
    pub error: Option<Value>,
    pub finish_reason: Option<String>,
    pub saw_done: bool,
    pub first_chunk_at: Option<f64>,
    pub first_emitted_at: Option<f64>,
    pub elapsed_ms: u64,
    pub emitted: bool,
    pub completed: bool,
    pub aborted: bool,
    pub client_aborted: bool,
    /// in_progress / completed / upstream_aborted / client_aborted.
    pub outcome: &'static str,
}

impl Default for StreamAccounting {
    fn default() -> Self {
        Self {
            usage: None,
            upstream_usage: None,
            usage_estimated: false,
            error: None,
            finish_reason: None,
            saw_done: false,
            first_chunk_at: None,
            first_emitted_at: None,
            elapsed_ms: 0,
            emitted: false,
            completed: false,
            aborted: false,
            client_aborted: false,
            outcome: "in_progress",
        }
    }
}

impl StreamAccounting {
    /// Common fields for the caller's usage log; no account/key/realm policy.
    pub fn usage_fields(&self) -> Value {
        Value::Object(extract_usage(self.usage.as_ref()))
    }
}

/// An upstream SSE line -> downstream SSE frame state machine, with no I/O.
///
/// `[DONE]` is recorded, not forwarded; call `finish()` after clean EOF so all
/// usage has been observed. `abort(error)` accepts an error string, error
/// object, or `{error: ...}` envelope and emits exactly one terminal error
/// (plus any already-buffered text), without a success tail. `client_aborted`
/// discards buffers silently. All operations after a terminal transition no-op.
///
/// The non-stream completion preserves the upstream id and model via wbcore's
/// Aggregator. The stream has one independent, stable gateway-generated id.
/// A transport failure is the caller's responsibility; EOF is not proof that
/// a Content-Length body was complete.
pub struct ChatStream {
    model: String,
    response_id: String,
    started_at: f64,
    created: i64,
    accounting: StreamAccounting,
    aggregator: Option<Aggregator>,
    completion: Option<Value>,
    filters: BTreeMap<i64, LeakFilter>,
    pending_finish: BTreeMap<i64, String>,
    synthesized: BTreeMap<i64, bool>,
    next_tool_index: BTreeMap<i64, i64>,
    structured_indices: HashMap<(i64, i64), i64>,
    structured_ids: HashMap<(i64, String), i64>,
    allocated_ids: HashMap<(i64, i64), String>,
    streamed_text: String,
    max_bytes: usize,
    received_bytes: usize,
}

impl ChatStream {
    /// Default cumulative upstream line-byte cap (including comments/metadata).
    pub const DEFAULT_MAX_BYTES: usize = 32 * 1024 * 1024;

    pub fn new(model: &str) -> Self {
        Self::with_max_bytes(model, Self::DEFAULT_MAX_BYTES)
    }

    /// Bound accumulation including the reused non-stream Aggregator. On
    /// overflow the stream aborts without DONE. The caller should separately
    /// bound unsplit network buffers before invoking `push_line`.
    pub fn with_max_bytes(model: &str, max_bytes: usize) -> Self {
        let started_at = wbcore::timeutil::now();
        Self {
            model: model.to_string(),
            response_id: format!("chatcmpl-wb-{}", &wbcore::ids::uuid4_hex()[..16]),
            started_at,
            created: started_at as i64,
            accounting: StreamAccounting::default(),
            aggregator: Some(Aggregator::new()),
            completion: None,
            filters: BTreeMap::new(),
            pending_finish: BTreeMap::new(),
            synthesized: BTreeMap::new(),
            next_tool_index: BTreeMap::new(),
            structured_indices: HashMap::new(),
            structured_ids: HashMap::new(),
            allocated_ids: HashMap::new(),
            streamed_text: String::new(),
            max_bytes,
            received_bytes: 0,
        }
    }

    /// Skip building the non-stream `completion()`; streaming proxies never read it.
    pub fn without_completion(mut self) -> Self {
        self.aggregator = None;
        self
    }

    pub fn response_id(&self) -> &str {
        &self.response_id
    }
    pub fn accounting(&self) -> &StreamAccounting {
        &self.accounting
    }
    pub fn completion(&self) -> Option<&Value> {
        self.completion.as_ref()
    }
    pub fn is_terminal(&self) -> bool {
        self.accounting.completed || self.accounting.aborted
    }

    fn clock(&mut self) {
        self.accounting.elapsed_ms =
            ((wbcore::timeutil::now() - self.started_at).max(0.0) * 1000.0) as u64;
    }

    fn emit(&mut self, value: &Value, output: &mut Vec<String>) {
        self.emit_raw(&wbcore::pyjson::dumps_u(value), output);
    }
    fn emit_raw(&mut self, text: &str, output: &mut Vec<String>) {
        if self.accounting.first_emitted_at.is_none() {
            self.accounting.first_emitted_at = Some(wbcore::timeutil::now());
        }
        self.accounting.emitted = true;
        output.push(format!("data: {text}\n\n"));
    }
    fn chunk(&self, choices: Value) -> Value {
        json!({"id":self.response_id, "object":"chat.completion.chunk", "created":self.created,
            "model":self.model, "choices":choices})
    }
    fn delta_chunk(&self, choice: i64, delta: Value) -> Value {
        self.chunk(json!([{"index":choice, "delta":delta, "finish_reason":null}]))
    }
    fn allocate_index(&mut self, choice: i64) -> i64 {
        let n = self.next_tool_index.entry(choice).or_default();
        let out = *n;
        *n += 1;
        out
    }

    fn release(&mut self, choice: i64, events: Vec<LeakOut>, output: &mut Vec<String>) {
        for event in events {
            match event {
                LeakOut::Text(text) if !text.is_empty() => {
                    self.emit(&self.delta_chunk(choice, json!({"content":text})), output)
                }
                LeakOut::Calls(calls) => {
                    for call in calls {
                        let index = self.allocate_index(choice);
                        let mut tc = call_to_tool_call(&call);
                        tc["index"] = json!(index);
                        self.emit(
                            &self.delta_chunk(choice, json!({"tool_calls":[tc]})),
                            output,
                        );
                        self.synthesized.insert(choice, true);
                    }
                }
                _ => {}
            }
        }
    }
    fn flush_filters(&mut self, output: &mut Vec<String>) {
        let choices: Vec<_> = self.filters.keys().copied().collect();
        for choice in choices {
            let events = self.filters.get_mut(&choice).unwrap().flush();
            self.release(choice, events, output);
        }
    }

    fn capture_usage(&mut self, chunk: &Value) {
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object() && truthy(u)) {
            keep_best(&mut self.accounting.upstream_usage, usage.clone());
            if let Some(u) = &mut self.accounting.upstream_usage {
                normalize_cache_aliases(u);
            }
            self.accounting.usage = self.accounting.upstream_usage.clone();
        }
    }

    fn normalize_stream_calls(&mut self, choice: i64, delta: &mut Value) {
        // Legacy argument-only continuation chunks would be dropped by
        // clean_chunk_value. Convert the legacy shape before cleaning.
        if delta.get("tool_calls").is_none() {
            if let Some(f) = delta
                .get("function_call")
                .filter(|f| f.is_object())
                .cloned()
            {
                if f.get("name").is_some_and(truthy) || f.get("arguments").is_some_and(truthy) {
                    delta.as_object_mut().unwrap().remove("function_call");
                    delta["tool_calls"] = json!([{"index":0, "type":"function", "function":f}]);
                }
            }
        }
        if let Some(calls) = delta.get_mut("tool_calls").and_then(Value::as_array_mut) {
            for (position, call) in calls.iter_mut().enumerate() {
                if !call.is_object() {
                    continue;
                }
                let input_index = call
                    .get("index")
                    .and_then(py_int)
                    .unwrap_or(position as i64);
                let id = call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let index = id
                    .as_ref()
                    .and_then(|id| self.structured_ids.get(&(choice, id.clone())))
                    .copied()
                    .or_else(|| {
                        // A new id with no explicit index starts a new call,
                        // even when each upstream batch contains one item.
                        if id.is_some() && call.get("index").is_none() {
                            None
                        } else {
                            self.structured_indices.get(&(choice, input_index)).copied()
                        }
                    })
                    .unwrap_or_else(|| self.allocate_index(choice));
                self.structured_indices.insert((choice, input_index), index);
                if let Some(id) = &id {
                    self.structured_ids.insert((choice, id.clone()), index);
                }
                let first = !self.allocated_ids.contains_key(&(choice, index));
                let stable_id = self
                    .allocated_ids
                    .entry((choice, index))
                    .or_insert_with(|| id.clone().unwrap_or_else(|| new_id("call_")))
                    .clone();
                call["index"] = json!(index);
                if first || id.is_some() {
                    call["id"] = json!(stable_id);
                }
                if first {
                    call["type"] = json!("function");
                }
            }
        }
    }

    /// Feed one already-split line. Returns zero or more complete SSE frames.
    pub fn push_line(&mut self, line: &str) -> Vec<String> {
        if self.is_terminal() {
            return Vec::new();
        }
        self.clock();
        self.received_bytes = self.received_bytes.saturating_add(line.len());
        if self.received_bytes > self.max_bytes {
            return self.abort(
                json!({"message":format!("upstream stream exceeds {} byte limit", self.max_bytes),
                "type":"upstream_error", "param":null, "code":"upstream_stream_error"}),
            );
        }
        let data = strip_data_prefix(line);
        if data.is_empty() {
            return Vec::new();
        }
        if data == "[DONE]" {
            self.accounting.saw_done = true;
            if let Some(agg) = &mut self.aggregator {
                agg.push_line("data: [DONE]");
            }
            return Vec::new();
        }
        let mut output = Vec::new();
        let chunk = match serde_json::from_str::<Value>(data) {
            Ok(v) if v.is_object() => v,
            // Baseline passthrough for non-object/non-JSON upstream data.
            _ => {
                self.emit_raw(data, &mut output);
                return output;
            }
        };
        self.push_object(chunk, output)
    }

    /// `push_line` for a line the caller already parsed: `raw` is the full
    /// line and `chunk` its decoded JSON object. Avoids parsing twice.
    pub fn push_value(&mut self, raw: &str, chunk: Value) -> Vec<String> {
        if self.is_terminal() {
            return Vec::new();
        }
        self.clock();
        self.received_bytes = self.received_bytes.saturating_add(raw.len());
        if self.received_bytes > self.max_bytes {
            return self.abort(
                json!({"message":format!("upstream stream exceeds {} byte limit", self.max_bytes),
                "type":"upstream_error", "param":null, "code":"upstream_stream_error"}),
            );
        }
        if !chunk.is_object() {
            let mut output = Vec::new();
            self.emit_raw(wbcore::sse::strip_data_prefix(raw), &mut output);
            return output;
        }
        self.push_object(chunk, Vec::new())
    }

    fn push_object(&mut self, mut chunk: Value, mut output: Vec<String>) -> Vec<String> {
        if self.accounting.first_chunk_at.is_none() {
            self.accounting.first_chunk_at = Some(wbcore::timeutil::now());
        }
        self.capture_usage(&chunk);
        if let Some(error) = chunk_error(&chunk) {
            return self.abort(error.clone());
        }
        if let Some(agg) = &mut self.aggregator {
            agg.push_chunk(&chunk);
        }
        chunk.as_object_mut().unwrap().remove("usage");
        // Unlike the baseline, absent ids also receive the stable id.
        chunk["id"] = json!(self.response_id);
        let mut error_finish = false;
        if let Some(choices) = chunk.get_mut("choices").and_then(Value::as_array_mut) {
            for (position, ch) in choices.iter_mut().enumerate() {
                if !ch.is_object() {
                    continue;
                }
                let choice = ch.get("index").and_then(py_int).unwrap_or(position as i64);
                if let Some(reason) = ch
                    .get("finish_reason")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    self.accounting.finish_reason = Some(reason.to_string());
                    self.pending_finish.insert(choice, reason.to_string());
                    if reason == "error" {
                        error_finish = true;
                    }
                    ch["finish_reason"] = Value::Null;
                }
                if let Some(delta) = ch.get_mut("delta").filter(|d| d.is_object()) {
                    self.normalize_stream_calls(choice, delta);
                    if let Some(reasoning) = delta["reasoning_content"].as_str() {
                        self.streamed_text.push_str(reasoning);
                    }
                    if let Some(content) = delta["content"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                    {
                        self.streamed_text.push_str(&content);
                        delta.as_object_mut().unwrap().remove("content");
                        let events = self.filters.entry(choice).or_default().feed(&content);
                        self.release(choice, events, &mut output);
                    }
                }
            }
            // Clean choices separately: an empty choice must not drop a useful
            // parallel choice from the same upstream frame.
            choices.retain_mut(|ch| {
                let mut one = json!({"choices":[ch.take()]});
                let keep = clean_chunk_value(&mut one) != CleanResult::Drop;
                *ch = one["choices"][0].take();
                keep
            });
            if !choices.is_empty() {
                self.emit(&chunk, &mut output);
            }
        } else if clean_chunk_value(&mut chunk) != CleanResult::Drop {
            self.emit(&chunk, &mut output);
        }
        if error_finish {
            output.extend(self.abort(json!({"message":"upstream stream finished with error", "type":"upstream_error", "param":null, "code":"upstream_stream_error"})));
        }
        output
    }

    /// Successful transport EOF. Flush text/calls, finish, upstream usage, DONE.
    /// Estimates are recorded in accounting only, never synthesized on the wire.
    pub fn finish(&mut self) -> Vec<String> {
        if self.is_terminal() {
            return Vec::new();
        }
        let mut output = Vec::new();
        self.flush_filters(&mut output);
        if !self.accounting.emitted
            && self.pending_finish.is_empty()
            && self.accounting.upstream_usage.is_none()
        {
            output.extend(
                self.abort(json!({"message":"empty upstream stream", "type":"server_error"})),
            );
            return output;
        }
        let mut finishes = self.pending_finish.clone();
        for choice in self.synthesized.keys() {
            finishes.insert(*choice, "tool_calls".into());
        }
        if !finishes.is_empty() {
            let choices: Vec<_> = finishes
                .iter()
                .map(|(index, reason)| json!({"index":index,"delta":{},"finish_reason":reason}))
                .collect();
            self.accounting.finish_reason = finishes.values().next_back().cloned();
            self.emit(&self.chunk(Value::Array(choices)), &mut output);
        }
        if let Some(usage) = &self.accounting.upstream_usage {
            let mut chunk = self.chunk(json!([]));
            chunk["usage"] = usage.clone();
            self.emit(&chunk, &mut output);
        }
        if self
            .accounting
            .usage
            .as_ref()
            .is_none_or(|u| total_tokens(u) == 0.0)
            && !self.streamed_text.is_empty()
        {
            let completion = estimate_tokens(&self.streamed_text);
            let prompt = (completion / 2).max(1);
            self.accounting.usage = Some(
                json!({"prompt_tokens":prompt,"completion_tokens":completion,"total_tokens":prompt+completion,
                "completion_tokens_details":{"reasoning_tokens":0},"prompt_tokens_details":{"cached_tokens":0}}),
            );
            self.accounting.usage_estimated = true;
        }
        if let Some(agg) = self.aggregator.take() {
            self.completion = Some(agg.finish(&self.model, Some(&self.response_id)).obj);
        }
        output.push("data: [DONE]\n\n".into());
        self.accounting.completed = true;
        self.accounting.outcome = "completed";
        self.clock();
        output
    }

    /// Terminal upstream failure; never emit a finish/usage/DONE success tail.
    pub fn abort(&mut self, error: Value) -> Vec<String> {
        if self.is_terminal() {
            return Vec::new();
        }
        let mut output = Vec::new();
        self.flush_filters(&mut output);
        let error = error.get("error").cloned().unwrap_or(error);
        let error = if error.is_object() {
            error
        } else {
            json!({"message":format!("upstream stream aborted: {}", error.as_str().map(str::to_string).unwrap_or_else(|| error.to_string())),
                "type":"upstream_error", "param":null, "code":"upstream_stream_error"})
        };
        self.emit(&json!({"error":error}), &mut output);
        self.accounting.error = Some(error);
        self.accounting.aborted = true;
        self.accounting.outcome = "upstream_aborted";
        self.aggregator = None;
        self.clock();
        output
    }

    /// Client disconnect: retain partial upstream usage, write nothing further.
    pub fn client_aborted(&mut self) {
        if self.is_terminal() {
            return;
        }
        self.filters.clear();
        self.aggregator = None;
        self.accounting.aborted = true;
        self.accounting.client_aborted = true;
        self.accounting.outcome = "client_aborted";
        self.clock();
    }
}
