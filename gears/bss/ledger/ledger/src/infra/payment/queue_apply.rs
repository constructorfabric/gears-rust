//! `QueueApplier` — the deferred-apply driver for queued payment allocations
//! (Slice 2b, Group D).
//!
//! An allocate of a not-yet-settled payment is durably queued at intake (§4.7,
//! `AllocationService::enqueue_allocation`). This applier drains those rows once
//! the settlement lands: for each due `QUEUED` row it re-derives the split
//! against THEN-CURRENT state (caps re-evaluated at apply time) and posts it
//! through the engine's `ClaimMode::QueuedApply` path, atomically flipping the
//! queue row `→APPLIED`.
//!
//! ## Why a thin wrapper over `AllocationService`
//! The apply re-runs the EXACT decide/validate/build path the inline allocate
//! uses (read open AR + cap, resolve precedence or validate a caller split, build
//! the entry, overwrite the header, bind the chart). That logic — and the private
//! helpers it leans on (`overwrite_header`, `resolve_line`, `new_line`,
//! `to_engine_inputs`) — lives on [`AllocationService`]. Rather than duplicate it
//! or widen those helpers' visibility, `QueueApplier` holds the same deps and
//! constructs an `AllocationService`, delegating to its `pub(crate)`
//! [`AllocationService::apply_queued_row`] / public [`AllocationService::drain`].
//!
//! ## Two-transaction apply (§4.7 "apply is a second txn")
//! Draining is two transaction shapes, never one: a short CLAIM txn selects due
//! rows under `FOR UPDATE SKIP LOCKED`, then EACH row is applied in its OWN txn.
//! The authoritative work-state flip (`→APPLIED`) rides the apply's post txn (the
//! composite sidecar), not the claim. This keeps a slow apply from holding the
//! claim lock across the batch and isolates a per-row failure.
//!
//! `SKIP LOCKED` only REDUCES overlap between concurrent appliers (the sweep job
//! + a drain-on-settle) — it does NOT hand them disjoint batches. The claim txn
//! holds each row lock only for its own (short) duration and does NOT flip the
//! status, so once it commits a second applier can re-select the same still-
//! `QUEUED` row. Exactly-once is therefore enforced NOT by the row lock but by
//! the apply's `SERIALIZABLE` post txn: two appliers of the same row collide on
//! the dedup-row finalize + the `payment_allocation` PK, the loser aborts, and
//! its retry reads `POSTED` and replays. See [`AllocationService::drain`] for the
//! per-outcome handling (`Blocked` ⇒ bump attempts + back off; `NotReady` ⇒ skip).

use std::sync::Arc;

use toolkit_db::secure::AccessScope;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::metrics::LedgerMetricsPort;
use crate::infra::adjustment::refund_service::{
    ClawbackDrainReport, DisputeHoldDrainReport, QuarantineDrainReport, RefundHandler,
};
use crate::infra::events::publisher::LedgerEventPublisher;
use crate::infra::payment::allocate::{
    AllocationService, DrainReport, MAX_INVOICES_PER_ALLOCATION,
};
use crate::infra::payment::chargeback::{ChargebackService, DrainReport as ChargebackDrainReport};

/// Drives the deferred apply of queued payment allocations. Holds the same deps
/// as [`AllocationService`] (`db` / `publisher` / `metrics`) and builds one to
/// delegate the re-derive + queued-apply post.
pub struct QueueApplier {
    db: DBProvider<DbError>,
    publisher: Arc<LedgerEventPublisher>,
    metrics: Arc<dyn LedgerMetricsPort>,
    /// The dual-control engine (Group G de-quarantine): a quarantined refund whose
    /// origin landed is re-driven through a GATED `RefundHandler` so an
    /// over-THEN-CURRENT-D2 de-quarantine routes to approval (never auto-posts).
    /// `None` ⇒ the de-quarantine path posts un-gated (router/unit tests without a
    /// governance DB). Wired in `module` via [`Self::with_approval`].
    approval: Option<Arc<crate::infra::approval::service::ApprovalService>>,
    /// The per-allocation touched-invoice cap, threaded into the delegate
    /// [`AllocationService`] so a DRAINED (deferred-apply) allocation uses the
    /// SAME configured cap as an inline one. Defaults to
    /// [`MAX_INVOICES_PER_ALLOCATION`]; set from config via
    /// [`Self::with_max_invoices_per_allocation`].
    max_invoices: usize,
}

