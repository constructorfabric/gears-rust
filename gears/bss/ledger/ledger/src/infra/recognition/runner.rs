//! Recognition releases each segment in one complete serializable retry attempt.
//! Due rows are advisory identities. Mutable money, status, predecessors, periods
//! and registry gates are rebuilt on that attempt. A period run resolves the
//! chart of accounts once per pass (account ids are immutable; the posting
//! transaction re-checks each bound account is still OPEN), while a direct
//! single-segment release resolves it inside its attempt. Historical reversals
//! restore the complete original journal through the core stored-evidence seam.
//! Publishers remain parked; no broker or transactional outbox guarantee is made.

use crate::domain::canonical::{digest32_hex, put_money, put_str};
use crate::domain::error::DomainError;
use crate::domain::model::{NewEntry, NewLine};
use crate::domain::ports::metrics::LedgerMetricsPort;
use crate::domain::ports::obligation_state::{
    AlwaysSatisfiedObligationState, ObligationContext, ObligationStateResolver,
};
use crate::domain::status::{
    SCHEDULE_STATUS_ACTIVE, SEGMENT_STATUS_DONE, SEGMENT_STATUS_PENDING, SEGMENT_STATUS_QUEUED,
};
use crate::infra::events::payloads::{
    AffectedItem, AlarmCategory, AlarmSeverity, LedgerInvariantAlarm,
};
use crate::infra::events::publisher::LedgerEventPublisher;
use crate::infra::posting::chart::{ChartIndex, load_chart_in};
use crate::infra::posting::idempotency::IdempotencyGate;
use crate::infra::posting::retry::{AttemptError, RetryOp, retry_transaction_for};
use crate::infra::posting::service::{ClaimSpec, PostSidecar, PostingService};
use crate::infra::recognition::repo_errors::map_recognition_repo_err;
use crate::infra::recognition::sidecar::{RecognitionReversalSidecar, RecognitionStampSidecar};
use crate::infra::storage::repo::recognition_repo::{
    DuePendingSegment, RecognitionRepo, ScheduleState, SegmentState,
};
use crate::infra::storage::repo::{JournalRepo, PaymentRepo, ReferenceRepo};
use bss_ledger_sdk::{
    AccountClass, MappingStatus, PostEntry, PostLine, PostedMoney, PostingRef, Side, SourceDocType,
};
use chrono::NaiveDate;
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, DbTx};
use toolkit_db::{DBProvider, DbError};
use toolkit_security::SecurityContext;
use uuid::Uuid;
const ORIGIN_SYSTEM: &str = "SYSTEM";

/// Advisory candidate. Only schedule and segment identities drive authoritative reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleasableSegment {
    pub schedule_id: String,
    pub segment_no: i32,
    pub period_id: String,
    pub amount: PostedMoney,
    pub revenue_stream: String,
}
impl From<DuePendingSegment> for ReleasableSegment {
    fn from(s: DuePendingSegment) -> Self {
        Self {
            schedule_id: s.schedule_id,
            segment_no: s.segment_no,
            period_id: s.period_id,
            amount: s.amount,
            revenue_stream: s.revenue_stream,
        }
    }
}
/// Committed release or replay reference.
#[derive(Clone, Debug)]
pub struct ReleasedSegment {
    pub schedule_id: String,
    pub segment_no: i32,
    pub posting: PostingRef,
}
/// Per-pass accounting; each release commits independently.
#[derive(Clone, Debug, Default)]
pub struct RunPeriodSummary {
    pub released: usize,
    pub replayed: usize,
    pub queued: usize,
    pub skipped: usize,
    pub segments: Vec<ReleasedSegment>,
}
/// A committed operation may post, park, or leave an ineligible candidate untouched.
enum SegmentOutcome {
    Posted(PostingRef, String),
    Queued(String),
    Skipped,
}

