//! `InvoicePostService` — the orchestrator that drives the pure invoice-post
//! domain (`crate::domain::invoice`) through the foundation engine.
//!
//! It ties the pieces together for one business post:
//! 1. **payer gate** — reject a post for a closed payer (`PAYER_CLOSED`); a
//!    reversal of an already-posted invoice bypasses this (a closed payer must
//!    still be able to have a wrong charge backed out).
//! 2. **map** each item to its GL target (`domain::invoice::mapping::resolve`).
//! 3. **build** the balanced direct-split entry
//!    (`domain::invoice::builder::build_invoice_entry`).
//! 4. **bind** the real chart `account_id` for each line from the provisioned
//!    chart of accounts (the pure builder emits a nil placeholder).
//! 5. Validate current metadata, bind chart and lock FX on the posting attempt.
//! 6. **emit metrics** — `invoice_post` (outcome) + duration once per operation;
//!    the suspense gauges when the post parks PENDING lines.
//!
//! Lives in `infra` (not `domain`) because it needs repo + posting access; the
//! domain modules it calls stay pure (dylint DE0301). It wraps the `pub`
//! [`PostingService`] + [`ReferenceRepo`] directly (rather than the SDK
//! `LedgerClientV1`, whose in-process impl `LedgerLocalClient::new` is
//! `pub(crate)`), so it is constructible from out-of-crate integration tests.

use std::sync::Arc;
use std::time::Instant;

use bss_ledger_sdk::{MappingStatus, PostEntry, PostLine, PostedMoney, PostingRef, SourceDocType};
use toolkit_db::secure::{AccessScope, DbTx};
use toolkit_db::{DBProvider, DbError};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::config::{FxConfig, RecognitionConfig};
use crate::domain::canonical::{
    digest32_hex, put, put_money, put_opt_str, put_opt_uuid, put_str, put_uuid,
};
use crate::domain::error::DomainError;
use crate::domain::invoice::builder::{
    InvoiceError, InvoiceItem, PostedInvoice, build_invoice_entry,
};
use crate::domain::invoice::mapping::{MappedLine, resolve};
use crate::domain::invoice::policy::MissingMappingMode;
use crate::domain::model::{NewEntry, NewLine};
use crate::domain::ports::metrics::{LedgerMetricsPort, PostFlow, PostResult};
use crate::domain::recognition::builder::{ScheduleBuilder, ScheduleOutcome, is_immaterial};
use crate::domain::recognition::input::RecognitionInput;
use crate::domain::recognition::input::RecognitionTiming;
use crate::domain::recognition::ports::{
    DefaultDeferralPolicyResolver, DefaultSspResolver, DefaultVcResolver, RecognitionContext,
};
use crate::infra::currency_scale::CurrencyScaleResolver;
use crate::infra::events::payloads::LedgerEntryReversed;
use crate::infra::events::publisher::LedgerEventPublisher;
use crate::infra::fx::rate_locker::RateLocker;
use crate::infra::fx::rate_source::RateSource;
use crate::infra::posting::chart::{ChartIndex, load_chart_in};
use crate::infra::posting::idempotency::IdempotencyGate;
use crate::infra::posting::retry::{AttemptError, RetryOp, retry_transaction_for};
use crate::infra::posting::service::{ClaimSpec, PostSidecar, PostedFacts, PostingService};
use crate::infra::recognition::sidecar::{PlannedScheduleMaterialization, ScheduleBuilderSidecar};
use crate::infra::storage::repo::posting_policy_repo::PostingPolicyRepo;
use crate::infra::storage::repo::recognition_repo::RecognitionRepo;
use crate::infra::storage::repo::{FxRepo, JournalRepo, ReferenceRepo};
use time::OffsetDateTime;

/// Origin literal stamped on posts made through this service.
const ORIGIN_SYSTEM: &str = "SYSTEM";

/// In-transaction sidecar that emits `billing.ledger.entry.reversed` (architecture
/// §6, VHP-1837) on the explicit reversal path. It rides the post's own
/// transaction (the transactional outbox), so the event row commits atomically
/// with the reversing entry or rolls back with it. It carries the operator
/// `reason` (which is NOT persisted on the entry header) and the original entry
/// id; the reversing entry's id comes from [`PostedFacts`]. A `MAPPING_CORRECTION`
/// does not attach this sidecar — it is a correction, not a §6 reversal.
struct ReversalEventSidecar {
    publisher: Arc<LedgerEventPublisher>,
    ctx: SecurityContext,
    tenant_id: Uuid,
    reverses_entry_id: Uuid,
    reason: String,
}

#[async_trait::async_trait]
impl PostSidecar for ReversalEventSidecar {
    async fn run(
        &self,
        txn: &DbTx<'_>,
        _scope: &AccessScope,
        posted: &PostedFacts,
    ) -> Result<(), DomainError> {
        self.publisher
            .publish_entry_reversed(
                &self.ctx,
                txn,
                LedgerEntryReversed {
                    entry_id: posted.entry_id,
                    reverses_entry_id: self.reverses_entry_id,
                    tenant_id: self.tenant_id,
                    reason: self.reason.clone(),
                },
            )
            .await
            .map_err(|e| DomainError::Internal(format!("publish entry_reversed: {e}")))
    }
}

/// Write port the journal-entry REST handlers post through. Abstracts the two
/// foundation-engine writes the surface needs — a fresh invoice post (payer
/// gate + map + build + bind) and a pre-built reversal/correction post — so the
/// router tests can stub the post path without a database. The production
/// implementation is [`InvoicePostService`].
#[async_trait::async_trait]
pub trait InvoicePoster: Send + Sync {
    /// Post a fully-recognized invoice (Variant A). `payer_open = false` rejects
    /// with [`DomainError::PayerClosed`] before any ledger effect.
    ///
    /// # Errors
    /// [`DomainError`] on a payer gate / foundation rejection or an infra fault.
    async fn post_invoice(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        inv: &PostedInvoice,
        payer_open: bool,
    ) -> Result<bss_ledger_sdk::PostingRef, DomainError>;

