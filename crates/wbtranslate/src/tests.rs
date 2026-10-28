use super::*;

fn options() -> ResponseOptions {
    ResponseOptions {
        model: "m".into(),
        response_id: Some("resp_fixed".into()),
        created_at: Some(1700000000),
        ..Default::default()
    }
}
fn rstream(lines: &[&str], options: ResponseOptions) -> Vec<SseEvent> {
    let mut stream = ResponsesStream::new(options);
    let mut frames = Vec::new();
    for line in lines {
        frames.extend(stream.push_line(line));
    }
    frames.extend(stream.finish());
    frames
}
fn mstream(lines: &[&str]) -> Vec<SseEvent> {
    let mut stream = MessagesStream::new("m");
    let mut frames = Vec::new();
    for line in lines {
        frames.extend(stream.push_line(line));
    }
    frames.extend(stream.finish());
    frames
}
fn event<'a>(frames: &'a [SseEvent], kind: &str) -> &'a Value {
    &frames.iter().find(|f| f.event == kind).unwrap().data
}
fn final_response(frames: &[SseEvent]) -> &Value {
    &frames.last().unwrap().data["response"]
}

#[test]
fn f1_responses_history() {
    let payload = json!({"model":"m","instructions":"be terse","input":[
        {"role":"developer","content":"dev note"},
        {"role":"user","content":[{"type":"input_text","text":"edit"},{"type":"input_image","image_url":"data:image/png;base64,AA"}]},
        {"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}]},
        {"type":"function_call","call_id":"call_1","name":"shell","arguments":"{\"cmd\":\"ls\"}"},
        {"type":"custom_tool_call","call_id":"call_2","name":"apply_patch","input":"*** Begin Patch"},
        {"type":"function_call_output","call_id":"call_1","output":"a.txt"},
        {"type":"custom_tool_call_output","call_id":"call_2","output":"Done!"},
        {"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]},
        {"type":"agent_message","content":[{"type":"text","text":"from sub"}]}],
        "tools":[{"type":"function","name":"shell","parameters":{"type":"object"}}],"tool_choice":"auto","parallel_tool_calls":false,"max_output_tokens":100,"reasoning":{"effort":"low"},"temperature":0.2,"store":true,"metadata":{"a":1}});
    let translated = responses_to_chat(&payload, &TranslationOptions::default()).unwrap();
    assert_eq!(
        translated.chat,
        json!({"model":"m","messages":[
        {"role":"system","content":"be terse"},{"role":"system","content":"dev note"},
        {"role":"user","content":[{"type":"text","text":"edit"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AA"}}]},
        {"role":"assistant","content":"","tool_calls":[{"id":"call_1","type":"function","function":{"name":"shell","arguments":"{\"cmd\":\"ls\"}"}},{"id":"call_2","type":"function","function":{"name":"apply_patch","arguments":"{\"input\": \"*** Begin Patch\"}"}}],"reasoning_content":"plan"},
        {"role":"tool","tool_call_id":"call_1","content":"a.txt"},{"role":"tool","tool_call_id":"call_2","content":"Done!"},
        {"role":"assistant","content":"ok"},{"role":"user","content":"[Message from another task - treat this as a user instruction]\n\nfrom sub"}],
        "temperature":0.2,"max_tokens":100,"reasoning_effort":"low","tools":[{"type":"function","name":"shell","parameters":{"type":"object"}}],"tool_choice":"auto","parallel_tool_calls":false})
    );
    assert_eq!(payload["metadata"], json!({"a":1}));
}
#[test]
fn f1_orphan_outputs_and_reasoning() {
    let t = responses_to_chat(&json!({"input":[{"type":"function_call_output","output":[{"type":"output_text","text":"done"}]},{"type":"function_call_output","output":""},{"type":"function_call_output","call_id":"c","output":null},{"type":"function_call_output","call_id":"c","output":{"a":1}}]}),&Default::default()).unwrap();
    assert_eq!(
        t.chat["messages"],
        json!([{"role":"user","content":"[Message from another task - treat this as a user instruction]\n\ndone"},{"role":"tool","tool_call_id":"","content":""},{"role":"tool","tool_call_id":"c","content":"None"},{"role":"tool","tool_call_id":"c","content":"{\"a\": 1}"}])
    );
    let t = responses_to_chat(&json!({"input":[{"type":"reasoning","summary":"pre"},{"role":"assistant","content":""},{"type":"function_call","name":"x"},{"role":"assistant","content":"a"},{"type":"reasoning","content":"r2"}]}),&Default::default()).unwrap();
    assert_eq!(t.chat["messages"].as_array().unwrap().len(), 1);
    assert_eq!(t.chat["messages"][0]["reasoning_content"], "r2");
    assert_eq!(t.chat["messages"][0]["content"], "a");
}
#[test]
fn f2_f3_custom_namespace_and_additional_tools() {
    let payload = json!({"model":"m","tools":[{"type":"function","name":"duplicate","parameters":{}}],"input":[{"type":"additional_tools","tools":[{"type":"namespace","name":"functions","children":[{"type":"custom","name":"exec","description":"run","format":{"definition":"start: /.*/s"}},{"name":"wait","input_schema":{"type":"object"}},{"name":"duplicate"}]}]}]});
    let t = responses_to_chat(&payload, &Default::default()).unwrap();
    assert_eq!(t.chat["tools"].as_array().unwrap().len(), 3);
    assert_eq!(
        t.chat["tools"][1]["parameters"]["required"],
        json!(["input"])
    );
    assert!(strval(&t.chat["tools"][1]["description"]).ends_with("Grammar:\nstart: /.*/s"));
    assert_eq!(t.metadata.namespace_map.get("exec").unwrap(), "functions");
    assert!(t.metadata.custom_names.contains("exec"));
    assert!(!t.metadata.namespace_map.contains_key("duplicate"));
    assert!(t.chat["messages"].as_array().unwrap().is_empty());
    for name in ["wait", "functions::wait", "functions__wait"] {
        assert_eq!(
            resolve_namespaced_name(name, &t.metadata.namespace_map),
            ("wait".into(), "functions".into())
        );
    }
    let mut o = options();
    o.metadata = t.metadata;
    let r = chat_to_response(&json!({"choices":[{"message":{"tool_calls":[{"id":"c","function":{"name":"functions::exec","arguments":"{\"input\":\"pwd\"}"}}]}}]}),&o).unwrap();
    assert_eq!(r["output"][0]["type"], "custom_tool_call");
    assert_eq!(r["output"][0]["namespace"], "functions");
    assert_eq!(r["output"][0]["input"], "pwd");
}
#[test]
fn f4_response_output_and_usage() {
    let chat = json!({"choices":[{"finish_reason":"length","message":{"content":"partial","reasoning_content":"r","tool_calls":[{"id":"call_8","function":{"name":"get_weather","arguments":"{\"city\":\"Beijing\"}"}}]}}],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7,"prompt_cache_hit_tokens":1}});
    let r = chat_to_response(&chat, &options()).unwrap();
    assert_eq!(r["status"], "incomplete");
    assert_eq!(
        r["incomplete_details"],
        json!({"reason":"max_output_tokens"})
    );
    assert_eq!(
        r["usage"],
        json!({"input_tokens":5,"input_tokens_details":{"cached_tokens":1},"output_tokens":2,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":7})
    );
    assert_eq!(
        r["output"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| strval(&i["type"]))
            .collect::<Vec<_>>(),
        vec!["reasoning", "function_call", "message"]
    );
    assert_eq!(r["output_text"], "partial");
    assert_eq!(r["id"], "resp_fixed");
    assert_eq!(r["created_at"], 1700000000);
}
#[test]
fn f5_response_stream_order_ids_and_incremental_text() {
    let mut s = ResponsesStream::new(options());
    let mut frames = s.push_line(r#"data: {"id":"c1","model":"m","choices":[{"index":0,"delta":{"reasoning_content":"think"}}]}"#);
    assert_eq!(frames.len(), 5);
    let text = s.push_line(r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#);
    assert_eq!(text.last().unwrap().event, "response.output_text.delta");
    assert!(!s.is_terminal());
    frames.extend(text);
    frames.extend(s.push_line(r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":3,"total_tokens":13,"prompt_tokens_details":{"cached_tokens":4},"completion_tokens_details":{"reasoning_tokens":1}}}"#));
    frames.extend(s.push_line("data: [DONE]"));
    frames.extend(s.finish());
    let kinds: Vec<_> = frames.iter().map(|f| f.event.as_str()).collect();
    assert_eq!(
        kinds,
        vec![
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.reasoning_summary_part.added",
            "response.reasoning_summary_text.delta",
            "response.reasoning_summary_text.done",
            "response.reasoning_summary_part.done",
            "response.output_item.done",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.completed"
        ]
    );
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(frame.data["sequence_number"], i + 1);
    }
    let r = final_response(&frames);
    assert_eq!(r["id"], "resp_fixed");
    assert_eq!(r["output_text"], "Hello");
    assert_eq!(r["usage"]["input_tokens_details"]["cached_tokens"], 4);
    assert_eq!(frames[2].data["item"]["id"], r["output"][0]["id"]);
    assert_eq!(frames[8].data["item"]["id"], r["output"][1]["id"]);
    assert!(s.finish().is_empty());
    assert!(s.abort("late").is_empty());
}
#[test]
fn f6_namespaced_tool_stream() {
    let mut o = options();
    o.metadata
        .namespace_map
        .insert("js".into(), "node_repl".into());
    let frames = rstream(
        &[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_A","function":{"name":"js","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"code\":1}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
        ],
        o,
    );
    assert_eq!(frames.len(), 7);
    assert_eq!(frames[2].data["item"]["namespace"], "node_repl");
    assert_eq!(frames[3].data["delta"], "{\"code\":1}");
    let r = final_response(&frames);
    assert_eq!(r["output"].as_array().unwrap().len(), 1);
    assert!(r.get("usage").is_none());
    assert_eq!(r["output"][0]["arguments"], "{\"code\":1}");
}
#[test]
fn f6_first_arguments_emit_after_added_and_match_done_once() {
    // Explicit divergence from 477ba36's f6_first_arguments_are_done_only:
    // de4d86d (included in 4f7ae6f/v1.8.0) now emits the opening fragment.
    for (first, later) in [
        (Some(r#"{"a":1}"#), ""),
        (Some(r#"{"a":"#), "1}"),
        (Some(""), r#"{"a":1}"#),
        (None, r#"{"a":1}"#),
    ] {
        let mut stream = ResponsesStream::new(options());
        let mut function = json!({"name":"f"});
        if let Some(args) = first {
            function["arguments"] = json!(args);
        }
        let mut frames = stream.push_chunk(&json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"c","function":function}
        ]}}]}));
        assert_eq!(frames[2].event, "response.output_item.added");
        assert_eq!(frames[2].data["item"]["arguments"], "");
        if first.is_some_and(|s| !s.is_empty()) {
            assert_eq!(frames.len(), 4);
            assert_eq!(frames[3].event, "response.function_call_arguments.delta");
            assert_eq!(frames[3].data["delta"], first.unwrap());
        } else {
            assert_eq!(frames.len(), 3);
        }
        frames.extend(
            stream.push_chunk(&json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"function":{"arguments":later}}
        ]},"finish_reason":"tool_calls"}]})),
        );
        assert!(stream.push_line("[DONE]").is_empty());
        let done = stream.finish();
        assert_eq!(done.len(), 3);
        assert_eq!(done[0].event, "response.function_call_arguments.done");
        assert_eq!(done[1].event, "response.output_item.done");
        assert_eq!(done[2].event, "response.completed");
        assert_eq!(done[0].data["arguments"], r#"{"a":1}"#);
        assert_eq!(done[1].data["item"]["arguments"], r#"{"a":1}"#);
        assert_eq!(
            done[2].data["response"]["output"][0]["arguments"],
            r#"{"a":1}"#
        );
        frames.extend(done);
        let deltas: Vec<_> = frames
            .iter()
            .filter(|f| f.event == "response.function_call_arguments.delta")
            .collect();
        assert!(deltas.iter().all(|f| !strval(&f.data["delta"]).is_empty()));
        assert_eq!(
            deltas
                .iter()
                .map(|f| strval(&f.data["delta"]))
                .collect::<String>(),
            r#"{"a":1}"#
        );
        for (i, frame) in frames.iter().enumerate() {
            assert_eq!(frame.data["sequence_number"], i + 1);
            if frame.data.get("item_id").is_some() {
                assert_eq!(frame.data["item_id"], frames[2].data["item"]["id"]);
                assert_eq!(frame.data["call_id"], "c");
                assert_eq!(frame.data["output_index"], 0);
            }
        }
        assert!(stream.finish().is_empty());
    }
}
#[test]
fn f7_custom_stream_raw_deltas_unwrapped_done() {
    let mut o = options();
    o.metadata.custom_names.insert("apply_patch".into());
    let frames = rstream(
        &[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_9","function":{"name":"apply_patch","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"input\":\"*** Begin"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":" Patch\\n+hi\\n*** End Patch\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        ],
        o,
    );
    assert_eq!(frames.len(), 8);
    assert_eq!(
        event(&frames, "response.custom_tool_call_input.done")["input"],
        "*** Begin Patch\n+hi\n*** End Patch"
    );
    assert_eq!(final_response(&frames)["status"], "completed");
}
#[test]
fn f8_truncated_call_dropped_without_renumbering() {
    let frames = rstream(
        &[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_r","function":{"name":"f","arguments":"{\"a\":"}}]},"finish_reason":"tool_calls"}]}"#,
        ],
        options(),
    );
    // de4d86d also emits the initial fragment of a subsequently dropped call.
    assert_eq!(frames.len(), 10);
    assert_eq!(frames[3].event, "response.function_call_arguments.delta");
    assert_eq!(frames[3].data["output_index"], 0);
    assert_eq!(frames[3].data["delta"], "{\"a\":");
    assert_eq!(frames[4].data["output_index"], 1);
    let r = final_response(&frames);
    assert_eq!(r["status"], "incomplete");
    assert!(r.get("incomplete_details").is_none());
    assert_eq!(r["output"][0]["type"], "message");
    assert!(!frames.iter().any(|f| f.event.ends_with("arguments.done")));
}
#[test]
fn f9_reasoning_empty_errors_abort_are_terminal() {
    let frames = rstream(
        &[r#"{"choices":[{"delta":{"reasoning_content":"thinking..."}}]}"#],
        options(),
    );
    assert_eq!(frames.len(), 9);
    assert_eq!(frames.last().unwrap().event, "response.failed");
    assert_eq!(final_response(&frames)["usage"]["total_tokens"], 4);
    let frames = rstream(&["[DONE]"], options());
    assert_eq!(frames.len(), 8);
    assert_eq!(final_response(&frames)["output"][0]["content"], json!([]));
    let mut s = ResponsesStream::new(options());
    let frames = s.push_line(r#"data: {"error":{"message":"boom"}}"#);
    assert_eq!(frames.last().unwrap().event, "response.failed");
    assert!(s.failed());
    assert!(s.finish().is_empty());
    assert!(s.push_line("[DONE]").is_empty());
    let mut s = ResponsesStream::new(options());
    assert_eq!(s.abort("x")[0].to_sse(),"event: error\ndata: {\"type\": \"error\", \"code\": \"upstream_stream_error\", \"message\": \"upstream stream aborted: x\", \"param\": null, \"sequence_number\": 0}\n\n");
    assert!(s.finish().is_empty());
}
#[test]
fn reasoning_custom_is_not_false_rejection() {
    let mut o = options();
    o.metadata.custom_names.insert("exec".into());
    let chat = json!({"choices":[{"message":{"reasoning_content":"plan","tool_calls":[{"function":{"name":"exec","arguments":"{\"input\":\"pwd\"}"}}]}}]});
    assert!(chat_to_response(&chat, &o).is_ok());
    assert!(chat_to_response(
        &json!({"choices":[{"message":{"reasoning_content":"plan"}}]}),
        &o
    )
    .is_err());
}
#[test]
fn f10_compaction_roundtrip_request_output() {
    assert_eq!(
        encode_compaction_summary("the summary"),
        "eyJ2IjoxLCJzdW1tYXJ5IjoidGhlIHN1bW1hcnkifQ=="
    );
    for s in ["the summary", "你好", "", "a", "ab", "abc", "🚀"] {
        assert_eq!(decode_compaction_summary(&encode_compaction_summary(s)), s);
    }
    assert_eq!(decode_compaction_summary("plain text"), "plain text");
    let o = TranslationOptions {
        default_max_output_tokens: Some(128000),
        ..Default::default()
    };
    let t = responses_to_chat(&json!({"model":"deepseek-v4.1-flash","input":[{"role":"user","content":"do the thing"},{"role":"assistant","content":"done"},{"type":"compaction_trigger"}],"tools":[{"name":"exec"}],"parallel_tool_calls":true}),&o).unwrap();
    assert!(t.compaction);
    assert_eq!(t.chat["messages"][2]["content"], COMPACTION_PROMPT);
    assert_eq!(t.chat["max_tokens"], 128000);
    assert!(t.chat.get("tools").is_none());
    assert!(t.chat.get("parallel_tool_calls").is_none());
    let chat = json!({"choices":[{"message":{"content":"handoff summary"}}],"usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}});
    let frames = compaction_events(&chat, &options()).unwrap();
    assert_eq!(frames.len(), 4);
    assert_eq!(frames[2].event, "response.output_item.done");
    assert_eq!(
        final_response(&frames)["output"][0]["encrypted_content"],
        "eyJ2IjoxLCJzdW1tYXJ5IjoiaGFuZG9mZiBzdW1tYXJ5In0="
    );
    assert!(chat_to_compaction(
        &json!({"choices":[{"message":{"content":" "}}]}),
        &options()
    )
    .is_err());
    let replay = responses_to_chat(&json!({"input":[{"type":"compaction","encrypted_content":encode_compaction_summary("earlier work summary")},{"role":"user","content":"continue"}]}),&Default::default()).unwrap();
    assert_eq!(
        replay.chat["messages"][0]["content"],
        "Summary of the earlier conversation (context checkpoint):\n\nearlier work summary"
    );
}
#[test]
fn f11_anthropic_request_golden() {
    let p = json!({"model":"deepseek-v4.1-flash","max_tokens":128,"stream":true,"system":[{"type":"text","text":"sys-a"},{"type":"text","text":"sys-b"}],"messages":[
        {"role":"user","content":[{"type":"text","text":"look"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"aaa"}}]},
        {"role":"assistant","content":[{"type":"thinking","thinking":"secret","signature":"sig"},{"type":"text","text":"ok"},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"cmd":"ls"}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"a.txt"},{"type":"text","text":"next"}]}],
        "tools":[{"name":"Bash","description":"run","input_schema":{"type":"object"}},{"type":"web_search_20250305","name":"web_search"}],"tool_choice":{"type":"tool","name":"Bash","disable_parallel_tool_use":true},"stop_sequences":["END"],"metadata":{"user_id":"u"},"thinking":{"type":"adaptive"},"output_config":{"effort":"high"},"top_k":5});
    let t = messages_to_chat(&p, &Default::default()).unwrap();
    assert_eq!(
        t.chat,
        json!({"model":"deepseek-v4.1-flash","messages":[
        {"role":"system","content":"sys-a\nsys-b\nThese Anthropic server tools are not available here: web_search. Do not call them."},
        {"role":"user","content":[{"type":"text","text":"look"},{"type":"image_url","image_url":{"url":"data:image/png;base64,aaa"}}]},
        {"role":"assistant","tool_calls":[{"id":"toolu_1","type":"function","function":{"name":"Bash","arguments":"{\"cmd\":\"ls\"}"}}],"content":"ok"},
        {"role":"tool","tool_call_id":"toolu_1","content":"a.txt"},{"role":"user","content":"next"}],"max_tokens":128,"stream":true,"stop":["END"],"user":"u","tools":[{"type":"function","function":{"name":"Bash","description":"run","parameters":{"type":"object"}}}],"tool_choice":{"type":"function","function":{"name":"Bash"}},"parallel_tool_calls":false,"reasoning_effort":"high"})
    );
}
#[test]
fn f11_midconversation_system_multimodal_result_effort() {
    let p = json!({"model":"m","system":"top","messages":[{"role":"user","content":"hi"},{"role":"system","content":[{"type":"text","text":"hook context"}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"c","is_error":true,"content":[{"type":"text","text":"screenshot read"},{"type":"image","source":{"type":"base64","data":"bbb"}},{"type":"document","source":{"type":"text","data":"doc"}}]}]}],"thinking":{"type":"enabled","budget_tokens":17000}});
    let t = messages_to_chat(&p, &Default::default()).unwrap();
    assert_eq!(t.chat["reasoning_effort"], "high");
    assert_eq!(t.chat["messages"][2]["content"], "hook context");
    assert_eq!(
        t.chat["messages"][3]["content"],
        json!([{"type":"text","text":"error:"},{"type":"text","text":"screenshot read"},{"type":"image_url","image_url":{"url":"data:image/png;base64,bbb"}},{"type":"text","text":"doc"}])
    );
}
#[test]
fn f12_anthropic_output_golden() {
    let chat = json!({"id":"chatcmpl_1","model":"deepseek-v4.1-flash","choices":[{"finish_reason":"tool_calls","message":{"content":"hello","reasoning_content":"secret","tool_calls":[{"id":"call_1","function":{"name":"Bash","arguments":"{\"cmd\":\"ls\"}"}}]}}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,"prompt_cache_hit_tokens":4,"prompt_cache_write_tokens":2}});
    assert_eq!(
        chat_to_messages(&chat).unwrap(),
        json!({"id":"msg_chatcmpl_1","type":"message","role":"assistant","model":"deepseek-v4.1-flash","content":[{"type":"thinking","thinking":"secret"},{"type":"text","text":"hello"},{"type":"tool_use","id":"call_1","name":"Bash","input":{"cmd":"ls"}}],"stop_reason":"tool_use","stop_sequence":null,"usage":{"input_tokens":4,"cache_creation_input_tokens":2,"cache_read_input_tokens":4,"output_tokens":5}})
    );
}
#[test]
fn f13_anthropic_stream_byte_golden() {
    let frames = mstream(&[
        r#"{"id":"chatcmpl_4","model":"m","choices":[{"delta":{"reasoning_content":"hmm"}}]}"#,
        r#"{"choices":[{"delta":{"content":"42"}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":2,"total_tokens":13,"prompt_cache_hit_tokens":3,"prompt_cache_write_tokens":1}}"#,
        "data: [DONE]",
    ]);
    let expected = [
        "event: message_start\ndata: {\"message\":{\"id\":\"msg_chatcmpl_4\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"m\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":0,\"output_tokens\":0}},\"type\":\"message_start\"}\n\n",
        "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"},\"type\":\"content_block_start\"}\n\n",
        "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"},\"type\":\"content_block_delta\"}\n\n",
        "event: content_block_stop\ndata: {\"index\":0,\"type\":\"content_block_stop\"}\n\n",
        "event: content_block_start\ndata: {\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"},\"type\":\"content_block_start\"}\n\n",
        "event: content_block_delta\ndata: {\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"42\"},\"type\":\"content_block_delta\"}\n\n",
        "event: content_block_stop\ndata: {\"index\":1,\"type\":\"content_block_stop\"}\n\n",
        "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"input_tokens\":7,\"cache_creation_input_tokens\":1,\"cache_read_input_tokens\":3,\"output_tokens\":2},\"type\":\"message_delta\"}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"];
    assert_eq!(
        frames.iter().map(SseEvent::to_sse).collect::<Vec<_>>(),
        expected
    );
}
#[test]
fn f13_tools_partial_args_and_error() {
    let mut stream = MessagesStream::new("m");
    let first = stream.push_line(r#"{"choices":[{"delta":{"tool_calls":[{"index":4,"id":"call_1","function":{"name":"Bash","arguments":"{\"cmd\":"}}]}}]}"#);
    assert_eq!(
        first.last().unwrap().data["delta"]["partial_json"],
        "{\"cmd\":"
    );
    assert!(!stream.is_terminal());
    let next = stream.push_line(r#"{"choices":[{"delta":{"tool_calls":[{"index":4,"id":"changed","function":{"arguments":"\"ls\"}"}}]},"finish_reason":"tool_calls"}]}"#);
    assert_eq!(next[0].data["index"], first.last().unwrap().data["index"]);
    assert_eq!(
        stream
            .finish()
            .iter()
            .find(|f| f.event == "message_delta")
            .unwrap()
            .data["delta"]["stop_reason"],
        "tool_use"
    );
    let frames = mstream(&[r#"data: {"error":{"message":"boom"}}"#]);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].to_sse(),"event: error\ndata: {\"error\":{\"type\":\"api_error\",\"message\":\"boom\"},\"type\":\"error\"}\n\n");
    assert_eq!(mstream(&["[DONE]"]).len(), 3);
}
#[test]
fn f14_validation_and_count_tokens() {
    for p in [
        json!({}),
        json!({"model":"m","messages":[]}),
        json!({"model":"m","messages":"x"}),
        json!({"model":"m","messages":[{"role":"tool","content":"x"}]}),
        json!({"model":"m","messages":[42]}),
    ] {
        assert_eq!(
            messages_to_chat(&p, &Default::default())
                .unwrap_err()
                .status,
            400
        );
    }
    assert_eq!(
        count_tokens(
            &json!({"model":"m","messages":[{"role":"user","content":"hello world"}]}),
            &Default::default()
        )
        .unwrap(),
        json!({"input_tokens":3})
    );
    assert_eq!(
        count_tokens(
            &json!({"model":"m","messages":[{"role":"user","content":"你好"}]}),
            &Default::default()
        )
        .unwrap(),
        json!({"input_tokens":3})
    );
}
#[test]
fn f15_tool_choice_none_keeps_tools() {
    let p = json!({"model":"m","input":"hello","tools":[{"type":"function","name":"f"}],"tool_choice":{"type":"none"}});
    let t = responses_to_chat(&p, &Default::default()).unwrap();
    assert_eq!(t.chat["tool_choice"], json!({"type":"none"}));
    assert_eq!(t.chat["tools"].as_array().unwrap().len(), 1);
    // Wire normalization belongs to wbchat; translation must not discard tools.
}
#[test]
fn unsupported_requests_are_explicit() {
    assert_eq!(
        responses_to_chat(&json!({"previous_response_id":"r"}), &Default::default())
            .unwrap_err()
            .status,
        400
    );
    let o = TranslationOptions {
        local_web_tools: true,
        ..Default::default()
    };
    assert!(
        responses_to_chat(&json!({"tools":[{"type":"web_search"}]}), &o)
            .unwrap_err()
            .message
            .contains("not supported")
    );
    assert!(chat_to_messages(&json!({"error":{"message":"boom"}})).is_err());
    assert!(chat_to_response(&json!({"error":{"message":"boom"}}), &options()).is_err());
}
#[test]
fn ids_indices_stable_multiple_calls_and_usage_best() {
    let frames = rstream(
        &[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":7,"id":"a","function":{"name":"f","arguments":""}},{"index":2,"id":"b","function":{"name":"g","arguments":""}}]}}],"usage":{"total_tokens":20,"prompt_tokens":10,"completion_tokens":10}}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":2,"id":"changed","function":{"arguments":"{}"}},{"index":7,"function":{"arguments":"{}"}}]}}],"usage":{"total_tokens":3}}"#,
            "[DONE]",
        ],
        options(),
    );
    let r = final_response(&frames);
    assert_eq!(r["usage"]["total_tokens"], 20);
    assert_eq!(r["output"][0]["call_id"], "a");
    assert_eq!(r["output"][1]["call_id"], "b");
    for f in frames
        .iter()
        .filter(|f| f.event == "response.output_item.done")
    {
        let i = f.data["output_index"].as_u64().unwrap() as usize;
        assert_eq!(f.data["item"]["id"], r["output"][i]["id"]);
    }
}
#[test]
fn no_deltas_after_reasoning_item_done() {
    let frames = rstream(
        &[
            r#"{"choices":[{"delta":{"reasoning_content":"r"}}]}"#,
            r#"{"choices":[{"delta":{"content":"x"}}]}"#,
            r#"{"choices":[{"delta":{"reasoning_content":"later"}}]}"#,
            "[DONE]",
        ],
        options(),
    );
    let r = final_response(&frames);
    assert_eq!(r["output"].as_array().unwrap().len(), 3);
    assert_eq!(r["output"][2]["summary"][0]["text"], "later");
    assert_ne!(r["output"][0]["id"], r["output"][2]["id"]);
}

