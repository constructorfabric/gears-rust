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

use crate::PostedMoney;
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
    /// Exact gross totals, one per currency, sorted by currency code. An empty
    /// manifest may have no totals.
    pub gross_totals: GrossTotals,
}

/// A manifest's per-currency control totals were rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrossTotalsError {
    /// Two totals name the same currency code (at any scale): one bucket per
    /// currency is the comparison contract.
    #[error("gross_totals repeats currency {0}")]
    DuplicateCurrency(String),
}

/// Per-currency control totals: at most one per currency code (whatever its
/// scale), kept sorted by code. Both rules hold by construction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrossTotals(Vec<PostedMoney>);

impl GrossTotals {
    /// Sort `totals` by currency code and reject a repeated code.
    ///
    /// # Errors
    /// [`GrossTotalsError::DuplicateCurrency`] when two totals name the same
    /// currency code, even with different scales.
    pub fn try_new(mut totals: Vec<PostedMoney>) -> Result<Self, GrossTotalsError> {
        totals.sort_by(|a, b| a.currency().code().cmp(b.currency().code()));
        if let Some(pair) = totals
            .windows(2)
            .find(|pair| pair[0].currency().code() == pair[1].currency().code())
        {
            return Err(GrossTotalsError::DuplicateCurrency(
                pair[0].currency().code().to_owned(),
            ));
        }
        Ok(Self(totals))
    }

    /// The totals, sorted by currency code.
    #[must_use]
    pub fn as_slice(&self) -> &[PostedMoney] {
        &self.0
    }

    /// The total for `currency`, if the manifest carries one.
    #[must_use]
    pub fn get(&self, currency: &str) -> Option<&PostedMoney> {
        self.0
            .binary_search_by(|total| total.currency().code().cmp(currency))
            .ok()
            .map(|index| &self.0[index])
    }

    /// Iterate the totals in currency-code order.
    pub fn iter(&self) -> std::slice::Iter<'_, PostedMoney> {
        self.0.iter()
    }

    /// The number of currencies.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the manifest carries no totals.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'a> IntoIterator for &'a GrossTotals {
    type Item = &'a PostedMoney;
    type IntoIter = std::slice::Iter<'a, PostedMoney>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
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

#[cfg(test)]
#[path = "issued_invoice_manifest_tests.rs"]
mod tests;