    /// Reverse the full immutable original referenced by the caller header. Caller
    /// lines never size history. The payer gate is intentionally bypassed.
    /// `reason` is `Some(audit reason)` for an explicit reversal — it is announced
    /// on the `billing.ledger.entry.reversed` event (VHP-1837), not persisted on
    /// the row — or `None` for a mapping-correction's internal reversal leg, which
    /// announces nothing (a correction is not a §6 reversal).
    ///
    /// # Errors
    /// [`DomainError`] on a foundation rejection or an infra fault.
    async fn post_reversal(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        reversal: PostEntry,
        reason: Option<String>,
    ) -> Result<bss_ledger_sdk::PostingRef, DomainError>;

    /// Post a corrected re-post (`MAPPING_CORRECTION`) whose lines were freshly
    /// built and carry placeholder nil `account_id`s — so unlike [`post_reversal`]
    /// this binds each line's chart `account_id` from the provisioned chart
    /// before posting. The `source_doc_type` + `reverses_*` header is preserved.
    ///
    /// # Errors
    /// [`DomainError`] on an unmapped account / foundation rejection / infra fault.
    async fn post_correction(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        correction: PostEntry,
    ) -> Result<bss_ledger_sdk::PostingRef, DomainError>;
}

/// Orchestrates the invoice-post domain over the foundation engine.
#[derive(Clone)]
pub struct InvoicePostService {
    db: DBProvider<DbError>,
    journal: JournalRepo,
    recognition_repo: Arc<RecognitionRepo>,
    #[cfg(test)]
    attempt_hook: Option<Arc<dyn decimal_tests::AttemptHook>>,
    posting: PostingService,
    reference: ReferenceRepo,
    resolver: Arc<CurrencyScaleResolver>,
    metrics: Arc<dyn LedgerMetricsPort>,
    /// ASC 606 recognition tunables (Slice 4): the per-schedule segment ceiling
    /// the pure `ScheduleBuilder` enforces during derivation.
    recognition_config: RecognitionConfig,
    /// The S1 FX lock (Slice 5): resolves + snapshots the locked rate and stamps
    /// the functional translation on a cross-currency invoice entry. Inert for a
    /// single-currency tenant (no functional currency configured).
    rate_locker: RateLocker,
    /// Event publisher, retained so the reversal path can attach a
    /// [`ReversalEventSidecar`] that emits `billing.ledger.entry.reversed` in the
    /// post txn (VHP-1837). The same handle is threaded into the posting engine.
    publisher: Arc<LedgerEventPublisher>,
    /// Tenant posting policy (VHP-1853): the missing-mapping mode (the hard-block
    /// gate below) + the AR-aging buckets. Read effective in the orchestrator
    /// on the supplied attempt; absent a row the gear default (`SUSPENSE`) applies.
    posting_policy_repo: PostingPolicyRepo,
}

impl InvoicePostService {
    /// Build the service over one database provider, the event publisher
    /// (threaded into the posting engine), the metrics sink, and the recognition
    /// config (the segment ceiling the derivation enforces, Slice 4).
    #[must_use]
    pub fn new(
        db: DBProvider<DbError>,
        publisher: Arc<LedgerEventPublisher>,
        metrics: Arc<dyn LedgerMetricsPort>,
        recognition_config: RecognitionConfig,
        fx_config: FxConfig,
    ) -> Self {
        let posting = PostingService::new(db.clone(), Arc::clone(&publisher));
        let reference = ReferenceRepo::new(db.clone());
        // S1 FX lock: resolve over the local rate store (provider order +
        // staleness from `fx_config`) and freeze a snapshot per cross-currency post.
        let rate_locker = RateLocker::new(
            RateSource::new(FxRepo::new(db.clone()), fx_config).with_metrics(Arc::clone(&metrics)),
            FxRepo::new(db.clone()),
        );
        let posting_policy_repo = PostingPolicyRepo::new(db.clone());
        let resolver = Arc::new(CurrencyScaleResolver::new(ReferenceRepo::new(db.clone())));
        let journal = JournalRepo::new(db.clone());
        let recognition_repo = Arc::new(RecognitionRepo::new(db.clone()));
        Self {
            db,
            journal,
            recognition_repo,
            #[cfg(test)]
            attempt_hook: None,
            posting,
            reference,
            resolver,
            metrics,
            recognition_config,
            rate_locker,
            publisher,
            posting_policy_repo,
        }
    }

    /// Post a fully-recognized invoice (Variant A). `payer_open` is the payer's
    /// lifecycle decision (resolved by the caller): `false` ⇒ the post is
    /// rejected with [`DomainError::PayerClosed`] before any ledger effect.
    ///
    /// On success emits `invoice_post(Posted | Replayed)` + the duration, and —
    /// when the post parked any PENDING line — the suspense gauges. Every
    /// rejection emits `invoice_post(Rejected)` + the duration.
    ///
    /// # Errors
    /// [`DomainError::PayerClosed`] when `!payer_open`; any foundation rejection
    /// (unbalanced/empty/period-closed/account-closed/negative-balance/…) or
    /// [`DomainError::Internal`] on an infrastructure fault.
    pub async fn post_invoice(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        inv: &PostedInvoice,
        payer_open: bool,
    ) -> Result<bss_ledger_sdk::PostingRef, DomainError> {
        let started = Instant::now();
        let result = self.post_invoice_inner(ctx, scope, inv, payer_open).await;
        self.emit_terminal_alarm(
            ctx,
            inv.seller_tenant_id,
            SourceDocType::InvoicePost,
            &inv.invoice_id,
            &result,
        )
        .await;
        self.record(&result, started, PostFlow::InvoicePost);
        // On a fresh post that parked PENDING lines, surface the suspense backlog
        // as a gauge (age 0 at post time; the tie-out job ages it durably).
        if let Ok(ref posted) = result
            && !posted.replayed
        {
            let pending = pending_line_count(inv);
            if pending > 0 {
                self.metrics
                    .suspense_pending(inv.seller_tenant_id, pending, 0.0);
            }
        }
        result
    }

