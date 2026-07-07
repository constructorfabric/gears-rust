//! The issued-invoice manifest control feed (`IssuedInvoiceManifestV1`).
//!
//! One of the three launch-blocking control feeds of Slice 7 Phase 3 (design §4.3 /
//! N-recon-1): the Invoice/Orchestration service publishes the authoritative set of
//! issued invoiceIds a `(tenant, period)` was billed for, and the ledger's
//! invoice-completeness check reads it back at close. A control feed ONLY — never a
//! posting source (design §1.2). Call-driven: the ledger never pulls a bus on the
//! post path. The default [`UnconfiguredIssuedInvoiceManifestV1`] is a fail-safe no-op
//! (returns `None` ⇒ the completeness check is inert until the feed lands; design §0
//! decision 3), mirroring [`crate::UnconfiguredRateProviderV1`].

use async_trait::async_trait;
use uuid::Uuid;

/// A configured control feed failed (unreachable / malformed). A configured-but-failing
/// feed fails the close gate loud (design §0 decision 3), never silently passes.
#[derive(Debug, thiserror::Error)]
pub enum ControlFeedError {
    #[error("control feed unavailable: {0}")]
    Unavailable(String),
}

/// The independent issued-invoice manifest a `(tenant, period)` was billed for —
/// the Invoice/Orchestration control feed (design §4.3 / N-recon-1). Control feed
/// ONLY, never a posting source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedInvoiceManifest {
    /// The authoritative set of issued invoiceIds for the period.
    pub invoice_ids: Vec<String>,
    /// Control total: count of issued invoices (`== invoice_ids.len()` on a consistent feed).
    pub count: u64,
    /// Control total: summed gross amount in minor units.
    pub gross_total_minor: i64,
}

/// Read port for the issued-invoice manifest (call-driven; the ledger never pulls a
/// bus on the post path). The fail-safe default returns `None` ⇒ the
/// invoice-completeness check is inert (design §0 decision 3).
#[async_trait]
pub trait IssuedInvoiceManifestV1: Send + Sync {
    /// The latest manifest the owning service published for `(tenant, period)`, or
    /// `None` when no manifest is available (feed not configured / nothing pushed yet).
    ///
    /// # Errors
    /// [`ControlFeedError`] when a CONFIGURED feed is unreachable / errors (the gate
    /// then fails loud, never silently passes).
    async fn latest_manifest(
        &self,
        tenant: Uuid,
        period: &str,
    ) -> Result<Option<IssuedInvoiceManifest>, ControlFeedError>;
}

/// Fail-safe default: no manifest ⇒ `None` ⇒ invoice-completeness inert (mirrors
/// `UnconfiguredRateProviderV1`).
#[derive(Debug, Default, Clone, Copy)]
pub struct UnconfiguredIssuedInvoiceManifestV1;

#[async_trait]
impl IssuedInvoiceManifestV1 for UnconfiguredIssuedInvoiceManifestV1 {
    async fn latest_manifest(
        &self,
        _tenant: Uuid,
        _period: &str,
    ) -> Result<Option<IssuedInvoiceManifest>, ControlFeedError> {
        Ok(None)
    }
}