/// Owns the per-segment transaction budget and post-commit observations.
#[derive(Clone)]
pub struct RecognitionRunner {
    db: DBProvider<DbError>,
    posting: PostingService,
    reference: ReferenceRepo,
    recognition: Arc<RecognitionRepo>,
    journal: JournalRepo,
    /// Carried account balances (the S6 functional-money gate).
    payments: PaymentRepo,
    idempotency: IdempotencyGate,
    obligation: Arc<dyn ObligationStateResolver>,
    metrics: Arc<dyn LedgerMetricsPort>,
    publisher: Arc<LedgerEventPublisher>,
}
impl RecognitionRunner {
    /// Bind all dependencies to the same database.
    #[must_use]
    pub fn new(
        db: DBProvider<DbError>,
        publisher: Arc<LedgerEventPublisher>,
        metrics: Arc<dyn LedgerMetricsPort>,
    ) -> Self {
        let posting = PostingService::new(db.clone(), publisher.clone());
        let reference = ReferenceRepo::new(db.clone());
        let recognition = Arc::new(RecognitionRepo::new(db.clone()));
        let journal = JournalRepo::new(db.clone());
        let payments = PaymentRepo::new(db.clone());
        Self {
            db,
            posting,
            reference,
            recognition,
            journal,
            payments,
            idempotency: IdempotencyGate::new(),
            obligation: Arc::new(AlwaysSatisfiedObligationState),
            metrics,
            publisher,
        }
    }
    /// Discover candidates, then independently commit each authoritative decision.
    ///
    /// # Errors
    /// Any [`DomainError`] a per-segment release raises ([`DomainError::OverRecognition`],
    /// [`DomainError::PeriodClosed`], [`DomainError::AccountClosed`], …), the mapped repository
    /// error when the due-segment scan fails, or [`DomainError::Internal`] on an infrastructure
    /// fault.
    pub async fn run_period(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant: Uuid,
        period_id: &str,
        run_id: Uuid,
    ) -> Result<RunPeriodSummary, DomainError> {
        let due = self
            .recognition
            .list_due_pending_segments(scope, tenant, period_id)
            .await
            .map_err(map_recognition_repo_err)?;
        let mut summary = RunPeriodSummary::default();
        // One chart scan per pass, not one per segment and attempt inside each
        // serializable transaction: ids are immutable and the posting transaction
        // re-checks each bound account is OPEN.
        let chart = if due.is_empty() {
            None
        } else {
            let conn = self
                .db
                .conn()
                .map_err(|e| DomainError::Internal(format!("recognition chart connection: {e}")))?;
            Some(Arc::new(
                load_chart_in(&self.reference, &conn, scope, tenant).await?,
            ))
        };
        for candidate in due {
            let candidate = ReleasableSegment::from(candidate);
            match self
                .release_operation_with_chart(
                    ctx,
                    scope,
                    tenant,
                    &candidate,
                    period_id,
                    run_id,
                    chart.clone(),
                )
                .await?
            {
                SegmentOutcome::Posted(posting, _) => {
                    if posting.replayed {
                        summary.replayed += 1;
                    } else {
                        summary.released += 1;
                    }
                    summary.segments.push(ReleasedSegment {
                        schedule_id: candidate.schedule_id,
                        segment_no: candidate.segment_no,
                        posting,
                    });
                }
                SegmentOutcome::Queued(_) => summary.queued += 1,
                SegmentOutcome::Skipped => summary.skipped += 1,
            }
        }
        self.metrics
            .recognition_period_queue_depth(i64::try_from(summary.queued).unwrap_or(i64::MAX));
        Ok(summary)
    }
    /// Release a discovered identity; caller money is advisory and never posted.
    /// Direct callers receive a policy error if the fresh decision parks or skips.
    ///
    /// # Errors
    /// [`DomainError::RecognitionPolicyConflict`] when the fresh in-transaction decision parks
    /// or skips the segment instead of posting it; otherwise any [`DomainError`] the release
    /// raises (a foundation rejection such as [`DomainError::PeriodClosed`] or
    /// [`DomainError::AccountClosed`], or [`DomainError::Internal`] on an infrastructure
    /// fault).
    pub async fn release_segment(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant: Uuid,
        segment: &ReleasableSegment,
        run_id: Uuid,
    ) -> Result<PostingRef, DomainError> {
        match self
            .release_operation(ctx, scope, tenant, segment, &segment.period_id, run_id)
            .await?
        {
            SegmentOutcome::Posted(posting, _) => Ok(posting),
            _ => Err(DomainError::RecognitionPolicyConflict(
                "segment is not releasable on this pass".into(),
            )),
        }
    }
    /// One whole-operation retry that resolves the chart inside its attempt.
    async fn release_operation(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant: Uuid,
        candidate: &ReleasableSegment,
        target_period: &str,
        run_id: Uuid,
    ) -> Result<SegmentOutcome, DomainError> {
        self.release_operation_with_chart(
            ctx,
            scope,
            tenant,
            candidate,
            target_period,
            run_id,
            None,
        )
        .await
    }
    /// One whole-operation retry; no retry-owning posting wrapper is nested here.
    /// `chart` is the pass's chart (a period run); `None` resolves it inside the
    /// attempt (a direct release).
    async fn release_operation_with_chart(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant: Uuid,
        candidate: &ReleasableSegment,
        target_period: &str,
        run_id: Uuid,
        chart: Option<Arc<ChartIndex>>,
    ) -> Result<SegmentOutcome, DomainError> {
        // E4 run gating, resolved before the serializable transaction opens: an
        // outbound obligation port must never hold a transaction open while it
        // answers. The answer is applied inside the attempt after the replay and
        // status checks, so an already-posted release still replays; a
        // NOT-satisfied obligation delays a releasable segment (left PENDING; a
        // later run retries).
        let subscription_ref = self
            .recognition
            .read_schedule(scope, tenant, &candidate.schedule_id)
            .await
            .map_err(map_recognition_repo_err)?
            .and_then(|schedule| schedule.subscription_ref);
        let obligation = ObligationContext {
            tenant_id: tenant,
            schedule_id: candidate.schedule_id.clone(),
            subscription_ref,
        };
        let obligation_satisfied = self.obligation.is_satisfied(&obligation).await;
        let svc = self.clone();
        let ctx_txn = ctx.clone();
        let scope_txn = scope.clone();
        let alarm_candidate = candidate.clone();
        let candidate = candidate.clone();
        let target_period = target_period.to_owned();
        let op = RetryOp::new("recognition release", Some(tenant));
        let outcome = retry_transaction_for(&self.db.db(), op, move |txn| {
            let svc = svc.clone();
            let ctx = ctx_txn.clone();
            let scope = scope_txn.clone();
            let candidate = candidate.clone();
            let target_period = target_period.clone();
            let chart = chart.clone();
            Box::pin(async move {
                svc.release_once(
                    txn,
                    &ctx,
                    &scope,
                    tenant,
                    &candidate,
                    &target_period,
                    run_id,
                    chart.as_deref(),
                    obligation_satisfied,
                )
                .await
            })
        })
        .await;
        match &outcome {
            Ok(SegmentOutcome::Posted(posting, stream)) if !posting.replayed => {
                self.metrics.revenue_recognized(stream);
            }
            Ok(SegmentOutcome::Queued(period)) => {
                self.emit_period_queued(
                    ctx,
                    tenant,
                    &alarm_candidate.schedule_id,
                    alarm_candidate.segment_no,
                    period,
                )
                .await;
            }
            Err(DomainError::OverRecognition(_)) => self.metrics.over_recognition(),
            _ => {}
        }
        if let Err(error) = &outcome {
            self.emit_post_error(ctx, scope, tenant, &alarm_candidate, error, true)
                .await;
        }
        outcome
    }
    /// Authorize target before reading claim evidence; terminal state still authorizes replay.
    async fn observed(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        id: &str,
        no: i32,
    ) -> Result<(ScheduleState, SegmentState), AttemptError> {
        let schedule = self
            .recognition
            .read_schedule_in_txn(txn, scope, tenant, id)
            .await
            .map_err(map_recognition_repo_err)?
            .ok_or_else(|| {
                DomainError::RecognitionPolicyConflict(
                    "schedule unavailable in caller scope".into(),
                )
            })?;
        let segment = self
            .recognition
            .read_segment_of(txn, scope, &schedule, no)
            .await
            .map_err(map_recognition_repo_err)?
            .ok_or_else(|| {
                DomainError::RecognitionPolicyConflict("segment unavailable in caller scope".into())
            })?;
        Ok((schedule, segment))
    }
    /// Immutable evidence replay precedes chart, registry, period and eligibility gates.
    async fn replay(
        &self,
        txn: &DbTx<'_>,
        tenant: Uuid,
        business: &str,
        hash: &str,
    ) -> Result<Option<PostingRef>, AttemptError> {
        let Some(prior) = self
            .idempotency
            .read(txn, tenant, SourceDocType::Recognition.as_str(), business)
            .await?
        else {
            return Ok(None);
        };
        if prior.payload_hash != hash {
            return Err(
                DomainError::IdempotencyConflict("recognition evidence differs".into()).into(),
            );
        }
        if prior.status != crate::infra::posting::idempotency::STATUS_POSTED {
            return Err(
                DomainError::Internal("recognition claim has no committed posting".into()).into(),
            );
        }
        let entry_id = prior
            .result_entry_id
            .ok_or_else(|| DomainError::Internal("recognition posting result is absent".into()))?;
        Ok(Some(PostingRef {
            entry_id,
            created_seq: 0,
            replayed: true,
        }))
    }
    async fn release_once(
        &self,
        txn: &DbTx<'_>,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant: Uuid,
        candidate: &ReleasableSegment,
        target_period: &str,
        run_id: Uuid,
        chart: Option<&ChartIndex>,
        obligation_satisfied: bool,
    ) -> Result<SegmentOutcome, AttemptError> {
        let (schedule, segment) = self
            .observed(
                txn,
                scope,
                tenant,
                &candidate.schedule_id,
                candidate.segment_no,
            )
            .await?;
        let business = recognition_business_id(&segment.schedule_id, segment.segment_no);
        let hash = release_hash(&schedule, &segment);
        if let Some(posting) = self.replay(txn, tenant, &business, &hash).await? {
            return Ok(SegmentOutcome::Posted(posting, schedule.revenue_stream));
        }
        if schedule.status != SCHEDULE_STATUS_ACTIVE
            || !(segment.status == SEGMENT_STATUS_PENDING
                || segment.status == SEGMENT_STATUS_QUEUED)
            || segment.period_id.as_str() > target_period
            || !obligation_satisfied
        {
            return Ok(SegmentOutcome::Skipped);
        }
        if self
            .recognition
            .count_predecessors_not_done_in(
                txn,
                scope,
                tenant,
                &segment.schedule_id,
                &segment.period_id,
            )
            .await
            .map_err(map_recognition_repo_err)?
            > 0
        {
            self.recognition
                .mark_segment_queued_in(
                    txn,
                    scope,
                    tenant,
                    &segment.schedule_id,
                    segment.segment_no,
                )
                .await
                .map_err(map_recognition_repo_err)?;
            return Ok(SegmentOutcome::Queued(segment.period_id));
        }
        let open = self
            .recognition
            .current_open_period_in(txn, scope, tenant)
            .await
            .map_err(map_recognition_repo_err)?;
        let period = open
            .filter(|p| segment.period_id < *p)
            .unwrap_or_else(|| segment.period_id.clone());
        let fresh = ReleasableSegment {
            schedule_id: schedule.schedule_id.clone(),
            segment_no: segment.segment_no,
            period_id: period.clone(),
            amount: segment.amount.clone(),
            revenue_stream: schedule.revenue_stream.clone(),
        };
        let loaded;
        let chart = if let Some(chart) = chart {
            chart
        } else {
            loaded = load_chart_in(&self.reference, txn, scope, tenant).await?;
            &loaded
        };
        let mut bound = build_recognition_entry(ctx, tenant, &fresh);
        for line in &mut bound.lines {
            line.account_id = resolve_line(chart, line).ok_or_else(|| {
                DomainError::AccountClosed(format!(
                    "no provisioned account for class {} / stream {:?} / currency {}",
                    line.account_class.as_str(),
                    line.revenue_stream,
                    line.money.currency().code()
                ))
            })?;
        }
        // S6 historical functional allocation is explicitly deferred. Detect actual
        // carried account evidence, never infer it from a current rate or registry.
        for line in &bound.lines {
            if self
                .payments
                .read_account_carried_in(
                    txn,
                    scope,
                    tenant,
                    line.account_id,
                    line.money.currency().code(),
                )
                .await?
                .is_some_and(|carried| carried.functional_balance.is_some())
            {
                return Err(DomainError::FxOperationUnsupported("recognition of carried functional money requires an approved S6 allocation policy".into()).into());
            }
        }
        let sidecar: Arc<dyn PostSidecar> = Arc::new(RecognitionStampSidecar {
            tenant_id: tenant,
            schedule_id: schedule.schedule_id,
            segment_no: segment.segment_no,
            period_id: period,
            amount: segment.amount,
            revenue_stream: schedule.revenue_stream.clone(),
            expected_schedule_version: schedule.version,
            expected_segment_version: segment.version,
            run_id,
            recognition_repo: self.recognition.clone(),
            publisher: self.publisher.clone(),
            ctx: ctx.clone(),
        });
        let lines = bound.lines.into_iter().map(new_line).collect();
        let entry = posting_header(
            ctx,
            tenant,
            &fresh.period_id,
            fresh.amount.currency().code(),
            business,
        );
        let posting = self
            .posting
            .post_once(
                ctx,
                txn,
                scope,
                entry,
                lines,
                Some(sidecar),
                ClaimSpec::fresh_with_request_hash(hash),
            )
            .await?;
        Ok(SegmentOutcome::Posted(posting, schedule.revenue_stream))
    }
    /// Reverse the durable original release, preserving its actual E2 posting period
    /// and full transaction/functional journal evidence. Caller amounts are ignored.
    ///
    /// # Errors
    /// [`DomainError::RecognitionPolicyConflict`] when the original release claim is absent or
    /// the segment is not in a reversible state; [`DomainError::Internal`] when the original
    /// claim carries no posted result, on corrupt stored evidence, or on an infrastructure
    /// fault; otherwise any [`DomainError`] the reversal posting raises.
    pub async fn release_reversal(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant: Uuid,
        candidate: &ReleasableSegment,
    ) -> Result<PostingRef, DomainError> {
        let svc = self.clone();
        let ctx_txn = ctx.clone();
        let scope_txn = scope.clone();
        let candidate_txn = candidate.clone();
        let op = RetryOp::new("recognition reversal", Some(tenant));
        let result = retry_transaction_for(&self.db.db(), op, move |txn| {
            let svc = svc.clone();
            let ctx = ctx_txn.clone();
            let scope = scope_txn.clone();
            let candidate = candidate_txn.clone();
            Box::pin(async move {
                let (schedule, segment) = svc
                    .observed(
                        txn,
                        &scope,
                        tenant,
                        &candidate.schedule_id,
                        candidate.segment_no,
                    )
                    .await?;
                let original_business =
                    recognition_business_id(&candidate.schedule_id, candidate.segment_no);
                let prior = svc
                    .idempotency
                    .read(
                        txn,
                        tenant,
                        SourceDocType::Recognition.as_str(),
                        &original_business,
                    )
                    .await?
                    .ok_or_else(|| {
                        DomainError::RecognitionPolicyConflict("original release is absent".into())
                    })?;
                let original_id = prior
                    .result_entry_id
                    .filter(|_| prior.status == crate::infra::posting::idempotency::STATUS_POSTED)
                    .ok_or_else(|| {
                        DomainError::Internal("original release has no posted result".into())
                    })?;
                let original = svc
                    .journal
                    .find_entry_with_lines_in(txn, &scope, tenant, original_id)
                    .await?
                    .ok_or_else(|| {
                        DomainError::RecognitionPolicyConflict(
                            "original release unavailable in caller scope".into(),
                        )
                    })?;
                if original.source_doc_type != SourceDocType::Recognition.as_str()
                    || original.source_business_id != original_business
                    || original.reverses_entry_id.is_some()
                    || original.lines.is_empty()
                {
                    return Err(DomainError::Internal(
                        "original release identity is inconsistent".into(),
                    )
                    .into());
                }
                validate_original_release(&original, &schedule, &segment)?;
                let business = reversal_business_id(&candidate.schedule_id, candidate.segment_no);
                let hash = reversal_hash(&prior.payload_hash, original_id, &original.period_id);
                if let Some(posting) = svc.replay(txn, tenant, &business, &hash).await? {
                    return Ok(posting);
                }
                let mut entry = posting_header(
                    &ctx,
                    tenant,
                    &original.period_id,
                    &original.entry_currency,
                    business,
                );
                entry.rounding_evidence = original.rounding_evidence.clone();
                entry.reverses_entry_id = Some(original_id);
                entry.reverses_period_id = Some(original.period_id.clone());
                let sidecar: Arc<dyn PostSidecar> = Arc::new(RecognitionReversalSidecar {
                    tenant_id: tenant,
                    schedule_id: schedule.schedule_id,
                    segment_no: segment.segment_no,
                    period_id: original.period_id,
                    amount: segment.amount,
                    revenue_stream: schedule.revenue_stream,
                    expected_schedule_version: schedule.version,
                    expected_segment_version: segment.version,
                    recognition_repo: svc.recognition.clone(),
                    publisher: svc.publisher.clone(),
                    ctx: ctx.clone(),
                });
                svc.posting
                    .post_reversal_once(
                        &ctx,
                        txn,
                        &scope,
                        entry,
                        Some(sidecar),
                        ClaimSpec::fresh_with_request_hash(hash),
                    )
                    .await
            })
        })
        .await;
        if let Err(error) = &result {
            self.emit_post_error(ctx, scope, tenant, candidate, error, false)
                .await;
        }
        result
    }
    /// Emit alarms after the complete attempt fails and its transaction rolls back.
    async fn emit_post_error(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant: Uuid,
        segment: &ReleasableSegment,
        error: &DomainError,
        detect_double_credit: bool,
    ) {
        if let Some((category, severity, code)) = crate::infra::posting::service::alarm_for(error) {
            self.publisher
                .emit_invariant_alarm(
                    ctx,
                    LedgerInvariantAlarm {
                        category,
                        severity,
                        tenant_id: tenant,
                        scope: format!(
                            "tenant:{tenant}/flow:RECOGNITION/business:{}:{}",
                            segment.schedule_id, segment.segment_no
                        ),
                        code: code.into(),
                        detail: error.to_string(),
                        affected: Vec::new(),
                    },
                )
                .await;
        }
        if detect_double_credit && matches!(error, DomainError::Internal(_)) {
            // Best-effort post-failure detection retains the existing DONE probe.
            let done = self
                .recognition
                .list_segments(scope, tenant, &segment.schedule_id)
                .await
                .ok()
                .into_iter()
                .flatten()
                .any(|row| {
                    row.segment_no == segment.segment_no && row.status == SEGMENT_STATUS_DONE
                });
            if done {
                self.metrics.recognition_double_credit();
                let actual_minor = crate::infra::v1_payload::v1_minor_units_for(
                    &segment.amount,
                    "actual_minor",
                    &format_args!(
                        "recognition double-credit schedule {} segment {} tenant {tenant}",
                        segment.schedule_id, segment.segment_no
                    ),
                );
                self.publisher
                    .emit_invariant_alarm(
                        ctx,
                        LedgerInvariantAlarm {
                            category: AlarmCategory::RecognitionDoubleCredit,
                            severity: AlarmSeverity::Critical,
                            tenant_id: tenant,
                            scope: format!(
                                "tenant:{tenant}/flow:RECOGNITION/business:{}:{}",
                                segment.schedule_id, segment.segment_no
                            ),
                            code: AlarmCategory::RecognitionDoubleCredit.as_str().to_owned(),
                            detail: format!(
                                "second credit attempted for an already-DONE segment \
                                 (schedule={}, segment={}): {error}",
                                segment.schedule_id, segment.segment_no
                            ),
                            affected: vec![AffectedItem {
                                id: format!("{}:{}", segment.schedule_id, segment.segment_no),
                                currency: segment.amount.currency().code().to_owned(),
                                expected_minor: 0,
                                actual_minor,
                            }],
                        },
                    )
                    .await;
            }
        }
    }

