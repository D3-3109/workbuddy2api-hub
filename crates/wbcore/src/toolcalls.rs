//! Printed tool-call recovery (wb_proxy.py:12426-12758).
//!
//! Positions are byte offsets into the text (Python uses char offsets; the
//! results are identical because every slice boundary is a match boundary).

use crate::pyjson::dumps_u;
use regex::Regex;
use serde_json::{Map, Value};
use std::sync::LazyLock;

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedCall {
    pub id: String,
    pub name: String,
    /// `json.dumps(params, ensure_ascii=False)`; every value is a string.
    pub arguments: String,
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}{}", crate::ids::uuid4_hex())
}

static DSML_CALLS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<[^>]*DSML[^>]*calls>(.*?)</[^>]*DSML[^>]*calls>").unwrap());
static DSML_INVOKE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<[^>]*DSML[^>]*invoke\s+name=["']([^"']+)["']>(.*?)</[^>]*invoke>"#).unwrap()
});
static DSML_PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?s)<[^>]*DSML[^>]*parameter\s+name=["']([^"']+)["'][^>]*>(.*?)</[^>]*parameter>"#,
    )
    .unwrap()
});
static LEAK_PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?si)<\s*(?:\|\s*DSML\s*\|\s*)?(?P<tag>parameter|param|arg|argument)\b(?P<attrs>[^>]*)>(?P<value>.*?)</\s*(?:\|\s*DSML\s*\|\s*)?(?:parameter|param|arg|argument)\s*>",
    )
    .unwrap()
});
const LEAK_TAG_PAT: &str =
    r"<\s*/?\s*(?:\|\s*DSML\s*\|\s*)?(?P<name>[A-Za-z_][A-Za-z0-9_]*)(?P<attrs>[^>]*)>";