#[test]
fn nonstream_checked_aggregator_cannot_swallow_errors() {
    let mut a = CheckedAggregator::new();
    a.push_line(r#"data: {"choices":[{"delta":{"content":"partial"}}]}"#)
        .unwrap();
    assert!(a
        .push_line(r#"data: {"error":{"message":"boom"}}"#)
        .is_err());
    assert!(a.push_line("[DONE]").is_err());
    assert_eq!(a.finish("m", None).unwrap_err().message, "boom");
    let mut a = CheckedAggregator::new();
    a.abort("read failure");
    assert!(a.finish("m", None).is_err());
    let mut a = CheckedAggregator::new();
    a.push_line(r#"data: {"choices":[{"delta":{"content":"hello"}}]}"#)
        .unwrap();
    a.push_line("[DONE]").unwrap();
    assert_eq!(
        a.finish("m", None).unwrap()["choices"][0]["message"]["content"],
        "hello"
    );
}

#[test]
fn empty_error_objects_are_terminal_too() {
    let mut m = MessagesStream::new("m");
    assert_eq!(m.push_chunk(&json!({"error":{}}))[0].event, "error");
    assert!(m.finish().is_empty());
    let mut r = ResponsesStream::new(options());
    assert_eq!(
        r.push_chunk(&json!({"error":{}})).last().unwrap().event,
        "response.failed"
    );
    assert!(r.finish().is_empty());
}

#[test]
fn response_dsml_split_filter_retains_incremental_plain_text() {
    let mut s = ResponsesStream::new(options());
    let first = s.push_chunk(&json!({"choices":[{"delta":{"content":"Hi <|DS"}}]}));
    assert_eq!(event(&first, "response.output_text.delta")["delta"], "Hi ");
    assert!(!s.is_terminal());
    let mut frames = first;
    frames.extend(s.push_chunk(&json!({"choices":[{"delta":{"content":"ML|calls><|DSML|invoke name=\"run_code\"><|DSML|parameter name=\"code\">x</|DSML|parameter></|DSML|invoke></|DSML|calls> bye"}}]})));
    frames.extend(s.finish());
    assert_eq!(final_response(&frames)["output_text"], "Hi  bye");
    assert!(array(&final_response(&frames)["output"])
        .iter()
        .any(|v| v["type"] == "function_call" && v["name"] == "run_code"));
    assert!(!frames
        .iter()
        .filter(|f| f.event == "response.output_text.delta")
        .any(|f| strval(&f.data["delta"]).contains("DSML")));
}

#[test]
fn response_missing_tool_indices_synthesized_and_choice_scoped() {
    let frames = rstream(
        &[
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"id":"a","function":{"name":"f","arguments":""}},{"id":"b","function":{"name":"g","arguments":""}}]}},{"index":1,"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"h","arguments":""}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"function":{"arguments":"{}"}},{"function":{"arguments":"{}"}}]}},{"index":1,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{}"}}]}}]}"#,
            "[DONE]",
        ],
        options(),
    );
    let r = final_response(&frames);
    assert_eq!(array(&r["output"]).len(), 3);
    for (i, cid) in ["a", "b", "c"].iter().enumerate() {
        assert_eq!(r["output"][i]["call_id"], *cid);
        assert_eq!(r["output"][i]["arguments"], "{}");
    }
}

#[test]
fn contract_and_metadata_nulls_preserved() {
    let p = json!({"input":"","text":{"format":{"type":"json_schema","name":"answer","schema":{"type":"object"},"strict":false}},"tool_choice":null,"parallel_tool_calls":null});
    let t = responses_to_chat(
        &p,
        &TranslationOptions {
            preserve_request_contract: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        t.chat["response_format"],
        json!({"type":"json_schema","json_schema":{"name":"answer","schema":{"type":"object"},"strict":false}})
    );
    assert_eq!(t.chat["messages"], json!([{"role":"user","content":""}]));
    let mut o = options();
    o.metadata = t.metadata;
    let r = chat_to_response(&json!({"choices":[{"message":{"content":"ok"}}]}), &o).unwrap();
    assert!(r["tool_choice"].is_null());
    assert!(r["parallel_tool_calls"].is_null());
    assert!(responses_to_chat(&p, &Default::default())
        .unwrap()
        .chat
        .get("response_format")
        .is_none());
}
