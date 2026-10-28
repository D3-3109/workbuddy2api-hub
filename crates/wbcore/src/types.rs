//! Types shared across crates. Builders for upstream requests are "sans-IO":
//! they return an [`OutRequest`] that the gateway's HTTP client executes, and
//! parsers take an [`InResponse`].

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Realm {
    Intl,
    Cn,
}

impl Realm {
    pub fn as_str(self) -> &'static str {
        match self {
            Realm::Intl => "intl",
            Realm::Cn => "cn",
        }
    }

    /// Strict parse: only `intl` / `cn` (case-insensitive, trimmed).
    pub fn parse(s: &str) -> Option<Realm> {
        match s.trim().to_ascii_lowercase().as_str() {
            "intl" => Some(Realm::Intl),
            "cn" => Some(Realm::Cn),
            _ => None,
        }
    }

    /// Python `get_realm_config` fallback: anything that is not `cn` is intl.
    pub fn parse_or_intl(s: &str) -> Realm {
        Realm::parse(s).unwrap_or(Realm::Intl)
    }

    pub const ALL: [Realm; 2] = [Realm::Intl, Realm::Cn];
}

impl std::fmt::Display for Realm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Outbound client identity (wb_identity.py `normalize_product`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Product {
    Workbuddy,
    Vscode,
    Cli,
}

impl Product {
    pub fn as_str(self) -> &'static str {
        match self {
            Product::Workbuddy => "workbuddy",
            Product::Vscode => "vscode",
            Product::Cli => "cli",
        }
    }

    /// Aliases `wb`/`desktop`, `vsc`/`ide`, `codebuddy`; unknown -> workbuddy.
    pub fn normalize(s: &str) -> Product {
        match s.trim().to_ascii_lowercase().as_str() {
            "vscode" | "vsc" | "ide" => Product::Vscode,
            "cli" | "codebuddy" => Product::Cli,
            _ => Product::Workbuddy,
        }
    }

    /// Strict parse used by the panel (`/accounts/product`).
    pub fn parse_strict(s: &str) -> Option<Product> {
        match s.trim().to_ascii_lowercase().as_str() {
            "workbuddy" => Some(Product::Workbuddy),
            "vscode" => Some(Product::Vscode),
            "cli" => Some(Product::Cli),
            _ => None,
        }
    }

    pub const ALL: [Product; 3] = [Product::Workbuddy, Product::Vscode, Product::Cli];
}

/// The credential/identity subset of an account that request builders need.
/// `Debug` redacts all free-form strings, including tokens and identity data.
#[derive(Clone)]
pub struct Cred {
    pub uid: String,
    pub access_token: String,
    pub refresh_token: String,
    pub realm: Realm,
    pub product: Product,
    pub enterprise_id: String,
    /// Stored `domain` (billing / refresh `X-Domain`); empty = realm default.
    pub domain: String,
    pub platform: String,
}

impl std::fmt::Debug for Cred {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cred")
            .field("uid", &"[REDACTED]")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("realm", &self.realm)
            .field("product", &self.product)
            .field("enterprise_id", &"[REDACTED]")
            .field("domain", &"[REDACTED]")
            .field("platform", &"[REDACTED]")
            .finish()
    }
}

impl Cred {
    /// Static API keys (`ck_` / `pt_` prefixes) are never refreshed.
    pub fn is_api_key(&self) -> bool {
        is_static_api_key(&self.access_token)
    }
}

pub fn is_static_api_key(token: &str) -> bool {
    token.starts_with("ck_") || token.starts_with("pt_")
}

/// How header names are written on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderCase {
    /// Names verbatim, in order (`wb_exact_headers`): chat path.
    Exact,
    /// urllib behaviour: `name.capitalize().title()` (billing, refresh, OAuth...).
    Title,
}

/// Which egress an outbound request uses. `Debug` never prints the proxy URL,
/// which can contain credentials in its authority, path or query.
#[derive(Clone, PartialEq, Eq)]
pub enum Egress {
    /// Direct connection regardless of the account's proxy (OAuth, ip-api).
    Direct,
    /// The proxy URL string of the account (empty = direct).
    Proxy(String),
}

impl std::fmt::Debug for Egress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Direct => f.write_str("Direct"),
            Self::Proxy(_) => f.debug_tuple("Proxy").field(&"[REDACTED]").finish(),
        }
    }
}

/// A fully described upstream request. The HTTP client adds
/// `Accept-Encoding: identity`, `Content-Length`, `Host` and
/// `Connection: keep-alive` exactly like CPython's http.client.
/// `Debug` shows only sizes, typed metadata and an allowlisted HTTP method;
/// URLs, all header names/values, and body contents are never logged.
#[derive(Clone)]
pub struct OutRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub case: HeaderCase,
    /// Socket timeout in seconds (per operation, like Python's `timeout=`).
    pub timeout_secs: f64,
    pub egress: Egress,
}

impl std::fmt::Debug for OutRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let method = match self.method.as_str() {
            "GET" | "HEAD" | "POST" | "PUT" | "DELETE" | "CONNECT" | "OPTIONS" | "TRACE"
            | "PATCH" => self.method.as_str(),
            _ => "[REDACTED]",
        };
        f.debug_struct("OutRequest")
            .field("method", &method)
            .field("url", &"[REDACTED]")
            .field("header_count", &self.headers.len())
            .field("body_bytes", &self.body.as_ref().map(Vec::len))
            .field("case", &self.case)
            .field("timeout_secs", &self.timeout_secs)
            .field("egress", &self.egress)
            .finish()
    }
}

impl OutRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A buffered upstream response (non-streaming calls). `Debug` shows status and
/// sizes only, never headers or body contents (which may contain tokens).
#[derive(Clone, Default)]
pub struct InResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for InResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InResponse")
            .field("status", &self.status)
            .field("header_count", &self.headers.len())
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

impl InResponse {
    pub fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}
