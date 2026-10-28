use super::*;
use crate::outputs::{
    anthropic_id, message_item, reason_item, response_base, stop_reason, text_part,
    unwrap_custom_input,
};
use crate::requests::{is_custom, stamp_namespace};
use wbcore::toolcalls::{LeakFilter, LeakOut, ParsedCall};

/// An owned frame ready for immediate delivery. Responses JSON uses Python's
/// spaced separators; Anthropic JSON uses compact separators.
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: String,
    pub data: Value,
}
impl SseEvent {
    pub fn to_sse(&self) -> String {
        let data = if self.data.get("sequence_number").is_some() {
            wbcore::pyjson::dumps_u(&self.data)
        } else {
            wbcore::pyjson::dumps_compact(&self.data)
        };
        format!("event: {}\ndata: {}\n\n", self.event, data)
    }
    pub(crate) fn response(kind: &str, sequence: u64, payload: Value) -> Self {
        let mut data = json!({"type":kind,"sequence_number":sequence});
        if let Some(m) = payload.as_object() {
            for (k, v) in m {
                data[k] = v.clone();
            }
        }
        Self {
            event: kind.into(),
            data,
        }
    }
    fn anthropic(kind: &str, mut data: Value) -> Self {
        data["type"] = json!(kind);
        Self {
            event: kind.into(),
            data,
        }
    }
}

struct ResponseCall {
    output_index: usize,
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
    custom: bool,
    opened: bool,
}
impl ResponseCall {
    fn item(&self, status: &str, metadata: &RequestMetadata) -> Value {
        let mut v = if status == "in_progress" {
            json!({"id":self.item_id,"status":status,"call_id":self.call_id,"name":self.name})
        } else {
            json!({"id":self.item_id,"type":if self.custom {"custom_tool_call"} else {"function_call"},"status":status,"call_id":self.call_id,"name":self.name})
        };
        v["type"] = json!(if self.custom {
            "custom_tool_call"
        } else {
            "function_call"
        });
        if self.custom {
            v["input"] = json!(if status == "in_progress" {
                String::new()
            } else {
                unwrap_custom_input(&json!(self.arguments))
            });
        } else {
            v["arguments"] = json!(if status == "in_progress" {
                ""
            } else {
                &self.arguments
            });
        }
        stamp_namespace(&mut v, metadata);
        v
    }
}