impl QueueApplier {
    /// Build the applier over one database provider, the event publisher
    /// (threaded into the posting engine the delegate builds), and the metrics
    /// sink. Same deps — and same shape — as [`AllocationService::new`] and
    /// [`crate::infra::payment::settle::SettlementService::new`].
    #[must_use]
    pub fn new(
        db: DBProvider<DbError>,
        publisher: Arc<LedgerEventPublisher>,
        metrics: Arc<dyn LedgerMetricsPort>,
    ) -> Self {
        Self {
            db,
            publisher,
            metrics,
            approval: None,
            max_invoices: MAX_INVOICES_PER_ALLOCATION,
        }
    }

    /// Override the per-allocation touched-invoice cap (from `payments` config),
    /// threaded into the delegate [`AllocationService`] so the deferred-apply
    /// drain honours the same cap as the inline path. Builder form; defaults to
    /// [`MAX_INVOICES_PER_ALLOCATION`].
    #[must_use]
    pub fn with_max_invoices_per_allocation(mut self, max_invoices: usize) -> Self {
        self.max_invoices = max_invoices;
        self
    }

    /// Attach the dual-control engine (Group G): the de-quarantine drain
    /// ([`Self::drain_quarantine`]) then re-drives a released refund through a GATED
    /// [`RefundHandler`], so an over-THEN-CURRENT-D2 de-quarantine routes to approval
    /// instead of auto-posting. Builder form (defaults to `None`).
    #[must_use]
    pub fn with_approval(
        mut self,
        approval: Arc<crate::infra::approval::service::ApprovalService>,
    ) -> Self {
        self.approval = Some(approval);
        self
    }

    /// Construct the delegate [`AllocationService`] (cheap — clones the provider
    /// Arc + the two `Arc` deps). A fresh service per call keeps `QueueApplier`
    /// itself dep-only and avoids holding a second long-lived service.
    fn service(&self) -> AllocationService {
        AllocationService::new(
            self.db.clone(),
            Arc::clone(&self.publisher),
            Arc::clone(&self.metrics),
        )
        .with_max_invoices_per_allocation(self.max_invoices)
    }

    /// Construct the delegate [`ChargebackService`] (same shape as
    /// [`Self::service`]) for the CHARGEBACK-flow drain (out-of-order `won`/`lost`
    /// phases whose `opened` has since landed).
    fn chargeback_service(&self) -> ChargebackService {
        ChargebackService::new(
            self.db.clone(),
            Arc::clone(&self.publisher),
            Arc::clone(&self.metrics),
        )
    }

    /// Construct the delegate [`RefundHandler`] (un-gated — the drain re-drives an
    /// already-accepted claw-back, never a fresh preparer decision) for the
    /// `REFUND_CLAWBACK`-flow drain (deferred refund-of-refund claw-backs whose
    /// matching outbound refund stage-1 may have since landed). Group E.
    fn refund_handler(&self) -> RefundHandler {
        RefundHandler::new(self.db.clone(), Arc::clone(&self.publisher))
    }

    /// Construct a GATED delegate [`RefundHandler`] for the de-quarantine drain
    /// (Group G): `.with_approval(...)` so a released-but-over-THEN-CURRENT-D2 refund
    /// routes to the preparer→approver queue instead of auto-posting (design §4.4 —
    /// de-quarantine NEVER auto-posts over threshold). `.with_metrics(...)` so a
    /// de-quarantine post bumps `ledger_refund_total`. When no approval engine is
    /// wired this falls back to the un-gated handler (router/unit tests).
    fn gated_refund_handler(&self) -> RefundHandler {
        let base = RefundHandler::new(self.db.clone(), Arc::clone(&self.publisher))
            .with_metrics(Arc::clone(&self.metrics));
        match &self.approval {
            Some(approval) => base.with_approval(Arc::clone(approval)),
            None => base,
        }
    }

    /// Drain up to `limit` due queued allocations for `tenant`, scoped by
    /// `scope`. Delegates to [`AllocationService::drain`] (claim under SKIP LOCKED
    /// in a short txn, then apply each row in its own txn). Used by both the
    /// drain-on-settle hook ([`crate::infra::payment::settle::SettlementService`])
    /// and the periodic sweep job ([`crate::infra::jobs::queue_applier`]).
    ///
    /// # Errors
    /// [`DomainError::Internal`] only if the initial claim txn fails; per-row
    /// faults are isolated inside the pass (logged, the row stays `QUEUED`).
    pub async fn drain(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant: Uuid,
        limit: u64,
    ) -> Result<DrainReport, DomainError> {
        self.service().drain(ctx, scope, tenant, limit).await
    }

