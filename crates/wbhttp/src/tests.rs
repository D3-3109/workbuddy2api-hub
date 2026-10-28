use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{
    net::SocketAddr,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
    task::JoinHandle,
};
use wbcore::types::{Egress, HeaderCase};

fn request(url: String) -> OutRequest {
    OutRequest {
        method: "POST".into(),
        url,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("X-Request-ID".into(), "abc".into()),
            ("Authorization".into(), "Bearer secret-one".into()),
        ],
        body: Some(b"{}".to_vec()),
        case: HeaderCase::Exact,
        timeout_secs: 2.0,
        egress: Egress::Direct,
    }
}

async fn read_request<S: AsyncRead + Unpin>(socket: &mut S) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0u8];
        if socket.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        bytes.push(byte[0]);
        assert!(bytes.len() < 128 * 1024);
    }
    let headers = String::from_utf8_lossy(&bytes);
    let length = headers
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .and_then(|length| length.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.ok()?;
    bytes.extend(body);
    Some(bytes)
}

struct Fake {
    addr: SocketAddr,
    events: mpsc::UnboundedReceiver<(usize, Vec<u8>)>,
    task: JoinHandle<()>,
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fake {
    async fn new(response: &'static [u8]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (send, events) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut index = 0;
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                index += 1;
                let send = send.clone();
                tokio::spawn(async move {
                    while let Some(bytes) = read_request(&mut socket).await {
                        let _ = send.send((index, bytes));
                        if socket.write_all(response).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        Self { addr, events, task }
    }
    fn url(&self) -> String {
        format!("http://{}/test?q=one", self.addr)
    }
    async fn event(&mut self) -> (usize, Vec<u8>) {
        timeout(Duration::from_secs(3), self.events.recv())
            .await
            .unwrap()
            .unwrap()
    }
}
const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Test: value\r\n\r\nok";

#[tokio::test]
async fn exact_and_title_wire_and_keepalive() {
    let mut server = Fake::new(OK).await;
    let client = Client::new().unwrap();
    let req = request(server.url());
    let response = client.execute(req.clone()).await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.header("x-test"), Some("value"));
    assert_eq!(response.collect(10).await.unwrap(), "ok");
    let (first_id, wire) = server.event().await;
    assert_eq!(wire, format!("POST /test?q=one HTTP/1.1\r\nAccept-Encoding: identity\r\nContent-Length: 2\r\nHost: {}\r\nContent-Type: application/json\r\nX-Request-ID: abc\r\nAuthorization: Bearer secret-one\r\nConnection: keep-alive\r\n\r\n{{}}", server.addr).as_bytes());
    let mut title = req;
    title.case = HeaderCase::Title;
    title
        .headers
        .push(("monitor_httpSendTime".into(), "123".into()));
    title.headers.push(("X-B3-TraceId".into(), "trace".into()));
    let response = client
        .clone()
        .execute(title)
        .await
        .unwrap()
        .into_in_response(10)
        .await
        .unwrap();
    assert_eq!(response.body, b"ok");
    let (second_id, wire) = server.event().await;
    assert_eq!(first_id, second_id);
    let wire = String::from_utf8(wire).unwrap();
    assert!(wire.contains("X-Request-Id: abc\r\n"));
    assert!(wire.contains("Monitor_Httpsendtime: 123\r\n"));
    assert!(wire.contains("X-B3-Traceid: trace\r\n"));
}

#[tokio::test]
async fn account_refresh_cookie_and_egress_pool_isolation() {
    let mut server = Fake::new(OK).await;
    let mut proxy = Fake::new(OK).await;
    let client = Client::new().unwrap();
    let base = request(server.url());
    client
        .execute(base.clone())
        .await
        .unwrap()
        .collect(10)
        .await
        .unwrap();
    let (one, _) = server.event().await;
    let mut other = base.clone();
    other.headers[2].1 = "Bearer secret-two".into();
    client
        .execute(other)
        .await
        .unwrap()
        .collect(10)
        .await
        .unwrap();
    assert_ne!(one, server.event().await.0);
    let mut refresh = base.clone();
    refresh.headers[2] = ("X-Refresh-Token".into(), "refresh-secret".into());
    client
        .execute(refresh)
        .await
        .unwrap()
        .collect(10)
        .await
        .unwrap();
    assert_ne!(one, server.event().await.0);
    let mut cookie = base.clone();
    cookie
        .headers
        .push(("Cookie".into(), "session=private".into()));
    client
        .execute(cookie)
        .await
        .unwrap()
        .collect(10)
        .await
        .unwrap();
    assert_ne!(one, server.event().await.0);
    let mut proxied = base.clone();
    proxied.egress = Egress::Proxy(format!("http://u:p@{}", proxy.addr));
    client
        .execute(proxied.clone())
        .await
        .unwrap()
        .collect(10)
        .await
        .unwrap();
    let (first_proxy, wire) = proxy.event().await;
    let wire = String::from_utf8(wire).unwrap();
    assert!(wire.starts_with(&format!("POST {} HTTP/1.1", server.url())));
    assert!(wire.contains("Proxy-Authorization: Basic dTpw\r\n"));
    proxied.egress = Egress::Proxy(format!("http://u:other@{}", proxy.addr));
    client
        .execute(proxied)
        .await
        .unwrap()
        .collect(10)
        .await
        .unwrap();
    assert_ne!(first_proxy, proxy.event().await.0);
    client
        .execute(base)
        .await
        .unwrap()
        .collect(10)
        .await
        .unwrap();
    assert_eq!(one, server.event().await.0);
}

#[tokio::test]
async fn chunked_extensions_trailers_interim_and_reuse() {
    let mut server = Fake::new(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3;foo=bar\r\nabc\r\n2\r\nde\r\n0\r\nX-Checksum: yes\r\n\r\n").await;
    let client = Client::new().unwrap();
    for _ in 0..2 {
        let response = client.execute(request(server.url())).await.unwrap();
        assert_eq!(response.collect(5).await.unwrap(), "abcde");
    }
    assert_eq!(server.event().await.0, server.event().await.0);
}

#[tokio::test]
async fn partial_drop_closes_connection() {
    let mut server = Fake::new(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\npart").await;
    let client = Client::new().unwrap();
    let mut first = client.execute(request(server.url())).await.unwrap();
    assert_eq!(first.next_chunk().await.unwrap().unwrap(), "part");
    let (one, _) = server.event().await;
    drop(first);
    let second = client.execute(request(server.url())).await.unwrap();
    assert_ne!(one, server.event().await.0);
    drop(second);
}

async fn once(bytes: &'static [u8]) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await.unwrap();
        socket.write_all(bytes).await.unwrap();
        socket.shutdown().await.unwrap();
    });
    (url, handle)
}

#[tokio::test]
async fn detects_truncated_content_length_chunks_and_headers() {
    let client = Client::new().unwrap();
    for bytes in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nshort".as_slice(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nshort",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nX-End: yes\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n",
    ] {
        let (url, server) = once(bytes).await;
        let result = match client.execute(request(url)).await {
            Ok(response) => response.collect(1024).await.map(|_| ()),
            Err(error) => Err(error),
        };
        let error = result.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Truncated);
        assert!(!error.safe_to_retry());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn close_delimited_and_http_error_bodies_are_returned() {
    let client = Client::new().unwrap();
    let (url, server) = once(b"HTTP/1.0 403 Forbidden\r\n\r\ndenied").await;
    let response = client.execute(request(url)).await.unwrap();
    assert_eq!(response.status, 403);
    assert_eq!(response.collect(10).await.unwrap(), "denied");
    server.await.unwrap();
}

#[tokio::test]
async fn explicit_close_and_limit_do_not_reuse_incomplete_socket() {
    let client = Client::new().unwrap();
    let mut server =
        Fake::new(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
    for _ in 0..2 {
        client
            .execute(request(server.url()))
            .await
            .unwrap()
            .collect(2)
            .await
            .unwrap();
    }
    assert_ne!(server.event().await.0, server.event().await.0);
    let mut large = Fake::new(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\ntoo large").await;
    assert_eq!(
        client
            .execute(request(large.url()))
            .await
            .unwrap()
            .collect(2)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::BodyTooLarge
    );
    drop(client.execute(request(large.url())).await.unwrap());
    assert_ne!(large.event().await.0, large.event().await.0);
}

#[tokio::test]
async fn header_and_body_timeouts_are_distinct_and_never_replayed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
    });
    let client = Client::new().unwrap();
    let mut req = request(url);
    req.timeout_secs = 0.05;
    let error = client.execute(req).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert!(!error.safe_to_retry());
    server.await.unwrap();
    let fake = Fake::new(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n").await;
    let mut response = client.execute(request(fake.url())).await.unwrap();
    response
        .set_read_timeout(Duration::from_millis(30))
        .unwrap();
    let error = response.next_chunk().await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert!(!error.safe_to_retry());
    assert_eq!(
        response.next_chunk().await.unwrap_err().kind,
        ErrorKind::Cancelled
    );
}

#[tokio::test]
async fn malformed_framing_is_rejected_without_secret_leaks() {
    let client = Client::new().unwrap();
    for bytes in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\nabc".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzzsecret\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nokXX",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nContent-Length: 5\r\n\r\n",
    ] {
        let (url, server) = once(bytes).await;
        let error = match client.execute(request(url)).await {
            Ok(response) => response.collect(1024).await.unwrap_err(),
            Err(error) => error,
        };
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(!format!("{error:?} {error}").contains("secret"));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cancelled_body_read_discards_socket() {
    let client = Client::new().unwrap();
    let mut server = Fake::new(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").await;
    let mut response = client.execute(request(server.url())).await.unwrap();
    assert!(timeout(Duration::from_millis(20), response.next_chunk())
        .await
        .is_err());
    assert_eq!(
        response.next_chunk().await.unwrap_err().kind,
        ErrorKind::Cancelled
    );
    drop(client.execute(request(server.url())).await.unwrap());
    assert_ne!(server.event().await.0, server.event().await.0);
}

#[tokio::test]
async fn response_drop_after_post_is_not_replayed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        assert!(read_request(&mut socket).await.unwrap().ends_with(b"{}"));
        seen.fetch_add(1, Ordering::SeqCst);
        drop(socket);
        assert!(timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err());
    });
    let error = Client::new()
        .unwrap()
        .execute(request(url))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Truncated);
    assert!(!error.safe_to_retry());
    server.await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn request_validation_and_debug_are_secret_free() {
    let client = Client::new().unwrap();
    assert!(!format!("{client:?}").contains("secret"));
    for (name, value) in [
        ("Bad\r\nInjected", "secret"),
        ("Authorization", "secret\r\nInjected: yes"),
        ("Authorization", "non-latin-秘密"),
        ("Content-Length", "999"),
        ("Transfer-Encoding", "chunked"),
        ("Host", "secret.invalid"),
        ("Proxy-Authorization", "secret"),
    ] {
        let mut req = request("http://127.0.0.1:1/secret".into());
        req.headers.push((name.into(), value.into()));
        let error = client.execute(req).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidRequest);
        assert!(!format!("{error:?} {error}").contains("secret"));
        assert!(!error.safe_to_retry());
    }
    let mut socks = request("https://127.0.0.1/".into());
    socks.egress = Egress::Proxy("socks5://secret:secret@localhost:1080".into());
    let error = client.execute(socks).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::UnsupportedProxy);
    assert!(!format!("{error:?}").contains("secret"));
    for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut req = request("http://127.0.0.1:1/".into());
        req.timeout_secs = invalid;
        assert_eq!(
            client.execute(req).await.unwrap_err().kind,
            ErrorKind::InvalidRequest
        );
    }
    let (url, server) =
        once(b"HTTP/1.1 200 OK\r\nSet-Cookie: session=secret\r\nContent-Length: 0\r\n\r\n").await;
    let response = client.execute(request(url)).await.unwrap();
    assert!(!format!("{response:?}").contains("secret"));
    server.await.unwrap();
}

