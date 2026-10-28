use super::*;
use serde_json::{json, Value};
use std::collections::HashSet;

fn normalize(v: Value) -> Value {
    build_upstream_body(&v, &json!({}), &json!({})).unwrap()
}
fn call(id: &str) -> Value {
    json!({"id":id,"type":"function","function":{"name":"f","arguments":"{}"}})
}
fn assistant(ids: &[&str]) -> Value {
    json!({"role":"assistant","content":null,"tool_calls":ids.iter().map(|id| call(id)).collect::<Vec<_>>()})
}
fn tool(id: &str) -> Value {
    json!({"role":"tool","tool_call_id":id,"content":"ok"})
}
fn frames(output: Vec<String>) -> Vec<Value> {
    output
        .into_iter()
        .map(|frame| {
            assert!(frame.starts_with("data: ") && frame.ends_with("\n\n"));
            let data = wbcore::sse::strip_data_prefix(&frame);
            if data == "[DONE]" {
                json!("[DONE]")
            } else {
                serde_json::from_str(data).unwrap()
            }
        })
        .collect()
}
fn push(stream: &mut ChatStream, value: Value) -> Vec<Value> {
    frames(stream.push_line(&format!("data: {}", value)))
}
fn delta(content: &str) -> Value {
    json!({"id":"upstream","choices":[{"index":0,"delta":{"content":content}}]})
}
fn emitted_calls(frames: &[Value]) -> Vec<&Value> {
    frames
        .iter()
        .flat_map(|f| f["choices"].as_array().into_iter().flatten())
        .flat_map(|ch| ch["delta"]["tool_calls"].as_array().into_iter().flatten())
        .collect()
}
fn emitted_text(frames: &[Value]) -> String {
    frames
        .iter()
        .flat_map(|f| f["choices"].as_array().into_iter().flatten())
        .filter_map(|ch| ch["delta"]["content"].as_str())
        .collect()
}

#[test]
fn full_normalization_example_and_input_immutable() {
    let payload = json!({"model":"deepseek-v4.1-flash", "stream":false, "stop":"END", "max_completion_tokens":512,
        "_private":1, "messages":[{"role":"user","content":"hi"}]});
    let original = payload.clone();
    let r = normalize_request(&payload, &json!({}), &json!({"maxOutputTokens":128000})).unwrap();
    assert!(!r.want_stream);
    assert_eq!(
        r.body,
        json!({"model":"deepseek-v4.1-flash","stream":true,"stop":["END"],
        "messages":[{"role":"system","content":"You are a helpful assistant."},{"role":"user","content":"hi"}],
        "max_tokens":512,"thinking":{"type":"enabled"},"reasoning_effort":"high","stream_options":{"include_usage":true}})
    );
    assert_eq!(payload, original);
}

#[test]
fn model_aliases_are_single_step_case_insensitive_and_background_follows() {
    let settings = json!({"model_aliases":[{"alias":" Client ","target":"gpt-5.6-terra"},{"from":"gpt-5.6-terra","to":"not-chained"}]});
    let payload = json!({"model":" CLIENT ","stream":"false","client_metadata":{"x":"{\"request_kind\":\"memory\",\"thread_source\":\"memory_consolidation\"}"}});
    let r = normalize_request(&payload, &settings, &json!({})).unwrap();
    assert_eq!(r.model, "deepseek-v4.1-flash");
    assert_eq!(r.background_reason.as_deref(), Some("memory_consolidation"));
    assert!(r.want_stream);
    let r = normalize_request(&json!({"model":" CLIENT "}), &settings, &json!({})).unwrap();
    assert_eq!(r.model, "gpt-5.6-terra");
    let r = normalize_request(
        &json!({"model":"a"}),
        &json!({"model_aliases":{" A ":"TargetCase"}}),
        &json!({}),
    )
    .unwrap();
    assert_eq!(r.model, "TargetCase");
}