    /// Build + post the entry (no metrics — the public wrapper records them).
    ///
    /// Slice 4: each item carrying a recognition spec is run through the pure
    /// [`ScheduleBuilder`] derivation FIRST (in [`Self::derive_recognition`]),
    /// which fills the item's `deferred` and yields the schedules to
    /// materialize. The builder then splits each stream's credit into
    /// `CR REVENUE (recognized now)` + `CR CONTRACT_LIABILITY (deferred)`; the
    /// schedules ride a [`ScheduleBuilderSidecar`] threaded into the post so they
    /// materialize in the same serializable transaction (or roll back with the
    /// entry). When NO item defers, the derivation yields no schedules, the
    /// builder emits no Contract-liability line, and the post is threaded a
    /// `None` sidecar.
    async fn post_invoice_inner(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        inv: &PostedInvoice,
        payer_open: bool,
    ) -> Result<bss_ledger_sdk::PostingRef, DomainError> {
        let request_hash = invoice_request_hash(inv);
        let svc = self.clone();
        let ctx = ctx.clone();
        let scope = scope.clone();
        let inv = inv.clone();
        let op = RetryOp::new("invoice post", Some(inv.seller_tenant_id));
        retry_transaction_for(&self.db.db(), op, move |txn| {
            let svc = svc.clone();
            let ctx = ctx.clone();
            let scope = scope.clone();
            let inv = inv.clone();
            let request_hash = request_hash.clone();
            Box::pin(async move {
                svc.post_invoice_attempt(txn, &ctx, &scope, &inv, request_hash, payer_open)
                    .await
            })
        })
        .await
    }

    /// One complete serializable attempt of an invoice post, in order: authorize
    /// the target, replay a committed claim, gate a closed payer, derive
    /// recognition, read the posting policy and enforce `HARD_BLOCK`, build the
    /// entry, assemble the schedule sidecar, post. Target authorization precedes
    /// the tenant-owned claim read; dependency operations retain the caller
    /// scope; constrained paths fail closed.
    async fn post_invoice_attempt(
        &self,
        txn: &DbTx<'_>,
        ctx: &SecurityContext,
        scope: &AccessScope,
        inv: &PostedInvoice,
        request_hash: String,
        payer_open: bool,
    ) -> Result<bss_ledger_sdk::PostingRef, AttemptError> {
        authorize_entry_in(
            &self.journal,
            txn,
            scope,
            ctx,
            inv.seller_tenant_id,
            SourceDocType::InvoicePost,
            &inv.invoice_id,
        )
        .await?;
        if let Some(result) = replay_in(
            txn,
            inv.seller_tenant_id,
            SourceDocType::InvoicePost,
            &inv.invoice_id,
            &request_hash,
        )
        .await?
        {
            return Ok(result);
        }
        if !payer_open {
            return Err(DomainError::PayerClosed(format!(
                "payer {} is closed",
                inv.payer_tenant_id
            ))
            .into());
        }
        #[cfg(test)]
        if let Some(hook) = &self.attempt_hook {
            hook.before(txn).await?;
        }
        let (items, schedules) = self.derive_recognition(inv)?;
        let derived = PostedInvoice {
            items,
            ..inv.clone()
        };
        let mapped: Vec<MappedLine> = derived.items.iter().map(resolve).collect();
        let policy = self
            .posting_policy_repo
            .read_effective_policy_in(txn, scope, inv.seller_tenant_id, OffsetDateTime::now_utc())
            .await?;
        if policy.missing_mapping_mode == MissingMappingMode::HardBlock
            && mapped
                .iter()
                .any(|m| m.mapping_status == MappingStatus::Pending)
        {
            return Err(DomainError::AccountMappingMissing(format!(
                "invoice {} has an unmapped item and the tenant posting policy is HARD_BLOCK",
                inv.invoice_id
            ))
            .into());
        }
        let entry = build_invoice_entry(&derived, &mapped).map_err(map_invoice_error)?;
        let sidecar: Option<Arc<dyn PostSidecar>> = if schedules.is_empty() {
            None
        } else {
            Some(Arc::new(ScheduleBuilderSidecar {
                tenant_id: inv.seller_tenant_id,
                payer_tenant_id: inv.payer_tenant_id,
                source_invoice_id: inv.invoice_id.clone(),
                schedules,
                idempotency: IdempotencyGate::new(),
                recognition_repo: Arc::clone(&self.recognition_repo),
                max_segments_per_schedule: self.recognition_config.max_segments_per_schedule,
                build_discriminator: None,
            }))
        };
        let posted = self
            .bind_post_once(
                ctx,
                txn,
                scope,
                entry,
                sidecar,
                ClaimSpec::fresh_with_request_hash(request_hash),
                true,
            )
            .await?;
        #[cfg(test)]
        if let Some(hook) = &self.attempt_hook {
            hook.after(txn).await?;
        }
        Ok(posted)
    }

