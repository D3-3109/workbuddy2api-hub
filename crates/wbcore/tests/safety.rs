use wbcore::sse::{LineSplitter, LineTooLong};
use wbcore::types::{Cred, Egress, HeaderCase, InResponse, OutRequest, Product, Realm};

#[test]
fn debug_redacts_credentials_headers_urls_and_bodies() {
    let cred = Cred {
        uid: "secret-uid".into(),
        access_token: "secret-access".into(),
        refresh_token: "secret-refresh".into(),
        realm: Realm::Intl,
        product: Product::Workbuddy,
        enterprise_id: "secret-enterprise".into(),
        domain: "secret-domain".into(),
        platform: "secret-platform".into(),
    };
    let proxy = Egress::Proxy(
        "https://secret-user:secret-pass@proxy.invalid/secret-path?key=secret-query".into(),
    );
    let headers = vec![
        ("Authorization".into(), "secret-access".into()),
        ("cOoKiE".into(), "secret-cookie".into()),
        ("Set-Cookie".into(), "secret-response-cookie".into()),
        ("X-Unrecognized".into(), "secret-custom-header".into()),
        ("secret-header-name".into(), "secret-header-value".into()),
    ];
    let body = b"secret-body-token-and-prompt".to_vec();
    let mut request = OutRequest {
        method: "POST".into(),
        url: "https://secret-user:secret-pass@example.invalid/secret-path?token=secret-access#secret-fragment".into(),
        headers: headers.clone(),
        body: Some(body.clone()),
        case: HeaderCase::Exact,
        timeout_secs: 30.0,
        egress: proxy.clone(),
    };
    let response = InResponse {
        status: 200,
        headers,
        body: body.clone(),
    };
    for debug in [
        format!("{cred:?}"),
        format!("{cred:#?}"),
        format!("{proxy:?}"),
        format!("{proxy:#?}"),
        format!("{request:?}"),
        format!("{request:#?}"),
        format!("{response:?}"),
        format!("{response:#?}"),
    ] {
        assert!(
            !debug.contains("secret"),
            "Debug leaked sensitive strings: {debug}"
        );
        assert!(!debug.contains("https://"), "Debug leaked a URL: {debug}");
        assert!(
            !debug.contains(&format!("{body:?}")),
            "Debug leaked body bytes: {debug}"
        );
        assert!(
            !debug.contains("115,"),
            "Pretty Debug leaked body bytes: {debug}"
        );
        assert!(
            debug.len() < 600,
            "Debug should not scale with sensitive payloads"
        );
    }
    assert!(format!("{cred:?}").contains("[REDACTED]"));
    assert!(format!("{request:?}").contains("POST"));
    assert!(format!("{response:?}").contains("status: 200"));
    assert_eq!(format!("{:?}", Egress::Direct), "Direct");
    request.method = "secret-custom-method".into();
    assert!(!format!("{request:#?}").contains("secret"));
    // Debug formatting must not mutate the public values or accessors.
    assert_eq!(request.header("authorization"), Some("secret-access"));
    assert_eq!(response.text(), String::from_utf8(body).unwrap());
    assert_eq!(cred.clone().refresh_token, "secret-refresh");
    assert_eq!(request.egress, proxy);
}

#[test]
fn bounded_splitter_is_transactional_and_counts_raw_bytes() {
    let mut splitter = LineSplitter::new();
    let mut out = vec!["existing".to_owned()];
    splitter.push_bounded(b"ab", &mut out, 3).unwrap();
    assert_eq!(
        splitter.push_bounded(b"c\ntoolong\n", &mut out, 3),
        Err(LineTooLong { max_line_bytes: 3 })
    );
    assert_eq!(out, ["existing"]);
    splitter.push_bounded(b"c\n", &mut out, 3).unwrap();
    assert_eq!(out, ["existing", "abc"]);
    assert_eq!(splitter.finish(), None);

    splitter.push_bounded(b"ab\r", &mut out, 3).unwrap();
    splitter.push_bounded(b"\n", &mut out, 3).unwrap();
    assert_eq!(out.last().unwrap(), "ab");
    assert!(splitter.push_bounded(b"abc\r\n", &mut out, 3).is_err());
    assert!(splitter.push_bounded("éé".as_bytes(), &mut out, 3).is_err());
    splitter.push_bounded("é".as_bytes(), &mut out, 2).unwrap();
    assert_eq!(splitter.finish().as_deref(), Some("é"));
}

#[test]
fn bounded_splitter_zero_large_chunks_and_limit_changes() {
    let mut splitter = LineSplitter::default();
    let mut out = vec![];
    splitter.push_bounded(b"\n\n", &mut out, 0).unwrap();
    assert_eq!(out, ["", ""]);
    assert!(splitter.push_bounded(b"\r\n", &mut out, 0).is_err());
    assert!(splitter
        .push_bounded(&vec![b'x'; 1024 * 1024], &mut out, 16)
        .is_err());
    assert_eq!(splitter.finish(), None);
    splitter.push_bounded(b"abc", &mut out, 3).unwrap();
    assert!(splitter.push_bounded(b"\n", &mut out, 2).is_err());
    assert_eq!(splitter.finish().as_deref(), Some("abc"));
    splitter.push(b"legacy", &mut out);
    assert!(splitter.push_bounded(b"", &mut out, 3).is_err());
    assert_eq!(splitter.finish().as_deref(), Some("legacy"));
    let chunk = b"a\n".repeat(10_000);
    out.clear();
    splitter.push_bounded(&chunk, &mut out, 1).unwrap();
    assert_eq!(out.len(), 10_000);
    assert!(out.iter().all(|line| line == "a"));
    assert_eq!(splitter.finish(), None);
}

#[test]
fn all_chunk_boundaries_preserve_legacy_line_and_utf8_semantics() {
    let stream = "data: 你好\r\n\nlast\r".as_bytes();
    for split in 0..=stream.len() {
        let mut legacy = LineSplitter::new();
        let mut bounded = LineSplitter::new();
        let (mut left, mut right) = (vec![], vec![]);
        for chunk in [&stream[..split], &stream[split..]] {
            legacy.push(chunk, &mut left);
            bounded.push_bounded(chunk, &mut right, 13).unwrap();
        }
        assert_eq!(left, ["data: 你好", ""]);
        assert_eq!(left, right);
        // Existing finish preserves a CR in an unterminated final line.
        assert_eq!(legacy.finish().as_deref(), Some("last\r"));
        assert_eq!(bounded.finish().as_deref(), Some("last\r"));
        assert_eq!(bounded.finish(), None);
    }
    let mut splitter = LineSplitter::new();
    let mut out = vec![];
    for byte in b"\xff\r\n" {
        splitter.push_bounded(&[*byte], &mut out, 2).unwrap();
    }
    assert_eq!(out, ["\u{fffd}"]);
}
