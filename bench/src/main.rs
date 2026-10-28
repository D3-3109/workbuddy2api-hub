//! Offline gateway load test. Never contacts a real upstream.
//!
//!   cargo run --release --manifest-path bench/Cargo.toml -- \
//!       --gateway target/release/wb2api --accounts 20 --conc 64 --secs 10
//!
//! Starts a keep-alive fake SSE upstream on loopback, writes synthetic accounts
//! into a temporary data dir, launches the gateway with --test-upstream and
//! drives `conc` keep-alive clients against /v1/chat/completions.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use std::{
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const KEY: &str = "bench-api-key";
const PASSWORD: &str = "bench-panel-password";

struct Opts {
    gateway: PathBuf,
    accounts: usize,
    conc: usize,
    secs: u64,
    frames: usize,
    stream: bool,
    usage_rows: usize,
    poll_usage_ms: u64,
}

fn opts() -> Opts {
    let mut o = Opts {
        gateway: PathBuf::new(),
        accounts: 20,
        conc: 64,
        secs: 10,
        frames: 40,
        stream: true,
        usage_rows: 0,
        poll_usage_ms: 0,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--gateway" => o.gateway = v.into(),
            "--accounts" => o.accounts = v.parse().unwrap(),
            "--conc" => o.conc = v.parse().unwrap(),
            "--secs" => o.secs = v.parse().unwrap(),
            "--frames" => o.frames = v.parse().unwrap(),
            "--nonstream" => {
                o.stream = false;
                i += 1;
                continue;
            }
            "--usage-rows" => o.usage_rows = v.parse().unwrap(),
            "--poll-usage-ms" => o.poll_usage_ms = v.parse().unwrap(),
            other => panic!("unknown argument {other}"),
        }
        i += 2;
    }
    assert!(o.gateway.is_file(), "--gateway must name the wb2api binary");
    o
}

fn jwt(uid: &str) -> String {
    let part = |v: serde_json::Value| URL_SAFE_NO_PAD.encode(v.to_string());
    format!(
        "{}.{}.bench-signature",
        part(serde_json::json!({"alg":"HS256","typ":"JWT"})),
        part(serde_json::json!({"sub":uid,"exp":4102444800u64}))
    )
}