    /// Emit one out-of-band `RECOGNITION_PERIOD_QUEUED` `Warn` alarm for a segment
    /// parked out-of-order (design §4.6 / §6). Fire-and-forget; the run continues.
    async fn emit_period_queued(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        schedule_id: &str,
        segment_no: i32,
        period_id: &str,
    ) {
        let code = AlarmCategory::RecognitionPeriodQueued.as_str().to_owned();
        let alarm = LedgerInvariantAlarm {
            category: AlarmCategory::RecognitionPeriodQueued,
            severity: AlarmSeverity::Warn,
            tenant_id: tenant,
            scope: format!("tenant:{tenant}/flow:RECOGNITION/business:{schedule_id}:{segment_no}"),
            code,
            detail: format!(
                "segment parked QUEUED out-of-order (schedule={schedule_id}, \
                 segment={segment_no}, period={period_id}): a lower-period \
                 predecessor is not yet DONE"
            ),
            affected: Vec::new(),
        };
        self.publisher.emit_invariant_alarm(ctx, alarm).await;
    }
}
/// Prove the historical journal's complete release amount without rebuilding its legs.
fn validate_original_release(
    original: &crate::domain::model::EntryRecord,
    schedule: &ScheduleState,
    segment: &SegmentState,
) -> Result<(), DomainError> {
    use crate::domain::exact_money::{ExactAmount, map_exact_error, matching_spec};
    use bss_ledger_sdk::{AccountClass, Side};
    use std::str::FromStr;
    let mut liability = ExactAmount::from_decimal(rust_decimal::Decimal::ZERO);
    let mut revenue = ExactAmount::from_decimal(rust_decimal::Decimal::ZERO);
    for line in &original.lines {
        // Stored literals are parsed, so an unexpected class or side is an
        // invariant failure instead of silently taking the credit branch.
        let class = AccountClass::from_str(&line.account_class).map_err(|_| {
            DomainError::Internal(format!(
                "original release line has an unknown account class {:?}",
                line.account_class
            ))
        })?;
        if !matches!(
            class,
            AccountClass::ContractLiability | AccountClass::Revenue
        ) {
            continue;
        }
        let side = Side::from_str(&line.side).map_err(|_| {
            DomainError::Internal(format!(
                "original release line has an unknown side {:?}",
                line.side
            ))
        })?;
        // Both specs are persisted evidence. Disagreement is corruption,
        // not a caller-input mismatch; omit the stored values from the error.
        matching_spec(&line.money, &segment.amount).map_err(|_| {
            DomainError::Internal("stored original release metadata is inconsistent".into())
        })?;
        if line.revenue_stream.as_deref() != Some(schedule.revenue_stream.as_str()) {
            return Err(DomainError::Internal(
                "original release stream differs from segment".into(),
            ));
        }
        let value = ExactAmount::from_decimal(line.money.amount());
        // A release debits the liability and credits revenue.
        let (total, increasing) = match class {
            AccountClass::ContractLiability => (&mut liability, Side::Debit),
            _ => (&mut revenue, Side::Credit),
        };
        *total = if side == increasing {
            total.checked_add(&value)
        } else {
            total.checked_sub(&value)
        }
        .map_err(map_exact_error)?;
    }
    let expected = ExactAmount::from_decimal(segment.amount.amount());
    if liability != expected || revenue != expected {
        return Err(DomainError::Internal(
            "original release money differs from segment".into(),
        ));
    }
    Ok(())
}