#[test]
fn operator_compaction_not_redirected_and_user_text_not_inspected() {
    let payload = json!({"model":"gpt-5.3-codex","client_metadata":{"request_kind":"compaction","thread_source":"user"}});
    assert_eq!(
        normalize_request(&payload, &json!({}), &json!({}))
            .unwrap()
            .model,
        "gpt-5.3-codex"
    );
    assert_eq!(
        background_request_reason(
            &json!({"messages":[{"role":"user","content":"memory title compaction"}]})
        ),
        None
    );
    let r = normalize_request(
        &json!({"model":"m","client_metadata":{"request_kind":"title"}}),
        &json!({"background_model_names":"m","background_model_target":"cheap"}),
        &json!({}),
    )
    .unwrap();
    assert_eq!(r.model, "cheap");
}

#[test]
fn effort_catalog_mapping_and_opt_out() {
    let meta = json!({"reasoning":{"supportedEfforts":["low","high","max"],"defaultEffort":"high","canDisableThinking":false}});
    let r = build_upstream_body(
        &json!({"model":"deepseek-x","reasoningEffort":"medium"}),
        &json!({}),
        &meta,
    )
    .unwrap();
    assert_eq!(r["reasoningEffort"], "high");
    assert!(r.get("reasoning_effort").is_none());
    let r = build_upstream_body(
        &json!({"model":"deepseek-x","reasoning_effort":"none"}),
        &json!({}),
        &meta,
    )
    .unwrap();
    assert_eq!(r["reasoning_effort"], "high");
    let r = build_upstream_body(
        &json!({"model":"deepseek-x","reasoning_effort":"medium"}),
        &json!({"reasoning_effort_map":false}),
        &meta,
    )
    .unwrap();
    assert_eq!(r["reasoning_effort"], "medium");
    let r = normalize(json!({"model":"deepseek-x","reasoning_effort":"none"}));
    assert!(r.get("thinking").is_none());
    let r = build_upstream_body(
        &json!({"model":"deepseek-x"}),
        &json!({"default_reasoning_effort":"off"}),
        &meta,
    )
    .unwrap();
    assert!(r.get("reasoning_effort").is_none());
    assert_eq!(r["thinking"]["type"], "enabled");
}

#[test]
fn catalog_forms_and_token_limits() {
    for catalog in [
        json!({"GPT-X":{"maxOutputTokens":100}}),
        json!([{"id":"gpt-x","maxOutputTokens":100}]),
        json!({"data":[{"id":"gpt-x","max_output_tokens":100}]}),
        json!({"maxOutputTokens":100}),
    ] {
        assert_eq!(
            build_upstream_body(&json!({"model":"gpt-x"}), &json!({}), &catalog).unwrap()
                ["max_tokens"],
            100
        );
    }
    assert_eq!(
        normalize(json!({"model":"gpt-x","max_completion_tokens":"1"}))["max_tokens"],
        16
    );
    assert_eq!(
        normalize(json!({"model":"m","max_tokens":9,"max_completion_tokens":99}))["max_tokens"],
        9
    );
    assert!(normalize(json!({"max_completion_tokens":"bad"}))
        .get("max_tokens")
        .is_none());
    assert!(normalize(json!({"max_completion_tokens":0}))
        .get("max_tokens")
        .is_none());
}

#[test]
fn messages_shape_errors_are_results_not_panics() {
    for messages in [
        json!("no"),
        json!({}),
        json!(12),
        json!([null]),
        json!(["hi"]),
        json!([{}]),
        json!([{"role":3}]),
        json!([{"role":"unknown"}]),
        json!([{"role":"user","content":3}]),
        json!([{"role":"user","content":["text"]}]),
        json!([{"role":"assistant","tool_calls":"bad"}]),
        json!([{"role":"assistant","tool_calls":[{}]}]),
    ] {
        let error =
            build_upstream_body(&json!({"messages":messages}), &json!({}), &json!({})).unwrap_err();
        assert_eq!(error.status_code(), 400);
        assert_eq!(error.as_value()["error"]["type"], "invalid_request_error");
    }
    // Non-object body parity is intentional, unlike malformed message values.
    assert_eq!(normalize(json!("body"))["messages"][0]["role"], "system");
}