    /// Run the pure recognition derivation for every item that carries a spec,
    /// returning (a) the items with their derived `deferred` filled and
    /// (b) the schedules to materialize (one per deferred item-stream, paired
    /// with the item's `invoice_item_ref`).
    ///
    /// Per item with a [`RecognitionInput`]: build a [`RecognitionContext`] (the
    /// item's ex-tax amount, the invoice period, the invoice gross total, the
    /// currency, the revenue stream) and call [`ScheduleBuilder::derive`]. A
    /// [`ScheduleOutcome::NoDeferral`] leaves `deferred = 0` (no schedule);
    /// a [`ScheduleOutcome::Schedule`] sets `deferred` and is collected.
    ///
    /// Two Slice-4 gates fire here (the derivation owns the trigger conditions,
    /// NOT Slice 1's endpoint):
    /// - **invoice-item-link** (§4.7): a deferred item MUST carry an
    ///   `invoice_item_ref` (the schedule's NOT-NULL `source_invoice_item_ref`,
    ///   the Contract-liability line it draws down). A deferred item without one
    ///   is blocked with [`DomainError::RecognitionWithoutInvoiceLink`] before the
    ///   post (no orphan deferral). [The SSP gate is enforced inside `derive` by
    ///   the `SspResolver`; the policy/segment gates likewise.]
    /// - **PO-allocation-group** (C4, §4.4): a deferred / multi-PO / VC item whose
    ///   PO allocation group cannot be resolved AND cannot be defaulted (Catalog
    ///   default) AND is not R4-exempt ⇒ [`DomainError::MissingPoAllocationGroup`].
    ///   An ordinary point-in-time line auto-defaults / never blocks.
    ///
    /// # Errors
    /// Any block the derivation raises ([`DomainError::SspSnapshotRequired`],
    /// [`DomainError::RecognitionPolicyConflict`], [`DomainError::ScheduleTooLong`]),
    /// the §4.7 invoice-item-link [`DomainError::RecognitionWithoutInvoiceLink`]
    /// gate, or the C4 [`DomainError::MissingPoAllocationGroup`] gate.
    fn derive_recognition(
        &self,
        inv: &PostedInvoice,
    ) -> Result<(Vec<InvoiceItem>, Vec<PlannedScheduleMaterialization>), DomainError> {
        let policy = DefaultDeferralPolicyResolver;
        let ssp = DefaultSspResolver;
        let vc = DefaultVcResolver;
        let builder = ScheduleBuilder::new(&policy, &ssp, &vc, &self.recognition_config);

        // The R4 exemption / invoice-share denominator is the invoice gross.
        let invoice_total = inv.gross().map_err(map_invoice_error)?;

        let mut items = Vec::with_capacity(inv.items.len());
        let mut schedules = Vec::new();
        for item in &inv.items {
            let mut out_item = item.clone();
            if let Some(input) = &item.recognition {
                // C4 PO-gate: a deferring / multi-PO / VC line needs a resolvable
                // (or defaultable) PO allocation group unless R4-exempt.
                check_po_allocation_group(input, &item.amount_ex_tax, &invoice_total)?;

                let ctx = RecognitionContext {
                    input,
                    invoice_period_id: &inv.period_id,
                    item_amount_ex_tax: &item.amount_ex_tax,
                    invoice_total: &invoice_total,
                    revenue_stream: &item.revenue_stream,
                };
                match builder.derive(&ctx)? {
                    ScheduleOutcome::NoDeferral => {}
                    ScheduleOutcome::Schedule(schedule) => {
                        // §4.7 invoice-item-link: a deferred item MUST resolve to
                        // its Contract-liability line via a non-empty
                        // `invoice_item_ref`. Block before the post (no orphan) with
                        // the SPECIFIC `RecognitionWithoutInvoiceLink` (wire
                        // `RECOGNITION_WITHOUT_INVOICE_LINK`, 400) — the §4.7
                        // invariant's own code, not the generic `AmountOutOfRange`.
                        let item_ref = item
                            .invoice_item_ref
                            .as_deref()
                            .filter(|r| !r.is_empty())
                            .ok_or_else(|| {
                            DomainError::RecognitionWithoutInvoiceLink(format!(
                                "deferred recognition line (stream `{}`) must carry an \
                                     invoice_item_ref to anchor its contract-liability schedule",
                                item.revenue_stream
                            ))
                        })?;
                        out_item.deferred = schedule.deferred.clone();
                        schedules.push(PlannedScheduleMaterialization {
                            schedule,
                            source_invoice_item_ref: item_ref.to_owned(),
                        });
                    }
                }
            }
            items.push(out_item);
        }
        Ok((items, schedules))
    }

    /// Post a reversal built from an original entry's [`PostEntry`] projection
    /// (the caller builds it via `domain::invoice::reversal::build_reversal`).
    /// Caller lines are advisory and never size history. Full stored lines and
    /// pinned money/evidence are loaded on the reversal attempt.
    /// The payer gate is intentionally NOT applied — a reversal must post even
    /// for a closed payer.
    ///
    /// # Errors
    /// Any foundation rejection or [`DomainError::Internal`] on an infrastructure
    /// fault.
    pub async fn post_reversal(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        reversal: PostEntry,
        reason: Option<String>,
    ) -> Result<bss_ledger_sdk::PostingRef, DomainError> {
        let started = Instant::now();
        let tenant = reversal.tenant_id;
        let business = reversal.source_business_id.clone();
        let result = self.reverse_inner(ctx, scope, reversal, reason).await;
        self.emit_terminal_alarm(ctx, tenant, SourceDocType::Reversal, &business, &result)
            .await;
        self.record(&result, started, PostFlow::Reversal);
        result
    }

