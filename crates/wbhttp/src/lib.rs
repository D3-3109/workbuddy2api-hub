//! Secure, ordered HTTP/1.1 transport for `wbcore::types::OutRequest`.
//!
//! `Client` is cheap to clone and shares its TLS context and bounded idle pool.
//! `execute` returns all HTTP statuses without redirecting or retrying a request.
//! Consume a response with `next_chunk`, `collect`, or `into_in_response`. Dropping
//! an incomplete response closes its socket. The request timeout covers connection
//! establishment, writes and response headers; `Response::set_read_timeout` can
//! select a different streaming idle timeout after receiving the headers.
//! Errors and Debug implementations deliberately omit URLs, headers and secrets.

mod connection;
mod request;
mod response;

pub use response::Response;

use connection::{Connection, Pool};
use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode, SslVersion};
use parking_lot::Mutex;
use std::{fmt, sync::Arc, time::Duration};
use tokio::time::timeout;
use wbcore::types::OutRequest;

pub type Result<T> = std::result::Result<T, Error>;

/// Stable, secret-free transport error classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    InvalidRequest,
    UnsupportedProxy,
    TlsConfiguration,
    Tls,
    Connect,
    Proxy,
    Io,
    Timeout,
    Protocol,
    Truncated,
    BodyTooLarge,
    Cancelled,
}

/// Contains no underlying error text: such text can contain credentials or URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub phase: &'static str,
    transmission_possible: bool,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, phase: &'static str) -> Self {
        Self {
            kind,
            phase,
            transmission_possible: false,
        }
    }

    pub(crate) fn transmitted(mut self) -> Self {
        self.transmission_possible = true;
        self
    }

    /// True only for network failures before any origin request write was begun.
    /// The client itself never replays, including on stale pooled connections.
    pub fn safe_to_retry(&self) -> bool {
        !self.transmission_possible
            && matches!(
                self.kind,
                ErrorKind::Connect | ErrorKind::Io | ErrorKind::Timeout
            )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HTTP transport {:?} during {}", self.kind, self.phase)
    }
}
impl std::error::Error for Error {}

pub(crate) fn duration(seconds: f64) -> Result<Duration> {
    if !seconds.is_finite() || seconds <= 0.0 || seconds > 86_400.0 {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "timeout configuration",
        ));
    }
    let value = Duration::try_from_secs_f64(seconds)
        .map_err(|_| Error::new(ErrorKind::InvalidRequest, "timeout configuration"))?;
    if value.is_zero() {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "timeout configuration",
        ));
    }
    Ok(value)
}

// Vendored/cross-compiled OpenSSL can embed its build-host installation prefix.
// Keep its normal default/environment lookup, but supplement it with a runtime
// bundle on Unix rather than relying on that prefix existing on the deployed OS.
#[cfg(any(unix, test))]
const UNIX_CA_FILES: &[&str] = &[
    "/etc/ssl/certs/ca-certificates.crt", // Debian, Ubuntu, Alpine
    "/etc/pki/tls/certs/ca-bundle.crt",   // Fedora, RHEL, CentOS
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "/etc/ssl/ca-bundle.pem",      // openSUSE
    "/etc/ssl/cert.pem",           // BSD, macOS
    "/usr/local/etc/ssl/cert.pem", // FreeBSD packages
];

#[cfg(any(unix, test))]
fn runtime_ca_file<'a>(
    cert_file: Option<&std::path::Path>,
    cert_dir: Option<&std::path::Path>,
    candidates: &'a [&'a str],
    mut is_file: impl FnMut(&std::path::Path) -> bool,
) -> Option<&'a std::path::Path> {
    // Respect explicitly configured trust sources, even if missing/empty. Do not
    // silently broaden an operator's SSL_CERT_FILE/SSL_CERT_DIR trust policy.
    if cert_file.is_some() || cert_dir.is_some() {
        return None;
    }
    candidates
        .iter()
        .map(std::path::Path::new)
        .find(|path| is_file(path))
}

