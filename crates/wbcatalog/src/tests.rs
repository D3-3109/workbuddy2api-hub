use super::*;
use wbcore::types::{Cred, Egress, HeaderCase, InResponse, Product};

fn payload(ids: &[&str], models: Value) -> Value {
    json!({"code": 0, "data": {"agents": [{"name": "cli", "models": ids}], "products": [{"models": models}]}})
}
fn ids(catalog: &Catalog, realm: Realm) -> Vec<&str> {
    catalog
        .entries(realm)
        .iter()
        .map(|(id, _)| id.as_str())
        .collect()
}

#[test]
fn bundled_names_and_snapshot_counts() {
    let catalog = Catalog::bundled();
    assert_eq!(bundled_models(Realm::Intl).len(), 24);
    assert_eq!(bundled_models(Realm::Cn).len(), 39);
    assert_eq!(ids(&catalog, Realm::Intl), INTL_UI_ORDER);
    assert_eq!(ids(&catalog, Realm::Cn), CN_UI_ORDER);
    for realm in Realm::ALL {
        let response = catalog.list_at(realm, 123.0);
        assert_eq!(response["object"], "list");
        assert_eq!(response["realm"], realm.as_str());
        assert_eq!(response["catalog"]["source"], "bundled");
        assert!(response["catalog"]["file_mtime"].is_null());
        assert!(response["catalog"]["fetched_at"].as_f64().unwrap() > 0.0);
        for entry in response["data"].as_array().unwrap() {
            assert_eq!(entry["created"], 123);
            assert_eq!(entry["owned_by"], "workbuddy");
            assert_eq!(entry["object"], "model");
            assert!(entry.get("entitlement").is_none());
        }
    }
}

#[test]
fn metadata_is_raw_case_insensitive_and_realm_local() {
    let catalog = Catalog::bundled();
    let meta = catalog.meta(Realm::Intl, "  DEEPSEEK-V4.1-FLASH  ");
    assert_eq!(meta["maxOutputTokens"], 128000);
    assert_eq!(
        meta["reasoning"]["supportedEfforts"],
        json!(["low", "high", "max"])
    );
    assert_eq!(meta["reasoning"]["defaultEffort"], "high");
    assert!(meta.get("max_output_tokens").is_none());
    assert_eq!(catalog.meta(Realm::Intl, "no-such-model"), json!({}));
    assert_eq!(catalog.meta(Realm::Cn, "gpt-6-astra"), json!({}));
    assert_eq!(
        catalog.meta(Realm::Intl, "gpt-5.3-codex")["id"],
        "gpt-5.3-codex"
    );
    assert!(!ids(&catalog, Realm::Intl).contains(&"gpt-5.3-codex"));
}

#[test]
fn minimal_entry_full_shape() {
    assert_eq!(
        model_entry_with(
            "unknown",
            &json!({}),
            ModelEntryOptions {
                created: 123,
                ..Default::default()
            }
        ),
        json!({
            "id": "unknown", "object": "model", "created": 123, "owned_by": "workbuddy",
            "capabilities": {"vision": false, "tool_calls": false, "reasoning": false},
            "supports_vision": false, "supports_images": false, "supports_tool_calls": false,
            "supports_reasoning": false, "vision": false, "multimodal": false,
            "abilities": {"vision": false, "functionCall": false, "function_call": false, "reasoning": false},
            "input_modalities": ["text"], "output_modalities": ["text"],
            "modalities": {"input": ["text"], "output": ["text"]},
            "architecture": {"input_modalities": ["text"], "output_modalities": ["text"], "modality": "text->text"},
            "context_length": 1000000, "max_input_tokens": 1000000
        })
    );
}