    /// Validate historical lineage and reverse every stored line on one attempt.
    async fn reverse_inner(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        reversal: PostEntry,
        reason: Option<String>,
    ) -> Result<PostingRef, DomainError> {
        let original_id = reversal.reverses_entry_id.ok_or_else(|| {
            DomainError::InvalidRequest("reversal requires original entry".into())
        })?;
        if reversal.source_doc_type != SourceDocType::Reversal
            || reversal.source_business_id
                != crate::domain::invoice::reversal::reversal_business_id(original_id)
        {
            return Err(DomainError::InvalidRequest(
                "invalid historical reversal identity".into(),
            ));
        }
        let request_hash = reversal_request_hash(&reversal, reason.as_deref());
        let svc = self.clone();
        let ctx = ctx.clone();
        let scope = scope.clone();
        let op = RetryOp::new("entry reversal", Some(reversal.tenant_id));
        retry_transaction_for(&self.db.db(), op, move |txn| {
            let svc = svc.clone();
            let ctx = ctx.clone();
            let scope = scope.clone();
            let reversal = reversal.clone();
            let reason = reason.clone();
            let request_hash = request_hash.clone();
            Box::pin(async move {
                let original = svc
                    .journal
                    .find_entry_with_lines_in(txn, &scope, reversal.tenant_id, original_id)
                    .await?
                    .ok_or_else(|| {
                        DomainError::InvalidRequest("original entry unavailable in scope".into())
                    })?;
                if original.source_doc_type == SourceDocType::Reversal.as_str() {
                    return Err(
                        DomainError::InvalidRequest("cannot reverse a reversal".into()).into(),
                    );
                }
                if original
                    .lines
                    .iter()
                    .any(|line| line.account_class == "REUSABLE_CREDIT")
                {
                    return Err(DomainError::InvalidRequest(
                        "cannot reverse an entry with a REUSABLE_CREDIT line".into(),
                    )
                    .into());
                }
                if reversal.reverses_period_id.as_deref() != Some(original.period_id.as_str())
                    || reversal.entry_currency != original.entry_currency
                {
                    return Err(DomainError::InvalidRequest(
                        "reversal original header references disagree".into(),
                    )
                    .into());
                }
                if let Some(result) = replay_in(
                    txn,
                    reversal.tenant_id,
                    reversal.source_doc_type,
                    &reversal.source_business_id,
                    &request_hash,
                )
                .await?
                {
                    return Ok(result);
                }
                let mut header = new_entry(&reversal);
                header.legal_entity_id = original.legal_entity_id;
                header.rounding_evidence = original.rounding_evidence;
                let sidecar = reason.map(|reason| {
                    Arc::new(ReversalEventSidecar {
                        publisher: Arc::clone(&svc.publisher),
                        ctx: ctx.clone(),
                        tenant_id: reversal.tenant_id,
                        reverses_entry_id: original_id,
                        reason,
                    }) as Arc<dyn PostSidecar>
                });
                svc.posting
                    .post_reversal_once(
                        &ctx,
                        txn,
                        &scope,
                        header,
                        sidecar,
                        ClaimSpec::fresh_with_request_hash(request_hash),
                    )
                    .await
            })
        })
        .await
    }

    /// Post a corrected re-post (`MAPPING_CORRECTION`) whose freshly-built lines
    /// carry nil placeholder `account_id`s: binds each from the provisioned chart
    /// (like the invoice-post path) then posts, preserving the correction's
    /// `source_doc_type` + `reverses_*` header. Records `invoice_post` + duration.
    ///
    /// # Errors
    /// [`DomainError`] on an unmapped account / foundation rejection / infra fault.
    pub async fn post_correction(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        correction: PostEntry,
    ) -> Result<bss_ledger_sdk::PostingRef, DomainError> {
        let started = Instant::now();
        // A mapping-correction re-post carries no recognition sidecar (it
        // re-books an already-recognized split; recognition schedules are
        // materialized on the original post).
        let tenant = correction.tenant_id;
        let business = correction.source_business_id.clone();
        let flow = correction.source_doc_type;
        let result = self.correction_inner(ctx, scope, correction).await;
        self.emit_terminal_alarm(ctx, tenant, flow, &business, &result)
            .await;
        self.record(&result, started, PostFlow::MappingCorrection);
        result
    }

    /// Fresh correction mapping, with all current metadata on the attempt runner.
    async fn correction_inner(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        correction: PostEntry,
    ) -> Result<PostingRef, DomainError> {
        let hash = IdempotencyGate::payload_hash(
            &new_entry(&correction),
            &correction
                .lines
                .iter()
                .cloned()
                .map(new_line)
                .collect::<Vec<_>>(),
        );
        let svc = self.clone();
        let ctx = ctx.clone();
        let scope = scope.clone();
        let op = RetryOp::new("mapping correction", Some(correction.tenant_id));
        retry_transaction_for(&self.db.db(), op, move |txn| {
            let svc = svc.clone();
            let ctx = ctx.clone();
            let scope = scope.clone();
            let correction = correction.clone();
            let hash = hash.clone();
            Box::pin(async move {
                authorize_entry_in(
                    &svc.journal,
                    txn,
                    &scope,
                    &ctx,
                    correction.tenant_id,
                    correction.source_doc_type,
                    &correction.source_business_id,
                )
                .await?;
                if let Some(result) = replay_in(
                    txn,
                    correction.tenant_id,
                    correction.source_doc_type,
                    &correction.source_business_id,
                    &hash,
                )
                .await?
                {
                    return Ok(result);
                }
                svc.bind_post_once(
                    &ctx,
                    txn,
                    &scope,
                    correction,
                    None,
                    ClaimSpec::fresh_with_request_hash(hash),
                    false,
                )
                .await
            })
        })
        .await
    }

    /// Bind, translate and post complete fresh effects on the supplied attempt.
    async fn bind_post_once(
        &self,
        ctx: &SecurityContext,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        mut entry: PostEntry,
        sidecar: Option<Arc<dyn PostSidecar>>,
        claim: ClaimSpec,
        translate: bool,
    ) -> Result<PostingRef, AttemptError> {
        let chart = load_chart_in(&self.reference, txn, scope, entry.tenant_id).await?;
        for line in &mut entry.lines {
            line.account_id = resolve_line(&chart, line).ok_or_else(|| {
                DomainError::AccountClosed(format!(
                    "no provisioned account for class {} / stream {:?} / currency {}",
                    line.account_class.as_str(),
                    line.revenue_stream,
                    line.money.currency().code()
                ))
            })?;
        }
        let mut header = new_entry(&entry);
        let mut lines: Vec<_> = entry.lines.into_iter().map(new_line).collect();
        if translate {
            let functional = self
                .reference
                .functional_currency_in(txn, scope, header.tenant_id)
                .await?;
            if let Some(code) = functional.filter(|code| code != &header.entry_currency) {
                let scale = self
                    .resolver
                    .resolve_in(txn, scope, header.tenant_id, &header.entry_currency)
                    .await?;
                let transaction =
                    bss_ledger_sdk::CurrencySpec::try_new(header.entry_currency.clone(), scale)
                        .map_err(|e| DomainError::InvalidRequest(e.to_string()))?;
                let scale = self
                    .resolver
                    .resolve_in(txn, scope, header.tenant_id, &code)
                    .await?;
                let functional = bss_ledger_sdk::CurrencySpec::try_new(code, scale)
                    .map_err(|e| DomainError::InvalidRequest(e.to_string()))?;
                header.rate_snapshot_ref = self
                    .rate_locker
                    .lock_and_stamp_in(
                        txn,
                        scope,
                        header.tenant_id,
                        &mut lines,
                        &transaction,
                        &functional,
                        OffsetDateTime::now_utc(),
                    )
                    .await?;
            }
        }
        self.posting
            .post_once(ctx, txn, scope, header, lines, sidecar, claim)
            .await
    }