/// Incremental chat SSE -> Responses SSE. Each `push_line` returns new frames;
/// only final content/item state is retained. `[DONE]` is observed, but usage
/// frames following it can still be consumed until the caller calls `finish`.
pub struct ResponsesStream {
    options: ResponseOptions,
    response_id: String,
    created: i64,
    sequence: u64,
    started: bool,
    terminal: bool,
    failed: bool,
    saw_done: bool,
    finish_reason: String,
    outputs: Vec<Option<Value>>,
    text: String,
    reasoning: String,
    reasoning_segment: String,
    reason_index: Option<usize>,
    reason_id: String,
    reason_closed: bool,
    msg_index: Option<usize>,
    msg_id: String,
    calls: BTreeMap<(i64, i64), ResponseCall>,
    filter: LeakFilter,
    printed_calls: Vec<ParsedCall>,
    usage: Option<Value>,
}
impl ResponsesStream {
    pub fn new(options: ResponseOptions) -> Self {
        Self {
            response_id: options.response_id.clone().unwrap_or_else(|| id("resp_")),
            created: created(&options),
            options,
            sequence: 0,
            started: false,
            terminal: false,
            failed: false,
            saw_done: false,
            finish_reason: "stop".into(),
            outputs: vec![],
            text: String::new(),
            reasoning: String::new(),
            reasoning_segment: String::new(),
            reason_index: None,
            reason_id: id("rs_"),
            reason_closed: false,
            msg_index: None,
            msg_id: id("msg_"),
            calls: BTreeMap::new(),
            filter: LeakFilter::new(),
            printed_calls: vec![],
            usage: None,
        }
    }
    pub fn is_terminal(&self) -> bool {
        self.terminal
    }
    pub fn failed(&self) -> bool {
        self.failed
    }
    /// Raw chat usage for logging; available after every push and after finish.
    pub fn usage(&self) -> Option<&Value> {
        self.usage.as_ref()
    }
    pub fn response_id(&self) -> &str {
        &self.response_id
    }
    fn emit(&mut self, frames: &mut Vec<SseEvent>, kind: &str, data: Value) {
        self.sequence += 1;
        frames.push(SseEvent::response(kind, self.sequence, data));
    }
    fn snapshot(&self, status: &str) -> Value {
        let mut v = response_base(
            &self.options,
            &self.response_id,
            self.created,
            status,
            self.outputs.iter().flatten().cloned().collect(),
            &self.text,
            true,
        );
        if let Some(u) = self.usage.as_ref().and_then(responses_usage) {
            v["usage"] = u;
        }
        v
    }
    pub fn start(&mut self) -> Vec<SseEvent> {
        let mut frames = vec![];
        if !self.started && !self.terminal {
            self.started = true;
            self.emit(
                &mut frames,
                "response.created",
                json!({"response":self.snapshot("in_progress")}),
            );
            self.emit(
                &mut frames,
                "response.in_progress",
                json!({"response":self.snapshot("in_progress")}),
            );
        }
        frames
    }
    fn close_reasoning(&mut self, frames: &mut Vec<SseEvent>) {
        let Some(index) = self.reason_index else {
            return;
        };
        if self.reason_closed {
            return;
        }
        let part = json!({"type":"summary_text","text":self.reasoning_segment});
        self.emit(frames,"response.reasoning_summary_text.done",json!({"item_id":self.reason_id,"output_index":index,"summary_index":0,"text":self.reasoning_segment}));
        self.emit(
            frames,
            "response.reasoning_summary_part.done",
            json!({"item_id":self.reason_id,"output_index":index,"summary_index":0,"part":part}),
        );
        let item = reason_item(&self.reason_id, "completed", &self.reasoning_segment);
        self.outputs[index] = Some(item.clone());
        self.reason_closed = true;
        self.emit(
            frames,
            "response.output_item.done",
            json!({"output_index":index,"item":item}),
        );
    }
    fn push_reasoning(&mut self, piece: &str, frames: &mut Vec<SseEvent>) {
        if piece.is_empty() {
            return;
        }
        // Do not emit deltas against an already-completed item (Python bug).
        if self.reason_index.is_none() || self.reason_closed {
            if self.reason_closed {
                self.reason_id = id("rs_");
                self.reasoning_segment.clear();
            }
            self.reason_closed = false;
            let index = self.outputs.len();
            self.outputs.push(None);
            self.reason_index = Some(index);
            self.emit(
                frames,
                "response.output_item.added",
                json!({"output_index":index,"item":reason_item(&self.reason_id,"in_progress","")}),
            );
            self.emit(frames,"response.reasoning_summary_part.added",json!({"item_id":self.reason_id,"output_index":index,"summary_index":0,"part":{"type":"summary_text","text":""}}));
        }
        self.reasoning.push_str(piece);
        self.reasoning_segment.push_str(piece);
        self.emit(frames,"response.reasoning_summary_text.delta",json!({"item_id":self.reason_id,"output_index":self.reason_index,"summary_index":0,"delta":piece}));
    }
    fn open_message(&mut self, frames: &mut Vec<SseEvent>) {
        if self.msg_index.is_some() {
            return;
        }
        self.close_reasoning(frames);
        let index = self.outputs.len();
        self.outputs.push(None);
        self.msg_index = Some(index);
        self.emit(
            frames,
            "response.output_item.added",
            json!({"output_index":index,"item":message_item(&self.msg_id,"in_progress","")}),
        );
        self.emit(frames,"response.content_part.added",json!({"item_id":self.msg_id,"output_index":index,"content_index":0,"part":text_part("")}));
    }
    fn filter_outputs(&mut self, outputs: Vec<LeakOut>, frames: &mut Vec<SseEvent>) {
        for out in outputs {
            match out {
                LeakOut::Text(s) if !s.is_empty() => {
                    self.open_message(frames);
                    self.text.push_str(&s);
                    self.emit(frames,"response.output_text.delta",json!({"item_id":self.msg_id,"output_index":self.msg_index,"content_index":0,"delta":s}));
                }
                LeakOut::Calls(calls) => self.printed_calls.extend(calls),
                _ => {}
            }
        }
    }
    fn push_call(
        &mut self,
        choice_index: i64,
        ordinal: usize,
        tc: &Value,
        frames: &mut Vec<SseEvent>,
    ) {
        let cid = text(&tc["id"]);
        let index = tc
            .get("index")
            .and_then(py_int)
            .or_else(|| {
                if cid.is_empty() {
                    None
                } else {
                    self.calls
                        .iter()
                        .find(|((c, _), v)| *c == choice_index && v.call_id == cid)
                        .map(|((_, i), _)| *i)
                }
            })
            .unwrap_or(ordinal as i64);
        let key = (choice_index, index);
        let name = text(&tc["function"]["name"]);
        let args = if tc["function"]["arguments"].is_null() {
            String::new()
        } else {
            json_text(&tc["function"]["arguments"])
        };
        let new = !self.calls.contains_key(&key);
        if new {
            let output_index = self.outputs.len();
            self.outputs.push(None);
            let custom = is_custom(&name, &self.options.metadata);
            self.calls.insert(
                key,
                ResponseCall {
                    output_index,
                    item_id: id(if custom { "ctc_" } else { "fc_" }),
                    call_id: if cid.is_empty() { id("call_") } else { cid },
                    name,
                    arguments: args.clone(),
                    custom,
                    opened: false,
                },
            );
        } else {
            let call = self.calls.get_mut(&key).unwrap();
            if call.name.is_empty() {
                call.name = name;
            }
            // Upstream may repeat the name, but names are not argument fragments.
            call.arguments.push_str(&args);
        }
        let call = self.calls.get_mut(&key).unwrap();
        let mut added = None;
        let mut delta = None;
        if !call.opened && !call.name.is_empty() {
            call.custom = is_custom(&call.name, &self.options.metadata);
            if call.custom && !call.item_id.starts_with("ctc_") {
                call.item_id = id("ctc_");
            }
            call.opened = true;
            added = Some(
                json!({"output_index":call.output_index,"item":call.item("in_progress",&self.options.metadata)}),
            );
        }
        // de4d86d: the opening chunk can already carry arguments. Emit them
        // after output_item.added, without accumulating that fragment twice.
        if call.opened && !args.is_empty() {
            delta = Some((
                if call.custom {
                    "response.custom_tool_call_input.delta"
                } else {
                    "response.function_call_arguments.delta"
                },
                json!({"output_index":call.output_index,"item_id":call.item_id,"call_id":call.call_id,"delta":args}),
            ));
        }
        if let Some(data) = added {
            self.emit(frames, "response.output_item.added", data);
        }
        if let Some((kind, data)) = delta {
            self.emit(frames, kind, data);
        }
    }
    /// Parse a single upstream chat SSE line. Malformed JSON / SSE comments are
    /// ignored for baseline parity; valid in-band errors are terminal failures.
    pub fn push_line(&mut self, line: &str) -> Vec<SseEvent> {
        if self.terminal {
            return vec![];
        }
        let mut frames = self.start();
        let data = wbcore::sse::strip_data_prefix(line);
        if data == "[DONE]" {
            self.saw_done = true;
            return frames;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return frames;
        };
        frames.extend(self.push_chunk(&chunk));
        frames
    }
    pub fn push_chunk(&mut self, chunk: &Value) -> Vec<SseEvent> {
        if self.terminal {
            return vec![];
        }
        let mut frames = self.start();
        if let Some(error) = upstream_error(chunk) {
            self.terminal = true;
            self.failed = true;
            let mut response = self.snapshot("failed");
            response["error"] = json!({"code":error.code,"message":error.message});
            self.emit(&mut frames, "response.failed", json!({"response":response}));
            return frames;
        }
        if nonempty(&chunk["usage"]) {
            wbcore::usage::keep_best(&mut self.usage, chunk["usage"].clone());
        }
        for (ordinal, choice) in array(&chunk["choices"]).iter().enumerate() {
            let delta = &choice["delta"];
            self.push_reasoning(strval(&delta["reasoning_content"]), &mut frames);
            let choice_index = choice
                .get("index")
                .and_then(py_int)
                .unwrap_or(ordinal as i64);
            for (i, tc) in array(&delta["tool_calls"])
                .iter()
                .enumerate()
                .filter(|(_, t)| t.is_object())
            {
                self.push_call(choice_index, i, tc, &mut frames);
            }
            if delta["function_call"].is_object() {
                self.push_call(
                    choice_index,
                    0,
                    &json!({"index":0,"function":delta["function_call"]}),
                    &mut frames,
                );
            }
            if let Some(piece) = delta["content"].as_str().filter(|s| !s.is_empty()) {
                self.open_message(&mut frames);
                let filtered = self.filter.feed(piece);
                self.filter_outputs(filtered, &mut frames);
            }
            if nonempty(&choice["finish_reason"]) {
                self.finish_reason = text(&choice["finish_reason"]);
            }
        }
        frames
    }
    pub fn finish(&mut self) -> Vec<SseEvent> {
        if self.terminal {
            return vec![];
        }
        let mut frames = self.start();
        self.close_reasoning(&mut frames);
        let mut dropped = 0;
        let calls = std::mem::take(&mut self.calls);
        let mut retained = 0;
        for (_, call) in calls {
            if call.name.is_empty() {
                dropped += 1;
                continue;
            }
            if (self.finish_reason == "length" || !self.saw_done)
                && wbcore::toolcalls::is_truncated_arguments(Some(&json!(call.arguments)))
            {
                dropped += 1;
                continue;
            }
            retained += 1;
            let mut payload = json!({"output_index":call.output_index,"item_id":call.item_id,"call_id":call.call_id});
            let event = if call.custom {
                payload["input"] = json!(unwrap_custom_input(&json!(call.arguments)));
                "response.custom_tool_call_input.done"
            } else {
                payload["arguments"] = json!(call.arguments);
                "response.function_call_arguments.done"
            };
            self.emit(&mut frames, event, payload);
            let item = call.item("completed", &self.options.metadata);
            self.outputs[call.output_index] = Some(item.clone());
            self.emit(
                &mut frames,
                "response.output_item.done",
                json!({"output_index":call.output_index,"item":item}),
            );
        }
        let filtered = self.filter.flush();
        self.filter_outputs(filtered, &mut frames);
        if retained == 0 {
            for call in std::mem::take(&mut self.printed_calls) {
                let output_index = self.outputs.len();
                let item_id = id("fc_");
                let mut item = json!({"id":item_id,"type":"function_call","status":"in_progress","call_id":call.id,"name":call.name,"arguments":""});
                stamp_namespace(&mut item, &self.options.metadata);
                self.emit(
                    &mut frames,
                    "response.output_item.added",
                    json!({"output_index":output_index,"item":item}),
                );
                self.emit(&mut frames,"response.function_call_arguments.delta",json!({"output_index":output_index,"item_id":item_id,"call_id":call.id,"delta":call.arguments}));
                self.emit(&mut frames,"response.function_call_arguments.done",json!({"output_index":output_index,"item_id":item_id,"call_id":call.id,"arguments":call.arguments}));
                item["status"] = json!("completed");
                item["arguments"] = json!(call.arguments);
                self.outputs.push(Some(item.clone()));
                retained += 1;
                self.emit(
                    &mut frames,
                    "response.output_item.done",
                    json!({"output_index":output_index,"item":item}),
                );
            }
        }
        if self.msg_index.is_some()
            || !self.text.is_empty()
            || self.outputs.iter().all(Option::is_none)
        {
            self.open_message(&mut frames);
            let index = self.msg_index.unwrap();
            self.emit(&mut frames,"response.output_text.done",json!({"item_id":self.msg_id,"output_index":index,"content_index":0,"text":self.text}));
            self.emit(&mut frames,"response.content_part.done",json!({"item_id":self.msg_id,"output_index":index,"content_index":0,"part":text_part(&self.text)}));
            let item = message_item(&self.msg_id, "completed", &self.text);
            self.outputs[index] = Some(item.clone());
            self.emit(
                &mut frames,
                "response.output_item.done",
                json!({"output_index":index,"item":item}),
            );
        }
        if self
            .usage
            .as_ref()
            .is_none_or(|u| wbcore::usage::total_tokens(u) == 0.0)
            && (!self.text.is_empty() || !self.reasoning.is_empty())
        {
            let reasoning = wbcore::usage::estimate_tokens(&self.reasoning);
            let completion = wbcore::usage::estimate_tokens(&self.text) + reasoning;
            self.usage = Some(
                json!({"prompt_tokens":1,"completion_tokens":completion,"total_tokens":completion+1,"completion_tokens_details":{"reasoning_tokens":reasoning},"prompt_tokens_details":{"cached_tokens":0}}),
            );
        }
        let length = self.finish_reason == "length";
        let reasoning_only = self.text.trim().is_empty()
            && !self.reasoning.trim().is_empty()
            && retained == 0
            && !length
            && dropped == 0;
        let status = if reasoning_only {
            "failed"
        } else if length || dropped > 0 {
            "incomplete"
        } else {
            "completed"
        };
        let mut response = self.snapshot(status);
        if length {
            response["incomplete_details"] = json!({"reason":"max_output_tokens"});
        }
        if reasoning_only {
            response["error"] = json!({"code":"empty_reasoning_only","message":"upstream returned reasoning without output text; retry"});
            self.failed = true;
        }
        self.emit(
            &mut frames,
            if reasoning_only {
                "response.failed"
            } else {
                "response.completed"
            },
            json!({"response":response}),
        );
        self.terminal = true;
        frames
    }
    /// End immediately with one error event; never call success finalization.
    pub fn abort(&mut self, message: &str) -> Vec<SseEvent> {
        if self.terminal {
            return vec![];
        }
        self.terminal = true;
        self.failed = true;
        vec![SseEvent {
            event: "error".into(),
            data: json!({"type":"error","code":"upstream_stream_error","message":format!("upstream stream aborted: {message}"),"param":null,"sequence_number":0}),
        }]
    }
}