fn sse_body(frames: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunk = |data: String| {
        let line = format!("data: {data}\n\n");
        out.extend_from_slice(format!("{:x}\r\n", line.len()).as_bytes());
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\r\n");
    };
    let frame = |delta: serde_json::Value, finish: serde_json::Value| {
        serde_json::json!({"id":"chatcmpl-bench","object":"chat.completion.chunk","created":1700000000,
            "model":"deepseek-v4.1-flash","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
        .to_string()
    };
    chunk(frame(serde_json::json!({"role":"assistant"}), serde_json::Value::Null));
    for i in 0..frames {
        chunk(frame(
            serde_json::json!({"content":format!("token-{i} ")}),
            serde_json::Value::Null,
        ));
    }
    chunk(frame(serde_json::json!({}), serde_json::json!("stop")));
    chunk(
        serde_json::json!({"id":"chatcmpl-bench","choices":[],"usage":{"prompt_tokens":11,"completion_tokens":frames,"total_tokens":11+frames,"credit":0.01}})
            .to_string(),
    );
    chunk("[DONE]".into());
    out.extend_from_slice(b"0\r\n\r\n");
    out
}

/// Reads one request (head + Content-Length body). Ok(false) on clean EOF.
async fn read_request(sock: &mut TcpStream, buf: &mut Vec<u8>) -> std::io::Result<bool> {
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4) {
            let mut headers = [httparse::EMPTY_HEADER; 128];
            let mut req = httparse::Request::new(&mut headers);
            req.parse(&buf[..end]).map_err(std::io::Error::other)?;
            let len = req
                .headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case("content-length"))
                .and_then(|h| std::str::from_utf8(h.value).ok()?.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < end + len {
                let mut tmp = [0u8; 65536];
                let n = sock.read(&mut tmp).await?;
                if n == 0 {
                    return Ok(false);
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            buf.drain(..end + len);
            return Ok(true);
        }
        let mut tmp = [0u8; 65536];
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(false);
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

async fn upstream(listener: TcpListener, body: Arc<Vec<u8>>, conns: Arc<AtomicU64>) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else {
            continue;
        };
        conns.fetch_add(1, Ordering::Relaxed);
        let body = body.clone();
        tokio::spawn(async move {
            let _ = sock.set_nodelay(true);
            let mut buf = Vec::new();
            while let Ok(true) = read_request(&mut sock, &mut buf).await {
                let head = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n";
                if sock.write_all(head).await.is_err() || sock.write_all(&body).await.is_err() {
                    return;
                }
            }
        });
    }
}

/// One keep-alive client request; returns false on any protocol or HTTP failure.
async fn call(
    sock: &mut Option<TcpStream>,
    port: u16,
    req: &[u8],
    buf: &mut Vec<u8>,
) -> bool {
    if sock.is_none() {
        match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                *sock = Some(s);
            }
            Err(_) => return false,
        }
    }
    let s = sock.as_mut().unwrap();
    if s.write_all(req).await.is_err() {
        *sock = None;
        return false;
    }
    buf.clear();
    let (status, close, chunked, length, mut at) = loop {
        let mut tmp = [0u8; 65536];
        let n = match s.read(&mut tmp).await {
            Ok(0) | Err(_) => {
                *sock = None;
                return false;
            }
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut resp = httparse::Response::new(&mut headers);
        if let Ok(httparse::Status::Complete(end)) = resp.parse(buf) {
            let get = |name: &str| {
                resp.headers
                    .iter()
                    .find(|h| h.name.eq_ignore_ascii_case(name))
                    .map(|h| String::from_utf8_lossy(h.value).to_ascii_lowercase())
            };
            break (
                resp.code.unwrap_or(0),
                get("connection").is_some_and(|v| v.contains("close")),
                get("transfer-encoding").is_some_and(|v| v.contains("chunked")),
                get("content-length").and_then(|v| v.parse::<usize>().ok()),
                end,
            );
        }
    };
    // Body: either Content-Length or chunked (terminated by 0\r\n\r\n).
    loop {
        let done = if let Some(len) = length {
            buf.len() >= at + len
        } else if chunked {
            let mut done = false;
            loop {
                let Some(eol) = buf[at..].windows(2).position(|w| w == b"\r\n") else {
                    break;
                };
                let size = usize::from_str_radix(
                    std::str::from_utf8(&buf[at..at + eol]).unwrap_or("x").trim(),
                    16,
                );
                let Ok(size) = size else {
                    *sock = None;
                    return false;
                };
                if buf.len() < at + eol + 2 + size + 2 {
                    break;
                }
                at += eol + 2 + size + 2;
                if size == 0 {
                    done = true;
                    break;
                }
            }
            done
        } else {
            false
        };
        if done {
            break;
        }
        let mut tmp = [0u8; 65536];
        match s.read(&mut tmp).await {
            Ok(0) | Err(_) => {
                *sock = None;
                return false;
            }
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
    }
    if close {
        *sock = None;
    }
    status == 200
}

async fn login(port: u16) -> String {
    let body = serde_json::json!({ "password": PASSWORD }).to_string();
    let req = format!("POST /panel/login HTTP/1.1\r\nHost: b\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    let text = String::from_utf8_lossy(&out);
    let json = &text[text.find("\r\n\r\n").unwrap() + 4..];
    serde_json::from_str::<serde_json::Value>(json).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn pct(sorted: &[u32], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p) as usize] as f64 / 1000.0
}

#[tokio::main]
async fn main() {
    let o = opts();
    let root = std::env::temp_dir().join(format!("wb2api-bench-{}", std::process::id()));
    let accounts = root.join("accounts");
    let usage = root.join("usage");
    std::fs::create_dir_all(&accounts).unwrap();
    std::fs::create_dir_all(&usage).unwrap();
    for i in 0..o.accounts {
        let uid = format!("bench-user-{i:03}");
        let doc = serde_json::json!({"uid":uid,"realm":"intl","accessToken":jwt(&uid),
            "refreshToken":"bench-refresh","product":"workbuddy","enabled":true,
            "credits":{"remain":100000.0,"used":0.0}});
        std::fs::write(accounts.join(format!("{uid}.json")), doc.to_string()).unwrap();
    }
    if o.usage_rows > 0 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let mut text = String::new();
        for i in 0..o.usage_rows {
            let at = now - 30.0 * 86400.0 + i as f64 * (30.0 * 86400.0 / o.usage_rows as f64);
            text.push_str(&serde_json::json!({"at":at,"model":"deepseek-v4.1-flash","stream":true,"outcome":"completed",
                "elapsed_ms":900,"ttft_ms":200,"gen_ms":700,"key":"launcher","account":format!("bench-user-{:03}",i%o.accounts.max(1)),
                "realm":"intl","prompt_tokens":11,"completion_tokens":40,"total_tokens":51,"credit":0.01}).to_string());
            text.push('\n');
        }
        std::fs::write(usage.join("usage.jsonl"), text).unwrap();
    }

    let up = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up_port = up.local_addr().unwrap().port();
    let upstream_conns = Arc::new(AtomicU64::new(0));
    tokio::spawn(upstream(up, Arc::new(sse_body(o.frames)), upstream_conns.clone()));

    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut child = tokio::process::Command::new(&o.gateway)
        .args(["--host", "127.0.0.1", "--port", &port.to_string()])
        .arg("--accounts-dir")
        .arg(&accounts)
        .arg("--usage-dir")
        .arg(&usage)
        .args(["--offline", "--test-upstream", &format!("http://127.0.0.1:{up_port}")])
        .env("API_KEY", KEY)
        .env("WB_PANEL_PASSWORD", PASSWORD)
        .env("WB_MAX_CONCURRENT_CHAT", "1024")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let start = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).await.is_err() {
        assert!(start.elapsed() < Duration::from_secs(20), "gateway did not start");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let body = serde_json::json!({"model":"deepseek-v4.1-flash","stream":o.stream,
        "messages":[{"role":"system","content":"You are a benchmark."},{"role":"user","content":"Reply with a short answer about load testing gateways."}]})
    .to_string();
    let req: Arc<Vec<u8>> = Arc::new(
        format!("POST /v1/chat/completions HTTP/1.1\r\nHost: bench\r\nAuthorization: Bearer {KEY}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len())
            .into_bytes(),
    );

    let stop = Arc::new(AtomicBool::new(false));
    let poller = if o.poll_usage_ms > 0 {
        let token = login(port).await;
        let stop = stop.clone();
        Some(tokio::spawn(async move {
            let req = format!("GET /usage?range=month HTTP/1.1\r\nHost: b\r\nX-Panel-Token: {token}\r\n\r\n").into_bytes();
            let (mut sock, mut buf, mut times) = (None, Vec::new(), Vec::new());
            while !stop.load(Ordering::Relaxed) {
                let t = Instant::now();
                call(&mut sock, port, &req, &mut buf).await;
                times.push(t.elapsed().as_micros() as u32);
                tokio::time::sleep(Duration::from_millis(o.poll_usage_ms)).await;
            }
            times
        }))
    } else {
        None
    };

    // Warm up connections and code paths before measuring.
    let deadline = Instant::now() + Duration::from_secs(o.secs);
    let mut tasks = Vec::new();
    for _ in 0..o.conc {
        let req = req.clone();
        tasks.push(tokio::spawn(async move {
            let (mut sock, mut buf) = (None, Vec::with_capacity(1 << 16));
            let (mut lat, mut fail) = (Vec::new(), 0u64);
            while Instant::now() < deadline {
                let t = Instant::now();
                if call(&mut sock, port, &req, &mut buf).await {
                    lat.push(t.elapsed().as_micros() as u32);
                } else {
                    fail += 1;
                }
            }
            (lat, fail)
        }));
    }
    let began = Instant::now();
    let (mut lat, mut fail) = (Vec::new(), 0);
    for t in tasks {
        let (l, f) = t.await.unwrap();
        lat.extend(l);
        fail += f;
    }
    let wall = began.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    let mut usage_times = match poller {
        Some(p) => p.await.unwrap(),
        None => vec![],
    };
    lat.sort_unstable();
    usage_times.sort_unstable();
    let _ = child.kill().await;
    println!(
        "{}",
        serde_json::json!({
            "mode": if o.stream {"stream"} else {"nonstream"}, "accounts": o.accounts, "conc": o.conc,
            "frames": o.frames, "usage_rows": o.usage_rows,
            "ok": lat.len(), "failed": fail, "rps": (lat.len() as f64 / wall * 10.0).round() / 10.0,
            "p50_ms": pct(&lat, 0.5), "p90_ms": pct(&lat, 0.9), "p99_ms": pct(&lat, 0.99), "max_ms": pct(&lat, 1.0),
            "upstream_connections": upstream_conns.load(Ordering::Relaxed),
            "usage_poll": usage_times.len(), "usage_p50_ms": pct(&usage_times, 0.5), "usage_max_ms": pct(&usage_times, 1.0),
        })
    );
    let _ = std::fs::remove_dir_all(&root);
}