#[test]
fn preserves_fingerprint_text_reasoning_multimodal_and_tool_arguments() {
    let text = "  error 11128 please; x-anthropic-billing-header: cc_version=1; cc_entrypoint=cli; You are Claude Code  ";
    let args = format!("{{\"text\":{}}}", serde_json::to_string(text).unwrap());
    let original = json!({"model":"m","messages":[
        {"role":"developer","content":text},
        {"role":"user","content":[{"type":"text","text":text},{"type":"image_url","image_url":{"url":"data:image/png;base64,abc"}}]},
        {"role":"assistant","content":text,"reasoning_content":text,"tool_calls":[{"id":"a","function":{"name":"f","arguments":args}}]},
        {"role":"tool","tool_call_id":"a","content":text}]});
    let result = normalize(original.clone());
    let mut expected = original["messages"].clone();
    expected[0]["role"] = json!("system");
    assert_eq!(result["messages"], expected);
}

#[test]
fn explicit_prompt_mode_and_reasoning_backfill() {
    let r = build_upstream_body(
        &json!({"model":"deepseek-x","messages":[{"role":"developer","content":"original"},
        {"role":"assistant","content":"hi","reasoning":"trace"},{"role":"user","content":"keep"}]}),
        &json!({"prompt":{"mode":"append","text":"operator"}}),
        &json!({}),
    )
    .unwrap();
    assert_eq!(r["messages"][0]["content"], "original");
    assert_eq!(r["messages"][1]["content"], "operator");
    assert_eq!(r["messages"][2]["reasoning_content"], "trace");
    assert_eq!(r["messages"][3]["content"], "keep");
    let r =
        normalize(json!({"model":"deepseek-x","messages":[{"role":"assistant","content":null}]}));
    assert_eq!(r["messages"][1]["reasoning_content"], "");
    assert_eq!(r["messages"][1]["reasoning"], " ");
}

#[test]
fn tool_pairing_merge_repack_cleanup_and_reasoning() {
    let mut a = assistant(&["a"]);
    a["reasoning_content"] = json!("first");
    let mut b = assistant(&["b"]);
    b["reasoning_content"] = json!("second");
    let r = normalize(
        json!({"messages":[a,b,{"role":"developer","content":"image notice"},tool("a"),tool("zzz")]}),
    );
    assert_eq!(r["messages"][1]["tool_calls"], json!([call("a")]));
    assert_eq!(r["messages"][1]["reasoning_content"], "first\nsecond");
    assert_eq!(r["messages"][2], tool("a"));
    assert_eq!(
        r["messages"][3],
        json!({"role":"system","content":"image notice"})
    );
    assert_eq!(r["messages"].as_array().unwrap().len(), 4);
    let r = normalize(
        json!({"messages":[assistant(&["a"]),{"role":"assistant","content":"call explanation"},tool("a"),
        assistant(&["b"]),tool("b")]}),
    );
    assert_eq!(r["messages"][1]["content"], "call explanation");
    assert_eq!(r["messages"][3]["tool_calls"][0]["id"], "b");
}

#[test]
fn tool_schemas_choice_arguments_and_stop_normalization() {
    let r = normalize(
        json!({"tools":[{"name":"ls","description":null,"strict":false,"parameters":{
        "type":"dict","properties":{"p":{"type":"str","description":null},"xs":{"type":"list","items":[{"type":"int"}]}},
        "required":[null,"p"],"$defs":{"flag":{"type":"bool"}},"anyOf":[{"type":"float"}]}}],
        "tool_choice":{"type":"function","function":{"name":" ls "}},"stop":["END","",null,4," "]}),
    );
    assert_eq!(r["tool_choice"], "ls");
    assert_eq!(r["tools"][0]["function"]["description"], "");
    assert_eq!(
        r["tools"][0]["function"]["parameters"]["properties"]["p"],
        json!({"type":"string"})
    );
    assert_eq!(
        r["tools"][0]["function"]["parameters"]["properties"]["xs"]["items"],
        json!({"type":"integer"})
    );
    assert_eq!(
        r["tools"][0]["function"]["parameters"]["$defs"]["flag"]["type"],
        "boolean"
    );
    assert_eq!(
        r["tools"][0]["function"]["parameters"]["anyOf"][0]["type"],
        "number"
    );
    assert_eq!(
        r["tools"][0]["function"]["parameters"]["required"],
        json!(["p"])
    );
    assert_eq!(r["stop"], json!(["END", " "]));
    let r = normalize(
        json!({"functions":[{"name":"f","parameters":null}],"tool_choice":{"type":"none"}}),
    );
    assert_eq!(r["tool_choice"], "none");
    assert!(r.get("tools").is_some());
    assert!(r.get("functions").is_none());
    assert_eq!(
        r["tools"][0]["function"]["parameters"],
        json!({"type":"object","properties":{}})
    );
    let mut a = assistant(&["a"]);
    a["tool_calls"][0]["function"]["arguments"] = json!({"x":"你好"});
    let r = normalize(json!({"messages":[a,tool("a")]}));
    assert_eq!(
        r["messages"][1]["tool_calls"][0]["function"]["arguments"],
        "{\"x\": \"你好\"}"
    );
}