static LEAK_TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(LEAK_TAG_PAT).unwrap());
static LEAK_TAG_AT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("^{LEAK_TAG_PAT}")).unwrap());
/// Case-insensitive anchored tag matcher for `could_be_leaked_call`. The Python
/// original matched the lowercased tail against the case-sensitive `DSML`
/// spelling, so DSML openings were never held back on the stream path (spec 03
/// §3.5 bug); the port fixes that.
static LEAK_TAG_AT_CI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("(?i)^{LEAK_TAG_PAT}")).unwrap());
const LEAK_CLOSE_PAT: &str = r"<\s*/\s*(?:\|\s*DSML\s*\|\s*|[\x{ff5c}]+\s*DSML\s*[\x{ff5c}]+\s*)?(?P<name>[A-Za-z_][A-Za-z0-9_]*)[^>]*>";
static LEAK_CLOSE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("(?i){LEAK_CLOSE_PAT}")).unwrap());
static LEAK_CLOSE_AT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("(?i)^{LEAK_CLOSE_PAT}")).unwrap());
static LEAK_NAME_ATTR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"name\s*=\s*["']([^"']*)["']"#).unwrap());
static DSML_BARS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x{ff5c}+\s*DSML\s*\x{ff5c}+").unwrap());

const LEAK_WRAPPER_TAGS: [&str; 4] = ["calls", "tool_calls", "function_calls", "function_call"];
const LEAK_PARAM_TAGS: [&str; 4] = ["parameter", "param", "arg", "argument"];
const CALL_CLOSE_TAGS: [&str; 7] = [
    "calls",
    "tool_calls",
    "function_calls",
    "function_call",
    "invoke",
    "run_code",
    "run_shell",
];
const LEAK_TAG_PREFIXES: [&str; 28] = [
    "<run_code",
    "<run_shell",
    "<invoke",
    "<parameter",
    "<param",
    "<arg",
    "<argument",
    "<calls",
    "<tool_calls",
    "<function_calls",
    "<function_call",
    "<think",
    "<thinking",
    "</run_code",
    "</run_shell",
    "</invoke",
    "</parameter",
    "</param",
    "</arg",
    "</argument",
    "</calls",
    "</tool_calls",
    "</function_calls",
    "</function_call",
    "</think",
    "</thinking",
    "<|dsml|",
    "</|dsml|",
];

/// Fold the full-width `｜｜DSML｜｜` spelling back to `|DSML|`.
pub fn normalize_dsml_bars(text: &str) -> String {
    if !text.contains('\u{ff5c}') {
        return text.to_string();
    }
    DSML_BARS_RE.replace_all(text, "|DSML|").into_owned()
}

fn strip_spans(text: &str, spans: &mut [(usize, usize)]) -> String {
    if spans.is_empty() {
        return text.to_string();
    }
    spans.sort();
    let mut kept = String::with_capacity(text.len());
    let mut last = 0;
    for &(s, e) in spans.iter() {
        if s < last {
            continue;
        }
        kept.push_str(&text[last..s]);
        last = e;
    }
    kept.push_str(&text[last..]);
    kept.trim().to_string()
}

fn params_json(params: &Map<String, Value>) -> String {
    dumps_u(&Value::Object(params.clone()))
}

/// `parse_dsml_tool_calls(text)` -> (calls, clean_text).
pub fn parse_dsml_tool_calls(text: &str) -> (Option<Vec<ParsedCall>>, String) {
    if text.is_empty() || !text.contains("DSML") {
        return (None, text.to_string());
    }
    let Some(m) = DSML_CALLS_RE.captures(text) else {
        return (None, text.to_string());
    };
    let whole = m.get(0).unwrap();
    let block = m.get(1).map(|g| g.as_str()).unwrap_or("");
    let mut calls = vec![];
    for inv in DSML_INVOKE_RE.captures_iter(block) {
        let name = inv.get(1).unwrap().as_str().to_string();
        let mut params = Map::new();
        for p in DSML_PARAM_RE.captures_iter(inv.get(2).unwrap().as_str()) {
            params.insert(p[1].to_string(), Value::String(p[2].trim().to_string()));
        }
        calls.push(ParsedCall {
            id: new_id("call_"),
            name,
            arguments: params_json(&params),
        });
    }
    let clean = format!(
        "{} {}",
        text[..whole.start()].trim(),
        text[whole.end()..].trim()
    )
    .trim()
    .to_string();
    (Some(calls), clean)
}

/// `parse_leaked_tool_calls(text)` -> (calls, clean_text); `None` when nothing
/// was recognised (the text is then returned unchanged).
pub fn parse_leaked_tool_calls(original: &str) -> (Option<Vec<ParsedCall>>, String) {
    if original.is_empty() || !original.contains('<') {
        return (None, original.to_string());
    }
    let text = normalize_dsml_bars(original);
    struct Found {
        start: usize,
        pname: String,
        value: String,
        span: (usize, usize),
    }
    let mut found = vec![];
    for c in LEAK_PARAM_RE.captures_iter(&text) {
        let attrs = c.name("attrs").map(|m| m.as_str()).unwrap_or("");
        let Some(a) = LEAK_NAME_ATTR_RE.captures(attrs) else {
            continue;
        };
        let pname = a.get(1).map(|m| m.as_str()).unwrap_or("");
        if pname.is_empty() {
            continue;
        }
        let whole = c.get(0).unwrap();
        found.push(Found {
            start: whole.start(),
            pname: pname.to_string(),
            value: c
                .name("value")
                .map(|m| m.as_str())
                .unwrap_or("")
                .trim()
                .to_string(),
            span: (whole.start(), whole.end()),
        });
    }
    if found.is_empty() {
        return (None, original.to_string());
    }
    let tags: Vec<regex::Captures> = LEAK_TAG_RE.captures_iter(&text).collect();
    struct Call {
        name: String,
        params: Map<String, Value>,
        open: Option<usize>,
        spans: Vec<(usize, usize)>,
    }
    let mut calls: Vec<Call> = vec![];
    for f in found {
        let mut owner: Option<&regex::Captures> = None;
        for t in &tags {
            let g = t.get(0).unwrap();
            if g.start() >= f.start {
                break;
            }
            if g.as_str().trim_start().starts_with("</") {
                continue;
            }
            let tname = t["name"].to_lowercase();
            if LEAK_WRAPPER_TAGS.contains(&tname.as_str())
                || LEAK_PARAM_TAGS.contains(&tname.as_str())
            {
                continue;
            }
            owner = Some(t);
        }
        let mut name = String::new();
        if let Some(o) = owner {
            let attrs = o.name("attrs").map(|m| m.as_str()).unwrap_or("");
            name = match LEAK_NAME_ATTR_RE.captures(attrs) {
                Some(a) => a[1].trim().to_string(),
                None => o["name"].trim().to_string(),
            };
        }
        if name.is_empty() {
            continue;
        }
        let key = owner.map(|o| o.get(0).unwrap().start());
        if let Some(last) = calls.last_mut().filter(|c| c.open == key) {
            last.params.insert(f.pname, Value::String(f.value));
            last.spans.push(f.span);
        } else {
            let mut params = Map::new();
            params.insert(f.pname, Value::String(f.value));
            calls.push(Call {
                name,
                params,
                open: key,
                spans: vec![f.span],
            });
        }
    }
    if calls.is_empty() {
        return (None, original.to_string());
    }
    let mut regions = vec![];
    for c in &calls {
        let first = c
            .open
            .unwrap_or_else(|| c.spans.iter().map(|s| s.0).min().unwrap());
        let mut last = c.spans.iter().map(|s| s.1).max().unwrap();
        loop {
            let nxt = skip_ws(&text, last);
            match LEAK_TAG_AT.find(&text[nxt..]) {
                Some(m) if m.as_str().trim_start().starts_with("</") => last = nxt + m.end(),
                _ => break,
            }
        }
        regions.push((first, last));
    }
    let clean = strip_spans(&text, &mut regions);
    let mut leftover = vec![];
    for t in LEAK_TAG_RE.captures_iter(&clean) {
        let g = t.get(0).unwrap();
        if normalize_dsml_bars(g.as_str())
            .to_uppercase()
            .contains("DSML")
            || LEAK_WRAPPER_TAGS.contains(&t["name"].to_lowercase().as_str())
        {
            leftover.push((g.start(), g.end()));
        }
    }
    let clean = strip_spans(&clean, &mut leftover);
    let out = calls
        .into_iter()
        .map(|c| ParsedCall {
            id: new_id("call_"),
            name: c.name,
            arguments: params_json(&c.params),
        })
        .collect();
    (Some(out), clean)
}

fn skip_ws(text: &str, mut i: usize) -> usize {
    let b = text.as_bytes();
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

/// Leaked parser first, then the DSML parser.
pub fn parse_printed_calls(text: &str) -> (Option<Vec<ParsedCall>>, String) {
    let (calls, clean) = parse_leaked_tool_calls(text);
    if calls.is_some() {
        return (calls, clean);
    }
    parse_dsml_tool_calls(text)
}

/// `repair_leaked_tool_calls(message)`: only when the message has no structured
/// call. Returns true when the message changed.
pub fn repair_leaked_tool_calls(message: &mut Value) -> bool {
    let Some(m) = message.as_object_mut() else {
        return false;
    };
    if let Some(Value::Array(a)) = m.get("tool_calls") {
        if !a.is_empty() {
            return false;
        }
    }
    let Some(content) = m.get("content").and_then(|c| c.as_str()) else {
        return false;
    };
    if !content.contains('<') {
        return false;
    }
    let (calls, clean) = parse_printed_calls(content);
    let Some(calls) = calls.filter(|c| !c.is_empty()) else {
        return false;
    };
    m.insert("content".into(), Value::String(clean));
    m.insert(
        "tool_calls".into(),
        Value::Array(calls.iter().map(call_to_tool_call).collect()),
    );
    true
}

pub fn call_to_tool_call(c: &ParsedCall) -> Value {
    serde_json::json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.arguments}})
}

