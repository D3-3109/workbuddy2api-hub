use crate::{Account, Result};
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;
use wbcore::{timeutil, types::InResponse};

pub fn json_code(value: &Value) -> Option<i64> {
    value["code"]
        .as_i64()
        .or_else(|| value["code"].as_str()?.parse().ok())
}
pub fn response_code(response: &InResponse) -> Option<i64> {
    response.json().as_ref().and_then(json_code)
}
pub fn auth_failure_text(text: &str) -> bool {
    let text = text.to_lowercase();
    !text.contains("<html")
        && [
            "401",
            "unauthorized",
            "authorization",
            "token expired",
            "invalid_token",
        ]
        .iter()
        .any(|s| text.contains(s))
}
pub fn restriction_is_account_level(detail: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let lower = detail.to_lowercase();
    RE.get_or_init(|| Regex::new(r#""code"\s*:\s*"?11140(?:"|\s|,|\})"#).unwrap())
        .is_match(detail)
        || ["unusual activity", "账号存在异常行为", "已被限制"]
            .iter()
            .any(|s| lower.contains(s))
}
pub fn parse_rate_limit_reset(detail: &str) -> Option<f64> {
    static STAMP: OnceLock<Regex> = OnceLock::new();
    static OFFSET: OnceLock<Regex> = OnceLock::new();
    // Decoding JSON also resolves escaped CJK and embedded message strings.
    let parsed = serde_json::from_str::<Value>(detail).ok();
    let decoded = parsed
        .as_ref()
        .map(wbcore::pyjson::dumps_u)
        .unwrap_or_else(|| detail.into());
    let re=STAMP.get_or_init(||Regex::new(r"(?:(?:reset at|将在)\s*(\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}))|(?:(\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2})\s*UTC[+-]\d{1,2})").unwrap());
    let cap = re.captures(&decoded)?;
    let stamp = cap.get(1).or_else(|| cap.get(2))?.as_str();
    let offset = OFFSET
        .get_or_init(|| Regex::new(r"UTC([+-])(\d{1,2})(?::?(\d{2}))?").unwrap())
        .captures(&decoded)
        .and_then(|c| {
            let hours = c[2].parse::<i64>().ok()?;
            let minutes = c
                .get(3)
                .and_then(|m| m.as_str().parse::<i64>().ok())
                .unwrap_or(0);
            if hours > 23 || minutes > 59 {
                return None;
            }
            Some((hours * 3600 + minutes * 60) * if &c[1] == "-" { -1 } else { 1 })
        })
        .unwrap_or(0);
    Some(timeutil::timegm_stamp(stamp)? - offset as f64)
}
pub fn rate_limit_is_account_level(detail: &str, reset: Option<f64>) -> bool {
    if reset.is_some() {
        return false;
    }
    let lower = detail.to_lowercase();
    if ["账号", "账户", "凭证", "令牌"]
        .iter()
        .any(|s| lower.contains(s))
    {
        return true;
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(?:account|credential|api[ _-]?key|token|user[ _-]?level|user[ _-]?scope)\b")
            .unwrap()
    })
    .is_match(&lower)
}
pub fn rate_limit_is_soft_shape(detail: &str) -> bool {
    serde_json::from_str::<Value>(detail)
        .ok()
        .as_ref()
        .and_then(json_code)
        == Some(14003)
        || [
            "too many requests",
            "usage exceeds frequency limit",
            "请求过于频繁",
            "请求频率",
        ]
        .iter()
        .any(|s| detail.to_lowercase().contains(s))
}
/// Advisory classification only. Gateway decides ordinary retries and records
/// request-local charged/refreshed sets. Restricted is ALWAYS terminal: no
/// product/account/realm rotation in response to it, and return original body.
#[derive(Clone, Debug, PartialEq)]
pub enum ResponseAction {
    Success,
    Restricted,
    Refresh,
    Balance,
    RateLimited {
        account_scope: bool,
        reset_at: Option<f64>,
        soft: bool,
        cap: bool,
    },
    ModelError,
    Failure,
    PassThrough,
}
pub fn classify_response(response: &InResponse) -> ResponseAction {
    let detail = response.text();
    let code = response_code(response);
    // Fail closed even if the upstream puts a restriction code in a 200 envelope.
    if code == Some(11140) || restriction_is_account_level(&detail) {
        return ResponseAction::Restricted;
    }
    if (200..300).contains(&response.status) {
        return ResponseAction::Success;
    }
    match response.status {
        401 if !detail.to_ascii_lowercase().contains("<html") => ResponseAction::Refresh,
        402 => ResponseAction::Balance,
        429 => {
            let reset_at = parse_rate_limit_reset(&detail);
            ResponseAction::RateLimited {
                account_scope: rate_limit_is_account_level(&detail, reset_at),
                reset_at,
                soft: rate_limit_is_soft_shape(&detail),
                cap: code == Some(6004),
            }
        }
        500..=599 if code.is_some_and(|c| [11102, 11129, 11134, 11150, 11158].contains(&c)) => {
            ResponseAction::ModelError
        }
        500..=599 => ResponseAction::Failure,
        _ => ResponseAction::PassThrough,
    }
}
impl Account {
    /// Apply non-network response consequences. 401 only advises refresh; call
    /// note_error after the gateway's single refresh attempt fails. 5xx charging
    /// is left to the gateway (once per uid/request; egress backoff may replace it).
    /// For Restricted, also call pool.note_account_restriction and STOP dispatch.
    pub fn apply_response(
        &self,
        response: &InResponse,
        model: &str,
        now: f64,
    ) -> Result<ResponseAction> {
        let action = classify_response(response);
        match action {
            ResponseAction::Success => self.note_success(model, now),
            ResponseAction::Restricted => self.note_restricted(&response.text(), now)?,
            ResponseAction::Balance => self.note_balance_cooled(),
            ResponseAction::RateLimited {
                account_scope,
                reset_at,
                soft,
                cap,
            } => {
                if account_scope {
                    self.note_soft_rate(now);
                } else {
                    let wait = if reset_at.is_none() && soft {
                        self.note_unscoped_rate(model, now)
                    } else {
                        60.0
                    };
                    self.note_error(
                        "HTTP 429 (model throttled)",
                        Some(model),
                        wait,
                        reset_at,
                        now,
                    );
                    if cap {
                        if let Some(reset) = reset_at {
                            self.note_model_cap(model, reset, now)?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(action)
    }
}