#[test]
fn stream_usage_hoist_finish_tail_and_stable_id() {
    let mut s = ChatStream::new("requested");
    let mut out = push(
        &mut s,
        json!({"id":"up-1","model":"actual","choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}),
    );
    out.extend(push(&mut s, delta("Hello")));
    out.extend(push(&mut s, json!({"id":"up-2","choices":[{"index":0,"delta":{}}],"usage":{"prompt_tokens":37,"completion_tokens":16,"total_tokens":53,"prompt_tokens_details":{"cached_tokens":5}}})));
    out.extend(push(
        &mut s,
        json!({"choices":[],"usage":{"total_tokens":10}}),
    ));
    out.extend(push(
        &mut s,
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
    ));
    assert!(out.iter().all(|f| f.get("usage").is_none()));
    assert!(s.push_line("data: [DONE]").is_empty());
    out.extend(frames(s.finish()));
    assert_eq!(out.last().unwrap(), "[DONE]");
    let usage_index = out.iter().position(|f| f.get("usage").is_some()).unwrap();
    assert_eq!(usage_index, out.len() - 2);
    assert_eq!(out[usage_index]["choices"], json!([]));
    assert_eq!(out[usage_index]["usage"]["total_tokens"], 53);
    assert_eq!(out[usage_index]["usage"]["cache_read_input_tokens"], 5);
    assert_eq!(out[usage_index - 1]["choices"][0]["finish_reason"], "stop");
    let ids: HashSet<_> = out.iter().filter_map(|f| f["id"].as_str()).collect();
    assert_eq!(ids.len(), 1);
    assert_eq!(*ids.iter().next().unwrap(), s.response_id());
    assert_eq!(s.accounting().usage.as_ref().unwrap()["total_tokens"], 53);
    assert!(s.accounting().saw_done && s.accounting().completed);
    assert!(s.accounting().first_chunk_at.is_some());
    assert_eq!(s.completion().unwrap()["model"], "actual");
    assert!(s.completion().unwrap().get("first_chunk_at").is_none());
    assert!(s.finish().is_empty());
    assert!(s.abort(json!("too late")).is_empty());
}

#[test]
fn stream_usage_ties_choose_later_and_estimates_are_accounting_only() {
    let mut s = ChatStream::new("m");
    push(
        &mut s,
        json!({"usage":{"total_tokens":7,"credit":1},"choices":[]}),
    );
    push(
        &mut s,
        json!({"usage":{"total_tokens":7,"credit":2},"choices":[]}),
    );
    assert_eq!(s.accounting().usage.as_ref().unwrap()["credit"], 2);
    frames(s.finish());
    assert!(!s.accounting().usage_estimated);
    let mut s = ChatStream::new("m");
    let mut out = push(&mut s, delta("hello world"));
    out.extend(frames(s.finish()));
    assert!(out.iter().all(|f| f.get("usage").is_none()));
    assert!(s.accounting().usage_estimated);
    assert!(
        s.accounting().usage.as_ref().unwrap()["total_tokens"]
            .as_i64()
            .unwrap()
            > 0
    );
    assert!(s.completion().unwrap()["usage"].is_object());
}

#[test]
fn stream_abort_flushes_held_text_preserves_partial_usage_and_never_done() {
    let mut s = ChatStream::new("m");
    let mut out = push(&mut s, delta("partial <think>held"));
    push(
        &mut s,
        json!({"choices":[],"usage":{"total_tokens":5,"credit":3}}),
    );
    out.extend(frames(s.abort(json!("read timeout"))));
    assert_eq!(emitted_text(&out), "partial <think>held");
    assert_eq!(
        out.last().unwrap()["error"]["code"],
        "upstream_stream_error"
    );
    assert!(out
        .iter()
        .all(|f| f != "[DONE]" && f.get("usage").is_none()));
    assert!(out
        .iter()
        .flat_map(|f| f["choices"].as_array().into_iter().flatten())
        .all(|ch| ch["finish_reason"].is_null()));
    assert_eq!(s.accounting().usage.as_ref().unwrap()["credit"], 3);
    assert!(s.accounting().aborted && !s.accounting().completed);
    assert!(!s.accounting().usage_estimated);
    assert!(s.completion().is_none());
    assert!(s.finish().is_empty());
    assert!(s.push_line("data: [DONE]").is_empty());
    assert!(s.abort(json!("again")).is_empty());
}

#[test]
fn in_band_error_and_error_finish_have_no_success_tail() {
    for error_chunk in [
        json!({"error":{"message":"denied","code":11140},"usage":{"total_tokens":2}}),
        json!({"choices":[{"delta":{"content":"partial"},"finish_reason":"error"}]}),
    ] {
        let mut s = ChatStream::new("m");
        let mut out = push(&mut s, error_chunk);
        out.extend(frames(s.finish()));
        assert!(out.last().unwrap().get("error").is_some());
        assert!(out.iter().all(|f| f != "[DONE]"));
        assert!(s.accounting().error.is_some());
        assert!(s.is_terminal());
    }
}

#[test]
fn empty_stream_is_error_without_done_and_client_abort_is_silent() {
    let mut s = ChatStream::new("m");
    assert!(s.push_line(": keepalive").is_empty());
    s.push_line("data: [DONE]");
    let out = frames(s.finish());
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["error"]["message"], "empty upstream stream");
    let mut s = ChatStream::new("m");
    push(&mut s, delta("partial"));
    s.client_aborted();
    assert!(s.finish().is_empty());
    assert!(s.abort(json!("do not write")).is_empty());
    assert!(s.accounting().client_aborted);
    assert_eq!(s.accounting().outcome, "client_aborted");
    assert!(s.accounting().usage.is_none());
}

#[test]
fn stream_size_limit_aborts_before_oversized_frame() {
    let mut s = ChatStream::with_max_bytes("m", 120);
    let mut out = push(&mut s, delta("small"));
    out.extend(push(&mut s, delta(&"a".repeat(200))));
    assert_eq!(
        out.last().unwrap()["error"]["code"],
        "upstream_stream_error"
    );
    assert!(!emitted_text(&out).contains("aaaa"));
    assert!(s.is_terminal());
    assert!(s.finish().is_empty());
}

#[test]
fn recovered_tool_batches_have_unique_indices_and_stable_ids() {
    let mut s = ChatStream::new("m");
    let mut out = push(&mut s, delta("Sure. <run_"));
    out.extend(push(
        &mut s,
        delta("code><parameter name=\"code\">print(1)</parameter>"),
    ));
    out.extend(push(&mut s, delta("</run_code> done")));
    out.extend(push(
        &mut s,
        delta("<invoke name=\"f\"><parameter name=\"p\">2</parameter></invoke>"),
    ));
    out.extend(push(&mut s, json!({"id":"up","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"real","function":{"name":"real_f","arguments":"{\"a\":"}}]}}]})));
    out.extend(push(&mut s, json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]},"finish_reason":"stop"}]})));
    out.extend(frames(s.finish()));
    let calls = emitted_calls(&out);
    assert_eq!(calls.len(), 4);
    assert_eq!(
        calls
            .iter()
            .map(|c| c["index"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 2]
    );
    assert_eq!(calls[0]["function"]["name"], "run_code");
    assert_eq!(
        calls[0]["function"]["arguments"],
        "{\"code\": \"print(1)\"}"
    );
    assert_eq!(calls[2]["id"], "real");
    assert_eq!(emitted_text(&out), "Sure.  done");
    assert_eq!(
        out[out.len() - 2]["choices"][0]["finish_reason"],
        "tool_calls"
    );
    assert_eq!(s.accounting().finish_reason.as_deref(), Some("tool_calls"));
}