#[tokio::test]
async fn head_and_no_content_do_not_wait_for_body() {
    let client = Client::new().unwrap();
    let head = Fake::new(b"HTTP/1.1 200 OK\r\nContent-Length: 999\r\n\r\n").await;
    let mut req = request(head.url());
    req.method = "HEAD".into();
    req.body = None;
    assert!(client
        .execute(req)
        .await
        .unwrap()
        .collect(0)
        .await
        .unwrap()
        .is_empty());
    let empty = Fake::new(b"HTTP/1.1 204 No Content\r\n\r\n").await;
    assert!(client
        .execute(request(empty.url()))
        .await
        .unwrap()
        .collect(0)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn pinned_body_future_survives_heartbeat_ticks() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(70)).await;
        socket.write_all(b"ok").await.unwrap();
    });
    let client = Client::new().unwrap();
    let mut response = client.execute(request(url)).await.unwrap();
    let mut ticks = 0;
    {
        let read = response.next_chunk();
        tokio::pin!(read);
        loop {
            tokio::select! {
                result = &mut read => {
                    assert_eq!(result.unwrap().unwrap(), "ok");
                    break;
                },
                _ = tokio::time::sleep(Duration::from_millis(10)) => ticks += 1,
            }
        }
    }
    assert!(ticks >= 1);
    assert!(response.next_chunk().await.unwrap().is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn collect_limit_on_final_chunk_closes_socket() {
    let client = Client::new().unwrap();
    let mut server = Fake::new(OK).await;
    assert_eq!(
        client
            .execute(request(server.url()))
            .await
            .unwrap()
            .collect(1)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::BodyTooLarge
    );
    client
        .execute(request(server.url()))
        .await
        .unwrap()
        .collect(2)
        .await
        .unwrap();
    assert_ne!(server.event().await.0, server.event().await.0);
}

#[tokio::test]
async fn oversized_metadata_and_fragmented_chunks() {
    let client = Client::new().unwrap();
    let mut oversized = b"HTTP/1.1 200 OK\r\nX-Large: ".to_vec();
    oversized.extend(vec![b'x'; 65536]);
    oversized.extend_from_slice(b"\r\n\r\n");
    let mut chunk_line = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1;".to_vec();
    chunk_line.extend(vec![b'x'; 8192]);
    chunk_line.extend_from_slice(b"\r\nx\r\n0\r\n\r\n");
    let mut trailers = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n".to_vec();
    for _ in 0..129 {
        trailers.extend_from_slice(b"X-Meta: x\r\n");
    }
    trailers.extend_from_slice(b"\r\n");
    for bytes in [oversized, chunk_line, trailers] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await.unwrap();
            let _ = socket.write_all(&bytes).await;
        });
        let error = match client.execute(request(url)).await {
            Ok(response) => response.collect(1024).await.unwrap_err(),
            Err(error) => error,
        };
        assert_eq!(error.kind, ErrorKind::Protocol);
        server.await.unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await.unwrap();
        for part in [
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".as_slice(),
            b"2\r",
            b"\no",
            b"k\r",
            b"\n0\r\n",
            b"X-End: yes\r",
            b"\n\r\n",
        ] {
            socket.write_all(part).await.unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });
    assert_eq!(
        client
            .execute(request(url))
            .await
            .unwrap()
            .collect(2)
            .await
            .unwrap(),
        "ok"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn proxy_denial_is_sanitized_and_never_sends_origin_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let connect = String::from_utf8(read_request(&mut socket).await.unwrap()).unwrap();
        assert!(!connect.contains("secret-one"));
        socket.write_all(b"HTTP/1.1 407 private-secret\r\nProxy-Authenticate: Basic realm=private-secret\r\nContent-Length: 0\r\n\r\n").await.unwrap();
        let mut extra = [0u8];
        assert_eq!(
            timeout(Duration::from_secs(2), socket.read(&mut extra))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    });
    let client = Client::new().unwrap();
    let mut req = request("https://localhost/".into());
    req.egress = Egress::Proxy(format!("http://user:private-secret@{proxy}"));
    let error = client.execute(req).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Proxy);
    assert!(!format!("{error} {error:?}").contains("private-secret"));
    server.await.unwrap();
}

