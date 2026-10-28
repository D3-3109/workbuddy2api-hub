//! Python-flavoured accessors over `serde_json::Value`, so translated code can
//! keep the exact truthiness / `or` semantics of the original.

use serde_json::{Map, Value};

/// Python `bool(v)`.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

pub fn truthy_opt(v: Option<&Value>) -> bool {
    v.map(truthy).unwrap_or(false)
}

/// `d.get(k)` that also returns None for a non-object.
pub fn get<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.as_object().and_then(|m| m.get(k))
}

/// `d.get(k) or None`: the value only when truthy.
pub fn get_truthy<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    get(v, k).filter(|x| truthy(x))
}

/// `str(d.get(k) or "")` for string values; other truthy scalars are
/// stringified the way Python's `str()` would (ints, bools).
pub fn get_str(v: &Value, k: &str) -> String {
    match get(v, k) {
        Some(x) => py_str_or_empty(x),
        None => String::new(),
    }
}

/// `str(x or "")`
pub fn py_str_or_empty(x: &Value) -> String {
    if !truthy(x) {
        return String::new();
    }
    py_str(x)
}

/// Python-style `str(x)` for JSON values; nested strings use `repr` quoting.
///
/// Only JSON's types are supported. Float formatting has the limitations of
/// [`crate::pyjson::float_repr`]; Unicode printability follows the regex crate's
/// Unicode tables, which can differ from a particular Python release. Callers
/// must bound input size and nesting, as with the JSON serializers.
pub fn py_str(x: &Value) -> String {
    if let Value::String(s) = x {
        return s.clone();
    }
    let mut out = String::new();
    write_repr(&mut out, x);
    out
}

fn write_repr(out: &mut String, x: &Value) {
    match x {
        Value::Null => out.push_str("None"),
        Value::Bool(true) => out.push_str("True"),
        Value::Bool(false) => out.push_str("False"),
        Value::String(s) => write_string_repr(out, s),
        Value::Number(n) => {
            if n.is_f64() {
                out.push_str(&crate::pyjson::float_repr(n.as_f64().unwrap_or(0.0)));
            } else {
                out.push_str(&n.to_string());
            }
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i != 0 {
                    out.push_str(", ");
                }
                write_repr(out, item);
            }
            out.push(']');
        }
        Value::Object(items) => {
            out.push('{');
            for (i, (key, value)) in items.iter().enumerate() {
                if i != 0 {
                    out.push_str(", ");
                }
                write_string_repr(out, key);
                out.push_str(": ");
                write_repr(out, value);
            }
            out.push('}');
        }
    }
}

fn write_string_repr(out: &mut String, s: &str) {
    use std::fmt::Write as _;
    use std::sync::OnceLock;

    static NON_PRINTABLE: OnceLock<regex::Regex> = OnceLock::new();
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => {
                let printable = if c.is_ascii() {
                    (' '..='~').contains(&c)
                } else {
                    let mut utf8 = [0; 4];
                    !NON_PRINTABLE
                        .get_or_init(|| {
                            regex::Regex::new(r"[\p{Other}\p{Separator}]")
                                .expect("constant Unicode regex")
                        })
                        .is_match(c.encode_utf8(&mut utf8))
                };
                if printable {
                    out.push(c);
                } else if (c as u32) <= 0xff {
                    let _ = write!(out, "\\x{:02x}", c as u32);
                } else if (c as u32) <= 0xffff {
                    let _ = write!(out, "\\u{:04x}", c as u32);
                } else {
                    let _ = write!(out, "\\U{:08x}", c as u32);
                }
            }
        }
    }
    out.push(quote);
}