#[test]
fn dsml_is_recovered_and_reasoning_is_not_filtered() {
    let mut s = ChatStream::new("m");
    let mut out = push(
        &mut s,
        json!({"choices":[{"delta":{"reasoning_content":"<think>reasoning</think>",
        "content":"Hi <|DSML|calls><|DSML|invoke name=\"ls\"><|DSML|parameter name=\"p\">.</|DSML|parameter></|DSML|invoke></|DSML|calls> bye"}}]}),
    );
    out.extend(frames(s.finish()));
    assert_eq!(emitted_calls(&out).len(), 1);
    assert_eq!(emitted_text(&out), "Hi  bye");
    assert!(out
        .iter()
        .any(|f| f["choices"][0]["delta"]["reasoning_content"] == "<think>reasoning</think>"));
}

#[test]
fn aggregation_reuses_core_tool_completion_and_drops_truncation() {
    let mut s = ChatStream::new("fallback");
    push(
        &mut s,
        json!({"id":"up-9","model":"actual","choices":[{"delta":{"reasoning_content":"hmm","tool_calls":[{"index":0,"id":"c1","function":{"name":"ls","arguments":"{\"p\":"}}]}}]}),
    );
    push(
        &mut s,
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]},"finish_reason":"tool_calls"}],"usage":{"total_tokens":14}}),
    );
    s.push_line("data: [DONE]");
    s.finish();
    let result = s.completion().unwrap();
    assert_eq!(result["id"], "up-9");
    assert_eq!(
        result["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{\"p\":1}"
    );
    assert_eq!(result["choices"][0]["finish_reason"], "tool_calls");
    let mut s = ChatStream::new("m");
    push(
        &mut s,
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"f","arguments":"{\"x\":"}}]},"finish_reason":"length"}]}),
    );
    s.finish();
    assert!(s.completion().unwrap()["choices"][0]["message"]
        .get("tool_calls")
        .is_none());
}

