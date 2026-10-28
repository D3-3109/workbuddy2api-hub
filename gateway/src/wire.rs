use bytes::Bytes;
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full, StreamBody};
use hyper::{
    body::{Frame, Incoming},
    header, HeaderMap, Response, StatusCode,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::mpsc;

pub type Body = UnsyncBoxBody<Bytes, Infallible>;
pub type Reply = Response<Body>;
pub type Query = BTreeMap<String, String>;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_headers_have_precedence_and_quotes_are_removed() {
        let mut h = HeaderMap::new();
        h.insert(
            "authorization",
            header::HeaderValue::from_static("bEaReR \"primary-secret\""),
        );
        h.insert(
            "x-api-key",
            header::HeaderValue::from_static("secondary-secret"),
        );
        assert_eq!(
            supplied_key(&h, &query(Some("key=query-secret"))),
            "primary-secret"
        );
        h.remove("authorization");
        assert_eq!(supplied_key(&h, &Query::new()), "secondary-secret");
        h.clear();
        assert_eq!(
            supplied_key(&h, &query(Some("api_key=query-secret"))),
            "query-secret"
        );
    }
    #[test]
    fn panel_cors_is_never_wildcard() {
        let mut panel = json_reply(200, json!({}));
        cors("/v1/usage", &mut panel);
        assert!(!panel
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
        let mut chat = json_reply(200, json!({}));
        cors("/v1/chat/completions", &mut chat);
        assert_eq!(chat.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    }
    #[test]
    fn weak_etag_match_has_no_body_length() {
        let mut h = HeaderMap::new();
        h.insert(
            "if-none-match",
            header::HeaderValue::from_static("W/\"same\""),
        );
        let r = etag(json_reply(200, json!({"ok":true})), &h, "\"same\"");
        assert_eq!(r.status(), 304);
        assert!(!r.headers().contains_key(header::CONTENT_LENGTH));
    }
}

pub fn json_reply(status: u16, value: Value) -> Reply {
    let data = wbcore::pyjson::dumps_u(&value);
    response(status, "application/json; charset=utf-8", data.into_bytes())
}

pub fn response(status: u16, content_type: &str, data: Vec<u8>) -> Reply {
    response_bytes(status, content_type, Bytes::from(data))
}

pub fn response_bytes(status: u16, content_type: &str, data: Bytes) -> Reply {
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, data.len())
        .header(header::SERVER, wbcore::SERVER_VERSION)
        .body(Full::new(data).boxed_unsync())
        .expect("constant response headers")
}

pub fn error(status: u16, message: impl Into<String>, anthropic: bool) -> Reply {
    let kind = match status {
        401 => "authentication_error",
        403 => "permission_error",
        429 => "rate_limit_error",
        400 | 404 | 409 | 413 | 422 | 501 => "invalid_request_error",
        503 => "overloaded_error",
        _ => "server_error",
    };
    let message = message.into();
    let value = if anthropic {
        json!({"type":"error","error":{"type":kind,"message":message}})
    } else {
        json!({"error":{"message":message,"type":kind,"code":status}})
    };
    json_reply(status, value)
}

/// An error after which the connection cannot be reused safely: the request
/// body may be partly unread (oversized or timed out).
pub fn fatal(status: u16, message: impl Into<String>, anthropic: bool) -> Reply {
    let mut reply = error(status, message, anthropic);
    reply.headers_mut().insert(
        header::CONNECTION,
        header::HeaderValue::from_static("close"),
    );
    reply
}

pub fn retry_error(wait: u64, message: &str, anthropic: bool) -> Reply {
    let wait = wait.max(1);
    let mut r = if anthropic {
        error(429, message, true)
    } else {
        json_reply(
            429,
            json!({"error":{"message":message,"type":"rate_limit_error","code":429,"retry_after":wait}}),
        )
    };
    r.headers_mut().insert(
        header::RETRY_AFTER,
        header::HeaderValue::from_str(&wait.to_string()).unwrap(),
    );
    r
}

pub fn cors(path: &str, reply: &mut Reply) {
    if [
        "/v1/usage",
        "/usage",
        "/accounts",
        "/settings",
        "/tasks",
        "/scheduler",
        "/panel",
        "/logs",
    ]
    .iter()
    .any(|p| path.starts_with(p))
    {
        return;
    }
    if ["/v1", "/chat", "/completions", "/models", "/responses"]
        .iter()
        .any(|p| path.starts_with(p))
    {
        reply.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            header::HeaderValue::from_static("*"),
        );
        reply.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            header::HeaderValue::from_static("*"),
        );
        reply.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            header::HeaderValue::from_static("GET, POST, OPTIONS"),
        );
    }
}