    /// Preserve the core's terminal invariant alarm outside the retry budget.
    async fn emit_terminal_alarm(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        flow: SourceDocType,
        business: &str,
        result: &Result<PostingRef, DomainError>,
    ) {
        if let Err(error) = result
            && let Some((category, severity, code)) =
                crate::infra::posting::service::alarm_for(error)
        {
            self.publisher
                .emit_invariant_alarm(
                    ctx,
                    crate::infra::events::payloads::LedgerInvariantAlarm {
                        category,
                        severity,
                        tenant_id: tenant,
                        scope: format!(
                            "tenant:{tenant}/flow:{}/business:{business}",
                            flow.as_str()
                        ),
                        code: code.into(),
                        detail: error.to_string(),
                        affected: Vec::new(),
                    },
                )
                .await;
        }
    }

    /// Emit `invoice_post(outcome, flow)` + the flow-labelled duration for one
    /// attempt. `flow` keeps reversals/corrections off the invoice-post rate.
    fn record(
        &self,
        result: &Result<bss_ledger_sdk::PostingRef, DomainError>,
        started: Instant,
        flow: PostFlow,
    ) {
        let outcome = match result {
            Ok(r) if r.replayed => PostResult::Replayed,
            Ok(_) => PostResult::Posted,
            Err(_) => PostResult::Rejected,
        };
        self.metrics.invoice_post(outcome, flow);
        self.metrics
            .invoice_post_duration(started.elapsed().as_secs_f64(), flow);
    }
}

/// The production [`InvoicePoster`]: delegates to the inherent methods (which
/// the in-crate integration tests also call on the concrete type). Lets the REST
/// surface hold `Arc<dyn InvoicePoster>` and the router tests stub the writes.
#[async_trait::async_trait]
impl InvoicePoster for InvoicePostService {
    async fn post_invoice(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        inv: &PostedInvoice,
        payer_open: bool,
    ) -> Result<bss_ledger_sdk::PostingRef, DomainError> {
        InvoicePostService::post_invoice(self, ctx, scope, inv, payer_open).await
    }

    async fn post_reversal(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        reversal: PostEntry,
        reason: Option<String>,
    ) -> Result<bss_ledger_sdk::PostingRef, DomainError> {
        InvoicePostService::post_reversal(self, ctx, scope, reversal, reason).await
    }

    async fn post_correction(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        correction: PostEntry,
    ) -> Result<bss_ledger_sdk::PostingRef, DomainError> {
        InvoicePostService::post_correction(self, ctx, scope, correction).await
    }
}

/// The C4 PO-allocation-group gate (design §4.4, Rev2 N-revrec-3) — owned by
/// Slice 4's recognition orchestration, NOT a mutation of Slice 1's invoice-post
/// endpoint. It fires ONLY for a genuinely ambiguous obligation: a **deferring**
/// (straight-line) / **multi-PO** / **VC** line whose PO allocation group cannot
/// be resolved (absent / blank on the input) AND cannot be defaulted by the
/// Catalog AND is not R4-exempt ⇒ [`DomainError::MissingPoAllocationGroup`].
///
/// An ordinary point-in-time line (the routine-billing common case) is never
/// blocked: it is auto-defaulted (the Catalog default group auto-tags it) and is
/// not a multi-PO / VC obligation, so the gate does not apply. v1 has no Catalog
/// reader, so "cannot be defaulted" reduces to "the input carries no
/// `po_allocation_group`" for an obligation that needs one; a future Catalog-
/// default resolver drops in here without changing the trigger condition.
///
/// The R4 immaterial-one-shot exemption short-circuits the gate (an exempt
/// one-shot recognizes point-in-time and needs no PO group); the materiality
/// threshold itself is re-checked inside the derivation, so here the
/// SKU-eligibility flag is the conservative exemption signal.
///
/// # Errors
/// [`DomainError::MissingPoAllocationGroup`] when the obligation needs a PO
/// allocation group and none is resolvable / defaultable.
fn check_po_allocation_group(
    input: &RecognitionInput,
    item_amount_ex_tax: &PostedMoney,
    invoice_total: &PostedMoney,
) -> Result<(), DomainError> {
    let needs_group =
        input.timing.is_deferred() || input.multi_po || input.vc_estimate_ref.is_some();
    if !needs_group {
        return Ok(());
    }
    // R4-exempt one-shots recognize point-in-time and need no PO group — but only
    // when ACTUALLY immaterial (the SKU flag AND under the materiality threshold),
    // matching the derivation's R4 check exactly: an over-threshold flagged line
    // still defers, so it must not skip the gate.
    if input.immaterial_one_shot_sku && is_immaterial(item_amount_ex_tax, invoice_total)? {
        return Ok(());
    }
    let resolvable = input
        .po_allocation_group
        .as_deref()
        .is_some_and(|g| !g.is_empty());
    if resolvable {
        return Ok(());
    }
    Err(DomainError::MissingPoAllocationGroup(format!(
        "deferring/multi-PO/VC recognition line (policy `{}`) has no resolvable or \
         defaultable po_allocation_group",
        input.policy_ref
    )))
}