#[test]
fn legacy_call_argument_continuations_survive_stream_cleaning() {
    let mut s = ChatStream::new("m");
    let mut out = push(
        &mut s,
        json!({"choices":[{"delta":{"function_call":{"name":"f","arguments":"{"}}}]}),
    );
    out.extend(push(&mut s, json!({"choices":[{"delta":{"function_call":{"arguments":"}"}},"finish_reason":"tool_calls"}]})));
    out.extend(frames(s.finish()));
    let calls = emitted_calls(&out);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["index"], calls[1]["index"]);
    assert!(calls[0]["id"].as_str().unwrap().starts_with("call_"));
    assert_eq!(calls[1]["function"]["arguments"], "}");
}

#[test]
fn model_resolver_matches_normalizer_for_catalog_lookup() {
    let settings = json!({"model_aliases":[{"from":"client","to":"gpt-5.6-terra"},{"from":"gpt-5.6-terra","to":"do-not-chain"}]});
    for payload in [
        json!({"model":" client "}),
        json!({"model":"client","client_metadata":{"request_kind":"title"}}),
        json!({}),
        json!(null),
    ] {
        let resolved = resolve_model(&payload, &settings).unwrap();
        assert_eq!(
            normalize_request(&payload, &settings, &json!({}))
                .unwrap()
                .model,
            resolved
        );
    }
    assert!(resolve_model(&json!({"model":false}), &settings).is_err());
}