pub fn query(raw: Option<&str>) -> Query {
    let mut out = Query::new();
    for (key, val) in url::form_urlencoded::parse(raw.unwrap_or("").as_bytes()) {
        out.entry(key.into_owned()).or_insert(val.into_owned());
    }
    out
}

pub fn header_text<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|x| x.to_str().ok())
}

pub fn supplied_key(h: &HeaderMap, q: &Query) -> String {
    let strip = |s: &str| {
        let s = s.trim();
        let s = if s.len() >= 2
            && ((s.starts_with('"') && s.ends_with('"'))
                || (s.starts_with('\'') && s.ends_with('\'')))
        {
            &s[1..s.len() - 1]
        } else {
            s
        };
        s.to_owned()
    };
    if let Some(a) = header_text(h, "authorization").filter(|s| !s.trim().is_empty()) {
        let a = a.trim();
        return strip(
            a.split_once(' ')
                .filter(|(s, _)| s.eq_ignore_ascii_case("bearer"))
                .map(|(_, v)| v)
                .unwrap_or(a),
        );
    }
    for name in ["x-api-key", "api-key", "x-auth-token"] {
        if let Some(v) = header_text(h, name).filter(|s| !s.trim().is_empty()) {
            return strip(v);
        }
    }
    for name in ["key", "api_key", "api-key"] {
        if let Some(v) = q.get(name).filter(|s| !s.trim().is_empty()) {
            return strip(v);
        }
    }
    String::new()
}

pub async fn read_json(
    body: Incoming,
    headers: &HeaderMap,
    cap: usize,
    allow_list: bool,
) -> Result<Value, (u16, &'static str)> {
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|x| x.to_str().ok())
        .and_then(|x| x.parse::<usize>().ok())
        .is_some_and(|n| n > cap)
    {
        return Err((413, "payload too large"));
    }
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        http_body_util::Limited::new(body, cap).collect(),
    )
    .await
    .map_err(|_| (408, "request body timeout"))?
    .map_err(|e| {
        if e.is::<http_body_util::LengthLimitError>() {
            (413, "payload too large")
        } else {
            (400, "invalid request body")
        }
    })?
    .to_bytes();
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| (400, "invalid JSON body"))?;
    Ok(if v.is_object() || (allow_list && v.is_array()) {
        v
    } else {
        json!({})
    })
}

struct Channel(mpsc::Receiver<Bytes>);
impl futures_util::Stream for Channel {
    type Item = Result<Frame<Bytes>, Infallible>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx).map(|x| x.map(|b| Ok(Frame::data(b))))
    }
}

pub fn sse_reply() -> (mpsc::Sender<Bytes>, Reply) {
    let (tx, rx) = mpsc::channel(8);
    let body = StreamBody::new(Channel(rx)).boxed_unsync();
    let reply = Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(body)
        .expect("constant SSE headers");
    (tx, reply)
}

pub fn etag(mut reply: Reply, request_headers: &HeaderMap, tag: &str) -> Reply {
    let matches = header_text(request_headers, "if-none-match").is_some_and(|v| {
        v.split(',')
            .any(|s| s.trim().trim_start_matches("W/") == tag || s.trim() == "*")
    });
    if matches {
        reply = response(304, "application/json", vec![]);
        reply.headers_mut().remove(header::CONTENT_LENGTH);
    }
    if let Ok(h) = header::HeaderValue::from_str(tag) {
        reply.headers_mut().insert(header::ETAG, h);
    }
    reply.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    reply
}
