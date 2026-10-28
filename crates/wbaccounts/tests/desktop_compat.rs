use serde_json::json;
use wbaccounts::{build_billing_headers, build_chat_request, ChatOptions};
use wbcore::types::{Cred, Egress, HeaderCase, OutRequest, Product, Realm};

fn credential(product: Product, realm: Realm) -> Cred {
    Cred {
        uid: "desktop-fixture".into(),
        access_token: "fixture".into(),
        refresh_token: String::new(),
        realm,
        product,
        enterprise_id: String::new(),
        domain: String::new(),
        platform: String::new(),
    }
}

fn options(enabled: bool) -> ChatOptions {
    ChatOptions {
        desktop_chat_compat: enabled,
        conversation_id: Some("fixture-conversation".into()),
        turn_request_id: Some("fixture-turn".into()),
        now: 1_700_000_000.0,
        ..Default::default()
    }
}

// Preserve every header's order and bytes except per-request generated IDs.
fn stable_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(key, value)| {
            let generated = matches!(
                key.as_str(),
                "X-Conversation-Request-ID"
                    | "X-Root-Request-ID"
                    | "X-Conversation-Message-ID"
                    | "X-Request-ID"
                    | "X-Request-Trace-Id"
                    | "X-Trace-ID"
                    | "traceparent"
                    | "b3"
                    | "X-B3-TraceId"
                    | "X-B3-SpanId"
                    | "X-B3-ParentSpanId"
            );
            (
                key.clone(),
                if generated {
                    "<id>".into()
                } else {
                    value.clone()
                },
            )
        })
        .collect()
}

fn header_names(request: &OutRequest) -> Vec<&str> {
    request
        .headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect()
}

#[test]
fn default_and_explicit_off_keep_legacy_header_order_and_body_bytes() {
    assert!(!ChatOptions::default().desktop_chat_compat);
    let expected_names = [
        "Content-Type",
        "Accept",
        "X-Requested-With",
        "Origin",
        "Referer",
        "X-CodeBuddy-Request",
        "Accept-Language",
        "X-Agent-Intent",
        "X-Agent-Purpose",
        "X-Agent-Type",
        "X-IDE-Name",
        "X-IDE-Type",
        "X-IDE-Version",
        "X-Private-Data",
        "X-Product",
        "X-Domain",
        "User-Agent",
        "X-User-Id",
        "Authorization",
        "X-Conversation-ID",
        "X-Conversation-Request-ID",
        "X-Root-Request-ID",
        "X-Conversation-Message-ID",
        "X-Request-ID",
        "X-Trace-ID",
        "traceparent",
        "b3",
        "X-B3-TraceId",
        "X-B3-SpanId",
        "X-B3-Sampled",
    ];
    let body = json!({"model":"fixture-model","messages":[],"stream":true,"n":0.00001});
    for realm in Realm::ALL {
        let cred = credential(Product::Workbuddy, realm);
        let default = build_chat_request(&cred, &body, &ChatOptions::default()).unwrap();
        let off = build_chat_request(&cred, &body, &options(false)).unwrap();
        assert_eq!(header_names(&default), expected_names);
        assert_eq!(header_names(&off), expected_names);
        assert_eq!(
            default.body,
            Some(wbcore::pyjson::dumps_compact(&body).into_bytes())
        );
        assert_eq!(default.body, off.body);
        assert_eq!(
            off.header("Accept"),
            Some("application/json, text/plain, */*")
        );
        assert_eq!(off.header("X-Requested-With"), Some("XMLHttpRequest"));
        assert!(off.header("x-stainless-runtime").is_none());
        assert!(off.header("X-B3-ParentSpanId").is_none());
        assert_eq!(off.header("b3").unwrap().split('-').count(), 3);
    }
}

#[test]
fn both_desktop_realms_opt_in_to_sdk_profile_without_browser_headers() {
    for realm in Realm::ALL {
        let cred = credential(Product::Workbuddy, realm);
        let req = build_chat_request(&cred, &json!({"messages":[]}), &options(true)).unwrap();
        assert_eq!(req.header("Content-Type"), Some("application/json"));
        assert_eq!(req.header("Accept"), Some("application/json"));
        assert_eq!(req.header("Authorization"), Some("Bearer fixture"));
        assert_eq!(req.header("X-User-Id"), Some("desktop-fixture"));
        assert_eq!(req.header("X-IDE-Type"), Some("WorkBuddy"));
        for name in [
            "Origin",
            "Referer",
            "Accept-Language",
            "X-Requested-With",
            "X-CodeBuddy-Request",
            "X-Machine-ID",
            "X-Session-ID",
            "x-stainless-timeout",
            "X-Agent-Runtime",
        ] {
            assert!(
                req.header(name).is_none(),
                "unexpected {name} for {realm:?}"
            );
        }
        for (name, value) in [
            ("x-stainless-lang", "js"),
            ("x-stainless-package-version", "6.25.0"),
            ("x-stainless-runtime", "node"),
            ("x-stainless-runtime-version", "v22.21.1"),
            ("x-stainless-retry-count", "0"),
        ] {
            assert_eq!(req.header(name), Some(value));
        }
        let names = header_names(&req);
        assert_eq!(
            &names[names.len() - 8..],
            &[
                "X-B3-ParentSpanId",
                "x-stainless-lang",
                "x-stainless-package-version",
                "x-stainless-os",
                "x-stainless-arch",
                "x-stainless-runtime",
                "x-stainless-runtime-version",
                "x-stainless-retry-count",
            ]
        );
        assert_eq!(cred.product, Product::Workbuddy);
    }
}