#[test]
fn structured_calls_without_indices_keep_distinct_ids_and_continuations() {
    let mut stream = ChatStream::new("m");
    let mut out = Vec::new();
    for id in ["a", "b"] {
        out.extend(push(&mut stream, json!({"choices":[{"delta":{"tool_calls":[{"id":id,"function":{"name":"f","arguments":"{"}}]}}]})));
    }
    out.extend(push(
        &mut stream,
        json!({"choices":[{"delta":{"tool_calls":[{"id":"a","function":{"arguments":"}"}}]}}]}),
    ));
    let calls = emitted_calls(&out);
    assert_eq!(
        calls
            .iter()
            .map(|tc| tc["index"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 0]
    );
    assert_eq!(calls[0]["id"], calls[2]["id"]);
}

fn golden_cases() -> Vec<Value> {
    let oracle: Value =
        serde_json::from_str(include_str!("../../../tests/baseline/golden.json")).unwrap();
    oracle["cases"].as_array().unwrap().clone()
}

#[test]
fn python_golden_schema_tools_choice_and_stop() {
    let mut checked = 0;
    for case in golden_cases() {
        let id = case["id"].as_str().unwrap();
        let input = &case["input"]["args"][0];
        if id.starts_with("schema.") {
            // Wrapping the original function in tools exercises the same schema
            // pass through the public API, not a Rust-generated expectation.
            let actual = normalize(json!({"tools":[{"type":"function","function":input}]}));
            assert_eq!(actual["tools"][0]["function"], case["expected"], "{id}");
            checked += 1;
        } else if id.starts_with("choice.") {
            let actual = normalize(input.clone());
            // The full builder additionally normalizes tools' parameter
            // schemas; this fixture isolates choice normalization, so compare
            // that field exactly and verify tool retention separately.
            assert_eq!(
                actual.get("tool_choice"),
                case["expected"].get("tool_choice"),
                "{id}"
            );
            assert_eq!(
                actual["tools"].as_array().map(Vec::len),
                case["expected"]["tools"].as_array().map(Vec::len),
                "{id}"
            );
            checked += 1;
        } else if id.starts_with("tools.") || id.starts_with("stop.") {
            let mut actual = normalize(input.clone());
            // Remove only the builder's unrelated mandatory defaults; compare
            // the complete remaining object to the Python helper's result.
            for key in ["model", "messages", "stream", "stream_options"] {
                if input.get(key).is_none() {
                    actual.as_object_mut().unwrap().remove(key);
                }
            }
            assert_eq!(actual, case["expected"], "{id}");
            checked += 1;
        }
    }
    assert_eq!(checked, 25);
}

#[test]
fn python_golden_pairing_and_full_body() {
    let mut checked = 0;
    for case in golden_cases() {
        let id = case["id"].as_str().unwrap();
        if id.starts_with("pair.") {
            let mut input = case["input"]["args"][0].clone();
            let mut expected = case["expected"][0].clone();
            // These Python fixtures exercise merge alone with unanswered calls.
            // Supply identical results to both sides so the full public
            // pipeline's subsequent orphan cleanup doesn't obscure that pass.
            if id == "pair.merge_reasoning" || id == "pair.trailing_text" {
                let ids: Vec<String> = expected
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|m| m["tool_calls"].as_array().into_iter().flatten())
                    .filter_map(|tc| tc["id"].as_str().map(str::to_string))
                    .collect();
                for id in ids {
                    input.as_array_mut().unwrap().push(tool(&id));
                    expected.as_array_mut().unwrap().push(tool(&id));
                }
            }
            // Developer->system is a separate documented normalization stage.
            for m in expected.as_array_mut().unwrap() {
                if m["role"] == "developer" {
                    m["role"] = json!("system");
                }
            }
            let actual = normalize(json!({"messages":input}));
            assert_eq!(
                &actual["messages"].as_array().unwrap()[1..],
                expected.as_array().unwrap(),
                "{id}"
            );
            checked += 1;
        } else if id == "body.full_repairs" {
            let actual = build_upstream_body(
                &case["input"]["args"][0],
                &json!({}),
                &json!({"maxOutputTokens":128000,"reasoning":{"defaultEffort":"high"}}),
            )
            .unwrap();
            assert_eq!(actual, case["expected"], "{id}");
            checked += 1;
        }
    }
    assert_eq!(checked, 9);
}