struct Inner {
    tls: SslConnector,
    pool: Mutex<Pool>,
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl Client {
    /// OpenSSL peer/hostname verification, TLS >= 1.2, system trust plus the
    /// optional PEM CA file named by `WB_CA_BUNDLE`. On Unix, discovers standard
    /// runtime CA bundles when `SSL_CERT_FILE`/`SSL_CERT_DIR` are not set, including
    /// for vendored cross-builds. Never disables verification.
    pub fn new() -> Result<Self> {
        Self::with_ca_bundle(
            std::env::var_os("WB_CA_BUNDLE")
                .map(std::path::PathBuf::from)
                .as_deref(),
        )
    }

    /// Same secure defaults as `new`, with an explicit additional CA file.
    /// `None` uses system trust only. Useful for deterministic private-PKI tests.
    pub fn with_ca_bundle(bundle: Option<&std::path::Path>) -> Result<Self> {
        let config_error = |_| Error::new(ErrorKind::TlsConfiguration, "TLS configuration");
        let mut tls = SslConnector::builder(SslMethod::tls_client()).map_err(config_error)?;
        tls.set_verify(SslVerifyMode::PEER);
        tls.set_min_proto_version(Some(SslVersion::TLS1_2))
            .map_err(config_error)?;
        tls.set_default_verify_paths().map_err(config_error)?;
        let cert_file = std::env::var_os("SSL_CERT_FILE").map(std::path::PathBuf::from);
        if let Some(path) = cert_file.as_deref() {
            // Default-path loading may silently ignore a missing explicit file.
            // Load it explicitly as well so bad configuration fails closed.
            tls.set_ca_file(path).map_err(config_error)?;
        }
        #[cfg(unix)]
        {
            let cert_dir = std::env::var_os("SSL_CERT_DIR").map(std::path::PathBuf::from);
            if let Some(path) = runtime_ca_file(
                cert_file.as_deref(),
                cert_dir.as_deref(),
                UNIX_CA_FILES,
                std::path::Path::is_file,
            ) {
                tls.set_ca_file(path).map_err(config_error)?;
            }
        }
        if let Some(path) = bundle {
            // WB_CA_BUNDLE / with_ca_bundle remains additive to system trust.
            tls.set_ca_file(path).map_err(config_error)?;
        }
        // Standard OpenSSL cipher/group/signature defaults. No impersonation,
        // custom extension ordering or ALPN (this transport only speaks HTTP/1).
        Ok(Self {
            inner: Arc::new(Inner {
                tls: tls.build(),
                pool: Mutex::new(Pool::default()),
            }),
        })
    }

    /// Execute exactly once. Non-2xx responses are returned normally. Redirects
    /// are not followed, avoiding credential forwarding and unsafe POST replay.
    pub async fn execute(&self, request: OutRequest) -> Result<Response> {
        let prepared = request::Prepared::new(&request)?;
        let wait = duration(request.timeout_secs)?;
        let pooled = self.inner.pool.lock().take(&prepared.key);
        let mut connection = match pooled {
            Some(connection) => connection,
            None => timeout(wait, Connection::connect(&prepared, &self.inner.tls, wait))
                .await
                .map_err(|_| Error::new(ErrorKind::Timeout, "connection establishment"))??,
        };
        // Even a failed first TLS write might have emitted data. Conservatively
        // mark every failure from this point non-retryable rather than guessing.
        connection
            .write_request(&prepared.wire, request.body.as_deref(), wait)
            .await
            .map_err(Error::transmitted)?;
        let head = connection
            .read_head(wait)
            .await
            .map_err(Error::transmitted)?;
        Response::new(
            head,
            connection,
            self.inner.clone(),
            prepared.key,
            wait,
            request.method.eq_ignore_ascii_case("HEAD"),
            prepared.request_close,
        )
        .map_err(Error::transmitted)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod ca_path_tests {
    use super::{runtime_ca_file, UNIX_CA_FILES};
    use std::path::Path;

    #[test]
    fn runtime_discovery_prefers_debian_alpine_then_redhat_bundle() {
        assert_eq!(
            runtime_ca_file(None, None, UNIX_CA_FILES, |_| true),
            Some(Path::new("/etc/ssl/certs/ca-certificates.crt")),
        );
        assert_eq!(
            runtime_ca_file(None, None, UNIX_CA_FILES, |path| {
                path == Path::new("/etc/pki/tls/certs/ca-bundle.crt")
            }),
            Some(Path::new("/etc/pki/tls/certs/ca-bundle.crt")),
        );
        assert!(runtime_ca_file(None, None, UNIX_CA_FILES, |_| false).is_none());
    }

    #[test]
    fn explicit_file_or_directory_disables_runtime_discovery() {
        for (file, dir) in [
            (Some(Path::new("/private/ca.pem")), None),
            (Some(Path::new("")), None),
            (None, Some(Path::new("/private/certs"))),
            (None, Some(Path::new(""))),
        ] {
            assert!(runtime_ca_file(file, dir, UNIX_CA_FILES, |_| {
                panic!("must not probe standard bundles with explicit trust settings")
            })
            .is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn runtime_discovery_uses_existing_files_not_directories() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("wbhttp-ca-path-{}-{unique}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let missing = directory.join("missing.pem");
        let bundle = directory.join("ca.pem");
        std::fs::write(&bundle, b"path selection fixture").unwrap();
        let candidates = [
            missing.to_str().unwrap(),
            directory.to_str().unwrap(),
            bundle.to_str().unwrap(),
        ];
        assert_eq!(
            runtime_ca_file(None, None, &candidates, Path::is_file),
            Some(bundle.as_path()),
        );
        std::fs::remove_file(bundle).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
