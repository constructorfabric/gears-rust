//! The bill-run-finished control feed (`BillRunFinishedV1`).
//!
//! One of the three launch-blocking control feeds of Slice 7 Phase 3 (design §4.3 /
//! N-recon-1): the owning Orchestration asserts whether a `(tenant, period)`'s
//! bill-run has finished, and the ledger's close gate reads it back. A control feed
//! ONLY — never a posting source (design §1.2). The default
//! [`UnconfiguredBillRunFinishedV1`] is a fail-safe no-op (returns `None` ⇒ the
//! bill-run assertion is inert until the feed lands; design §0 decision 3), mirroring
//! [`crate::UnconfiguredRateProviderV1`].

use async_trait::async_trait;
use uuid::Uuid;

use crate::issued_invoice_manifest::ControlFeedError;

/// Read port for the owning Orchestration's bill-run-finished assertion (call-driven;
/// the ledger never pulls a bus on the post path). The fail-safe default returns
/// `None` ⇒ the assertion is inert (design §0 decision 3).
#[async_trait]
pub trait BillRunFinishedV1: Send + Sync {
    /// `Some(true)`/`Some(false)` when the owning Orchestration asserted the period's
    /// bill-run state; `None` when not asserted (feed not configured).
    ///
    /// # Errors
    /// [`ControlFeedError`] on a configured-feed failure.
    async fn is_finished(
        &self,
        tenant: Uuid,
        period: &str,
    ) -> Result<Option<bool>, ControlFeedError>;
}

/// Fail-safe default: not asserted ⇒ `None` ⇒ the bill-run check inert (mirrors
/// `UnconfiguredRateProviderV1`).
#[derive(Debug, Default, Clone, Copy)]
pub struct UnconfiguredBillRunFinishedV1;

#[async_trait]
impl BillRunFinishedV1 for UnconfiguredBillRunFinishedV1 {
    async fn is_finished(
        &self,
        _tenant: Uuid,
        _period: &str,
    ) -> Result<Option<bool>, ControlFeedError> {
        Ok(None)
    }
}
