use crate::{Error, ErrorKind, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use percent_encoding::percent_decode_str;
use sha2::{Digest, Sha256};
use url::{Host, Url};
use wbcore::types::{Egress, HeaderCase, OutRequest};

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct PoolKey {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    // Neither raw origin credentials nor proxy userinfo are stored in the pool.
    pub proxy: [u8; 32],
    pub identity: [u8; 32],
}

pub(crate) struct Proxy {
    pub host: String,
    pub port: u16,
    pub authorization: Option<String>,
}

pub(crate) struct Prepared {
    pub key: PoolKey,
    pub authority: String,
    pub proxy: Option<Proxy>,
    pub wire: Vec<u8>,
    pub request_close: bool,
}

fn invalid() -> Error {
    Error::new(ErrorKind::InvalidRequest, "request validation")
}

pub(crate) fn token(value: &[u8]) -> bool {
    !value.is_empty()
        && value
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(b))
}

pub(crate) fn latin1(value: &str) -> Result<Vec<u8>> {
    value
        .chars()
        .map(|c| {
            let n = c as u32;
            if n > 255 || n == 127 || (n < 32 && n != 9) {
                Err(invalid())
            } else {
                Ok(n as u8)
            }
        })
        .collect()
}

fn title(name: &str) -> String {
    let mut word = false;
    name.chars()
        .map(|c| {
            let out = if word {
                c.to_ascii_lowercase()
            } else {
                c.to_ascii_uppercase()
            };
            word = c.is_ascii_alphabetic();
            out
        })
        .collect()
}

fn host(url: &Url) -> Result<String> {
    match url.host().ok_or_else(invalid)? {
        Host::Domain(name) => Ok(name.to_owned()),
        Host::Ipv4(ip) => Ok(ip.to_string()),
        Host::Ipv6(ip) => Ok(ip.to_string()),
    }
}

fn authority(host: &str, port: Option<u16>) -> String {
    let name = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    match port {
        Some(port) => format!("{name}:{port}"),
        None => name,
    }
}

fn proxy(raw: &str, target_tls: bool) -> Result<Proxy> {
    let parsed = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    };
    if raw.chars().any(|c| c.is_control()) {
        return Err(invalid());
    }
    let url = Url::parse(&parsed).map_err(|_| invalid())?;
    if url.scheme() != "http" {
        return Err(Error::new(
            ErrorKind::UnsupportedProxy,
            "proxy configuration",
        ));
    }
    let username = percent_decode_str(url.username()).collect::<Vec<_>>();
    let password = percent_decode_str(url.password().unwrap_or("")).collect::<Vec<_>>();
    let authorization = if !username.is_empty() || url.password().is_some() {
        let mut auth = username;
        auth.push(b':');
        auth.extend(password);
        Some(format!("Basic {}", STANDARD.encode(auth)))
    } else {
        None
    };
    Ok(Proxy {
        host: host(&url)?,
        // Url removes an explicitly written :80 for HTTP. Read the authority
        // suffix so https targets through an explicit port 80 stay on port 80.
        port: {
            let netloc = parsed
                .split_once("://")
                .ok_or_else(invalid)?
                .1
                .split(['/', '?', '#'])
                .next()
                .ok_or_else(invalid)?;
            let hostport = netloc.rsplit('@').next().ok_or_else(invalid)?;
            let explicit = if hostport.starts_with('[') {
                hostport
                    .split_once(']')
                    .and_then(|(_, suffix)| suffix.strip_prefix(':'))
            } else {
                hostport.rsplit_once(':').map(|(_, port)| port)
            };
            match explicit {
                Some(port) if !port.is_empty() => port.parse::<u16>().map_err(|_| invalid())?,
                Some(_) => return Err(invalid()),
                None => url.port().unwrap_or(if target_tls { 443 } else { 80 }),
            }
        },
        authorization,
    })
}

fn digest_part(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_be_bytes());
    hash.update(value);
}