/// Thin `PostLine` adapter over [`ChartIndex::resolve`]: projects a built
/// line's `(account_class, currency, revenue_stream)` onto the key-based
/// resolver. Per-stream classes key on the line's stream; the rest resolve
/// stream-less.
fn resolve_line(chart: &ChartIndex, line: &PostLine) -> Option<Uuid> {
    chart.resolve(
        line.account_class,
        line.money.currency().code(),
        line.revenue_stream.as_deref(),
    )
}

/// Count the built lines that would park on SUSPENSE/PENDING — the suspense
/// backlog this invoice contributes. Pure over the input (mirrors the mapping
/// resolver) so the gauge needs no read-back.
fn pending_line_count(inv: &PostedInvoice) -> i64 {
    let n = inv
        .items
        .iter()
        .filter(|i| resolve(i).mapping_status == MappingStatus::Pending)
        .count();
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Copy one SDK [`PostLine`] into the engine without replacing money metadata.
fn new_line(line: PostLine) -> NewLine {
    NewLine {
        line_id: line.line_id,
        payer_tenant_id: line.payer_tenant_id,
        seller_tenant_id: line.seller_tenant_id,
        resource_tenant_id: line.resource_tenant_id,
        account_id: line.account_id,
        account_class: line.account_class,
        gl_code: line.gl_code,
        side: line.side,
        money: line.money,
        invoice_id: line.invoice_id,
        due_date: line.due_date,
        revenue_stream: line.revenue_stream,
        mapping_status: line.mapping_status,
        functional_money: line.functional_money,
        tax_jurisdiction: line.tax_jurisdiction,
        tax_filing_period: line.tax_filing_period,
        tax_rate_ref: line.tax_rate_ref,
        legal_entity_id: None,
        invoice_item_ref: line.invoice_item_ref,
        sku_or_plan_ref: line.sku_or_plan_ref,
        price_id: line.price_id,
        pricing_snapshot_ref: line.pricing_snapshot_ref,
        po_allocation_group: line.po_allocation_group,
        credit_grant_event_type: line.credit_grant_event_type,
        ar_status: line.ar_status,
    }
}

/// Preserve SDK money exactly; the core validates current metadata on this runner.
fn new_entry(entry: &PostEntry) -> NewEntry {
    NewEntry {
        entry_id: entry.entry_id,
        tenant_id: entry.tenant_id,
        legal_entity_id: entry.tenant_id,
        period_id: entry.period_id.clone(),
        entry_currency: entry.entry_currency.clone(),
        source_doc_type: entry.source_doc_type,
        source_business_id: entry.source_business_id.clone(),
        reverses_entry_id: entry.reverses_entry_id,
        reverses_period_id: entry.reverses_period_id.clone(),
        posted_at_utc: OffsetDateTime::now_utc(),
        effective_at: entry.effective_at,
        origin: ORIGIN_SYSTEM.into(),
        posted_by_actor_id: entry.posted_by_actor_id,
        correlation_id: entry.correlation_id,
        rounding_evidence: serde_json::Value::Null,
        rate_snapshot_ref: None,
    }
}

/// Request replay reads a tenant-owned claim only AFTER the scoped target check.
/// No writes occur here; the complete core seam still owns claim/finalize races.
async fn replay_in(
    txn: &DbTx<'_>,
    tenant: Uuid,
    flow: SourceDocType,
    business: &str,
    hash: &str,
) -> Result<Option<PostingRef>, AttemptError> {
    let Some(row) = IdempotencyGate::new()
        .read(txn, tenant, flow.as_str(), business)
        .await?
    else {
        return Ok(None);
    };
    if row.payload_hash != hash {
        return Err(DomainError::IdempotencyConflict(
            "idempotency key reused with a different invoice intent".into(),
        )
        .into());
    }
    match (row.status.as_str(), row.result_entry_id) {
        ("POSTED", Some(entry_id)) => Ok(Some(PostingRef {
            entry_id,
            created_seq: 0,
            replayed: true,
        })),
        _ => Err(DomainError::Internal("invoice claim is not finalized".into()).into()),
    }
}

/// ENTRY scopes cannot be reused as account/schedule resource identities. Tenant
/// grants support fresh operations; constrained grants can authorize an existing
/// target replay. Dependency queries retain the caller scope and fail closed. A
/// refused fresh operation is logged with the caller, the flow and the business
/// id it tried to post.
async fn authorize_entry_in(
    journal: &JournalRepo,
    txn: &DbTx<'_>,
    scope: &AccessScope,
    ctx: &SecurityContext,
    tenant: Uuid,
    flow: SourceDocType,
    business: &str,
) -> Result<(), AttemptError> {
    let existing = journal
        .find_entry_id_by_business_key_in(txn, scope, tenant, flow, business)
        .await?;
    if existing.is_some() {
        return Ok(());
    }
    authorize_tenant_scope(scope, tenant, flow).map_err(|refusal| {
        tracing::warn!(
            target: "bss-ledger",
            subject_id = %ctx.subject_id(),
            subject_tenant_id = %ctx.subject_tenant_id(),
            tenant_id = %tenant,
            flow = flow.as_str(),
            business_id = business,
            refusal = %refusal,
            "bss-ledger: fresh {} refused by the caller's grant",
            flow_label(flow)
        );
        refusal.into()
    })
}

/// The human label of a flow this module posts, for the refusal log.
fn flow_label(flow: SourceDocType) -> &'static str {
    match flow {
        SourceDocType::InvoicePost => "invoice post",
        SourceDocType::MappingCorrection => "mapping correction",
        other => other.as_str(),
    }
}

