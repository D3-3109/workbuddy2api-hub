//! Sans-IO Chat Completions request normalization and output state machine.
//!
//! [`normalize_request`] accepts the client JSON, a settings/config snapshot and
//! catalog metadata as `serde_json::Value`; no settings/catalog crate or network
//! access is needed. [`ChatStream`] consumes upstream SSE **lines**, returning
//! complete `data: ...\n\n` frames. Call `finish` only on a clean transport EOF,
//! `abort` on an upstream read failure, or `client_aborted` on a downstream
//! disconnect. These terminal operations are idempotent. After successful
//! `finish`, `completion()` exposes the non-stream result built by wbcore's
//! Aggregator. Accounting is available throughout, including after failure.
//!
//! Intentional baseline divergences: message text, reasoning, tool arguments
//! and fingerprints are never sanitized to evade upstream channel/WAF rules.
//! There is no content-review degradation, automatic retry, or prompt rewrite
//! based on an upstream rejection. Only an explicitly configured prompt mode
//! can replace/append a system prompt. Invalid message shapes return HTTP-400
//! compatible errors instead of panicking. All error endings (including empty
//! streams and finish_reason=error) omit DONE. Stream tool indices are unique
//! across recovered batches and structured calls, rather than restarting at 0.
//! Internal timing/accounting is never added to client JSON.

mod normalize;
mod stream;

pub use normalize::{
    background_request_reason, build_upstream_body, normalize_request, resolve_model, ChatError,
    NormalizedRequest,
};
pub use stream::{ChatStream, StreamAccounting};

#[cfg(test)]
mod tests;
