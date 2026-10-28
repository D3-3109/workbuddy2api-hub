use crate::{default_domain, normalize_epoch, response_code, Account, Error, Result};
use flate2::{Compression, GzBuilder};
use serde_json::{json, Value};
use std::io::Write;
use wbcore::{
    ids, pyjson, timeutil,
    types::{Cred, Egress, HeaderCase, InResponse, OutRequest, Product, Realm},
};

#[derive(Clone, Debug)]
pub struct RequestOptions {
    /// Resolved proxy (gateway resolves proxySlot first); never used for OAuth.
    pub proxy: String,
    pub cn_chat_base: Option<String>,
    pub timeout_secs: f64,
}
impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            proxy: String::new(),
            cn_chat_base: None,
            timeout_secs: 30.0,
        }
    }
}
#[derive(Clone, Debug)]
pub struct IdentityVersions {
    pub desktop_intl: String,
    pub desktop_cn: String,
    pub desktop_cli: String,
    pub vscode_product: String,
    pub vscode_ide: String,
    pub cli: String,
    pub sdk: String,
    pub node: String,
    pub os: String,
    pub arch: String,
}
impl Default for IdentityVersions {
    fn default() -> Self {
        Self {
            desktop_intl: "5.8.4".into(),
            desktop_cn: "5.8.3".into(),
            desktop_cli: "2.161.0".into(),
            vscode_product: "4.12.39997873".into(),
            vscode_ide: "1.119.0".into(),
            cli: "2.168.2".into(),
            sdk: "6.25.0".into(),
            node: "v22.21.1".into(),
            os: match std::env::consts::OS {
                "windows" => "Windows",
                "macos" => "MacOS",
                _ => "Linux",
            }
            .into(),
            arch: match std::env::consts::ARCH {
                "x86_64" => "x64",
                "aarch64" => "arm64",
                "x86" => "ia32",
                s => s,
            }
            .into(),
        }
    }
}
#[derive(Clone, Debug)]
pub struct ChatOptions {
    pub request: RequestOptions,
    /// Explicit desktop chat protocol opt-in; does not change product, body or TLS.
    pub desktop_chat_compat: bool,
    pub session_key: String,
    /// Gateway may supply one stable VSCode conversation id across attempts.
    pub conversation_id: Option<String>,
    /// CLI turn id, kept stable across ordinary network retries.
    pub turn_request_id: Option<String>,
    pub model: Option<String>,
    /// Optional operator-provided standard device credential; never generated here.
    pub device_token: Option<String>,
    pub private_data: Option<bool>,
    pub versions: IdentityVersions,
    pub now: f64,
}
impl Default for ChatOptions {
    fn default() -> Self {
        Self {
            request: RequestOptions {
                timeout_secs: 120.0,
                ..Default::default()
            },
            desktop_chat_compat: false,
            session_key: String::new(),
            conversation_id: None,
            turn_request_id: None,
            model: None,
            device_token: None,
            private_data: None,
            versions: IdentityVersions::default(),
            now: timeutil::now(),
        }
    }
}
pub fn realm_origin(realm: Realm) -> &'static str {
    if realm == Realm::Cn {
        "https://www.codebuddy.cn"
    } else {
        "https://www.workbuddy.ai"
    }
}
pub fn billing_base(realm: Realm) -> &'static str {
    realm_origin(realm)
}
pub fn billing_user_agent(realm: Realm) -> &'static str {
    if realm == Realm::Cn {
        "WorkBuddy/5.8.3"
    } else {
        "WorkBuddy/5.8.4"
    }
}
fn realm_chat_base(realm: Realm, options: &RequestOptions) -> Result<String> {
    if realm == Realm::Intl {
        return Ok("https://www.workbuddy.ai".into());
    }
    let base = options
        .cn_chat_base
        .as_deref()
        .unwrap_or("https://copilot.tencent.com")
        .trim_end_matches('/');
    let authority = base
        .strip_prefix("https://")
        .or_else(|| base.strip_prefix("http://"))
        .ok_or_else(|| Error::Invalid("CN chat base must use http or https".into()))?;
    if authority.is_empty()
        || authority.starts_with('/')
        || authority.contains(['@', '?', '#'])
        || authority
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(Error::Invalid("invalid CN chat base".into()));
    }
    Ok(base.into())
}
pub fn chat_endpoint(cred: &Cred, options: &RequestOptions) -> Result<String> {
    if cred.realm == Realm::Cn && cred.product == Product::Vscode {
        Ok("https://www.workbuddy.cn/v2/chat/completions".into())
    } else {
        Ok(format!(
            "{}/v2/chat/completions",
            realm_chat_base(cred.realm, options)?
        ))
    }
}
fn chat_domain(cred: &Cred) -> &'static str {
    if cred.realm == Realm::Cn && cred.product == Product::Vscode {
        "www.workbuddy.cn"
    } else {
        default_domain(cred.realm)
    }
}
fn add(headers: &mut Vec<(String, String)>, key: &str, value: impl Into<String>) {
    headers.push((key.into(), value.into()));
}
fn common_headers(realm: Realm) -> Vec<(String, String)> {
    let origin = realm_origin(realm);
    vec![
        ("Content-Type".into(), "application/json".into()),
        ("Accept".into(), "application/json, text/plain, */*".into()),
        ("X-Requested-With".into(), "XMLHttpRequest".into()),
        ("User-Agent".into(), billing_user_agent(realm).into()),
        ("Origin".into(), origin.into()),
        ("Referer".into(), format!("{origin}/")),
    ]
}
fn language(realm: Realm) -> &'static str {
    if realm == Realm::Cn {
        "zh-CN"
    } else {
        "en-US"
    }
}
fn validate_headers(headers: &[(String, String)]) -> Result<()> {
    for (name, value) in headers {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || value
                .chars()
                .any(|c| c.is_control() && c != '\t' || c as u32 > 255)
        {
            return Err(Error::Invalid(format!("invalid HTTP header {name}")));
        }
    }
    Ok(())
}
fn validate_cred(cred: &Cred) -> Result<()> {
    if cred.access_token.is_empty() {
        return Err(Error::Invalid("missing access token".into()));
    }
    if cred.is_api_key() && cred.realm != Realm::Cn {
        return Err(Error::Invalid(
            "static API keys require the cn realm".into(),
        ));
    }
    Ok(())
}
fn validate_versions(v: &IdentityVersions) -> Result<()> {
    for version in [
        &v.desktop_intl,
        &v.desktop_cn,
        &v.desktop_cli,
        &v.vscode_product,
        &v.vscode_ide,
        &v.cli,
        &v.sdk,
        &v.node,
        &v.os,
        &v.arch,
    ] {
        if version.is_empty()
            || version.len() > 64
            || !version.as_bytes()[0].is_ascii_alphanumeric()
            || !version
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        {
            return Err(Error::Invalid("invalid identity version".into()));
        }
    }
    Ok(())
}
fn finish(
    method: &str,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    case: HeaderCase,
    options: &RequestOptions,
    direct: bool,
) -> Result<OutRequest> {
    validate_headers(&headers)?;
    if !options.timeout_secs.is_finite() || options.timeout_secs <= 0.0 {
        return Err(Error::Invalid("timeout must be positive and finite".into()));
    }
    Ok(OutRequest {
        method: method.into(),
        url,
        headers,
        body,
        case,
        timeout_secs: options.timeout_secs,
        egress: if direct || options.proxy.is_empty() {
            Egress::Direct
        } else {
            Egress::Proxy(options.proxy.clone())
        },
    })
}
fn trace_headers(h: &mut Vec<(String, String)>, product: Product, desktop_compat: bool) {
    let trace = ids::new_message_id();
    let span = ids::token_hex(8);
    let parent = ids::token_hex(8);
    add(h, "X-Trace-ID", &trace);
    if product != Product::Vscode {
        add(h, "traceparent", format!("00-{trace}-{span}-01"));
    }
    add(
        h,
        "b3",
        if product == Product::Workbuddy && !desktop_compat {
            format!("{trace}-{span}-1")
        } else {
            format!("{trace}-{span}-1-{parent}")
        },
    );
    add(h, "X-B3-TraceId", trace);
    if product == Product::Vscode {
        add(h, "X-B3-ParentSpanId", &parent);
    }
    add(h, "X-B3-SpanId", span);
    add(h, "X-B3-Sampled", "1");
    if product == Product::Cli || (product == Product::Workbuddy && desktop_compat) {
        add(h, "X-B3-ParentSpanId", parent);
    }
}
fn stainless_headers(h: &mut Vec<(String, String)>, v: &IdentityVersions, cli_timeout: bool) {
    for (key, value) in [
        ("x-stainless-lang", "js"),
        ("x-stainless-package-version", v.sdk.as_str()),
        ("x-stainless-os", v.os.as_str()),
        ("x-stainless-arch", v.arch.as_str()),
        ("x-stainless-runtime", "node"),
        ("x-stainless-runtime-version", v.node.as_str()),
        ("x-stainless-retry-count", "0"),
    ] {
        add(h, key, value);
    }
    // Desktop's timeout metadata is unverified; do not inherit CLI's budget.
    if cli_timeout {
        add(h, "x-stainless-timeout", "1200");
    }
}
/// Protocol compatibility/authentication headers, not a restriction workaround.
/// This function never selects or changes an account's product.
pub fn build_identity_headers(cred: &Cred, options: &ChatOptions) -> Result<Vec<(String, String)>> {
    validate_cred(cred)?;
    validate_versions(&options.versions)?;
    let v = &options.versions;
    let mut h = Vec::new();
    let product = cred.product;
    let message = ids::new_message_id();
    let request = if product == Product::Cli {
        options
            .turn_request_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(ids::new_message_id)
    } else {
        ids::new_message_id()
    };
    let conversation = options
        .conversation_id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if product == Product::Vscode {
                ids::new_message_id()
            } else {
                let stable = ids::conversation_id_for(&cred.uid, &options.session_key);
                if stable.is_empty() {
                    ids::new_conversation_id()
                } else {
                    stable
                }
            }
        });
    add(&mut h, "X-Agent-Intent", "craft");
    if product != Product::Vscode {
        add(&mut h, "X-Agent-Purpose", "conversation");
    }
    if product == Product::Cli {
        add(&mut h, "X-Agent-Runtime", "other");
    }
    add(&mut h, "X-Agent-Type", "main");
    match product {
        Product::Workbuddy => {
            let version = if cred.realm == Realm::Cn {
                &v.desktop_cn
            } else {
                &v.desktop_intl
            };
            add(&mut h, "X-IDE-Name", "WorkBuddy");
            add(&mut h, "X-IDE-Type", "WorkBuddy");
            add(&mut h, "X-IDE-Version", version);
            add(&mut h, "X-Private-Data", "true");
            add(&mut h, "X-Product", "SaaS");
            add(&mut h, "X-Domain", chat_domain(cred));
            add(
                &mut h,
                "User-Agent",
                format!(
                    "WorkBuddy/{version} {}/{version} CLI/{}",
                    if cred.realm == Realm::Cn {
                        "WorkBuddy"
                    } else {
                        "workbuddy-ai"
                    },
                    v.desktop_cli
                ),
            );
        }
        Product::Vscode => {
            add(&mut h, "X-IDE-Type", "VSCode");
            add(&mut h, "X-IDE-Name", "VSCode");
            add(&mut h, "X-IDE-Version", &v.vscode_ide);
            add(&mut h, "X-Product-Version", &v.vscode_product);
            add(&mut h, "X-Env-ID", "production");
            add(&mut h, "X-Product", "SaaS");
            add(&mut h, "X-Domain", chat_domain(cred));
            add(
                &mut h,
                "User-Agent",
                format!("VSCode/{} CodeBuddy/{}", v.vscode_ide, v.vscode_product),
            );
        }
        Product::Cli => {
            add(&mut h, "X-IDE-Type", "CLI");
            add(&mut h, "X-IDE-Name", "CLI");
            add(&mut h, "X-IDE-Version", &v.cli);
            add(&mut h, "X-Product", "SaaS");
            add(&mut h, "X-Domain", chat_domain(cred));
            add(
                &mut h,
                "User-Agent",
                format!("CLI/{} CodeBuddy/{}", v.cli, v.cli),
            );
        }
    }
    add(
        &mut h,
        "X-User-Id",
        if cred.uid.is_empty() {
            "anonymous"
        } else {
            &cred.uid
        },
    );
    add(
        &mut h,
        "Authorization",
        format!("Bearer {}", cred.access_token),
    );
    if product == Product::Vscode {
        add(&mut h, "X-Request-Trace-Id", ids::uuid4_str());
    }
    add(&mut h, "X-Conversation-ID", conversation);
    add(&mut h, "X-Conversation-Request-ID", &request);
    if product != Product::Vscode {
        add(&mut h, "X-Root-Request-ID", &request);
    }
    add(&mut h, "X-Conversation-Message-ID", &message);
    add(
        &mut h,
        "X-Request-ID",
        if product == Product::Vscode {
            request
        } else {
            message
        },
    );
    if product == Product::Vscode {
        if let Some(model) = options.model.as_ref().filter(|s| !s.is_empty()) {
            add(&mut h, "X-Model-ID", model);
            let ms = (options.now * 1000.0) as i64;
            add(&mut h, "monitor_promptPrepareStartTime", ms.to_string());
            add(&mut h, "monitor_httpSendTime", ms.to_string());
        }
        if cred.realm == Realm::Intl {
            add(&mut h, "X-Private-Data", "true");
        }
    }
    if product == Product::Cli {
        add(
            &mut h,
            "X-Private-Data",
            if cred.enterprise_id.is_empty() {
                "false"
            } else {
                "true"
            },
        );
        stainless_headers(&mut h, v, true);
    }
    trace_headers(&mut h, product, options.desktop_chat_compat);
    if product == Product::Workbuddy && options.desktop_chat_compat {
        stainless_headers(&mut h, v, false);
    }
    if cred.is_api_key() {
        add(&mut h, "X-API-Key", &cred.access_token);
    }
    if !cred.enterprise_id.is_empty() {
        add(&mut h, "X-Enterprise-Id", &cred.enterprise_id);
        add(&mut h, "X-Tenant-Id", &cred.enterprise_id);
    }
    if let Some(private) = options.private_data {
        let value = private.to_string();
        if let Some((_, old)) = h.iter_mut().find(|(k, _)| k == "X-Private-Data") {
            *old = value;
        } else {
            add(&mut h, "X-Private-Data", value);
        }
    }
    validate_headers(&h)?;
    Ok(h)
}
pub fn build_chat_headers(cred: &Cred, options: &ChatOptions) -> Result<Vec<(String, String)>> {
    let mut h = vec![("Content-Type".into(), "application/json".into())];
    if cred.product == Product::Cli
        || (cred.product == Product::Workbuddy && options.desktop_chat_compat)
    {
        add(&mut h, "Accept", "application/json");
    } else {
        add(&mut h, "Accept", "application/json, text/plain, */*");
        add(&mut h, "X-Requested-With", "XMLHttpRequest");
        add(&mut h, "Origin", realm_origin(cred.realm));
        add(&mut h, "Referer", format!("{}/", realm_origin(cred.realm)));
        add(&mut h, "X-CodeBuddy-Request", "1");
        add(&mut h, "Accept-Language", language(cred.realm));
    }
    h.extend(build_identity_headers(cred, options)?);
    let device = options.device_token.as_deref().filter(|s| !s.is_empty());
    if let Some(device) = device.filter(|_| cred.realm == Realm::Cn && cred.product != Product::Cli)
    {
        add(&mut h, "X-Device-Token", device);
    } else if cred.product == Product::Vscode && cred.enterprise_id.is_empty() {
        add(&mut h, "X-Device-Token-Error", "sdk_not_initialized");
    }
    validate_headers(&h)?;
    Ok(h)
}
/// Body is already normalized by wbchat. Only product-specific wire changes are
/// applied here; input is not mutated. CLI/VSCode payloads use deterministic gzip.
pub fn build_chat_request(cred: &Cred, body: &Value, options: &ChatOptions) -> Result<OutRequest> {
    if !body.is_object() {
        return Err(Error::Invalid("chat body must be an object".into()));
    }
    let mut options = options.clone();
    if options.model.is_none() {
        options.model = body["model"].as_str().map(str::to_owned);
    }
    let mut body = body.clone();
    if cred.product == Product::Cli {
        for key in ["metadata", "conversation_id", "previous_response_id"] {
            body.as_object_mut().unwrap().shift_remove(key);
        }
        body["cache_retention"] = json!("long");
    }
    let mut bytes = pyjson::dumps_compact(&body).into_bytes();
    let mut headers = build_chat_headers(cred, &options)?;
    if cred.product != Product::Workbuddy {
        let mut encoder = GzBuilder::new()
            .mtime(0)
            .operating_system(255)
            .write(Vec::new(), Compression::best());
        encoder.write_all(&bytes)?;
        bytes = encoder.finish()?;
        add(&mut headers, "Content-Encoding", "gzip");
    }
    finish(
        "POST",
        chat_endpoint(cred, &options.request)?,
        headers,
        Some(bytes),
        HeaderCase::Exact,
        &options.request,
        false,
    )
}
pub fn build_refresh_request(cred: &Cred, options: &RequestOptions) -> Result<OutRequest> {
    if cred.is_api_key() {
        return Err(Error::Invalid("static API keys cannot be refreshed".into()));
    }
    if cred.refresh_token.is_empty() {
        return Err(Error::Invalid("missing refresh token".into()));
    }
    let mut h = common_headers(cred.realm);
    add(&mut h, "X-Refresh-Token", &cred.refresh_token);
    add(
        &mut h,
        "X-Auth-Refresh-Source",
        if cred.realm == Realm::Cn {
            "workbuddy"
        } else {
            "plugin"
        },
    );
    add(&mut h, "X-User-Id", &cred.uid);
    add(
        &mut h,
        "X-Domain",
        if cred.domain.is_empty() {
            default_domain(cred.realm)
        } else {
            &cred.domain
        },
    );
    add(&mut h, "X-CodeBuddy-Request", "1");
    add(&mut h, "Accept-Language", language(cred.realm));
    if !cred.enterprise_id.is_empty() {
        add(&mut h, "X-Enterprise-Id", &cred.enterprise_id);
    }
    finish(
        "POST",
        format!(
            "{}/v2/plugin/auth/token/refresh",
            realm_chat_base(cred.realm, options)?
        ),
        h,
        Some(b"{}".to_vec()),
        HeaderCase::Title,
        options,
        false,
    )
}
fn quote_query(value: &str) -> String {
    let mut s = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}
