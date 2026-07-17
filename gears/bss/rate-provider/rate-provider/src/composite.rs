//! `CompositeRateProvider` — ordered fallback over the discovered source
//! plugins. Tries its ordered sources and returns the FIRST whole successful
//! document (never a merge). Records the serving source index so `provider_id()`
//! reports the true upstream.
//!
//! Provenance correctness depends on call order: `provider_id()` reflects
//! `last_served`, set during `fetch_latest`. This is correct because the ledger's
//! `RateSyncJob` calls `fetch_latest` before `provider_id` in the same pass
//! (`rate_sync.rs`: fetch at line 111, `provider_id` at 149), under a single
//! non-concurrent ticker. If that job is ever made concurrent or reordered, this
//! must be revisited (or the ledger changed so `ProviderRate` carries its own
//! source id).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use bss_ledger_sdk::{CurrencyPair, ProviderRate, RateProviderError, RateProviderV1};
use toolkit_security::SecurityContext;

/// Ordered composite over one-or-more source plugins with last-served provenance.
pub struct CompositeRateProvider {
    /// Ordered sources; index 0 is the primary. Guaranteed non-empty by the core gear.
    sources: Vec<Arc<dyn RateProviderV1>>,
    /// Index of the source that produced the most recent successful document.
    last_served: AtomicUsize,
}

impl CompositeRateProvider {
    /// Build a composite over an ordered, NON-EMPTY source list.
    ///
    /// # Panics
    /// Only in debug builds, if `sources` is empty — the core gear guarantees
    /// non-emptiness before constructing (see `discovery.rs::discover`), so
    /// this is a defensive invariant check, not a normal error path. There is
    /// no release-mode panic: `provider_id()` never indexes an empty list (see
    /// below), and `fetch_latest`/`health` degrade to an "empty composite"
    /// error, never a panic.
    #[must_use]
    pub fn new(sources: Vec<Arc<dyn RateProviderV1>>) -> Self {
        debug_assert!(!sources.is_empty(), "composite requires >= 1 source");
        Self {
            sources,
            last_served: AtomicUsize::new(0),
        }
    }
}

/// Reported by `provider_id()` in the (invariant-violating) case of an empty
/// source list — mirrors the ledger's own `UnconfiguredRateProviderV1` sentinel
/// rather than panicking on an out-of-bounds index.
const NO_SOURCES_SENTINEL: &str = "none";

#[async_trait]
impl RateProviderV1 for CompositeRateProvider {
    fn provider_id(&self) -> &str {
        // `debug_assert!` in `new` only checks non-emptiness in debug builds, so
        // this must not assume it holds: `.get(idx)` (never a bare `[idx]`)
        // keeps a release-mode empty list from panicking on an out-of-bounds index.
        let idx = self.last_served.load(Ordering::Relaxed);
        self.sources
            .get(idx)
            .map_or(NO_SOURCES_SENTINEL, |s| s.provider_id())
    }

    async fn fetch_latest(
        &self,
        ctx: &SecurityContext,
        pairs: &[CurrencyPair],
        request_id: &str,
    ) -> Result<Vec<ProviderRate>, RateProviderError> {
        let mut last_err: Option<RateProviderError> = None;
        for (i, source) in self.sources.iter().enumerate() {
            match source.fetch_latest(ctx, pairs, request_id).await {
                Ok(doc) => {
                    self.last_served.store(i, Ordering::Relaxed);
                    return Ok(doc);
                }
                Err(e) => {
                    tracing::warn!(
                        source = source.provider_id(),
                        error = %e,
                        "bss-rate-provider: source fetch failed; trying the next source"
                    );
                    last_err = Some(e);
                }
            }
        }
        Err(last_err
            .unwrap_or_else(|| RateProviderError::Internal("composite has no sources".to_owned())))
    }

    async fn health(
        &self,
        ctx: &SecurityContext,
        request_id: &str,
    ) -> Result<(), RateProviderError> {
        let mut last_err: Option<RateProviderError> = None;
        for source in &self.sources {
            match source.health(ctx, request_id).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!(
                        source = source.provider_id(),
                        error = %e,
                        "bss-rate-provider: source health probe failed; trying the next source"
                    );
                    last_err = Some(e);
                }
            }
        }
        Err(last_err
            .unwrap_or_else(|| RateProviderError::Internal("composite has no sources".to_owned())))
    }
}

#[cfg(test)]
#[path = "composite_tests.rs"]
mod tests;
