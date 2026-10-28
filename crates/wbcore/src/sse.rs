//! Upstream SSE line handling (wb_proxy.py:11505-11557, 15299-15349).

use crate::pyval::{get, truthy};
use serde_json::Value;

/// Extra delta keys the upstream pads with; dropped when present but falsy.
pub const NOISE_KEYS: [&str; 3] = ["extra_fields", "refusal", "reasoning_content"];

/// `strip_data_prefix`: "" for comments / blank lines, else the payload after
/// any number of `data:` prefixes.
pub fn strip_data_prefix(line: &str) -> &str {
    let mut l = line.trim();
    if l.is_empty() || l.starts_with(':') {
        return "";
    }
    while let Some(rest) = l.strip_prefix("data:") {
        l = rest.trim();
    }
    if l.is_empty() || l.starts_with(':') {
        return "";
    }
    l
}

/// Splits a byte stream into `\n`-terminated lines (the terminator and a
/// trailing `\r` are removed). Bytes are decoded as UTF-8 per line with
/// replacement, so a multibyte char split across reads is never corrupted.
/// `push` remains unbounded for compatibility; use `push_bounded` on every push
/// when reading an untrusted stream. Callers must also bound chunk/output sizes.
#[derive(Default)]
pub struct LineSplitter {
    buf: Vec<u8>,
}

/// A raw line exceeded the supplied byte limit (LF excluded, CR included).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineTooLong {
    pub max_line_bytes: usize,
}

impl std::fmt::Display for LineTooLong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SSE line exceeds {} bytes", self.max_line_bytes)
    }
}

impl std::error::Error for LineTooLong {}

impl LineSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, mut chunk: &[u8], out: &mut Vec<String>) {
        // Scan only the new bytes, avoiding quadratic rescans of a long partial
        // line and avoiding a buffer allocation for an entire multi-line chunk.
        while let Some(end) = chunk.iter().position(|b| *b == b'\n') {
            let part = &chunk[..end];
            let line = if self.buf.is_empty() {
                part
            } else {
                self.buf.extend_from_slice(part);
                &self.buf
            };
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            out.push(String::from_utf8_lossy(line).into_owned());
            self.buf.clear();
            chunk = &chunk[end + 1..];
        }
        self.buf.extend_from_slice(chunk);
    }

    /// Reject any complete OR partial raw line longer than `max_line_bytes`.
    /// LF is excluded from the limit; a CR before LF counts as one byte.
    /// A zero limit accepts empty LF lines only. UTF-8 is counted as raw bytes.
    ///
    /// On error neither the buffer nor `out` changes; no part of the chunk is
    /// consumed. Drop/reset the splitter or retry with a larger limit. Checking
    /// happens before allocation, but `out` and the supplied chunk are not
    /// size-limited here. Use this method consistently, not mixed with `push`,
    /// to keep the pending buffer bounded. Allocation capacity can include Vec
    /// growth slack. The retained final line still uses existing `finish` rules.
    pub fn push_bounded(
        &mut self,
        chunk: &[u8],
        out: &mut Vec<String>,
        max_line_bytes: usize,
    ) -> Result<(), LineTooLong> {
        let error = LineTooLong { max_line_bytes };
        let mut pending = self.buf.len();
        if pending > max_line_bytes {
            return Err(error);
        }
        for byte in chunk {
            if *byte == b'\n' {
                pending = 0;
            } else {
                if pending == max_line_bytes {
                    return Err(error);
                }
                pending += 1;
            }
        }
        self.push(chunk, out);
        Ok(())
    }

    /// Whatever is left after the stream ended (a final line without `\n`).
    pub fn finish(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            return None;
        }
        let s = String::from_utf8_lossy(&self.buf).into_owned();
        self.buf.clear();
        Some(s)
    }
}