#[test]
fn sdk_metadata_respects_explicit_version_and_host_overrides() {
    let mut opts = options(true);
    opts.versions.sdk = "9.1.0".into();
    opts.versions.node = "v23.0.0".into();
    opts.versions.os = "Linux".into();
    opts.versions.arch = "arm64".into();
    let req = build_chat_request(
        &credential(Product::Workbuddy, Realm::Cn),
        &json!({}),
        &opts,
    )
    .unwrap();
    for (name, value) in [
        ("x-stainless-package-version", "9.1.0"),
        ("x-stainless-runtime-version", "v23.0.0"),
        ("x-stainless-os", "Linux"),
        ("x-stainless-arch", "arm64"),
    ] {
        assert_eq!(req.header(name), Some(value));
    }
}

#[test]
fn b3_parent_trace_span_and_traceparent_agree() {
    for realm in Realm::ALL {
        let req = build_chat_request(
            &credential(Product::Workbuddy, realm),
            &json!({}),
            &options(true),
        )
        .unwrap();
        let pieces: Vec<_> = req.header("b3").unwrap().split('-').collect();
        assert_eq!(pieces.len(), 4);
        let (trace, span, sampled, parent) = (pieces[0], pieces[1], pieces[2], pieces[3]);
        for (value, length) in [(trace, 32), (span, 16), (parent, 16)] {
            assert_eq!(value.len(), length);
            assert!(value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        }
        assert_eq!(sampled, "1");
        assert_eq!(req.header("X-Trace-ID"), Some(trace));
        assert_eq!(req.header("X-B3-TraceId"), Some(trace));
        assert_eq!(req.header("X-B3-SpanId"), Some(span));
        assert_eq!(req.header("X-B3-ParentSpanId"), Some(parent));
        assert_eq!(req.header("X-B3-Sampled"), Some(sampled));
        assert_eq!(
            req.header("traceparent"),
            Some(format!("00-{trace}-{span}-01").as_str())
        );
        assert_eq!(
            req.header("X-Request-ID"),
            req.header("X-Conversation-Message-ID")
        );
        assert_eq!(
            req.header("X-Root-Request-ID"),
            req.header("X-Conversation-Request-ID")
        );
    }
}

#[test]
fn other_products_billing_and_all_request_bodies_are_unchanged() {
    let body = json!({"model":"fixture-model","messages":[],"stream":true,
        "metadata":{"fixture":true},"conversation_id":"body-conversation",
        "previous_response_id":"body-previous","stream_options":{"include_usage":true}});
    let original = body.clone();
    for product in Product::ALL {
        for realm in Realm::ALL {
            let cred = credential(product, realm);
            let before_billing = build_billing_headers(&cred, None).unwrap();
            let before = build_chat_request(&cred, &body, &options(false)).unwrap();
            let after = build_chat_request(&cred, &body, &options(true)).unwrap();
            assert_eq!(before.body, after.body);
            assert_eq!(before.url, after.url);
            assert_eq!(before.method, after.method);
            assert_eq!(before.case, after.case);
            assert_eq!(before.timeout_secs, after.timeout_secs);
            assert_eq!(before.egress, after.egress);
            assert_eq!(after.case, HeaderCase::Exact);
            assert_eq!(after.egress, Egress::Direct);
            assert_eq!(
                before.header("Content-Encoding"),
                after.header("Content-Encoding")
            );
            if product == Product::Workbuddy {
                assert_eq!(
                    after.body,
                    Some(wbcore::pyjson::dumps_compact(&body).into_bytes())
                );
                assert!(after.header("Content-Encoding").is_none());
            } else {
                assert_eq!(
                    stable_headers(&before.headers),
                    stable_headers(&after.headers)
                );
                assert_eq!(after.header("Content-Encoding"), Some("gzip"));
            }
            let after_billing = build_billing_headers(&cred, None).unwrap();
            assert_eq!(
                stable_headers(&before_billing),
                stable_headers(&after_billing)
            );
            assert!(after_billing.iter().any(|(name, _)| name == "Origin"));
            assert!(!after_billing
                .iter()
                .any(|(name, _)| name.starts_with("x-stainless-")));
            assert_eq!(cred.product, product);
        }
    }
    assert_eq!(body, original);
}

#[test]
fn device_token_is_not_invented_and_keeps_cn_desktop_boundary() {
    for realm in Realm::ALL {
        let cred = credential(Product::Workbuddy, realm);
        let absent = build_chat_request(&cred, &json!({}), &options(true)).unwrap();
        assert!(absent.header("X-Device-Token").is_none());
        assert!(absent.header("X-Device-Token-Error").is_none());
        let mut opts = options(true);
        opts.device_token = Some("operator-provided-fixture".into());
        let supplied = build_chat_request(&cred, &json!({}), &opts).unwrap();
        assert_eq!(
            supplied.header("X-Device-Token"),
            if realm == Realm::Cn {
                Some("operator-provided-fixture")
            } else {
                None
            }
        );
        assert!(supplied.header("X-Device-Token-Error").is_none());
    }
}
