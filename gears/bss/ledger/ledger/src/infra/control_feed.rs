//! In-process control-feed store (Slice 7 Phase 3, design §0 decision 3 / §3.4).
//! The v1 default source for the three launch-blocking control feeds (issued-invoice
//! manifest, bill-run-finished, PSP settlement report): the REST `…/control/*` ingest
//! endpoints push into it, the `ReconciliationFramework` + close gate read it back. Empty
//! ⇒ every read is `None` ⇒ the corresponding check is inert (the design's
//! inert-until-the-feed-lands contract). A real external adapter-gear, when present in
//! `ClientHub`, overrides this per port. No durable table (control feeds are not posting
//! sources, design §1.2; Phase 3 adds no tables).

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use bss_ledger_sdk::{
    BillRunFinishedV1, ControlFeedError, IssuedInvoiceManifest, IssuedInvoiceManifestV1,
    PspSettlementFeedV1, PspSettlementReport,
};
use uuid::Uuid;

/// In-process, per-`(tenant, period)` store backing all three control-feed ports. The
/// REST ingest endpoints push via `ingest_*`; the framework / close gate read via the
/// SDK port traits. Default-constructed empty ⇒ every read returns `Ok(None)`.
///
/// Each feed has its own `Mutex<HashMap<_>>`: a control feed carries no cross-feed
/// invariant (the gate reads each independently), so independent locks avoid coupling
/// an ingest on one feed to a read on another. A poisoned lock here is non-fatal data
/// (last writer wins, no money invariant), so we recover the guard rather than panic.
#[derive(Default)]
pub struct InProcessControlFeeds {
    manifests: Mutex<HashMap<(Uuid, String), IssuedInvoiceManifest>>,
    bill_runs: Mutex<HashMap<(Uuid, String), bool>>,
    psp_reports: Mutex<HashMap<(Uuid, String), PspSettlementReport>>,
}

impl InProcessControlFeeds {
    /// An empty store (every read is `None` until something is ingested).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push the latest issued-invoice manifest for `(tenant, period)` (REST ingest).
    /// Last writer wins — the feed is a snapshot, not an append log.
    pub fn ingest_manifest(&self, tenant: Uuid, period: &str, manifest: IssuedInvoiceManifest) {
        self.manifests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((tenant, period.to_owned()), manifest);
    }

    /// Push the owning Orchestration's bill-run-finished assertion for
    /// `(tenant, period)` (REST ingest). Last writer wins.
    pub fn ingest_bill_run_finished(&self, tenant: Uuid, period: &str, finished: bool) {
        self.bill_runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((tenant, period.to_owned()), finished);
    }

    /// Push the PSP settlement report for `(tenant, period)` (REST ingest). Last
    /// writer wins (the `report_id` carries the external idempotency grain).
    pub fn ingest_psp_report(&self, tenant: Uuid, period: &str, report: PspSettlementReport) {
        self.psp_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((tenant, period.to_owned()), report);
    }
}

#[async_trait]
impl IssuedInvoiceManifestV1 for InProcessControlFeeds {
    async fn latest_manifest(
        &self,
        tenant: Uuid,
        period: &str,
    ) -> Result<Option<IssuedInvoiceManifest>, ControlFeedError> {
        Ok(self
            .manifests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(tenant, period.to_owned()))
            .cloned())
    }
}

#[async_trait]
impl BillRunFinishedV1 for InProcessControlFeeds {
    async fn is_finished(
        &self,
        tenant: Uuid,
        period: &str,
    ) -> Result<Option<bool>, ControlFeedError> {
        Ok(self
            .bill_runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(tenant, period.to_owned()))
            .copied())
    }
}

#[async_trait]
impl PspSettlementFeedV1 for InProcessControlFeeds {
    async fn settlement_report(
        &self,
        tenant: Uuid,
        period: &str,
    ) -> Result<Option<PspSettlementReport>, ControlFeedError> {
        Ok(self
            .psp_reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(tenant, period.to_owned()))
            .cloned())
    }
}

#[cfg(test)]
#[path = "control_feed_tests.rs"]
mod tests;
