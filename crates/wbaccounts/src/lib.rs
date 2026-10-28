//! Thread-safe account storage, scheduling and sans-network upstream requests.
//!
//! Gateway integration: load an [`AccountPool`], set per-account [`Guards`], then
//! [`AccountPool::acquire`] with [`PickOptions`]. Keep the returned [`AccountLease`]
//! alive until the upstream stream closes (all error/cancellation paths release on
//! Drop). Build requests with the account's [`Account::cred`] snapshot and execute
//! them in the gateway. [`Readiness::NeedsRefresh`] is not dispatchable: acquire
//! [`Account::try_refresh_slot`], execute [`build_refresh_request`], then call
//! [`Account::apply_refresh_response`]. Retry/network policy belongs to the gateway.
//!
//! [`Account::to_json`] and [`export_document`] with secrets enabled contain live
//! credentials. Only [`Account::public_metadata`] is intended for public display.
//! Account Debug output is redacted. 11140 is always terminal; restrictions and
//! live model caps survive saves, imports, refresh and late concurrent successes.
//! There is deliberately no automatic product switching or restriction bypass.

mod account;
mod errors;
mod pool;
mod requests;

pub use account::*;
pub use errors::*;
pub use pool::*;
pub use requests::*;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    Invalid(String),
    Upstream { status: u16, code: Option<i64> },
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "account file I/O: {e}"),
            Self::Json(_) => f.write_str("invalid account/upstream JSON"),
            Self::Invalid(s) => f.write_str(s),
            Self::Upstream { status, code } => write!(f, "upstream HTTP {status} (code {code:?})"),
        }
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests;