/// Stable release evidence excludes versions, run IDs and E2's mutable open period.
fn release_hash(schedule: &ScheduleState, segment: &SegmentState) -> String {
    let mut bytes = Vec::new();
    put_str(&mut bytes, "ledger.recognition-release.v1");
    put_str(&mut bytes, &schedule.tenant_id.to_string());
    put_str(&mut bytes, &schedule.schedule_id);
    put_str(&mut bytes, &segment.segment_no.to_string());
    put_str(&mut bytes, &segment.period_id);
    put_money(&mut bytes, &segment.amount);
    put_str(&mut bytes, &schedule.revenue_stream);
    digest32_hex(&bytes)
}
/// Immutable original claim and original journal identity bind the reversal request.
fn reversal_hash(original_hash: &str, original: Uuid, period: &str) -> String {
    let mut bytes = Vec::new();
    put_str(&mut bytes, "ledger.recognition-reversal.v1");
    put_str(&mut bytes, original_hash);
    put_str(&mut bytes, &original.to_string());
    put_str(&mut bytes, period);
    digest32_hex(&bytes)
}
/// Recognition header for the actual posting period.
fn posting_header(
    ctx: &SecurityContext,
    tenant: Uuid,
    period: &str,
    currency: &str,
    business: String,
) -> NewEntry {
    NewEntry {
        entry_id: Uuid::now_v7(),
        tenant_id: tenant,
        legal_entity_id: tenant,
        period_id: period.into(),
        entry_currency: currency.into(),
        source_doc_type: SourceDocType::Recognition,
        source_business_id: business,
        reverses_entry_id: None,
        reverses_period_id: None,
        posted_at_utc: OffsetDateTime::now_utc(),
        effective_at: first_day_of_period(period),
        origin: ORIGIN_SYSTEM.into(),
        posted_by_actor_id: ctx.subject_id(),
        correlation_id: Uuid::now_v7(),
        rounding_evidence: serde_json::Value::Null,
        rate_snapshot_ref: None,
    }
}
/// The `RECOGNITION` idempotency business id for one released segment:
/// `"{schedule_id}:{segment_no}"` (design §4.1 / §7). Set as the entry's
/// `source_business_id`; with `source_doc_type = RECOGNITION` it keys the Slice 1
/// `IdempotencyGate` at-most-once per `(tenant, RECOGNITION, schedule_id:segment_no)`.
#[must_use]
fn recognition_business_id(schedule_id: &str, segment_no: i32) -> String {
    format!("{schedule_id}:{segment_no}")
}