/// `could_be_leaked_call(tail)`: true while `tail` (starting at `<`) may still
/// grow into printed markup.
pub fn could_be_leaked_call(tail: &str) -> bool {
    let seg_end = tail.find('>').map(|i| i + 1);
    let seg = &tail[..seg_end.unwrap_or(tail.len())];
    let low = normalize_dsml_bars(seg).to_lowercase();
    if let Some(m) = LEAK_TAG_AT_CI.find(&low) {
        if LEAK_TAG_PREFIXES.iter().any(|p| m.as_str().starts_with(p)) {
            return true;
        }
    }
    if seg_end.is_some() || low.chars().count() > 96 {
        return false;
    }
    LEAK_TAG_PREFIXES
        .iter()
        .any(|p| p.starts_with(low.as_str()) || low.starts_with(p))
}

/// Split after the first complete call-closing tag (and any closing tags that
/// immediately follow it).
pub fn split_at_call_close(text: &str) -> Option<(String, String)> {
    for c in LEAK_CLOSE_RE.captures_iter(text) {
        if !CALL_CLOSE_TAGS.contains(&c["name"].to_lowercase().as_str()) {
            continue;
        }
        let mut end = c.get(0).unwrap().end();
        loop {
            let nxt = skip_ws(text, end);
            match LEAK_CLOSE_AT.find(&text[nxt..]) {
                Some(m) => end = nxt + m.end(),
                None => break,
            }
        }
        return Some((text[..end].to_string(), text[end..].to_string()));
    }
    None
}