impl Prepared {
    pub fn new(request: &OutRequest) -> Result<Self> {
        if !token(request.method.as_bytes()) || request.method.eq_ignore_ascii_case("CONNECT") {
            return Err(invalid());
        }
        if request.url.chars().any(|c| c.is_control()) {
            return Err(invalid());
        }
        let url = Url::parse(&request.url).map_err(|_| invalid())?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid());
        }
        let host = host(&url)?;
        let port = url.port_or_known_default().ok_or_else(invalid)?;
        let host_header = authority(&host, url.port());
        let tunnel_authority = authority(&host, Some(port));
        let raw_proxy = match &request.egress {
            Egress::Direct => "",
            Egress::Proxy(value) => value.as_str(),
        };
        let proxy = if raw_proxy.is_empty() {
            None
        } else {
            Some(proxy(raw_proxy, url.scheme() == "https")?)
        };
        let mut identity = Sha256::new();
        let mut caller = Vec::new();
        let mut request_close = false;
        let mut seen_reserved = std::collections::HashSet::new();
        for (name, value) in &request.headers {
            if !token(name.as_bytes()) {
                return Err(invalid());
            }
            let encoded = latin1(value)?;
            let lower = name.to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "authorization"
                    | "x-refresh-token"
                    | "x-user-id"
                    | "x-api-key"
                    | "cookie"
                    | "x-device-token"
                    | "x-enterprise-id"
                    | "x-tenant-id"
            ) {
                digest_part(&mut identity, lower.as_bytes());
                digest_part(&mut identity, &encoded);
            }
            if matches!(
                lower.as_str(),
                "host" | "content-length" | "accept-encoding" | "connection"
            ) {
                if !seen_reserved.insert(lower.clone()) {
                    return Err(invalid());
                }
                match lower.as_str() {
                    "host" if !value.eq_ignore_ascii_case(&host_header) => return Err(invalid()),
                    "content-length" => {
                        if value.parse::<usize>().ok()
                            != Some(request.body.as_ref().map_or(0, Vec::len))
                            || !value.bytes().all(|b| b.is_ascii_digit())
                        {
                            return Err(invalid());
                        }
                    }
                    "accept-encoding" if !value.eq_ignore_ascii_case("identity") => {
                        return Err(invalid())
                    }
                    "connection" => {
                        if value.eq_ignore_ascii_case("close") {
                            request_close = true;
                        } else if !value.eq_ignore_ascii_case("keep-alive") {
                            return Err(invalid());
                        }
                    }
                    _ => {}
                }
                continue;
            }
            // Never forward caller-supplied proxy credentials to an origin, and
            // never accept alternate framing, upgrades or a 100-continue wait.
            if matches!(
                lower.as_str(),
                "proxy-authorization"
                    | "proxy-connection"
                    | "transfer-encoding"
                    | "upgrade"
                    | "expect"
            ) {
                return Err(invalid());
            }
            let name = match request.case {
                HeaderCase::Exact => name.clone(),
                HeaderCase::Title => title(name),
            };
            caller.extend_from_slice(name.as_bytes());
            caller.extend_from_slice(b": ");
            caller.extend_from_slice(&encoded);
            caller.extend_from_slice(b"\r\n");
        }
        let mut path = url.path().to_owned();
        if path.is_empty() {
            path.push('/');
        }
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }
        let target = if proxy.is_some() && url.scheme() == "http" {
            format!("http://{host_header}{path}")
        } else {
            path
        };
        let mut wire = format!(
            "{} {} HTTP/1.1\r\nAccept-Encoding: identity\r\n",
            request.method, target
        )
        .into_bytes();
        if let Some(body) = &request.body {
            wire.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
        } else if seen_reserved.contains("content-length") {
            wire.extend_from_slice(b"Content-Length: 0\r\n");
        }
        wire.extend_from_slice(format!("Host: {host_header}\r\n").as_bytes());
        wire.extend(caller);
        if url.scheme() == "http" {
            if let Some(auth) = proxy.as_ref().and_then(|p| p.authorization.as_ref()) {
                wire.extend_from_slice(format!("Proxy-Authorization: {auth}\r\n").as_bytes());
            }
        }
        wire.extend_from_slice(if request_close {
            b"Connection: close\r\n\r\n"
        } else {
            b"Connection: keep-alive\r\n\r\n"
        });
        if wire.len() > crate::connection::MAX_HEADERS {
            return Err(invalid());
        }
        Ok(Self {
            key: PoolKey {
                scheme: url.scheme().to_owned(),
                host,
                port,
                proxy: Sha256::digest(raw_proxy.as_bytes()).into(),
                identity: identity.finalize().into(),
            },
            authority: tunnel_authority,
            proxy,
            wire,
            request_close,
        })
    }
}