/// Bounded Python-style `int(x)`: bools, integers, truncated floats, and trimmed
/// signed ASCII decimal strings. Out-of-i64 results return `None`, never clamp.
/// Python's arbitrary-size ints, underscores and Unicode digits are unsupported.
pub fn py_int(x: &Value) -> Option<i64> {
    match x {
        Value::Bool(b) => Some(*b as i64),
        Value::Number(n) => n.as_i64().or_else(|| {
            if !n.is_f64() {
                return None;
            }
            let f = n.as_f64()?.trunc();
            // i64::MAX rounds UP to 2^63 as f64, so the upper bound is exclusive.
            (f >= i64::MIN as f64 && f < -(i64::MIN as f64)).then_some(f as i64)
        }),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// Python-style `float(x)` for bools, JSON numbers and Rust-parsable numeric
/// strings. Python-only string syntax (e.g. underscores/Unicode digits) is not
/// supported. Non-finite string results cannot be stored in a JSON number.
pub fn py_float(x: &Value) -> Option<f64> {
    match x {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// `d.get(k) or 0` as a number (int when integral, else float).
pub fn num_or_zero(v: &Value, k: &str) -> Value {
    match get(v, k) {
        Some(x) if truthy(x) => x.clone(),
        _ => Value::from(0),
    }
}

pub fn as_obj_mut(v: &mut Value) -> &mut Map<String, Value> {
    if !v.is_object() {
        *v = Value::Object(Map::new());
    }
    v.as_object_mut().unwrap()
}

/// `dict(x or {})` style: object copy or empty map.
pub fn obj_or_empty(v: Option<&Value>) -> Map<String, Value> {
    v.and_then(|x| x.as_object()).cloned().unwrap_or_default()
}

/// Number value from an f64 that serialises as Python would (`int` stays int
/// when the caller had an int; here we keep integral floats as float).
pub fn float_value(f: f64) -> Value {
    serde_json::Number::from_f64(f)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// Decimal half-even rounding, following Python's `round(x, nd)` for finite
/// f64 inputs, including signed zero. Round the original binary float's decimal
/// digits, not `x * 10^nd` (which introduces a second rounding, e.g. for 2.675).
///
/// Work is bounded even for extreme `nd`: precision is at most 323 places.
/// Non-finite inputs pass through. Unlike Python's `OverflowError`, an overflow
/// returns signed infinity because the existing f64 API cannot return an error.
pub fn py_round(x: f64, nd: i32) -> f64 {
    if !x.is_finite() || x == 0.0 || nd > 323 {
        return x;
    }
    if nd < -308 {
        return 0.0_f64.copysign(x);
    }
    if nd >= 0 {
        // Rust's fixed-precision formatter rounds the actual f64 half-even.
        return format!("{:.*}", nd as usize, x)
            .parse()
            .expect("formatted finite float");
    }

    // For negative precision, the rounding boundary is an integer. Keep exact
    // integer digits and separately remember any fractional part so 149.9 is
    // not double-rounded through 150 before rounding to hundreds.
    let magnitude = x.abs();
    let digits = format!("{:.0}", magnitude.trunc());
    let places = (-nd) as usize;
    if places > digits.len() {
        return 0.0_f64.copysign(x);
    }
    let split = digits.len() - places;
    let discarded = &digits.as_bytes()[split..];
    let odd = split > 0 && (digits.as_bytes()[split - 1] - b'0') % 2 != 0;
    let up = discarded[0] > b'5'
        || (discarded[0] == b'5'
            && (discarded[1..].iter().any(|b| *b != b'0') || magnitude.fract() != 0.0 || odd));
    let mut kept = digits.as_bytes()[..split].to_vec();
    if up {
        let mut carry = true;
        for digit in kept.iter_mut().rev() {
            if *digit == b'9' {
                *digit = b'0';
            } else {
                *digit += 1;
                carry = false;
                break;
            }
        }
        if carry {
            kept.insert(0, b'1');
        }
    }
    if kept.is_empty() {
        return 0.0_f64.copysign(x);
    }
    let decimal = format!(
        "{}e{}",
        String::from_utf8(kept).expect("decimal digits"),
        places
    );
    decimal
        .parse::<f64>()
        .expect("rounded decimal float")
        .copysign(x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truthiness() {
        assert!(!truthy(&json!(0)));
        assert!(!truthy(&json!(0.0)));
        assert!(!truthy(&json!("")));
        assert!(!truthy(&json!([])));
        assert!(truthy(&json!("false")));
        assert!(truthy(&json!([0])));
    }

    #[test]
    fn rounding() {
        assert_eq!(py_round(2.675, 2), 2.67);
        assert_eq!(py_round(0.5, 0), 0.0);
        assert_eq!(py_round(1.5, 0), 2.0);
        assert_eq!(py_round(12.34, 1), 12.3);
        assert_eq!(py_round(149.9, -2), 100.0);
        assert_eq!(py_round(150.0, -2), 200.0);
        assert_eq!(py_round(250.0, -2), 200.0);
        assert_eq!(py_round(-0.5, 0).to_bits(), (-0.0_f64).to_bits());
        assert_eq!(py_round(-1.0, i32::MIN).to_bits(), (-0.0_f64).to_bits());
        assert_eq!(py_round(1.25, i32::MAX), 1.25);
        assert_eq!(py_round(f64::MAX, -308), f64::INFINITY);
        assert_eq!(py_round(f64::NEG_INFINITY, 0), f64::NEG_INFINITY);
        assert!(py_round(f64::NAN, 0).is_nan());
    }
}