fn first_call_candidate(text: &str) -> Option<usize> {
    text.char_indices()
        .find(|(i, ch)| *ch == '<' && could_be_leaked_call(&text[*i..]))
        .map(|(i, _)| i)
}

#[derive(Debug, PartialEq)]
pub enum LeakOut {
    Text(String),
    Calls(Vec<ParsedCall>),
}

/// `LeakedToolCallStreamFilter`
#[derive(Default)]
pub struct LeakFilter {
    buf: String,
}

impl LeakFilter {
    pub const MAX_HOLD: usize = 262_144;

    pub fn new() -> Self {
        Self::default()
    }

    fn release_all(&mut self, out: &mut Vec<LeakOut>) {
        let b = std::mem::take(&mut self.buf);
        if !b.is_empty() {
            out.push(LeakOut::Text(b));
        }
    }

    pub fn feed(&mut self, text: &str) -> Vec<LeakOut> {
        let mut out = vec![];
        if text.is_empty() {
            return out;
        }
        self.buf.push_str(text);
        let Some(idx) = first_call_candidate(&self.buf) else {
            self.release_all(&mut out);
            return out;
        };
        if idx > 0 {
            out.push(LeakOut::Text(self.buf[..idx].to_string()));
            self.buf.drain(..idx);
        }
        if self.buf.len() > Self::MAX_HOLD && self.buf.chars().count() > Self::MAX_HOLD {
            self.release_all(&mut out);
            return out;
        }
        let Some((prefix, rest)) = split_at_call_close(&self.buf) else {
            return out;
        };
        let (calls, clean) = parse_printed_calls(&prefix);
        let Some(calls) = calls.filter(|c| !c.is_empty()) else {
            return out;
        };
        self.buf = rest;
        out.push(LeakOut::Calls(calls));
        if !clean.is_empty() {
            out.push(LeakOut::Text(clean));
        }
        match first_call_candidate(&self.buf) {
            None => self.release_all(&mut out),
            Some(0) => {}
            Some(i) => {
                out.push(LeakOut::Text(self.buf[..i].to_string()));
                self.buf.drain(..i);
            }
        }
        out
    }

    pub fn flush(&mut self) -> Vec<LeakOut> {
        let mut out = vec![];
        if self.buf.is_empty() {
            return out;
        }
        let buf = std::mem::take(&mut self.buf);
        let (calls, clean) = parse_printed_calls(&buf);
        match calls.filter(|c| !c.is_empty()) {
            None => out.push(LeakOut::Text(buf)),
            Some(calls) => {
                if !clean.is_empty() {
                    out.push(LeakOut::Text(clean));
                }
                out.push(LeakOut::Calls(calls));
            }
        }
        out
    }
}