#[test]
fn proxy_port_normalization_and_latin1_headers() {
    for (proxy, port) in [
        ("http://user:pass@localhost:80", 80),
        ("http://[::1]:80", 80),
        ("localhost:8080", 8080),
        ("http://localhost", 443),
    ] {
        let mut req = request("https://localhost/".into());
        req.egress = Egress::Proxy(proxy.into());
        let prepared = crate::request::Prepared::new(&req).unwrap();
        assert_eq!(prepared.proxy.unwrap().port, port);
    }
    let mut req = request("http://localhost/".into());
    req.headers.push(("X-Latin".into(), "caf\u{e9}".into()));
    let prepared = crate::request::Prepared::new(&req).unwrap();
    assert!(prepared
        .wire
        .windows(15)
        .any(|w| w == b"X-Latin: caf\xe9\r\n"));
}

// All certificates and network endpoints below are generated locally; no public
// upstream and no global environment variable changes are used by these tests.
struct TlsFixture {
    acceptor: openssl::ssl::SslAcceptor,
    path: std::path::PathBuf,
}
impl Drop for TlsFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
impl TlsFixture {
    fn new() -> Self {
        use openssl::{
            asn1::Asn1Time,
            hash::MessageDigest,
            pkey::PKey,
            rsa::Rsa,
            x509::{
                extension::{BasicConstraints, KeyUsage, SubjectAlternativeName},
                X509NameBuilder, X509,
            },
        };
        let key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_text("CN", "localhost").unwrap();
        let name = name.build();
        let mut cert = X509::builder().unwrap();
        cert.set_version(2).unwrap();
        cert.set_subject_name(&name).unwrap();
        cert.set_issuer_name(&name).unwrap();
        cert.set_pubkey(&key).unwrap();
        cert.set_serial_number(
            &openssl::bn::BigNum::from_u32(1)
                .unwrap()
                .to_asn1_integer()
                .unwrap(),
        )
        .unwrap();
        cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
            .unwrap();
        cert.append_extension(BasicConstraints::new().critical().ca().build().unwrap())
            .unwrap();
        cert.append_extension(
            KeyUsage::new()
                .digital_signature()
                .key_encipherment()
                .key_cert_sign()
                .build()
                .unwrap(),
        )
        .unwrap();
        let san = SubjectAlternativeName::new()
            .dns("localhost")
            .build(&cert.x509v3_context(None, None))
            .unwrap();
        cert.append_extension(san).unwrap();
        cert.sign(&key, MessageDigest::sha256()).unwrap();
        let cert = cert.build();
        let mut acceptor =
            openssl::ssl::SslAcceptor::mozilla_intermediate(openssl::ssl::SslMethod::tls_server())
                .unwrap();
        acceptor.set_private_key(&key).unwrap();
        acceptor.set_certificate(&cert).unwrap();
        acceptor.check_private_key().unwrap();
        acceptor
            .set_min_proto_version(Some(SslVersion::TLS1_2))
            .unwrap();
        static ID: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "wbhttp-ca-{}-{}.pem",
            std::process::id(),
            ID.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(&path, cert.to_pem().unwrap()).unwrap();
        Self {
            acceptor: acceptor.build(),
            path,
        }
    }
}

