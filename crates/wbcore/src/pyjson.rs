//! Python-style `json.dumps` over `serde_json::Value` (built with
//! `preserve_order`, retaining insertion order like a Python dict).
//!
//! Synthetic CPython fixtures cover separators, indentation, key sorting,
//! Unicode escaping and representative numeric boundaries. This is not a
//! general byte-equivalence guarantee: Rust's shortest float digit selection
//! can differ from CPython's at ties. JSON Values also cannot represent Python's
//! arbitrary-size integers, non-finite numbers or lone Unicode surrogates.
//! Callers using bytes for persistent hashes must verify their input domain.
//!
//! Inputs, nesting and custom indentation must be bounded by the caller; these
//! infallible serializers do not impose an output-size or recursion limit.

use serde_json::Value;
use std::fmt::Write as _;

#[derive(Clone, Copy, Debug)]
pub struct Opts {
    pub item_sep: &'static str,
    pub key_sep: &'static str,
    pub ensure_ascii: bool,
    pub sort_keys: bool,
    /// `Some(n)` = `indent=n`; Python then uses "," as the item separator.
    pub indent: Option<usize>,
}

impl Opts {
    /// `json.dumps(v)`
    pub const DEFAULT: Opts = Opts {
        item_sep: ", ",
        key_sep: ": ",
        ensure_ascii: true,
        sort_keys: false,
        indent: None,
    };
    /// `json.dumps(v, ensure_ascii=False)`
    pub const UNICODE: Opts = Opts {
        item_sep: ", ",
        key_sep: ": ",
        ensure_ascii: false,
        sort_keys: false,
        indent: None,
    };
    /// `json.dumps(v, ensure_ascii=False, separators=(",", ":"))`
    pub const COMPACT: Opts = Opts {
        item_sep: ",",
        key_sep: ":",
        ensure_ascii: false,
        sort_keys: false,
        indent: None,
    };
    /// `json.dumps(v, ensure_ascii=False, sort_keys=True)`
    pub const SORTED: Opts = Opts {
        item_sep: ", ",
        key_sep: ": ",
        ensure_ascii: false,
        sort_keys: true,
        indent: None,
    };
    /// `dumps_sorted` of a list given as a slice, without building a `Value::Array`.
    pub fn dumps_sorted_slice(items: &[Value]) -> String {
        let o = Opts::SORTED;
        let mut out = String::with_capacity(128);
        if items.is_empty() {
            out.push_str("[]");
            return out;
        }
        out.push('[');
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                out.push_str(o.item_sep);
            }
            write_value(&mut out, item, &o, 1);
        }
        out.push(']');
        out
    }

    /// `json.dumps(v, ensure_ascii=False, indent=2)`
    pub const PRETTY: Opts = Opts {
        item_sep: ",",
        key_sep: ": ",
        ensure_ascii: false,
        sort_keys: false,
        indent: Some(2),
    };

    pub fn sorted(mut self) -> Opts {
        self.sort_keys = true;
        self
    }
}

pub fn dumps_with(v: &Value, opts: Opts) -> String {
    let mut out = String::with_capacity(128);
    write_value(&mut out, v, &opts, 0);
    out
}

/// `json.dumps(v)` (ASCII-escaped, `", "` / `": "`).
pub fn dumps(v: &Value) -> String {
    dumps_with(v, Opts::DEFAULT)
}

/// `json.dumps(v, ensure_ascii=False)`
pub fn dumps_u(v: &Value) -> String {
    dumps_with(v, Opts::UNICODE)
}

/// `json.dumps(v, ensure_ascii=False, separators=(",", ":"))`
pub fn dumps_compact(v: &Value) -> String {
    dumps_with(v, Opts::COMPACT)
}

/// `json.dumps(v, ensure_ascii=False, sort_keys=True)`
pub fn dumps_sorted(v: &Value) -> String {
    dumps_with(v, Opts::SORTED)
}

/// `dumps_sorted` of a list given as a slice, without building a `Value::Array`.
pub fn dumps_sorted_slice(items: &[Value]) -> String {
    let o = Opts::SORTED;
    let mut out = String::with_capacity(128);
    if items.is_empty() {
        out.push_str("[]");
        return out;
    }
    out.push('[');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(o.item_sep);
        }
        write_value(&mut out, item, &o, 1);
    }
    out.push(']');
    out
}

/// `json.dumps(v, ensure_ascii=False, indent=2)`
pub fn dumps_pretty(v: &Value) -> String {
    dumps_with(v, Opts::PRETTY)
}

fn newline(out: &mut String, indent: usize, level: usize) {
    out.push('\n');
    for _ in 0..indent * level {
        out.push(' ');
    }
}

fn write_value(out: &mut String, v: &Value, o: &Opts, level: usize) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                let _ = write!(out, "{}", i);
            } else if let Some(u) = n.as_u64() {
                let _ = write!(out, "{}", u);
            } else {
                out.push_str(&float_repr(n.as_f64().unwrap_or(0.0)));
            }
        }
        Value::String(s) => write_str(out, s, o.ensure_ascii),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(o.item_sep);
                }
                if let Some(ind) = o.indent {
                    newline(out, ind, level + 1);
                }
                write_value(out, item, o, level + 1);
            }
            if let Some(ind) = o.indent {
                newline(out, ind, level);
            }
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if o.sort_keys {
                // Python sorts str keys by code point; UTF-8 byte order is identical.
                entries.sort_by(|a, b| a.0.cmp(b.0));
            }
            for (i, (k, val)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(o.item_sep);
                }
                if let Some(ind) = o.indent {
                    newline(out, ind, level + 1);
                }
                write_str(out, k, o.ensure_ascii);
                out.push_str(o.key_sep);
                write_value(out, val, o, level + 1);
            }
            if let Some(ind) = o.indent {
                newline(out, ind, level);
            }
            out.push('}');
        }
    }
}