pub fn build_oauth_start_request(
    realm: Realm,
    platform: &str,
    options: &RequestOptions,
) -> Result<OutRequest> {
    let platform = if platform.is_empty() { "CLI" } else { platform };
    finish(
        "POST",
        format!(
            "{}/v2/plugin/auth/state?platform={}",
            realm_chat_base(realm, options)?,
            quote_query(platform)
        ),
        common_headers(realm),
        Some(b"{}".to_vec()),
        HeaderCase::Title,
        options,
        true,
    )
}
pub fn build_oauth_poll_request(
    realm: Realm,
    state: &str,
    options: &RequestOptions,
) -> Result<OutRequest> {
    if state.is_empty() {
        return Err(Error::Invalid("OAuth state is empty".into()));
    }
    finish(
        "GET",
        format!(
            "{}/v2/plugin/auth/token?state={}",
            realm_chat_base(realm, options)?,
            quote_query(state)
        ),
        common_headers(realm),
        None,
        HeaderCase::Title,
        options,
        true,
    )
}
pub fn build_oauth_account_request(
    cred: &Cred,
    state: &str,
    options: &RequestOptions,
) -> Result<OutRequest> {
    validate_cred(cred)?;
    let mut h = common_headers(cred.realm);
    add(
        &mut h,
        "Authorization",
        format!("Bearer {}", cred.access_token),
    );
    let mut options = options.clone();
    options.timeout_secs = 15.0;
    finish(
        "GET",
        format!(
            "{}/v2/plugin/login/account?state={}",
            realm_chat_base(cred.realm, &options)?,
            quote_query(state)
        ),
        h,
        None,
        HeaderCase::Title,
        &options,
        true,
    )
}
#[derive(Clone)]
pub struct OAuthStart {
    pub state: String,
    pub auth_url: String,
    pub expires_at: f64,
}
impl std::fmt::Debug for OAuthStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthStart")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}
#[derive(Debug)]
pub enum OAuthPoll {
    Pending { message: String },
    Authorized(Account),
}
fn parse_response(response: &InResponse) -> Result<Value> {
    if !(200..300).contains(&response.status) {
        return Err(Error::Upstream {
            status: response.status,
            code: response_code(response),
        });
    }
    Ok(serde_json::from_slice(&response.body)?)
}
pub fn parse_oauth_start(response: &InResponse, now: f64) -> Result<OAuthStart> {
    let v = parse_response(response)?;
    let state = v["data"]["state"].as_str().filter(|s| !s.is_empty());
    let url = v["data"]["authUrl"]
        .as_str()
        .filter(|s| s.starts_with("https://") || s.starts_with("http://"));
    match (state, url) {
        (Some(state), Some(url)) => Ok(OAuthStart {
            state: state.into(),
            auth_url: url.into(),
            expires_at: now + 600.0,
        }),
        _ => Err(Error::Invalid(
            "OAuth start returned no state/authUrl".into(),
        )),
    }
}
/// Caller owns state binding/TTL and polling; network failures are not parsed here.
pub fn parse_oauth_poll(response: &InResponse, realm: Realm, platform: &str) -> Result<OAuthPoll> {
    let v = parse_response(response)?;
    let code = crate::json_code(&v);
    if code == Some(11217) {
        return Ok(OAuthPoll::Pending {
            message: v["msg"].as_str().unwrap_or("login pending").into(),
        });
    }
    if code != Some(0) {
        return Err(Error::Upstream {
            status: response.status,
            code,
        });
    }
    let mut data = v["data"].clone();
    if !data.is_object() {
        return Err(Error::Invalid("OAuth returned no account data".into()));
    }
    data["realm"] = json!(realm.as_str());
    data["platform"] = json!(if platform.is_empty() { "CLI" } else { platform });
    data["source"] = json!("oauth");
    data["expiresAt"] = json!(normalize_epoch(&data["expiresAt"]));
    let a = Account::from_json(&data)?;
    if a.to_json()["nickname"] == "" {
        a.patch(&json!({"nickname":a.uid().chars().take(8).collect::<String>()}))?;
    }
    Ok(OAuthPoll::Authorized(a))
}
pub fn parse_oauth_nickname(response: &InResponse) -> Result<Option<String>> {
    let v = parse_response(response)?;
    Ok(v["data"]["nickname"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned))
}