#[test]
fn rich_entry_full_shape() {
    let meta = json!({
        "name": "Model", "descriptionEn": "English", "descriptionZh": "中文",
        "supportsImages": true, "supportsToolCall": true, "supportsReasoning": true,
        "maxInputTokens": "12345", "maxOutputTokens": 300,
        "contextWindow": {"supportedLengths": [500, 12345]},
        "reasoning": {"supportedEfforts": ["low", "high"], "effort": "high", "defaultEffort": "low", "canDisableThinking": false},
        "onlyReasoning": true, "credits": "x0.00", "vendor": "f", "temperature": 0, "top_p": 0,
        "isDefault": true, "tags": ["craft", "badge:pro", 5, null, "text"]
    });
    let probes = json!({"rich": {"clamped": "120"}});
    let item = model_entry_with(
        "rich",
        &meta,
        ModelEntryOptions {
            created: 456,
            output_probes: Some(&probes),
            limit_index: None,
        },
    );
    assert_eq!(
        item,
        json!({
            "id": "rich", "object": "model", "created": 456, "owned_by": "workbuddy", "name": "Model", "description": "English",
            "capabilities": {"vision": true, "tool_calls": true, "reasoning": true},
            "supports_vision": true, "supports_images": true, "supports_tool_calls": true,
            "supports_reasoning": true, "vision": true, "multimodal": true,
            "abilities": {"vision": true, "functionCall": true, "function_call": true, "reasoning": true},
            "input_modalities": ["text", "image"], "output_modalities": ["text"],
            "modalities": {"input": ["text", "image"], "output": ["text"]},
            "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"], "modality": "text+image->text"},
            "context_length": 12345, "max_input_tokens": 12345,
            "max_output_tokens": 300, "max_completion_tokens": 300,
            "output_clamp": 120, "max_output_tokens_clamped": 120,
            "context_windows": [500, 12345], "reasoning_efforts": ["low", "high"],
            "reasoning_fixed_effort": "high", "reasoning_default_effort": "low", "reasoning_can_disable": false,
            "always_reasoning": true, "credits": "x0.00", "vendor": "f", "temperature": 0, "top_p": 0,
            "is_default": true, "tags": ["craft", "text"]
        })
    );
}

#[test]
fn vision_overrides_and_python_truthiness() {
    let item = model_entry("glm-5.3-flash", &json!({"supportsImages": true}));
    assert_eq!(item["vision"], false);
    assert_eq!(
        item["vision_note"],
        "上游宣告多模态但实际通道不支持读图，已修正为纯文本"
    );
    assert_eq!(item["input_modalities"], json!(["text"]));
    assert_eq!(
        model_entry(
            "x",
            &json!({"supportsImages": true, "disabledMultimodal": "false"})
        )["vision"],
        false
    );
    assert_eq!(
        model_entry(
            "x",
            &json!({"supportsToolCall": "false", "onlyReasoning": []})
        )["supports_tool_calls"],
        true
    );
    assert_eq!(
        model_entry("x", &json!({"onlyReasoning": []}))["always_reasoning"],
        false
    );
    assert_eq!(
        model_entry("x", &json!({"descriptionEn": "", "descriptionZh": "中文"}))["description"],
        "中文"
    );
}

