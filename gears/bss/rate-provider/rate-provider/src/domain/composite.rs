//! `CompositeRateProvider` — ordered fallback over the discovered source
//! plugins. Tries its ordered sources and returns the FIRST whole successful
//! document (never a merge).
//!
//! Provenance is NOT tracked here. Each source stamps its own id onto every
//! [`ProviderRate`] it returns, so the serving upstream travels with the data
//! and the ledger records it per row.
//!
//! `provider_id()` is the composite's own constant identity, never "whoever
//! served last": it carries no information about which `fetch_latest` it
//! relates to, and the composite is registered process-wide in `ClientHub`, so
//! a second consumer fetching in between would make the ledger stamp the wrong
//! source onto financial records.

use std::sync::Arc;

use async_trait::async_trait;
use bss_ledger_sdk::{CurrencyPair, ProviderRate, RateProviderError, RateProviderV1};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

/// Ordered composite over one-or-more source plugins.
///
/// A domain type: it holds only the source *ports* (`dyn RateProviderV1`) and the
/// fallback policy over them — no transport, registry, or service-locator type.
/// `#[domain_model]` enforces that at compile time (DE0309).
#[domain_model]
pub struct CompositeRateProvider {
    /// Stable identity of the composite itself (the core gear's configured id) —
    /// used for log/alarm attribution, never as a rate's provenance.
    id: String,
    /// Ordered sources; index 0 is the primary. Guaranteed non-empty by the core gear.
    sources: Vec<Arc<dyn RateProviderV1>>,
}

impl CompositeRateProvider {
    /// Build a composite over an ordered, NON-EMPTY source list.
    ///
    /// # Panics
    /// Only in debug builds, if `sources` is empty — the core gear guarantees
    /// non-emptiness before constructing (see `discovery.rs::discover`), so this
    /// is a defensive invariant check. In release, `provider_id()` returns the
    /// configured id without touching the list and `fetch_latest`/`health`
    /// degrade to an "empty composite" error.
    #[must_use]
    pub fn new(id: String, sources: Vec<Arc<dyn RateProviderV1>>) -> Self {
        debug_assert!(!sources.is_empty(), "composite requires >= 1 source");
        Self { id, sources }
    }
}

#[async_trait]
impl RateProviderV1 for CompositeRateProvider {
    fn provider_id(&self) -> &str {
        &self.id
    }

    async fn fetch_latest(
        &self,
        ctx: &SecurityContext,
        pairs: &[CurrencyPair],
        request_id: &str,
    ) -> Result<Vec<ProviderRate>, RateProviderError> {
        let mut last_err: Option<RateProviderError> = None;
        for source in &self.sources {
            match source.fetch_latest(ctx, pairs, request_id).await {
                // As-is: each rate already carries the serving source's id, so
                // there is nothing for the composite to stamp or record.
                Ok(doc) => return Ok(doc),
                Err(e) => {
                    // Carries `request_id` so every attempt of one tick can be
                    // collected back together. Only the LAST error reaches the
                    // caller, so these log lines are the only place the earlier
                    // sources' failures survive.
                    tracing::warn!(
                        source = source.provider_id(),
                        request_id = request_id,
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
                        request_id = request_id,
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
