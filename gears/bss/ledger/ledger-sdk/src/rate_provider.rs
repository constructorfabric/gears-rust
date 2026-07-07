//! The FX rate-provider plugin contract (`RateProviderV1`).
//!
//! A cross-gear, GTS-versioned SDK trait (`gts.cf.bss.ledger.rate-provider.v1`,
//! mirroring [`crate::LedgerClientV1`]): an external adapter-gear (ECB primary /
//! PSP-bank fallback) implements it and registers an `Arc<dyn RateProviderV1>` in
//! the `ClientHub`; a ledger-side `RateSyncJob` pulls `fetch_latest` into the
//! local rate store. The adapter ONLY fetches — translation, triangulation, and
//! staleness all stay in the ledger. The default [`UnconfiguredRateProviderV1`]
//! is a fail-safe no-op (the store stays empty → FX-needing posts block).

use async_trait::async_trait;
use toolkit_security::SecurityContext;

/// A currency pair to fetch a rate for (ISO 4217 codes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrencyPair {
    pub base: String,
    pub quote: String,
}

/// A rate as published by a provider at a point in time. `rate_micro` is the
/// fixed-precision multiplier (functional per unit transaction × 1e6). `as_of`
/// drives the ledger's staleness rule; the provider id is recorded separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderRate {
    pub base: String,
    pub quote: String,
    pub rate_micro: i64,
    pub as_of: chrono::DateTime<chrono::Utc>,
}

/// A rate-provider failure. Semantic (the ledger maps it to a sync-job alarm, or
/// at lock time to `FX_RATE_UNAVAILABLE`); never an HTTP status here.
#[derive(Debug, thiserror::Error)]
pub enum RateProviderError {
    #[error("pair {base}->{quote} not published")]
    PairUnavailable { base: String, quote: String },
    #[error("provider unreachable: {0}")]
    Unreachable(String),
    #[error("upstream status {0}")]
    UpstreamStatus(u16),
    #[error("invalid pair: {0}")]
    InvalidPair(String),
    #[error("internal: {0}")]
    Internal(String),
}

/// The FX rate-provider plugin contract — implemented out-of-gear, resolved from
/// `ClientHub` by GTS instance. See the module docs.
#[async_trait]
pub trait RateProviderV1: Send + Sync {
    /// Stable id recorded verbatim on every `rate_snapshot.provider`; the
    /// fallback-order key. E.g. "ecb", "bank-x", "psp-stripe".
    fn provider_id(&self) -> &str;

    /// Fetch the latest published rates for the requested pairs — one round-trip.
    /// An adapter that publishes a whole table returns everything it has and OMITS
    /// pairs it cannot serve (the caller treats a missing pair as no acceptable
    /// rate). MUST NOT be called on the posting path — only the background
    /// `RateSyncJob` calls it (a provider outage fails the job, never a post).
    ///
    /// # Errors
    /// [`RateProviderError`] on an upstream failure.
    async fn fetch_latest(
        &self,
        ctx: &SecurityContext,
        pairs: &[CurrencyPair],
        request_id: &str,
    ) -> Result<Vec<ProviderRate>, RateProviderError>;

    /// Liveness probe for the sync-job reachability alarm. Default = a trivial
    /// `fetch_latest`; an adapter MAY override with a cheaper ping.
    ///
    /// # Errors
    /// [`RateProviderError`] when the provider is unreachable.
    async fn health(
        &self,
        ctx: &SecurityContext,
        request_id: &str,
    ) -> Result<(), RateProviderError> {
        self.fetch_latest(ctx, &[], request_id).await.map(|_| ())
    }
}

/// Fail-safe default until a real adapter is wired: every fetch fails, so the
/// local rate store stays empty and FX-needing posts block with
/// `FX_RATE_UNAVAILABLE` (never a silent wrong rate). Mirrors the gear's
/// `AlwaysSatisfiedObligationState` / `NoopLedgerMetrics` no-op ports.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnconfiguredRateProviderV1;

#[async_trait]
impl RateProviderV1 for UnconfiguredRateProviderV1 {
    // The trait returns `&str` (tied to `&self`) so a real adapter can return a
    // borrowed `&self.id` field; this no-op default happens to return a `'static`
    // literal, which clippy would prefer typed `&'static str` — but that would
    // not match the trait method signature. Allow the literal bound here.
    #[allow(
        clippy::unnecessary_literal_bound,
        reason = "trait signature is `-> &str` for adapters with a borrowed id; this default returns a literal"
    )]
    fn provider_id(&self) -> &str {
        "none"
    }

    async fn fetch_latest(
        &self,
        _ctx: &SecurityContext,
        _pairs: &[CurrencyPair],
        _request_id: &str,
    ) -> Result<Vec<ProviderRate>, RateProviderError> {
        Err(RateProviderError::Unreachable(
            "no FX rate adapter configured".to_owned(),
        ))
    }
}