#[test]
fn limit_precedence_and_offline_annotations() {
    let index = json!({"zai/glm-5.3": [111, 222], "vendor/new": {"context": 321, "output": 654}});
    let options = ModelEntryOptions {
        created: 1,
        limit_index: Some(&index),
        output_probes: None,
    };
    let item = model_entry_with("GLM-5.3", &json!({}), options);
    assert_eq!(item["context_length"], 1000000);
    assert_eq!(item["max_output_tokens"], 131072);
    let item = model_entry_with("new", &json!({"maxInputTokens": 900}), options);
    assert_eq!(item["context_length"], 900);
    assert_eq!(item["max_output_tokens"], 654);
    let item = model_entry_with("new", &json!({"maxOutputTokens": "42"}), options);
    assert_eq!(item["context_length"], 321);
    assert_eq!(item["max_output_tokens"], 42);
    let item = model_entry(
        "unknown",
        &json!({"maxInputTokens": -1, "maxOutputTokens": "junk"}),
    );
    assert_eq!(item["context_length"], 1000000);
    assert!(item.get("max_output_tokens").is_none());

    let mut catalog = Catalog::bundled();
    catalog.set_output_probes(json!({"hy3": {"clamped": 4096}}));
    let data = catalog.list_at(Realm::Intl, 1.0);
    let hy3 = data["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "hy3")
        .unwrap();
    assert_eq!(hy3["output_clamp"], 4096);
    assert_eq!(hy3["max_output_tokens"], 64000);
}

#[test]
fn config_prefers_cli_and_keeps_first_longest_deduplicated() {
    let payload = json!({"data": {"agents": [
        {"name": "compact", "models": ["lite", "other", "long", "list", "ignored"]},
        {"name": " CLI ", "models": ["a", " b ", "a", ""]},
        {"name": "cli", "models": ["x", "y", "z"]}
    ]}});
    assert_eq!(parse_remote_catalog(&payload).unwrap().ids, ["a", "b"]);
    let payload = json!({"agents": {"planner": {"models": ["a"]}, "cli": {"models": ["b", "c"]}}});
    assert_eq!(parse_remote_catalog(&payload).unwrap().ids, ["b", "c"]);
    let payload = json!({"agents": [{"name": "planner", "models": ["a"]}, {"name": "main", "models": ["a", "b"]}]});
    assert_eq!(parse_remote_catalog(&payload).unwrap().ids, ["a", "b"]);
}

#[test]
fn every_bundled_entry_matches_current_python_golden() {
    // Generated offline by AST-extracting merge_catalog/model_entry and
    // modelsdev.lookup from the baseline; no wb_proxy import or network.
    let golden: Value = serde_json::from_str(include_str!("bundled_entries_golden.json")).unwrap();
    let catalog = Catalog::bundled();
    for realm in Realm::ALL {
        assert_eq!(
            catalog.list_at(realm, 123.0)["data"],
            golden[realm.as_str()]
        );
    }
}

#[test]
fn legacy_endpoint_unions_agents_without_cli_preference() {
    let mut catalog = Catalog::bundled();
    catalog
        .ingest_endpoint(
            Realm::Cn,
            json!({"data":{"agents":[
                {"name":"cli", "models":["hy3"]},
                {"name":"other", "models":["hy4-preview", "hy3"]}
            ]}}),
        )
        .unwrap();
    assert_eq!(ids(&catalog, Realm::Cn)[0], "hy4-preview");
    assert!(!ids(&catalog, Realm::Cn).contains(&"hy4-preview-f"));
}

#[test]
fn config_nested_agents_and_recursive_metadata_first_wins() {
    let payload = json!({"data": {"config": {"products": [{
        "agents": {"cli": {"models": ["new", "new", "x", null]}},
        "models": [{"id": "new", "credits": "x1.5"}, "junk", {"id": " x ", "credits": "x0"}]
    }]}, "models": [{"id": "new", "credits": "x2"}]}});
    let parsed = parse_remote_catalog(&payload).unwrap();
    assert_eq!(parsed.ids, ["new", "x"]);
    // A containing node's models array is collected before descending into products.
    assert_eq!(parsed.metadata["new"]["credits"], "x2");
    assert_eq!(parsed.metadata["x"]["credits"], "x0");
}

#[test]
fn config_rejects_empty_and_error_payloads() {
    for value in [
        json!({}),
        json!({"data": {"agents": []}}),
        json!({"agents": [{"models": []}]}),
        json!({"models": [{"id": "a"}]}),
        json!({"agents": [{"models": [{"id": "a"}]}]}),
    ] {
        assert_eq!(
            parse_remote_catalog(&value).unwrap_err(),
            CatalogError::EmptyCatalog
        );
    }
    for value in [Value::Null, json!([]), json!(3), json!("x")] {
        assert_eq!(
            parse_remote_catalog(&value).unwrap_err(),
            CatalogError::InvalidPayload
        );
    }
    let mut value = payload(&["a"], json!([]));
    value["code"] = json!(12403);
    assert_eq!(
        parse_remote_catalog(&value).unwrap_err(),
        CatalogError::UpstreamCode("12403".into())
    );
}

#[test]
fn curation_removes_auxiliary_variants_and_paid_siblings() {
    let ids: Vec<String> = [
        "default-model",
        "fast-model",
        "balanced-model",
        "primary-model",
        "deep-model",
        "auto",
        "lite",
        "codewise-a",
        "completion-a",
        "a-image-alpha",
        "a-image-alpha-edit",
        "a-taco-completion",
        "deepseek-v4.1-flash-sg",
        "hy3-x",
        "hy4-preview",
        "hy4-preview-f",
        "hy3",
        "grok-4.7",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let meta = json!({"hy4-preview": {"credits": "x0.29"}, "hy4-preview-f": {"credits": "x0.00"}, "hy3": {"credits": "x0"}});
    assert_eq!(
        curate_remote_catalog(Realm::Intl, &ids, meta.as_object().unwrap()),
        ["hy4-preview-f", "hy3", "grok-4.7"]
    );
    let ids = ["a", "a-f", "b", "b-f"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let meta = json!({"a": {"credits":"0"}, "a-f": {"credits":"x1"}, "b": {"credits":"x1"}, "b-f": {"credits":"0.00"}});
    assert_eq!(
        curate_remote_catalog(Realm::Cn, &ids, meta.as_object().unwrap()),
        ["a", "b-f"]
    );
}

#[test]
fn live_merge_credits_reasoning_and_extras() {
    let mut catalog = Catalog::bundled();
    catalog.ingest_at(Realm::Intl, payload(&["hy4-preview-f", "deepseek-v4.1-flash", "new-model", "unpriced", "default-model"], json!([
        {"id":"hy4-preview-f", "credits":"x0.29"},
        {"id":"deepseek-v4.1-flash", "credits":"x0.11", "reasoning":{"effort":"high", "summary":"auto"}},
        {"id":"new-model", "credits":"x1.90", "supportsImages":true}
    ])), 900.25).unwrap();
    assert_eq!(
        catalog.meta(Realm::Intl, "hy4-preview-f")["credits"],
        "x0.29"
    );
    assert_eq!(catalog.meta(Realm::Cn, "hy4-preview-f")["credits"], "x0.00");
    assert_eq!(ids(&catalog, Realm::Intl).last(), Some(&"new-model"));
    assert!(!ids(&catalog, Realm::Intl).contains(&"unpriced"));
    assert!(!ids(&catalog, Realm::Intl).contains(&"default-model"));
    let meta = catalog.meta(Realm::Intl, "deepseek-v4.1-flash");
    assert_eq!(
        meta["reasoning"]["supportedEfforts"],
        json!(["low", "high", "max"])
    );
    assert_eq!(meta["reasoning"]["defaultEffort"], "high");
    assert!(meta["reasoning"].get("effort").is_none());
    assert_eq!(meta["maxOutputTokens"], 128000);
    assert_eq!(
        catalog.provenance(Realm::Intl),
        json!({"source":"remote", "fetched_at":900.25, "file_mtime":null})
    );
    assert_eq!(catalog.provenance(Realm::Cn)["source"], "bundled");
    catalog.ingest(Realm::Intl, payload(&["deepseek-v4.1-flash"], json!([
        {"id":"deepseek-v4.1-flash", "reasoning":{"supportedEfforts":["low","medium","high"],"defaultEffort":"medium"}}
    ]))).unwrap();
    assert_eq!(
        catalog.meta(Realm::Intl, "deepseek-v4.1-flash")["reasoning"]["defaultEffort"],
        "medium"
    );
    // A fresh successful response starts from the snapshot, not previous live metadata.
    assert_eq!(
        catalog.meta(Realm::Intl, "hy4-preview-f")["credits"],
        "x0.00"
    );
    assert!(!ids(&catalog, Realm::Intl).contains(&"new-model"));
}

#[test]
fn cn_paid_hy4_replaces_unlisted_free_alias() {
    let mut catalog = Catalog::bundled();
    catalog
        .ingest(
            Realm::Cn,
            payload(
                &["auto", "hy4-preview", "hy3-x", "hy3"],
                json!([
                    {"id":"hy4-preview", "credits":"x0.29"}, {"id":"hy3", "credits":"x0.00"}
                ]),
            ),
        )
        .unwrap();
    assert_eq!(ids(&catalog, Realm::Cn)[0], "hy4-preview");
    assert!(!ids(&catalog, Realm::Cn).contains(&"hy4-preview-f"));
    assert!(!ids(&catalog, Realm::Cn).contains(&"hy3-x"));
}

#[test]
fn endpoint_fallback_never_advertises_unknown_names() {
    let mut catalog = Catalog::bundled();
    catalog
        .ingest_endpoint(
            Realm::Intl,
            payload(&["kimi-k3", "internal-only-model"], json!([])),
        )
        .unwrap();
    assert_eq!(ids(&catalog, Realm::Intl), INTL_UI_ORDER);
    assert_eq!(catalog.provenance(Realm::Intl)["source"], "endpoint");
}

#[test]
fn failed_ingest_preserves_models_provenance_and_tiers() {
    let mut catalog = Catalog::bundled();
    let mut good = payload(&["gpt-6-astra"], json!([]));
    good["data"]["modelTiers"] = json!([{"modelIds":["gpt-6-astra"],"tier":"pro"}]);
    catalog.ingest_at(Realm::Intl, good, 100.0).unwrap();
    let before = catalog.list_at(Realm::Intl, 123.0);
    assert!(catalog
        .ingest(Realm::Intl, json!({"data":{"agents":[], "modelTiers":[]}}))
        .is_err());
    assert_eq!(catalog.list_at(Realm::Intl, 123.0), before);
    let response = InResponse {
        status: 401,
        body: b"{}".to_vec(),
        ..Default::default()
    };
    assert_eq!(
        catalog.ingest_response(Realm::Intl, &response).unwrap_err(),
        CatalogError::HttpStatus(401)
    );
    let response = InResponse {
        status: 200,
        body: b"not json".to_vec(),
        ..Default::default()
    };
    assert_eq!(
        catalog.ingest_response(Realm::Intl, &response).unwrap_err(),
        CatalogError::InvalidJson
    );
    assert_eq!(catalog.list_at(Realm::Intl, 123.0), before);
}

#[test]
fn tiers_free_disabled_priority_exclusive_and_entitlement() {
    let mut catalog = Catalog::bundled();
    let mut doc = payload(&["gpt-6-astra", "hy3"], json!([]));
    doc["data"]["modelTiers"] = json!([
        {"modelIds":["gpt-6-astra"], "requiredUserType":"PERSONAL_PRO_USER"},
        {"modelIds":["free"], "tier":"free"},
        {"modelIds":["free2"], "tier":"personal_free_user"},
        {"modelIds":["free3"], "tier":"personal-free-user"},
        {"modelIds":["disabled"], "tier":"pro", "enabled":false},
        {"modelIds":["hy3"], "tier":"standard", "badge":{"label":"订阅优先"}},
        {"modelIds":["priority"], "tier":"pro", "badge":{"labelEn":"Subscription priority"}},
        {"modelIds":["priority2"], "tier":"pro", "badge":" PRIORITY "},
        {"modelIds":["exclusive"], "tier":"unknown-paid-type", "accessMode":" Exclusive ", "badge":"priority"},
        {"modelIds":["no-tier"], "badge":"PRO"},
        {"modelIds":"wrong-shape", "tier":"pro"},
        null,
        {"modelIds":["override"], "tier":"pro"},
        {"modelIds":["override"], "tier":"free"}
    ]);
    catalog.ingest_at(Realm::Intl, doc, 222.5).unwrap();
    assert!(catalog.requires_pro(Realm::Intl, "gpt-6-astra"));
    assert!(catalog.requires_pro(Realm::Intl, " exclusive "));
    assert!(!catalog.requires_pro(Realm::Cn, "gpt-6-astra"));
    for id in [
        "free",
        "free2",
        "free3",
        "disabled",
        "hy3",
        "priority",
        "priority2",
        "no-tier",
        "unknown",
        "override",
    ] {
        assert!(!catalog.requires_pro(Realm::Intl, id), "{id}");
    }
    assert_eq!(catalog.tiers(Realm::Intl)["free3"], "free");
    assert_eq!(catalog.tiers_ingested_at(Realm::Intl), Some(222.5));
    let list = catalog.list_at(Realm::Intl, 1.0);
    let gpt = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "gpt-6-astra")
        .unwrap();
    assert_eq!(
        gpt["entitlement"],
        json!({"source":"catalog", "required":"standard", "at":222.5})
    );
}

#[test]
fn missing_tiers_preserves_map_explicit_empty_clears() {
    let mut catalog = Catalog::bundled();
    assert!(catalog.ingest_tiers(
        Realm::Cn,
        &json!({"modelTiers":[{"modelIds":["x"],"tier":"standard"}]})
    ));
    let at = catalog.tiers_ingested_at(Realm::Cn);
    for doc in [
        json!({}),
        json!({"modelTiers":null}),
        json!({"modelTiers":{}}),
    ] {
        assert!(!catalog.ingest_tiers(Realm::Cn, &doc));
        assert!(catalog.requires_pro(Realm::Cn, "x"));
        assert_eq!(catalog.tiers_ingested_at(Realm::Cn), at);
    }
    catalog
        .ingest(Realm::Cn, payload(&["hy3"], json!([])))
        .unwrap();
    assert!(catalog.requires_pro(Realm::Cn, "x"));
    assert!(catalog.ingest_tiers(Realm::Cn, &json!({"data":{"modelTiers":[]}})));
    assert!(!catalog.requires_pro(Realm::Cn, "x"));
}

#[test]
fn reasoning_merge_keeps_fixed_models_fixed() {
    assert_eq!(
        merge_reasoning(&json!({}), &json!({"effort":"medium"})),
        json!({"effort":"medium"})
    );
    let catalog = Catalog::bundled();
    assert_eq!(
        model_entry(
            "gemini-3.5-flash",
            &catalog.meta(Realm::Intl, "gemini-3.5-flash")
        )["reasoning_fixed_effort"],
        "medium"
    );
    assert_eq!(
        merge_reasoning(
            &json!({"supportedEfforts":["low","high"]}),
            &json!({"effort":"high"})
        ),
        json!({"supportedEfforts":["low","high"],"defaultEffort":"high"})
    );
}

#[test]
fn config_request_wire_contract() {
    let mut cred = Cred {
        uid: "u-123".into(),
        access_token: "ck_test.secret".into(),
        refresh_token: "".into(),
        realm: Realm::Cn,
        product: Product::Cli,
        enterprise_id: "e-1".into(),
        domain: "wrong".into(),
        platform: "CLI".into(),
    };
    let requests = config_requests(
        &cred,
        Egress::Proxy("http://proxy:8080".into()),
        Some("https://cn.invalid/base/"),
    );
    assert_eq!(requests.len(), 2);
    let request = &requests[0];
    assert_eq!(request.method, "GET");
    assert_eq!(request.url, "https://cn.invalid/base/v3/config");
    assert_eq!(request.case, HeaderCase::Title);
    assert_eq!(request.timeout_secs, 10.0);
    assert_eq!(request.body, None);
    assert_eq!(request.egress, Egress::Proxy("http://proxy:8080".into()));
    assert_eq!(
        request.headers,
        [
            ("Accept", "application/json, text/plain, */*"),
            ("User-Agent", "WorkBuddy/5.8.3 WorkBuddy/5.8.3 CLI/2.161.0"),
            ("Origin", "https://www.codebuddy.cn"),
            ("Referer", "https://www.codebuddy.cn/"),
            ("Authorization", "Bearer ck_test.secret"),
            ("X-User-Id", "u-123"),
            ("X-API-Key", "ck_test.secret"),
        ]
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
    );
    assert_eq!(requests[1].header("User-Agent"), Some("WorkBuddy/5.8.3"));
    assert_eq!(
        config_requests(&cred, Egress::Direct, Some("bad"))[0].url,
        "https://copilot.tencent.com/v3/config"
    );
    cred.realm = Realm::Intl;
    cred.access_token = "oauth-token".into();
    let request = &config_requests(&cred, Egress::Direct, Some("https://ignored.invalid"))[0];
    assert_eq!(request.url, "https://www.workbuddy.ai/v3/config");
    assert_eq!(
        request.header("User-Agent"),
        Some("WorkBuddy/5.8.4 workbuddy-ai/5.8.4 CLI/2.161.0")
    );
    assert!(request.header("X-API-Key").is_none());
}

#[test]
fn catalogue_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Catalog>();
}