/// Basic non-chat authenticated headers. No billing orchestration/network retries.
pub fn build_billing_headers(
    cred: &Cred,
    device_token: Option<&str>,
) -> Result<Vec<(String, String)>> {
    validate_cred(cred)?;
    let mut h = common_headers(cred.realm);
    add(
        &mut h,
        "Authorization",
        format!("Bearer {}", cred.access_token),
    );
    add(&mut h, "X-User-Id", &cred.uid);
    add(
        &mut h,
        "X-Domain",
        if cred.domain.is_empty() {
            default_domain(cred.realm)
        } else {
            &cred.domain
        },
    );
    add(&mut h, "X-CodeBuddy-Request", "1");
    add(&mut h, "Accept-Language", language(cred.realm));
    add(&mut h, "X-Request-ID", ids::uuid4_hex());
    add(&mut h, "X-Machine-ID", ids::machine_id(&cred.uid));
    add(&mut h, "X-Session-ID", ids::session_id(&cred.uid));
    if cred.is_api_key() {
        add(&mut h, "X-API-Key", &cred.access_token);
    }
    if cred.enterprise_id.is_empty() {
        add(&mut h, "X-No-Enterprise-Id", "1");
    } else {
        add(&mut h, "X-Enterprise-Id", &cred.enterprise_id);
        add(&mut h, "X-Tenant-Id", &cred.enterprise_id);
    }
    if cred.realm == Realm::Cn {
        add(&mut h, "X-Product", "SaaS");
    }
    if cred.product != Product::Cli {
        if let Some(token) = device_token.filter(|s| !s.is_empty()) {
            add(&mut h, "X-Device-Token", token);
        }
    }
    validate_headers(&h)?;
    Ok(h)
}
/// Optional minimal billing requests. Package aggregation/fallback scheduling is
/// intentionally left to a subsequent implementation, not hidden behind a stub.
#[derive(Clone, Copy, Debug)]
pub enum BillingOperation {
    Summary,
    Resource,
    EnterpriseUsage,
    FreePackages,
    PaidPackages,
    CheckinStatus,
    Checkin,
}
pub fn build_billing_request(
    cred: &Cred,
    operation: BillingOperation,
    body: &Value,
    options: &RequestOptions,
) -> Result<OutRequest> {
    if !body.is_object() {
        return Err(Error::Invalid("billing body must be an object".into()));
    }
    let mut h = build_billing_headers(cred, None)?;
    let path = match operation {
        BillingOperation::Summary => "/billing/meter/get-user-resource-summary",
        BillingOperation::Resource => "/v2/billing/meter/get-user-resource",
        BillingOperation::EnterpriseUsage => {
            if cred.is_api_key() {
                "/v2/billing/meter/get-enterprise-user-usage"
            } else {
                "/billing/meter/get-enterprise-user-usage"
            }
        }
        BillingOperation::FreePackages => "/billing/meter/get-user-resource-free-packages",
        BillingOperation::PaidPackages => "/billing/meter/get-user-resource-paid-packages",
        BillingOperation::CheckinStatus => "/v2/billing/meter/checkin-activity-status",
        BillingOperation::Checkin => "/v2/billing/meter/daily-checkin",
    };
    if matches!(
        operation,
        BillingOperation::Summary
            | BillingOperation::EnterpriseUsage
            | BillingOperation::FreePackages
            | BillingOperation::PaidPackages
    ) {
        add(&mut h, "X-Client-Platform", "web");
    }
    if matches!(
        operation,
        BillingOperation::Checkin | BillingOperation::CheckinStatus
    ) && (cred.realm != Realm::Cn || cred.is_api_key() || !cred.enterprise_id.is_empty())
    {
        return Err(Error::Invalid(
            "checkin requires a CN personal OAuth account".into(),
        ));
    }
    let base = if matches!(operation, BillingOperation::CheckinStatus) {
        realm_chat_base(cred.realm, options)?
    } else {
        billing_base(cred.realm).into()
    };
    finish(
        "POST",
        format!("{base}{path}"),
        h,
        Some(pyjson::dumps(body).into_bytes()),
        HeaderCase::Title,
        options,
        false,
    )
}