/// The `RECOGNITION` idempotency business id for one segment **reversal**:
/// `"{schedule_id}:{segment_no}:reversal"` (design §4.3). Distinct from the
/// forward-release key (`schedule_id:segment_no`), so a reversal is its own
/// at-most-once unit and can never collide with the original `DONE` release.
#[must_use]
fn reversal_business_id(schedule_id: &str, segment_no: i32) -> String {
    format!("{schedule_id}:{segment_no}:reversal")
}

/// Build the balanced `DR CONTRACT_LIABILITY / CR REVENUE` [`PostEntry`] for one
/// segment release. Both legs carry the segment's `revenue_stream` (per-stream
/// disaggregation, §4.5) and the schedule's `currency`; the amounts are equal
/// (`amount`), so `Σ DR == Σ CR` exactly. Account ids are nil placeholders
/// (bound from the chart by the caller). The `effective_at` is the first day of
/// the segment's `period_id` month (Group D natural-period convention; the
/// OPEN-period gate + E-2 reassignment are the foundation's / Group E's).
fn build_recognition_entry(
    ctx: &SecurityContext,
    tenant: Uuid,
    segment: &ReleasableSegment,
) -> PostEntry {
    let effective_at = first_day_of_period(&segment.period_id);
    let dr = recognition_line(segment, AccountClass::ContractLiability, Side::Debit);
    let cr = recognition_line(segment, AccountClass::Revenue, Side::Credit);
    PostEntry {
        entry_id: Uuid::now_v7(),
        tenant_id: tenant,
        period_id: segment.period_id.clone(),
        entry_currency: segment.amount.currency().code().to_owned(),
        source_doc_type: SourceDocType::Recognition,
        source_business_id: recognition_business_id(&segment.schedule_id, segment.segment_no),
        effective_at,
        posted_by_actor_id: ctx.subject_id(),
        correlation_id: Uuid::now_v7(),
        reverses_entry_id: None,
        reverses_period_id: None,
        lines: vec![dr, cr],
    }
}

