//! Per-token rate-limit admission with AIMD adaptive concurrency (ADR-0003).
//!
//! Ported from the reference implementation's `rate-limit` crate as modules.
//!
//! - [`controller`]: [`RateLimitController`], the per-token state machine with
//!   `admit` / `release` / `observe`.
//! - [`headers`]: [`RateLimitHeaders`], the parsed `X-RateLimit-*` and
//!   `Retry-After` values of one response.
//! - [`probe`]: [`QuotaProbe`], the authoritative `GET /rate_limit`
//!   reconciliation hook the HTTP layer implements.
//! - [`registry`]: [`ControllerRegistry`], one controller per
//!   [`TokenFingerprint`], shared by every sync that uses that token.

pub mod controller;
pub mod headers;
pub mod probe;
pub mod registry;

pub use controller::{ControllerSnapshot, RateLimitController};
pub use headers::RateLimitHeaders;
pub use probe::{AuthoritativeQuota, QuotaProbe};
pub use registry::{ControllerRegistry, TokenFingerprint};

/// Errors that can occur in rate-limit admission.
#[derive(Debug, thiserror::Error)]
pub enum RateLimitError {
    /// No budget remains and no future reset window is known to wait for.
    #[error("rate-limit budget exhausted; no reset window to wait for")]
    BudgetExhausted,
}