fn needs_escape(b: u8, ensure_ascii: bool) -> bool {
    b < 0x20 || b == b'"' || b == b'\\' || (ensure_ascii && b > 0x7e)
}

fn write_str(out: &mut String, s: &str, ensure_ascii: bool) {
    out.push('"');
    // Copy runs that need no escaping in one go; most model text is such a run.
    let bytes = s.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        let run = bytes[start..]
            .iter()
            .position(|b| needs_escape(*b, ensure_ascii))
            .map_or(bytes.len(), |n| start + n);
        out.push_str(&s[start..run]);
        if run == bytes.len() {
            break;
        }
        let c = s[run..].chars().next().expect("char boundary");
        start = run + c.len_utf8();
        write_char(out, c, ensure_ascii);
    }
    out.push('"');
}

fn write_char(out: &mut String, c: char, ensure_ascii: bool) {
    match c {
        '"' => out.push_str("\\\""),
        '\\' => out.push_str("\\\\"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\u{08}' => out.push_str("\\b"),
        '\u{0c}' => out.push_str("\\f"),
        c if (c as u32) < 0x20 => {
            let _ = write!(out, "\\u{:04x}", c as u32);
        }
        c if ensure_ascii && (c as u32) > 0x7e => {
            let cp = c as u32;
            if cp > 0xffff {
                let v = cp - 0x10000;
                let hi = 0xd800 + (v >> 10);
                let lo = 0xdc00 + (v & 0x3ff);
                let _ = write!(out, "\\u{:04x}\\u{:04x}", hi, lo);
            } else {
                let _ = write!(out, "\\u{:04x}", cp);
            }
        }
        c => out.push(c),
    }
}

/// Python-style finite float formatting: Rust's shortest round-trip digits,
/// fixed notation for decimal exponents in [-4, 16), otherwise `d.ddde+XX`.
/// Digit tie-breaking is not guaranteed identical to CPython for every f64.
/// Non-finite inputs use JSON's `NaN`/`Infinity` spellings, NOT Python `repr`'s
/// lowercase `nan`/`inf`; these inputs cannot occur in a `serde_json::Value`.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    if f == 0.0 {
        return if f.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    // Rust's `{:e}` is shortest-round-trip, e.g. "1.2345e2", "1e-5".
    let sci = format!("{:e}", f);
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let mant = mant.trim_start_matches('-');
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let int_len = (exp + 1) as usize;
            if digits.len() <= int_len {
                out.push_str(&digits);
                for _ in digits.len()..int_len {
                    out.push('0');
                }
                out.push_str(".0");
            } else {
                out.push_str(&digits[..int_len]);
                out.push('.');
                out.push_str(&digits[int_len..]);
            }
        } else {
            out.push_str("0.");
            for _ in 0..(-exp - 1) {
                out.push('0');
            }
            out.push_str(&digits);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn floats_match_cpython() {
        let cases = [
            (1.0, "1.0"),
            (0.1, "0.1"),
            (1e-5, "1e-05"),
            (1.5e-5, "1.5e-05"),
            (0.0001, "0.0001"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (123456789012345.0, "123456789012345.0"),
            (1234567890123456.0, "1234567890123456.0"),
            (-2.5, "-2.5"),
            (1790001234.5, "1790001234.5"),
            (0.12, "0.12"),
            (100.0, "100.0"),
        ];
        for (f, want) in cases {
            assert_eq!(float_repr(f), want, "{f}");
        }
    }

    #[test]
    fn slice_and_run_escaping_match_value_path() {
        let items = vec![
            json!({"role":"user","content":"plain ascii run then \"quote\" and \\ back\u{1}"}),
            json!({"z":1,"a":["混合 text 😀", "\t\r\n"]}),
        ];
        assert_eq!(
            dumps_sorted_slice(&items),
            dumps_sorted(&Value::Array(items.clone()))
        );
        assert_eq!(dumps_sorted_slice(&[]), "[]");
        assert_eq!(
            dumps(&json!("a\u{7f}é😀\"")),
            r#""a\u007f\u00e9\ud83d\ude00\"""#
        );
        assert_eq!(dumps_u(&json!("a\u{7f}é")), "\"a\u{7f}é\"");
    }

    #[test]
    fn separators_and_escaping() {
        let v = json!({"b": [1, 2.0, "é\n"], "a": {"x": null, "y": true}});
        assert_eq!(
            dumps(&v),
            r#"{"b": [1, 2.0, "\u00e9\n"], "a": {"x": null, "y": true}}"#
        );
        assert_eq!(
            dumps_u(&v),
            "{\"b\": [1, 2.0, \"é\\n\"], \"a\": {\"x\": null, \"y\": true}}"
        );
        assert_eq!(
            dumps_compact(&v),
            "{\"b\":[1,2.0,\"é\\n\"],\"a\":{\"x\":null,\"y\":true}}"
        );
        assert_eq!(
            dumps_sorted(&v),
            "{\"a\": {\"x\": null, \"y\": true}, \"b\": [1, 2.0, \"é\\n\"]}"
        );
        assert_eq!(dumps(&json!("😀")), r#""\ud83d\ude00""#);
    }

    #[test]
    fn pretty_matches_indent2() {
        let v = json!({"a": [], "b": {}, "c": [1, {"d": "x"}]});
        let want = "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": [\n    1,\n    {\n      \"d\": \"x\"\n    }\n  ]\n}";
        assert_eq!(dumps_pretty(&v), want);
    }
}
