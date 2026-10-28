//! Cross-language comparisons against captured, unchanged Python functions.
//! The original 477ba36 cases stay intact; de4d86d's two intentional stream
//! divergences are explicit below and independently checked against 4f7ae6f.
use super::*;

fn metadata(meta: &Value, custom: &Value, namespace: &Value) -> RequestMetadata {
    RequestMetadata {
        tools: meta.get("tools").cloned().unwrap_or(json!([])),
        tool_choice: meta.get("tool_choice").cloned().unwrap_or(json!("auto")),
        parallel_tool_calls: meta
            .get("parallel_tool_calls")
            .cloned()
            .unwrap_or(json!(true)),
        custom_names: array(custom).iter().map(text).collect(),
        namespace_map: namespace
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), text(v))).collect())
            .unwrap_or_default(),
    }
}
// Compare generated ids by a bijection, not by deleting them: changing an id
// midway through a stream is still a failure. Non-generated caller ids are exact.
fn canonicalize_ids(value: &mut Value, ids: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(m) => {
            // Sort traversal only; JSON object insertion order is not the oracle contract.
            let mut keys: Vec<_> = m.keys().cloned().collect();
            keys.sort();
            for key in keys {
                let v = m.get_mut(&key).unwrap();
                if matches!(key.as_str(), "id" | "item_id" | "call_id") {
                    if let Some(s) = v.as_str() {
                        if let Some((prefix, suffix)) = s.rsplit_once('_') {
                            if suffix.len() == 32 && suffix.bytes().all(|c| c.is_ascii_hexdigit()) {
                                let n = ids.len();
                                let replacement = ids
                                    .entry(s.to_string())
                                    .or_insert_with(|| format!("{prefix}_<generated_{n}>"));
                                *v = json!(replacement);
                                continue;
                            }
                        }
                    }
                }
                canonicalize_ids(v, ids);
            }
        }
        Value::Array(values) => {
            for v in values {
                canonicalize_ids(v, ids);
            }
        }
        _ => {}
    }
}
fn assert_oracle(mut actual: Value, mut expected: Value, case: &str) {
    canonicalize_ids(&mut actual, &mut BTreeMap::new());
    canonicalize_ids(&mut expected, &mut BTreeMap::new());
    assert_eq!(actual, expected, "Python oracle case {case}");
}

// Do not replace/delete historical golden cases or mask all argument deltas.
// Only these two baseline cases changed: insert exactly the missing first delta
// immediately after added, then shift subsequent sequence numbers by one.
fn first_argument_divergence(case: &Value, baseline: &Value) -> Value {
    let (kind, fragment) = match strval(&case["id"]) {
        "responses.stream_custom" => ("response.custom_tool_call_input.delta", "{\"input\":"),
        "responses.stream_truncated" => ("response.function_call_arguments.delta", "{\"a\":"),
        _ => return baseline.clone(),
    };
    assert_eq!(
        case["input"]["chunks"][0]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
        fragment
    );
    let mut frames = array(baseline).to_vec();
    assert_eq!(frames[2]["event"], "response.output_item.added");
    let added = &frames[2]["data"];
    let delta = json!({"event":kind,"data":{
        "type":kind,"sequence_number":4,"output_index":added["output_index"],
        "item_id":added["item"]["id"],"call_id":added["item"]["call_id"],"delta":fragment
    }});
    frames.insert(3, delta);
    for (i, frame) in frames.iter_mut().enumerate().skip(4) {
        assert_eq!(frame["data"]["sequence_number"], i);
        frame["data"]["sequence_number"] = json!(i + 1);
    }
    json!(frames)
}