/// `is_truncated_arguments`: a non-empty string that is not valid JSON.
pub fn is_truncated_arguments(raw: Option<&Value>) -> bool {
    let Some(Value::String(s)) = raw else {
        return false;
    };
    let t = s.trim();
    !t.is_empty() && serde_json::from_str::<Value>(t).is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts_and_calls(out: Vec<LeakOut>) -> Vec<String> {
        out.into_iter()
            .map(|o| match o {
                LeakOut::Text(t) => format!("T:{t}"),
                LeakOut::Calls(c) => format!(
                    "C:{}",
                    c.iter()
                        .map(|c| format!("{}{}", c.name, c.arguments))
                        .collect::<Vec<_>>()
                        .join("|")
                ),
            })
            .collect()
    }

    #[test]
    fn stream_hybrid_call() {
        let mut f = LeakFilter::new();
        let mut all = vec![];
        all.extend(texts_and_calls(f.feed("Sure. <run_")));
        all.extend(texts_and_calls(
            f.feed("code><parameter name=\"code\">print(1)</parameter>"),
        ));
        all.extend(texts_and_calls(f.feed("</run_code> done")));
        all.extend(texts_and_calls(f.flush()));
        assert_eq!(
            all,
            vec!["T:Sure. ", "C:run_code{\"code\": \"print(1)\"}", "T: done"]
        );
    }

    #[test]
    fn stream_invoke_one_delta() {
        let mut f = LeakFilter::new();
        let out = texts_and_calls(
            f.feed("x <invoke name=\"a\"><parameter name=\"p\">1</parameter></invoke> y"),
        );
        assert_eq!(out, vec!["T:x ", "C:a{\"p\": \"1\"}", "T: y"]);
    }

    #[test]
    fn stream_think_held_until_end() {
        let mut f = LeakFilter::new();
        assert!(f.feed("<think>abc</think> answer").is_empty());
        assert!(f.feed(" more").is_empty());
        assert_eq!(
            texts_and_calls(f.flush()),
            vec!["T:<think>abc</think> answer more"]
        );
    }

    #[test]
    fn stream_plain_lt() {
        let mut f = LeakFilter::new();
        assert_eq!(
            texts_and_calls(f.feed("a < b and c > d")),
            vec!["T:a < b and c > d"]
        );
        let mut f = LeakFilter::new();
        assert_eq!(
            texts_and_calls(f.feed("use <parameter> tags")),
            vec!["T:use "]
        );
        assert!(f.feed(" ok").is_empty());
        assert_eq!(texts_and_calls(f.flush()), vec!["T:<parameter> tags ok"]);
    }

    #[test]
    fn stream_dsml_is_converted() {
        let mut f = LeakFilter::new();
        let mut all = texts_and_calls(f.feed(
            "Hi <|DSML|calls><|DSML|invoke name=\"run_code\"><|DSML|parameter name=\"code\">x</|DSML|parameter></|DSML|invoke></|DSML|calls> bye",
        ));
        all.extend(texts_and_calls(f.flush()));
        assert_eq!(all, vec!["T:Hi ", "C:run_code{\"code\": \"x\"}", "T: bye"]);
    }

    #[test]
    fn fullwidth_dsml_nonstream() {
        let text = "ok <\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c}calls><\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c}invoke name=\"ls\"><\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c}parameter name=\"p\">.</\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c}parameter></\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c}invoke></\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c}calls>";
        let mut msg = serde_json::json!({"role": "assistant", "content": text});
        assert!(repair_leaked_tool_calls(&mut msg));
        assert_eq!(msg["content"], "ok");
        assert_eq!(msg["tool_calls"][0]["function"]["name"], "ls");
        assert_eq!(
            msg["tool_calls"][0]["function"]["arguments"],
            "{\"p\": \".\"}"
        );
    }

    #[test]
    fn truncated() {
        assert!(is_truncated_arguments(Some(&Value::String(
            "{\"a\":".into()
        ))));
        assert!(!is_truncated_arguments(Some(&Value::String("  ".into()))));
        assert!(!is_truncated_arguments(Some(&Value::String("null".into()))));
    }
}