#[test]
fn python_golden_aggregated_completions() {
    let mut checked = 0;
    for case in golden_cases() {
        if !case["id"].as_str().unwrap().starts_with("aggregate.") {
            continue;
        }
        let mut stream = ChatStream::new(case["input"]["model"].as_str().unwrap());
        for chunk in case["input"]["chunks"].as_array().unwrap() {
            if chunk == "[DONE]" {
                stream.push_line("data: [DONE]");
            } else {
                push(&mut stream, chunk.clone());
            }
        }
        stream.finish();
        let mut expected = case["expected"].clone();
        let mut actual = stream.completion().unwrap().clone();
        // Time is nondeterministic; internal timing deliberately no longer
        // leaks to clients (architecture decision, not a hidden mismatch).
        for key in ["elapsed_ms", "first_chunk_at"] {
            assert!(actual.get(key).is_none());
            expected.as_object_mut().unwrap().remove(key);
        }
        assert!(actual["created"].is_i64());
        actual["created"] = expected["created"].clone();
        assert_eq!(actual, expected, "{}", case["id"]);
        checked += 1;
    }
    assert_eq!(checked, 3);
}

#[test]
fn python_golden_stream_usage_and_abort() {
    let mut checked = 0;
    for case in golden_cases() {
        let id = case["id"].as_str().unwrap();
        if id != "chat.stream_usage_single_id" && id != "chat.stream_abort" {
            continue;
        }
        let mut stream = ChatStream::new("deepseek-v4.1-flash");
        let mut actual = Vec::new();
        for chunk in case["input"]["chunks"].as_array().unwrap() {
            if chunk == "[DONE]" {
                stream.push_line("data: [DONE]");
            } else {
                actual.extend(push(&mut stream, chunk.clone()));
            }
        }
        if case["input"]["abort"] == true {
            actual.extend(frames(
                stream.abort(json!("IncompleteRead(0 bytes read, 4096 more expected)")),
            ));
        } else {
            actual.extend(frames(stream.finish()));
        }
        let expected: Vec<_> = case["expected"]["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["data"].clone())
            .collect();
        assert_eq!(actual.len(), expected.len(), "{id}");
        for (actual, expected) in actual.iter_mut().zip(&expected) {
            // Only generated id/time are made deterministic. Check id stability
            // before replacing; every other field is compared exactly.
            if actual.get("id").is_some() {
                assert_eq!(actual["id"], stream.response_id());
                actual["id"] = expected["id"].clone();
            }
            if actual.get("created").is_some() {
                actual["created"] = expected["created"].clone();
            }
        }
        assert_eq!(actual, expected, "{id}");
        if id == "chat.stream_usage_single_id" {
            assert_eq!(
                stream.accounting().usage.as_ref().unwrap(),
                &case["expected"]["records"][0]["usage"]
            );
        } else {
            assert_eq!(stream.accounting().outcome, "upstream_aborted");
            assert!(stream.accounting().usage.is_none());
        }
        checked += 1;
    }
    assert_eq!(checked, 2);
}

#[test]
fn parallel_choices_do_not_drop_each_other_or_mix_filters() {
    let mut s = ChatStream::new("m");
    let mut out = push(
        &mut s,
        json!({"choices":[{"index":0,"delta":{}},
        {"index":1,"delta":{"role":"assistant","reasoning_content":"second"}}]}),
    );
    assert_eq!(out[0]["choices"].as_array().unwrap().len(), 1);
    assert_eq!(out[0]["choices"][0]["index"], 1);
    out.extend(push(
        &mut s,
        json!({"choices":[{"index":0,"delta":{"content":"<think>zero"}},
        {"index":1,"delta":{"content":"one"}}]}),
    ));
    out.extend(frames(s.finish()));
    assert!(out
        .iter()
        .any(|f| f["choices"][0]["index"] == 1 && f["choices"][0]["delta"]["content"] == "one"));
    assert!(out
        .iter()
        .any(|f| f["choices"][0]["index"] == 0
            && f["choices"][0]["delta"]["content"] == "<think>zero"));
}