/// Build one recognition [`PostLine`] for `class`/`side` from the segment: the
/// stream-tagged per-stream class (`CONTRACT_LIABILITY` / `REVENUE`), the
/// schedule's currency, the segment amount. The `account_id` is a nil placeholder
/// (bound from the chart by the caller). Recognition lines carry no
/// payer/invoice/tax dims — they move deferred revenue to earned revenue within
/// the seller's own ledger — so `payer_tenant_id` is the nil placeholder
/// ([`segment_payer_placeholder`]; both legs share it, so the entry is trivially
/// single-payer) and the optional dims are `None`.
fn recognition_line(segment: &ReleasableSegment, class: AccountClass, side: Side) -> PostLine {
    PostLine {
        line_id: Uuid::now_v7(),
        payer_tenant_id: segment_payer_placeholder(),
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: Uuid::nil(),
        account_class: class,
        gl_code: None,
        side,
        money: segment.amount.clone(),
        invoice_id: None,
        due_date: None,
        revenue_stream: Some(segment.revenue_stream.clone()),
        mapping_status: MappingStatus::Resolved,
        functional_money: None,
        tax_jurisdiction: None,
        tax_filing_period: None,
        tax_rate_ref: None,
        invoice_item_ref: None,
        sku_or_plan_ref: None,
        price_id: None,
        pricing_snapshot_ref: None,
        po_allocation_group: None,
        credit_grant_event_type: None,
        ar_status: None,
    }
}

