use crate::text;
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use serde_json::{json, Value};
use sha2::Sha256;
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};
use wbcore::ids;

pub const DEFAULT_PANEL_PASSWORD: &str = "admin";
pub const PBKDF2_ROUNDS: u32 = 120_000;
pub const SESSION_TTL: u64 = 7 * 24 * 3600;
/// The HTTP layer delays failed logins by this amount, without blocking a mutex.
pub const LOGIN_FAILURE_DELAY: Duration = Duration::from_millis(500);

// HMAC's tag verification is constant-time; comparing fixed-size keyed digests
// also avoids any input-prefix timing leak when credential lengths differ.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut left = Hmac::<Sha256>::new_from_slice(b"wbsettings comparison").unwrap();
    left.update(a);
    let mut right = Hmac::<Sha256>::new_from_slice(b"wbsettings comparison").unwrap();
    right.update(b);
    right.verify_slice(&left.finalize().into_bytes()).is_ok()
}
pub(crate) fn password_is_default(data: &Value) -> bool {
    data["panel_password_default"] == true
        || matches!(data.get("panel_password_hash"), None | Some(Value::Null))
        || data["panel_password_hash"] == ""
}
pub(crate) fn verify_password(data: &Value, password: &str) -> bool {
    if password_is_default(data) {
        return constant_time_eq(password.as_bytes(), DEFAULT_PANEL_PASSWORD.as_bytes());
    }
    let Ok(salt) = hex::decode(text(data, "panel_password_salt")) else {
        return false;
    };
    if salt.is_empty() {
        return false;
    }
    let Ok(expected) = hex::decode(text(data, "panel_password_hash")) else {
        return false;
    };
    if expected.len() != 32 {
        return false;
    }
    let rounds = match data.get("panel_password_rounds") {
        None | Some(Value::Null) => PBKDF2_ROUNDS,
        Some(v) => match v
            .as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        {
            Some(0) => PBKDF2_ROUNDS,
            Some(n) if n <= 10_000_000 => n as u32,
            _ => return false,
        },
    };
    let mut digest = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, rounds, &mut digest);
    constant_time_eq(&digest, &expected)
}
pub(crate) fn set_password(data: &mut Value, password: &str) {
    if password == DEFAULT_PANEL_PASSWORD {
        data.as_object_mut().unwrap().remove("panel_password_salt");
        data.as_object_mut()
            .unwrap()
            .remove("panel_password_rounds");
        data["panel_password_hash"] = json!("");
        data["panel_password_default"] = json!(true);
    } else {
        let salt = ids::token_hex(16);
        let mut digest = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<Sha256>(
            password.as_bytes(),
            &hex::decode(&salt).unwrap(),
            PBKDF2_ROUNDS,
            &mut digest,
        );
        data["panel_password_salt"] = json!(salt);
        data["panel_password_hash"] = json!(hex::encode(digest));
        data["panel_password_rounds"] = json!(PBKDF2_ROUNDS);
        data["panel_password_default"] = json!(false);
    }
}

/// In-memory, absolute-expiry sessions. No cookies, persistence, or sliding TTL.
pub struct PanelSessions {
    ttl: Duration,
    tokens: Mutex<HashMap<String, Instant>>,
}
impl Default for PanelSessions {
    fn default() -> Self {
        Self::new()
    }
}
impl PanelSessions {
    pub fn new() -> Self {
        Self::with_ttl(Duration::from_secs(SESSION_TTL))
    }
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            tokens: Mutex::new(HashMap::new()),
        }
    }
    pub fn issue(&self) -> String {
        self.issue_at(Instant::now())
    }
    pub fn create(&self) -> String {
        self.issue()
    }
    pub fn valid(&self, token: &str) -> bool {
        self.valid_at(token, Instant::now())
    }
    pub fn revoke(&self, token: &str) {
        self.tokens.lock().remove(token);
    }
    pub fn revoke_all(&self) {
        self.tokens.lock().clear();
    }
    fn issue_at(&self, now: Instant) -> String {
        let token = ids::token_urlsafe(24);
        let mut tokens = self.tokens.lock();
        tokens.retain(|_, expiry| *expiry > now);
        tokens.insert(token.clone(), now + self.ttl);
        token
    }
    fn valid_at(&self, token: &str, now: Instant) -> bool {
        let mut tokens = self.tokens.lock();
        tokens.retain(|_, expiry| *expiry > now);
        !token.is_empty() && tokens.contains_key(token)
    }
}