/// `clean_chunk(raw)`: returns `None` to drop the frame, `Some(text)` to
/// forward it (the original text when nothing changed).
pub fn clean_chunk(raw: &str) -> Option<String> {
    let Ok(mut obj) = serde_json::from_str::<Value>(raw) else {
        return Some(raw.to_string());
    };
    let changed = clean_chunk_value(&mut obj);
    match changed {
        CleanResult::Drop => None,
        CleanResult::Unchanged => Some(raw.to_string()),
        CleanResult::Changed => Some(crate::pyjson::dumps_u(&obj)),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CleanResult {
    Unchanged,
    Changed,
    Drop,
}

/// In-place version of `clean_chunk` on a parsed frame.
pub fn clean_chunk_value(obj: &mut Value) -> CleanResult {
    let mut changed = false;
    if let Some(u) = obj.get_mut("usage") {
        if u.is_object() {
            let before = u.clone();
            crate::usage::normalize_cache_aliases(u);
            if *u != before {
                changed = true;
            }
        }
    }
    let Some(choices) = obj.get_mut("choices").and_then(|c| c.as_array_mut()) else {
        return if changed {
            CleanResult::Changed
        } else {
            CleanResult::Unchanged
        };
    };
    for ch in choices.iter_mut() {
        let Some(chm) = ch.as_object_mut() else {
            continue;
        };
        if chm.get("finish_reason").and_then(|v| v.as_str()) == Some("") {
            chm.insert("finish_reason".into(), Value::Null);
            changed = true;
        }
        let finish_truthy = chm.get("finish_reason").map(truthy).unwrap_or(false);
        let Some(delta) = chm.get_mut("delta").and_then(|d| d.as_object_mut()) else {
            continue;
        };
        if let Some(fc) = delta.get("function_call") {
            let keep = truthy(fc)
                && fc
                    .as_object()
                    .map(|m| m.get("name").map(truthy).unwrap_or(false))
                    .unwrap_or(true);
            if !keep {
                delta.remove("function_call");
                changed = true;
            }
        }
        if let Some(Value::Array(tc)) = delta.get("tool_calls") {
            if tc.is_empty() {
                delta.remove("tool_calls");
                changed = true;
            }
        }
        for k in NOISE_KEYS {
            if let Some(v) = delta.get(k) {
                if !truthy(v) {
                    delta.remove(k);
                    changed = true;
                }
            }
        }
        if delta.is_empty() && !finish_truthy {
            return CleanResult::Drop;
        }
    }
    if changed {
        CleanResult::Changed
    } else {
        CleanResult::Unchanged
    }
}

/// True when a parsed chunk carries an `error` object (in-band upstream error).
pub fn chunk_error(obj: &Value) -> Option<&Value> {
    get(obj, "error").filter(|e| truthy(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes() {
        assert_eq!(strip_data_prefix("data: data: {\"a\":1}"), "{\"a\":1}");
        assert_eq!(strip_data_prefix(": keepalive"), "");
        assert_eq!(strip_data_prefix("data: : heartbeat"), "");
        assert_eq!(strip_data_prefix("event: x"), "event: x");
    }

    #[test]
    fn splitter_handles_split_utf8() {
        let mut s = LineSplitter::new();
        let mut out = vec![];
        let bytes = "data: 你好\r\n\nx".as_bytes();
        s.push(&bytes[..8], &mut out);
        s.push(&bytes[8..], &mut out);
        assert_eq!(out, vec!["data: 你好".to_string(), String::new()]);
        assert_eq!(s.finish().as_deref(), Some("x"));
    }

    #[test]
    fn clean() {
        let raw = r#"{"id":"x","choices":[{"index":0,"delta":{"content":"Hi","reasoning_content":"","refusal":"","extra_fields":null,"function_call":{"name":"","arguments":""},"tool_calls":[]},"finish_reason":""}]}"#;
        assert_eq!(
            clean_chunk(raw).unwrap(),
            r#"{"id": "x", "choices": [{"index": 0, "delta": {"content": "Hi"}, "finish_reason": null}]}"#
        );
        let empty =
            r#"{"choices":[{"index":0,"delta":{"reasoning_content":""},"finish_reason":""}]}"#;
        assert_eq!(clean_chunk(empty), None);
        assert_eq!(clean_chunk("not json").as_deref(), Some("not json"));
        let keep = r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#;
        assert_eq!(clean_chunk(keep).as_deref(), Some(keep));
    }
}