/// The payer-tenant placeholder for a recognition line. A recognition entry moves
/// the seller's own deferred revenue to earned revenue (no buyer is party to it),
/// so there is no real payer; the foundation's single-payer-tenant entry
/// invariant still wants a value, so the nil UUID stands in (both legs share it,
/// so the entry is trivially single-payer). The payer-on-the-schedule
/// (`payer_tenant_id`) is the audit fact, recorded on the schedule, not re-stamped
/// on the recognition lines.
#[must_use]
fn segment_payer_placeholder() -> Uuid {
    Uuid::nil()
}

/// First day of a `YYYYMM` `period_id` as the entry's `effective_at`. A
/// malformed period (not a parseable `YYYYMM`) falls back to [`NaiveDate::MIN`],
/// which the foundation's OPEN-period gate rejects — a malformed segment never
/// silently posts to a wrong date. (Group D's natural-period convention; E-2
/// missed-close reassignment is Group E.)
#[must_use]
fn first_day_of_period(period_id: &str) -> NaiveDate {
    parse_period(period_id)
        .and_then(|(y, m)| NaiveDate::from_ymd_opt(y, m, 1))
        // Defensive only — the segment row's period is validated at
        // schedule-build (`period_id_plus`), so this never fires in practice;
        // `MIN` is a const (no panic) the OPEN-period gate rejects.
        .unwrap_or(NaiveDate::MIN)
}

