//! Shared building blocks for the workbuddy2api Rust port.

pub mod aggregate;
pub mod ids;
pub mod pyjson;
pub mod pyval;
pub mod sse;
pub mod timeutil;
pub mod toolcalls;
pub mod types;
pub mod usage;

/// Latest upstream version reviewed for selective compatibility updates.
pub const VERSION: &str = "1.8.0";
pub const RUST_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BASELINE_COMMIT: &str = "477ba36ceea78ec4252c47a68e94a5a1612bab0b";
pub const SYNCED_UPSTREAM_COMMIT: &str = "4f7ae6f2efcae5e6ae85aefa732035206044514b";
pub const SERVER_VERSION: &str = concat!("wb2api-rust/", env!("CARGO_PKG_VERSION"));