struct MessageCall {
    index: Option<usize>,
    id: String,
    name: String,
}
/// Incremental chat SSE -> Anthropic Messages SSE. No full text/argument
/// buffering, synthetic signatures, ping events, or estimated usage.
pub struct MessagesStream {
    model: String,
    started: bool,
    terminal: bool,
    failed: bool,
    next: usize,
    thinking: Option<usize>,
    text: Option<usize>,
    calls: BTreeMap<i64, MessageCall>,
    order: Vec<i64>,
    stop: String,
    usage: Option<Value>,
}
impl MessagesStream {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            started: false,
            terminal: false,
            failed: false,
            next: 0,
            thinking: None,
            text: None,
            calls: BTreeMap::new(),
            order: vec![],
            stop: "end_turn".into(),
            usage: None,
        }
    }
    pub fn is_terminal(&self) -> bool {
        self.terminal
    }
    pub fn failed(&self) -> bool {
        self.failed
    }
    pub fn usage(&self) -> Option<&Value> {
        self.usage.as_ref()
    }
    fn start(&mut self, chunk: &Value, frames: &mut Vec<SseEvent>) {
        if self.started {
            return;
        }
        self.started = true;
        frames.push(SseEvent::anthropic("message_start",json!({"message":{"id":anthropic_id(&chunk["id"]),"type":"message","role":"assistant","content":[],"model":if nonempty(&chunk["model"]) {text(&chunk["model"])} else {self.model.clone()},"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}})));
    }
    fn close(index: &mut Option<usize>, frames: &mut Vec<SseEvent>) {
        if let Some(index) = index.take() {
            frames.push(SseEvent::anthropic(
                "content_block_stop",
                json!({"index":index}),
            ));
        }
    }
    fn open(&mut self, block: Value, frames: &mut Vec<SseEvent>) -> usize {
        let index = self.next;
        self.next += 1;
        frames.push(SseEvent::anthropic(
            "content_block_start",
            json!({"index":index,"content_block":block}),
        ));
        index
    }
    pub fn push_line(&mut self, line: &str) -> Vec<SseEvent> {
        if self.terminal {
            return vec![];
        }
        let data = wbcore::sse::strip_data_prefix(line);
        if data == "[DONE]" {
            return self.finish();
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return vec![];
        };
        self.push_chunk(&chunk)
    }
    pub fn push_chunk(&mut self, chunk: &Value) -> Vec<SseEvent> {
        if self.terminal || !chunk.is_object() {
            return vec![];
        }
        if let Some(e) = upstream_error(chunk) {
            return self.abort(&e.message);
        }
        let mut frames = vec![];
        self.start(chunk, &mut frames);
        if chunk["usage"].is_object() {
            self.usage = Some(chunk["usage"].clone());
        }
        let choice = &chunk["choices"][0];
        let delta = &choice["delta"];
        if let Some(s) = delta["reasoning_content"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            let index = match self.thinking {
                Some(i) => i,
                None => {
                    let i = self.open(json!({"type":"thinking","thinking":""}), &mut frames);
                    self.thinking = Some(i);
                    i
                }
            };
            frames.push(SseEvent::anthropic(
                "content_block_delta",
                json!({"index":index,"delta":{"type":"thinking_delta","thinking":s}}),
            ));
        }
        if let Some(s) = delta["content"].as_str().filter(|s| !s.is_empty()) {
            let index = match self.text {
                Some(i) => i,
                None => {
                    Self::close(&mut self.thinking, &mut frames);
                    let i = self.open(json!({"type":"text","text":""}), &mut frames);
                    self.text = Some(i);
                    i
                }
            };
            frames.push(SseEvent::anthropic(
                "content_block_delta",
                json!({"index":index,"delta":{"type":"text_delta","text":s}}),
            ));
        }
        let mut calls = array(&delta["tool_calls"]).to_vec();
        if delta["function_call"].is_object() {
            calls.push(json!({"index":0,"function":delta["function_call"]}));
        }
        for (ordinal, tc) in calls.iter().enumerate().filter(|(_, tc)| tc.is_object()) {
            let cid = text(&tc["id"]);
            let key = tc
                .get("index")
                .and_then(py_int)
                .or_else(|| {
                    self.calls
                        .iter()
                        .find(|(_, c)| !cid.is_empty() && c.id == cid)
                        .map(|(i, _)| *i)
                })
                .unwrap_or(ordinal as i64);
            let call = self.calls.entry(key).or_insert_with(|| MessageCall {
                index: None,
                id: String::new(),
                name: String::new(),
            });
            // Once emitted, block id/name are immutable, even if upstream changes id.
            if call.index.is_none() {
                if !cid.is_empty() {
                    call.id = cid;
                }
                if nonempty(&tc["function"]["name"]) {
                    call.name = text(&tc["function"]["name"]);
                }
            }
            let args = if tc["function"]["arguments"].is_null() {
                String::new()
            } else {
                json_text(&tc["function"]["arguments"])
            };
            let block = if call.index.is_none()
                && (!call.id.is_empty() || !call.name.is_empty() || !args.is_empty())
            {
                if call.id.is_empty() {
                    call.id = format!("toolu_{key}");
                }
                Some(json!({"type":"tool_use","id":call.id,"name":call.name,"input":{}}))
            } else {
                None
            };
            if let Some(block) = block {
                Self::close(&mut self.thinking, &mut frames);
                Self::close(&mut self.text, &mut frames);
                let index = self.open(block, &mut frames);
                self.calls.get_mut(&key).unwrap().index = Some(index);
                self.order.push(key);
            }
            if let Some(index) = self.calls[&key].index {
                if !args.is_empty() {
                    frames.push(SseEvent::anthropic("content_block_delta",json!({"index":index,"delta":{"type":"input_json_delta","partial_json":args}})));
                }
            }
        }
        if nonempty(&choice["finish_reason"]) {
            self.stop = stop_reason(strval(&choice["finish_reason"])).into();
        }
        frames
    }
    pub fn finish(&mut self) -> Vec<SseEvent> {
        if self.terminal {
            return vec![];
        }
        let mut frames = vec![];
        self.start(&Value::Null, &mut frames);
        Self::close(&mut self.thinking, &mut frames);
        Self::close(&mut self.text, &mut frames);
        for key in &self.order {
            if let Some(index) = self.calls[key].index {
                frames.push(SseEvent::anthropic(
                    "content_block_stop",
                    json!({"index":index}),
                ));
            }
        }
        frames.push(SseEvent::anthropic("message_delta",json!({"delta":{"stop_reason":self.stop,"stop_sequence":null},"usage":anthropic_usage(self.usage.as_ref().unwrap_or(&Value::Null))})));
        frames.push(SseEvent::anthropic("message_stop", json!({})));
        self.terminal = true;
        frames
    }
    pub fn abort(&mut self, message: &str) -> Vec<SseEvent> {
        if self.terminal {
            return vec![];
        }
        self.terminal = true;
        self.failed = true;
        vec![SseEvent::anthropic(
            "error",
            json!({"error":{"type":"api_error","message":message}}),
        )]
    }
}
