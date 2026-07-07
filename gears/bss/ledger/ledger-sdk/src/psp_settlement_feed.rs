//! The PSP settlement-report control feed (`PspSettlementFeedV1`).
//!
//! One of the three launch-blocking control feeds of Slice 7 Phase 3 (design §4.3 /
//! N-recon-1): the PSP (or its adapter) reports the net settled amount for a
//! `(tenant, period)`, and the ledger's close gate reconciles it against recorded
//! settlements. A control feed ONLY — never a posting source (design §1.2). The
//! default [`UnconfiguredPspSettlementFeedV1`] is a fail-safe no-op (returns `None` ⇒
//! the settlement reconciliation is inert until the feed lands; design §0 decision 3),
//! mirroring [`crate::UnconfiguredRateProviderV1`].

use async_trait::async_trait;
use uuid::Uuid;

use crate::issued_invoice_manifest::ControlFeedError;

/// A PSP settlement report for a `(tenant, period)` — the net settled amount the PSP
/// reconciled, as a control feed (never a posting source).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PspSettlementReport {
    /// External PSP report identity (idempotency grain for ingest).
    pub report_id: String,
    /// Net settled amount in minor units the PSP reports for the period (net of refunds/returns).
    pub settled_minor: i64,
    /// ISO-4217 currency of the report.
    pub currency: String,
}

/// Read port for the PSP settlement report (call-driven; the ledger never pulls a
/// bus on the post path). The fail-safe default returns `None` ⇒ the settlement
/// reconciliation is inert (design §0 decision 3).
#[async_trait]
pub trait PspSettlementFeedV1: Send + Sync {
    /// The PSP settlement report for `(tenant, period)`, or `None` when not available.
    ///
    /// # Errors
    /// [`ControlFeedError`] on a configured-feed failure.
    async fn settlement_report(
        &self,
        tenant: Uuid,
        period: &str,
    ) -> Result<Option<PspSettlementReport>, ControlFeedError>;
}

/// Fail-safe default: no report ⇒ `None` ⇒ settlement reconciliation inert (mirrors
/// `UnconfiguredRateProviderV1`).
#[derive(Debug, Default, Clone, Copy)]
pub struct UnconfiguredPspSettlementFeedV1;

#[async_trait]
impl PspSettlementFeedV1 for UnconfiguredPspSettlementFeedV1 {
    async fn settlement_report(
        &self,
        _tenant: Uuid,
        _period: &str,
    ) -> Result<Option<PspSettlementReport>, ControlFeedError> {
        Ok(None)
    }
}