#[tokio::test]
async fn verified_tls_custom_ca_and_authenticated_connect() {
    let fixture = TlsFixture::new();
    let client = Client::with_ca_bundle(Some(&fixture.path)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let acceptor = fixture.acceptor.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let connect = String::from_utf8(read_request(&mut socket).await.unwrap()).unwrap();
        assert_eq!(connect, format!("CONNECT localhost:443 HTTP/1.1\r\nHost: localhost:443\r\nProxy-Authorization: Basic {}\r\n\r\n", STANDARD.encode(b"user:pa:ss")));
        assert!(!connect.contains("secret-one"));
        socket
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await
            .unwrap();
        let ssl = openssl::ssl::Ssl::new(acceptor.context()).unwrap();
        let mut socket = tokio_openssl::SslStream::new(ssl, socket).unwrap();
        std::pin::Pin::new(&mut socket).accept().await.unwrap();
        assert!(socket.ssl().selected_alpn_protocol().is_none());
        assert_eq!(
            socket.ssl().servername(openssl::ssl::NameType::HOST_NAME),
            Some("localhost")
        );
        assert!(matches!(socket.ssl().version_str(), "TLSv1.2" | "TLSv1.3"));
        let origin = String::from_utf8(read_request(&mut socket).await.unwrap()).unwrap();
        assert!(origin.starts_with("POST /test HTTP/1.1\r\n"));
        assert!(!origin.contains("Proxy-Authorization"));
        assert!(!origin.contains(&STANDARD.encode(b"user:pa:ss")));
        socket.write_all(OK).await.unwrap();
    });
    let mut req = request("https://localhost/test".into());
    req.egress = Egress::Proxy(format!("http://user:pa%3Ass@{proxy_addr}"));
    assert_eq!(
        client
            .execute(req)
            .await
            .unwrap()
            .collect(10)
            .await
            .unwrap(),
        "ok"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn tls_rejects_untrusted_ca_and_wrong_hostname_before_origin_send() {
    let fixture = TlsFixture::new();
    for trust in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = fixture.acceptor.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
            let ssl = openssl::ssl::Ssl::new(acceptor.context()).unwrap();
            let mut socket = tokio_openssl::SslStream::new(ssl, socket).unwrap();
            assert!(std::pin::Pin::new(&mut socket).accept().await.is_err());
        });
        let client =
            Client::with_ca_bundle(if trust { Some(&fixture.path) } else { None }).unwrap();
        // Trusted cert covers localhost, not this IP. The other attempt has the
        // right DNS name but no trust anchor.
        let host = if trust { "127.0.0.1" } else { "localhost" };
        // Tunnel over a numeric loopback address to avoid platform-dependent
        // localhost IPv6-first connect delays in this verification-only test.
        let mut req = request(format!("https://{host}/"));
        req.egress = Egress::Proxy(format!("http://{addr}"));
        let error = client.execute(req).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Tls);
        assert!(!error.safe_to_retry());
        server.await.unwrap();
    }
}