/// Parse a `YYYYMM` period id into `(year, month)`; `None` when it is not a
/// 6-char string with a `1..=12` month (mirrors the validation in
/// [`crate::domain::period`]).
fn parse_period(period_id: &str) -> Option<(i32, u32)> {
    if period_id.len() != 6 {
        return None;
    }
    let year: i32 = period_id.get(0..4)?.parse().ok()?;
    let month: u32 = period_id.get(4..6)?.parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    Some((year, month))
}

/// Thin `PostLine` adapter over [`ChartIndex::resolve`]: per-stream classes
/// (`CONTRACT_LIABILITY` / `REVENUE`) key on the line's stream. Mirrors the
/// invoice-post / settlement `resolve_line`.
fn resolve_line(chart: &ChartIndex, line: &PostLine) -> Option<Uuid> {
    chart.resolve(
        line.account_class,
        line.money.currency().code(),
        line.revenue_stream.as_deref(),
    )
}

/// Map one SDK [`PostLine`] with its stored money spec to the engine's [`NewLine`]
/// (mirrors `invoice_post::new_line` / `settle::new_line`).
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

#[cfg(test)]
#[path = "runner_tests.rs"]
mod runner_tests;

#[cfg(test)]
#[path = "runner_decimal_tests.rs"]
pub(crate) mod decimal_tests;

#[cfg(test)]
#[path = "runner_reversal_tests.rs"]
mod reversal_tests;