#[test]
fn captured_python_translation_oracle() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../tests/baseline/golden.json")).unwrap();
    let mut compared = 0;
    for case in array(&fixture["cases"]) {
        let operation = strval(&case["operation"]);
        let args = &case["input"]["args"];
        let expected = case["expected"].clone();
        let actual = match operation {
            "wb_proxy.responses_to_chat" => {
                let t = responses_to_chat(&args[0], &TranslationOptions::default()).unwrap();
                let mut v = t.chat;
                // The Python private map is deliberately transported out-of-band.
                if expected.get("_namespace_map").is_some() {
                    v["_namespace_map"] = json!(t.metadata.namespace_map);
                }
                v
            }
            "wb_proxy.messages_to_chat" => {
                messages_to_chat(&args[0], &TranslationOptions::default())
                    .unwrap()
                    .chat
            }
            "wb_proxy.chat_to_response" => {
                let o = ResponseOptions {
                    model: text(&args[1]),
                    metadata: metadata(&args[3], &args[2], &args[4]),
                    created_at: Some(1700000000),
                    ..Default::default()
                };
                chat_to_response(&args[0], &o).unwrap()
            }
            "wb_proxy.chat_to_messages" => chat_to_messages(&args[0]).unwrap(),
            "wb_proxy._anthropic_stop_reason" => {
                json!(crate::outputs::stop_reason(strval(&args[0])))
            }
            "wb_proxy.anthropic_effort_from_messages" => {
                let mut p = args[0].clone();
                p["model"] = json!("m");
                p["messages"] = json!([{"role":"user","content":"x"}]);
                messages_to_chat(&p, &Default::default()).unwrap().chat["reasoning_effort"].clone()
            }
            "wb_proxy._anthropic_usage" => anthropic_usage(&args[0]),
            "wb_proxy._responses_usage" => responses_usage(&args[0]).unwrap_or(Value::Null),
            "wb_proxy.resolve_namespaced_name" => {
                let mapping = metadata(&Value::Null, &Value::Null, &args[1]).namespace_map;
                let (name, ns) = resolve_namespaced_name(strval(&args[0]), &mapping);
                json!([name, ns])
            }
            "wb_proxy.stream_responses_events" => {
                let input = &case["input"];
                let holder = &input["holder"];
                let o = ResponseOptions {
                    model: text(&input["model"]),
                    metadata: metadata(
                        &holder["request_meta"],
                        &holder["custom_names"],
                        &holder["namespace_map"],
                    ),
                    created_at: Some(1700000000),
                    ..Default::default()
                };
                let mut stream = ResponsesStream::new(o);
                let mut frames = Vec::new();
                for chunk in array(&input["chunks"]) {
                    frames.extend(if chunk.is_string() {
                        stream.push_line(strval(chunk))
                    } else {
                        stream.push_chunk(chunk)
                    });
                }
                frames.extend(stream.finish());
                assert_oracle(
                    json!(frames
                        .iter()
                        .map(|f| json!({"event":f.event,"data":f.data}))
                        .collect::<Vec<_>>()),
                    first_argument_divergence(case, &expected["frames"]),
                    strval(&case["id"]),
                );
                assert_eq!(
                    stream.usage().cloned().unwrap_or(Value::Null),
                    expected["holder"]["usage"],
                    "usage {}",
                    case["id"]
                );
                compared += 1;
                continue;
            }
            "wb_proxy.stream_messages_events" => {
                let input = &case["input"];
                let mut stream = MessagesStream::new(text(&input["model"]));
                let mut frames = Vec::new();
                for chunk in array(&input["chunks"]) {
                    frames.extend(if chunk.is_string() {
                        stream.push_line(strval(chunk))
                    } else {
                        stream.push_chunk(chunk)
                    });
                }
                frames.extend(stream.finish());
                assert_oracle(
                    json!(frames
                        .iter()
                        .map(|f| json!({"event":f.event,"data":f.data}))
                        .collect::<Vec<_>>()),
                    expected["frames"].clone(),
                    strval(&case["id"]),
                );
                assert_eq!(
                    stream.usage().cloned().unwrap_or(Value::Null),
                    expected["holder"]["usage"],
                    "usage {}",
                    case["id"]
                );
                compared += 1;
                continue;
            }
            _ => continue,
        };
        assert_oracle(actual, expected, strval(&case["id"]));
        compared += 1;
    }
    assert_eq!(
        compared, 32,
        "must compare all 32 supported captured Python cases"
    );
}

#[test]
fn responses_first_arguments_match_v1_8_0_python_chunk_by_chunk() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/responses_first_arguments.json"
    ))
    .unwrap();
    assert_eq!(fixture["oracle"]["revision"], "4f7ae6f");
    assert_eq!(fixture["oracle"]["fix_commit"], "de4d86d");
    assert_eq!(array(&fixture["cases"]).len(), 14);
    for case in array(&fixture["cases"]) {
        let input = &case["input"];
        let holder = &input["holder"];
        let mut stream = ResponsesStream::new(ResponseOptions {
            model: text(&input["model"]),
            metadata: metadata(
                &holder["request_meta"],
                &holder["custom_names"],
                &holder["namespace_map"],
            ),
            created_at: Some(1700000000),
            ..Default::default()
        });
        let mut batches = vec![stream.start()];
        for chunk in array(&input["chunks"]) {
            batches.push(if chunk.is_string() {
                stream.push_line(strval(chunk))
            } else {
                stream.push_chunk(chunk)
            });
        }
        batches.push(stream.finish());
        // Compare batch boundaries too: matching a flattened final stream is
        // insufficient if the first fragment was buffered/replayed at EOF.
        assert_oracle(
            json!(batches
                .iter()
                .map(|batch| batch
                    .iter()
                    .map(|f| json!({"event":f.event,"data":f.data}))
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>()),
            case["expected"]["batches"].clone(),
            strval(&case["id"]),
        );
        assert_eq!(
            stream.usage().cloned().unwrap_or(Value::Null),
            case["expected"]["holder"]["usage"]
        );
        for (i, frame) in batches.iter().flatten().enumerate() {
            assert_eq!(frame.data["sequence_number"], i + 1, "{}", case["id"]);
        }
        assert!(stream.finish().is_empty());
        assert!(stream.push_line("[DONE]").is_empty());
    }
}