/// Sliding failure window (5 failures / 60 seconds), keyed by trusted client IP.
/// The HTTP layer invokes check, verifies the password, then success/failure.
/// No sleeping or network operations occur here.
pub struct LoginRateLimiter {
    attempts: Mutex<HashMap<String, VecDeque<Instant>>>,
    limit: usize,
    window: Duration,
}
impl Default for LoginRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}
impl LoginRateLimiter {
    pub fn new() -> Self {
        Self::with_window(5, Duration::from_secs(60))
    }
    pub fn with_window(limit: usize, window: Duration) -> Self {
        Self {
            attempts: Mutex::new(HashMap::new()),
            limit: limit.max(1),
            window,
        }
    }
    pub fn check(&self, key: &str) -> std::result::Result<(), u64> {
        self.check_at(key, Instant::now())
    }
    pub fn failure(&self, key: &str) {
        self.failure_at(key, Instant::now());
    }
    pub fn success(&self, key: &str) {
        self.attempts.lock().remove(key);
    }
    fn prune(&self, buckets: &mut HashMap<String, VecDeque<Instant>>, now: Instant) {
        buckets.retain(|_, times| {
            while times
                .front()
                .is_some_and(|t| now.saturating_duration_since(*t) >= self.window)
            {
                times.pop_front();
            }
            !times.is_empty()
        });
    }
    fn check_at(&self, key: &str, now: Instant) -> std::result::Result<(), u64> {
        let mut buckets = self.attempts.lock();
        self.prune(&mut buckets, now);
        if let Some(times) = buckets.get(key) {
            if times.len() >= self.limit {
                let left = self
                    .window
                    .saturating_sub(now.saturating_duration_since(*times.front().unwrap()));
                return Err((left.as_secs() + u64::from(left.subsec_nanos() > 0)).max(1));
            }
        }
        Ok(())
    }
    fn failure_at(&self, key: &str, now: Instant) {
        let mut buckets = self.attempts.lock();
        self.prune(&mut buckets, now);
        buckets.entry(key.to_owned()).or_default().push_back(now);
    }
}

fn parse_ip(value: &str) -> Option<IpAddr> {
    let value = value.trim();
    if value.is_empty() || value.len() > 64 || value.contains(',') {
        return None;
    }
    value
        .parse::<IpAddr>()
        .ok()
        .or_else(|| value.parse::<SocketAddr>().ok().map(|s| s.ip()))
        .or_else(|| {
            value
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .and_then(|s| s.parse().ok())
        })
}
/// Forwarded headers are trusted only for loopback (including mapped IPv4) or
/// explicitly configured CIDRs. Invalid CIDRs and header IPs are ignored.
pub fn login_rate_limit_key(
    peer: &str,
    x_real_ip: Option<&str>,
    x_forwarded_for: Option<&str>,
    trusted_cidrs: &[String],
) -> String {
    let Some(ip) = parse_ip(peer) else {
        return peer.to_owned();
    };
    let unmapped = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        _ => ip,
    };
    let trusted = unmapped.is_loopback()
        || trusted_cidrs
            .iter()
            .filter_map(|s| s.trim().parse::<ipnet::IpNet>().ok())
            .any(|net| net.contains(&ip) || net.contains(&unmapped));
    if trusted {
        if let Some(real) = x_real_ip.and_then(parse_ip) {
            return real.to_string();
        }
        if let Some(forwarded) = x_forwarded_for
            .and_then(|s| s.split(',').next())
            .and_then(parse_ip)
        {
            return forwarded.to_string();
        }
    }
    ip.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sessions_absolute_expiry_and_revocation() {
        let sessions = PanelSessions::with_ttl(Duration::from_secs(10));
        let now = Instant::now();
        let token = sessions.issue_at(now);
        assert_eq!(token.len(), 32);
        assert!(sessions.valid_at(&token, now + Duration::from_secs(9)));
        assert!(!sessions.valid_at(&token, now + Duration::from_secs(10)));
        let token = sessions.issue();
        sessions.revoke(&token);
        assert!(!sessions.valid(&token));
        let token = sessions.issue();
        sessions.revoke_all();
        assert!(!sessions.valid(&token));
    }
    #[test]
    fn limiter_window_and_success() {
        let limiter = LoginRateLimiter::new();
        let now = Instant::now();
        for i in 0..5 {
            assert_eq!(limiter.check_at("a", now + Duration::from_secs(i)), Ok(()));
            limiter.failure_at("a", now + Duration::from_secs(i));
        }
        assert_eq!(limiter.check_at("a", now + Duration::from_secs(5)), Err(55));
        assert_eq!(limiter.check_at("b", now + Duration::from_secs(5)), Ok(()));
        assert_eq!(limiter.check_at("a", now + Duration::from_secs(60)), Ok(()));
        limiter.failure_at("a", now + Duration::from_secs(60));
        limiter.success("a");
        assert_eq!(limiter.check_at("a", now + Duration::from_secs(60)), Ok(()));
    }
    #[test]
    fn proxy_header_trust_and_parsing() {
        assert_eq!(
            login_rate_limit_key("203.0.113.1", Some("1.1.1.1"), None, &[]),
            "203.0.113.1"
        );
        assert_eq!(
            login_rate_limit_key("127.0.0.1", Some("1.1.1.1:443"), None, &[]),
            "1.1.1.1"
        );
        assert_eq!(
            login_rate_limit_key("::ffff:127.0.0.1", Some("[2001:db8::1]:90"), None, &[]),
            "2001:db8::1"
        );
        assert_eq!(
            login_rate_limit_key(
                "10.1.2.3",
                Some("junk"),
                Some("8.8.8.8, 9.9.9.9"),
                &["10.0.0.0/8".into(), "junk".into()]
            ),
            "8.8.8.8"
        );
        assert_eq!(
            login_rate_limit_key(
                "127.0.0.1",
                Some("1.1.1.1, 2.2.2.2"),
                Some("invalid, 8.8.8.8"),
                &[]
            ),
            "127.0.0.1"
        );
    }
    #[test]
    fn python_pbkdf2_vector() {
        let data = json!({"panel_password_hash":"120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b", "panel_password_salt":"73616c74", "panel_password_rounds":1});
        assert!(verify_password(&data, "password"));
        assert!(!verify_password(&data, "Password"));
        assert!(!verify_password(
            &json!({"panel_password_hash":"broken","panel_password_salt":"00"}),
            "admin"
        ));
    }
}