    /// Drain up to `limit` due queued CHARGEBACK phases for `tenant` (out-of-order
    /// `won`/`lost` whose `opened` has since landed). Delegates to
    /// [`ChargebackService::drain`] (claim under SKIP LOCKED in a short txn, then
    /// apply each row in its own txn). Driven both by the inline drain-on-`opened`
    /// hook ([`ChargebackService::record_phase`]) and the periodic sweep job
    /// ([`crate::infra::jobs::queue_applier`], the backstop).
    ///
    /// # Errors
    /// [`DomainError::Internal`] only if the initial claim txn fails; per-row
    /// faults are isolated inside the pass.
    pub async fn drain_chargeback(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant: Uuid,
        limit: u64,
    ) -> Result<ChargebackDrainReport, DomainError> {
        self.chargeback_service()
            .drain(ctx, scope, tenant, limit)
            .await
    }

    /// Drain up to `limit` due deferred `REFUND_CLAWBACK` rows for `tenant` (Group E):
    /// a refund-of-refund claw-back that underflowed at intake and was queued, whose
    /// matching outbound refund stage-1 may have since landed. Delegates to
    /// [`RefundHandler::drain_clawbacks`] (claim under SKIP LOCKED, then re-try each
    /// row in its own txn; reconciled → APPLIED, still-underflow → back off, aged out
    /// → CANCELLED + escalate). Driven by the periodic sweep job
    /// ([`crate::infra::jobs::queue_applier`]).
    ///
    /// # Errors
    /// [`DomainError::Internal`] only if the initial claim txn fails; per-row faults
    /// are isolated inside the pass.
    pub async fn drain_clawback(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant: Uuid,
        limit: u64,
    ) -> Result<ClawbackDrainReport, DomainError> {
        self.refund_handler()
            .drain_clawbacks(ctx, scope, tenant, limit)
            .await
    }

    /// Drain up to `limit` due QUARANTINED refund-before-payment rows for `tenant`
    /// (Group G de-quarantine): a refund whose origin payment was absent at intake
    /// and may have since landed. Delegates to [`RefundHandler::drain_quarantine`]
    /// (claim under SKIP LOCKED, then RE-VALIDATE each — re-resolve the origin +
    /// re-check the §4.7 caps + the THEN-CURRENT D2 threshold; released → APPLIED,
    /// over-threshold → an approval opens (NEVER auto-posts), still-missing → back
    /// off, aged out → CANCELLED + escalate). Uses the GATED handler. Driven by the
    /// periodic sweep job ([`crate::infra::jobs::queue_applier`]).
    ///
    /// # Errors
    /// [`DomainError::Internal`] only if the initial claim txn fails; per-row faults
    /// are isolated inside the pass.
    pub async fn drain_quarantine(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant: Uuid,
        limit: u64,
    ) -> Result<QuarantineDrainReport, DomainError> {
        self.gated_refund_handler()
            .drain_quarantine(ctx, scope, tenant, limit)
            .await
    }

    /// Drain up to `limit` due DISPUTE-HELD refund rows for `tenant` (Z5-2 / design
    /// §5): a refund whose origin payment had an OPEN dispute at intake (the cash leg
    /// must not move while the dispute is sub judice). Delegates to
    /// [`RefundHandler::drain_dispute_hold`] (claim under SKIP LOCKED, then RE-READ
    /// the dispute per row — WON → re-drive the gated post (over-threshold → an
    /// approval opens, NEVER auto-posts); LOST → CANCEL the hold + escalate (a
    /// chargeback already returned the money — posting would double-pay); still-OPEN →
    /// back off, aged out → CANCEL + escalate). Uses the GATED handler. Driven by the
    /// periodic sweep job ([`crate::infra::jobs::queue_applier`]).
    ///
    /// # Errors
    /// [`DomainError::Internal`] only if the initial claim txn fails; per-row faults
    /// are isolated inside the pass.
    pub async fn drain_dispute_hold(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant: Uuid,
        limit: u64,
    ) -> Result<DisputeHoldDrainReport, DomainError> {
        self.gated_refund_handler()
            .drain_dispute_hold(ctx, scope, tenant, limit)
            .await
    }
}