/// Fresh operation authorization is limited to representable tenant-only grants.
///
/// # Errors
/// [`DomainError::CrossTenantAccessDenied`] when no grant names `tenant` at all;
/// [`DomainError::InvalidRequest`] when a grant names `tenant` but also
/// constrains something else (a resource id, or a filter that cannot be
/// evaluated in memory), so it cannot authorize a fresh operation.
fn authorize_tenant_scope(
    scope: &AccessScope,
    tenant: Uuid,
    flow: SourceDocType,
) -> Result<(), DomainError> {
    if scope.is_unconstrained() {
        return Ok(());
    }
    let property = toolkit_security::pep_properties::OWNER_TENANT_ID;
    let names_tenant = |filter: &toolkit_security::ScopeFilter| {
        filter.property() == property
            && filter
                .values()
                .contains(&toolkit_security::ScopeValue::Uuid(tenant))
    };
    let permitted = scope.constraints().iter().any(|constraint| {
        !constraint.filters().is_empty()
            && constraint
                .filters()
                .iter()
                .all(|filter| names_tenant(filter) && filter.is_representable_in_memory())
    });
    if permitted {
        return Ok(());
    }
    let tenant_named = scope
        .constraints()
        .iter()
        .any(|constraint| constraint.filters().iter().any(names_tenant));
    if tenant_named {
        Err(DomainError::InvalidRequest(format!(
            "the caller's grant for tenant {tenant} carries constraints beyond the tenant; \
             only a tenant-wide grant can authorize a fresh {}",
            flow_label(flow)
        )))
    } else {
        Err(DomainError::CrossTenantAccessDenied(format!(
            "tenant {tenant} is outside the caller's scope"
        )))
    }
}

/// Invoice failures preserve named numeric metadata/range errors.
pub(crate) fn map_invoice_error(error: InvoiceError) -> DomainError {
    match error {
        // The one exact-arithmetic table: named money errors keep their wire
        // codes, the arithmetic limit is a range error, structural defects are
        // invariant failures.
        InvoiceError::Exact(error) => crate::domain::exact_money::map_exact_error(error),
        InvoiceError::EmptyInvoice | InvoiceError::MappingLengthMismatch => {
            DomainError::InvalidRequest(error.to_string())
        }
        InvoiceError::NegativeAmount | InvoiceError::InvalidDeferral => {
            DomainError::AmountOutOfRange(error.to_string())
        }
    }
}

/// Closed typed invoice frame. Item/tax ordering is intentional (first source
/// reference wins grouping). Mutable payer state, generated IDs, correlation and
/// derivation/config snapshots do not identify the caller's stable intent.
fn invoice_request_hash(inv: &PostedInvoice) -> String {
    let mut bytes = Vec::new();
    put_str(&mut bytes, "ledger.invoice-intent.v1");
    put_str(&mut bytes, &inv.invoice_id);
    put_uuid(&mut bytes, inv.seller_tenant_id);
    put_uuid(&mut bytes, inv.payer_tenant_id);
    put_opt_uuid(&mut bytes, inv.resource_tenant_id);
    put_str(&mut bytes, &inv.period_id);
    put_str(&mut bytes, &inv.effective_at.to_string());
    put_opt_str(
        &mut bytes,
        inv.due_date.map(|date| date.to_string()).as_deref(),
    );
    put_str(&mut bytes, &inv.items.len().to_string());
    for item in &inv.items {
        put_money(&mut bytes, &item.amount_ex_tax);
        put_money(&mut bytes, &item.deferred);
        put_str(&mut bytes, &item.revenue_stream);
        put_opt_str(&mut bytes, item.catalog_class.map(|class| class.as_str()));
        put_opt_str(&mut bytes, item.contract_class.map(|class| class.as_str()));
        for field in [
            &item.gl_code,
            &item.invoice_item_ref,
            &item.sku_or_plan_ref,
            &item.price_id,
            &item.pricing_snapshot_ref,
        ] {
            put_opt_str(&mut bytes, field.as_deref());
        }
        match &item.recognition {
            None => put(&mut bytes, &[0]),
            Some(input) => {
                put(&mut bytes, &[1]);
                put_str(&mut bytes, &input.policy_ref);
                match &input.timing {
                    RecognitionTiming::PointInTime => put_str(&mut bytes, "POINT_IN_TIME"),
                    RecognitionTiming::StraightLine {
                        periods,
                        first_period_id,
                    } => {
                        put_str(&mut bytes, "STRAIGHT_LINE");
                        put(&mut bytes, &periods.to_be_bytes());
                        put_opt_str(&mut bytes, first_period_id.as_deref());
                    }
                }
                for field in [
                    &input.po_allocation_group,
                    &input.ssp_snapshot_ref,
                    &input.subscription_ref,
                    &input.vc_estimate_ref,
                    &input.vc_method_ref,
                ] {
                    put_opt_str(&mut bytes, field.as_deref());
                }
                put(&mut bytes, &[u8::from(input.multi_po)]);
                put(&mut bytes, &[u8::from(input.immaterial_one_shot_sku)]);
            }
        }
    }
    put_str(&mut bytes, &inv.tax.len().to_string());
    for tax in &inv.tax {
        put_money(&mut bytes, &tax.amount);
        put_str(&mut bytes, &tax.tax_jurisdiction);
        put_str(&mut bytes, &tax.tax_filing_period);
        put_opt_str(&mut bytes, tax.tax_rate_ref.as_deref());
    }
    hash_bytes(&bytes)
}

/// Historical money is not caller intent. Original lineage, destination and
/// explicit reason (including absent versus present-empty) bind reversal audit.
fn reversal_request_hash(entry: &PostEntry, reason: Option<&str>) -> String {
    let mut bytes = Vec::new();
    put_str(&mut bytes, "ledger.invoice-reversal-intent.v1");
    put_uuid(&mut bytes, entry.tenant_id);
    put_str(&mut bytes, &entry.source_business_id);
    put_opt_uuid(&mut bytes, entry.reverses_entry_id);
    put_opt_str(&mut bytes, entry.reverses_period_id.as_deref());
    put_str(&mut bytes, &entry.period_id);
    put_str(&mut bytes, &entry.entry_currency);
    put_str(&mut bytes, &entry.effective_at.to_string());
    put_opt_str(&mut bytes, reason);
    hash_bytes(&bytes)
}

/// Existing canonical digest rendered as lowercase hexadecimal.
fn hash_bytes(bytes: &[u8]) -> String {
    digest32_hex(bytes)
}

#[cfg(test)]
#[path = "invoice_post_decimal_tests.rs"]
mod decimal_tests;
