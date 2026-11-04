//! [`QuotaProbe`], the authoritative quota reconciliation hook.
//!
//! GitHub's inline `X-RateLimit-Remaining` response headers are documented to be
//! inaccurate when many requests are issued concurrently: each serving node
//! decrements its own view optimistically, so under a burst the inline
//! `remaining` can collapse far below the reconciled, authoritative count that
//! GitHub actually enforces. The authoritative figure is available, without
//! consuming quota, from the `GET /rate_limit` endpoint.
//!
//! [`QuotaProbe`] lets the [`super::controller::RateLimitController`] consult
//! that authoritative endpoint before committing to a long sleep-until-reset,
//! without this module depending on an HTTP stack: the GitHub client
//! implements the trait and injects it via
//! [`super::controller::RateLimitController::set_quota_probe`].

use chrono::{DateTime, Utc};

/// Authoritative core-pool quota snapshot from the free `GET /rate_limit`
/// endpoint, used to reconcile the unreliable inline `X-RateLimit-Remaining`
/// header under high request concurrency.
#[derive(Debug, Clone, Copy)]
pub struct AuthoritativeQuota {
    /// Core REST calls remaining in the current window.
    pub remaining: u32,
    /// Total hourly core quota.
    pub limit: u32,
    /// When the current core window resets, if known.
    pub reset_at: Option<DateTime<Utc>>,
}

/// Fetches the authoritative core-pool quota for a token.
///
/// Implemented by the HTTP layer (which owns the GitHub client) and injected
/// into the controller so the gate can reconcile against `GET /rate_limit`
/// before pausing. The call must **not** consume quota.
#[async_trait::async_trait]
pub trait QuotaProbe: Send + Sync + std::fmt::Debug {
    /// Fetch the authoritative `core` quota via `GET /rate_limit`.
    ///
    /// Returns `None` on any network or parse failure so the caller can fall
    /// back to the inline headers.
    async fn fetch_core_quota(&self) -> Option<AuthoritativeQuota>;
}
