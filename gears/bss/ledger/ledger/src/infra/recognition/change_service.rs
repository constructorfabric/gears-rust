//! Prospective schedule cancel/replace in one complete serializable retry attempt.
//! No journal is posted. The publisher is parked and does not enqueue an outbox row.
use super::repo_errors::map_recognition_repo_err;
use crate::domain::canonical::{digest32_hex, put, put_money, put_none, put_str, put_uuid};
use crate::domain::error::DomainError;
use crate::domain::exact_money::{ExactAmount, map_exact_error, matching_spec};
use crate::domain::recognition::change::{ChangeAction, gate_treatment};
use crate::domain::status::{
    SCHEDULE_STATUS_ACTIVE, SCHEDULE_STATUS_CANCELLED, SCHEDULE_STATUS_REPLACED,
};
use crate::infra::events::{payloads::LedgerScheduleChanged, publisher::LedgerEventPublisher};
use crate::infra::posting::{
    idempotency::{ClaimOutcome, FLOW_SCHEDULE_CHANGE, IdempotencyGate},
    retry::{AttemptError, RetryOp, retry_transaction_for},
};
use crate::infra::storage::repo::recognition_repo::{
    NewSegment, RecognitionRepo, ReplacementSchedule, ScheduleState,
};
use bss_ledger_sdk::{ChangeRecognitionSchedule, ChangeSegment, ScheduleChangeRef};
use std::sync::Arc;
use toolkit_db::secure::{AccessScope, DbTx};
use toolkit_db::{DBProvider, DbError};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Applies schedule lifecycle changes without changing contract liability.
pub struct RecognitionChangeService {
    db: DBProvider<DbError>,
    repo: RecognitionRepo,
    idempotency: IdempotencyGate,
    publisher: Arc<LedgerEventPublisher>,
}
impl RecognitionChangeService {
    /// Bind the repository and publisher to the operation database.
    #[must_use]
    pub fn new(db: DBProvider<DbError>, publisher: Arc<LedgerEventPublisher>) -> Self {
        let repo = RecognitionRepo::new(db.clone());
        Self {
            db,
            repo,
            idempotency: IdempotencyGate::new(),
            publisher,
        }
    }
    /// Gate treatment before any transaction, then retry the entire operation.
    /// # Errors
    /// Returns treatment review, invalid shape/sum/period, money metadata errors,
    /// corruption as Internal, or exhausted contention as ConcurrentModification.
    pub async fn change(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        cmd: ChangeRecognitionSchedule,
    ) -> Result<ScheduleChangeRef, DomainError> {
        gate_treatment(&cmd.treatment)?;
        toolkit_db::secure::validate_tenant_in_scope(cmd.tenant_id, scope).map_err(|error| {
            tracing::warn!(
                target: "bss-ledger",
                subject_id = %ctx.subject_id(),
                subject_tenant_id = %ctx.subject_tenant_id(),
                tenant_id = %cmd.tenant_id,
                schedule_id = %cmd.schedule_id,
                error = %error,
                "bss-ledger: recognition schedule change target outside the caller's scope"
            );
            DomainError::CrossTenantAccessDenied(
                "schedule change tenant is outside caller scope".into(),
            )
        })?;
        let action = ChangeAction::parse(&cmd.action)?;
        if action == ChangeAction::Replace {
            let segments = cmd.new_segments.as_deref().unwrap_or(&[]);
            if segments.is_empty() {
                return Err(DomainError::InvalidRequest(
                    "schedule replace requires at least one replacement segment".into(),
                ));
            }
            check_segment_count(segments.len())?;
            for (i, seg) in segments.iter().enumerate() {
                if seg.money.amount().is_sign_negative() && !seg.money.amount().is_zero() {
                    return Err(DomainError::InvalidRequest(format!(
                        "replacement segment {i} amount must be >= 0"
                    )));
                }
            }
        }
        let ctx = ctx.clone();
        let scope = scope.clone();
        let repo = self.repo.clone();
        let idempotency = self.idempotency.clone();
        let publisher = self.publisher.clone();
        let op = RetryOp::new("recognition schedule change", Some(cmd.tenant_id));
        retry_transaction_for(&self.db.db(), op, move |txn| {
            let cmd = cmd.clone();
            let ctx = ctx.clone();
            let scope = scope.clone();
            let repo = repo.clone();
            let idempotency = idempotency.clone();
            let publisher = publisher.clone();
            Box::pin(async move {
                change_in_txn(
                    txn,
                    &repo,
                    &idempotency,
                    &publisher,
                    &ctx,
                    &scope,
                    &cmd,
                    action,
                )
                .await
            })
        })
        .await
    }
}

/// Hash every request field with ordered, NULL-safe canonical money framing.
fn request_hash(cmd: &ChangeRecognitionSchedule) -> String {
    let mut bytes = Vec::new();
    put_str(&mut bytes, "ledger.schedule-change.v1");
    put_uuid(&mut bytes, cmd.tenant_id);
    for text in [
        &cmd.schedule_id,
        &cmd.change_id,
        &cmd.action,
        &cmd.treatment,
    ] {
        put_str(&mut bytes, text);
    }
    match &cmd.new_segments {
        None => put_none(&mut bytes),
        Some(segments) => {
            put(
                &mut bytes,
                &u64::try_from(segments.len())
                    .unwrap_or(u64::MAX)
                    .to_be_bytes(),
            );
            for segment in segments {
                put_str(&mut bytes, &segment.period_id);
                put_money(&mut bytes, &segment.money);
            }
        }
    }
    digest32_hex(&bytes)
}

/// Keep the existing i32 segment-number bound; manual replacements have no configured builder ceiling.
fn check_segment_count(count: usize) -> Result<(), DomainError> {
    i32::try_from(count)
        .map(|_| ())
        .map_err(|_| DomainError::ScheduleTooLong("replacement segment number overflow".into()))
}

/// Claim, read fresh state, calculate exactly, mutate, and call the parked publisher.
async fn change_in_txn(
    txn: &DbTx<'_>,
    repo: &RecognitionRepo,
    idempotency: &IdempotencyGate,
    publisher: &Arc<LedgerEventPublisher>,
    ctx: &SecurityContext,
    scope: &AccessScope,
    cmd: &ChangeRecognitionSchedule,
    action: ChangeAction,
) -> Result<ScheduleChangeRef, AttemptError> {
    // Authorize the original schedule identity before revealing claim/hash evidence.
    // Terminal originals remain valid authorization targets for immutable replay.
    let old = repo
        .read_schedule_in_txn(txn, scope, cmd.tenant_id, &cmd.schedule_id)
        .await
        .map_err(map_recognition_repo_err)?
        .ok_or_else(|| {
            DomainError::InvalidRequest(
                "recognition schedule is unavailable in caller scope".into(),
            )
        })?;
    let payload_hash = request_hash(cmd);
    match idempotency
        .claim(
            txn,
            cmd.tenant_id,
            FLOW_SCHEDULE_CHANGE,
            &cmd.change_id,
            &payload_hash,
        )
        .await?
    {
        ClaimOutcome::Claimed => {}
        ClaimOutcome::Replay(row) => {
            if row.payload_hash != payload_hash {
                return Err(DomainError::IdempotencyConflict(
                    "schedule change request differs from the claimed request".into(),
                )
                .into());
            }
            return replay_result(txn, idempotency, cmd, action).await;
        }
    }
    if old.status != SCHEDULE_STATUS_ACTIVE {
        return Err(DomainError::InvalidRequest(format!(
            "no ACTIVE recognition schedule {} for tenant {} to change",
            cmd.schedule_id, cmd.tenant_id
        ))
        .into());
    }
    let result = match action {
        ChangeAction::Cancel => {
            transition(txn, repo, scope, &old, SCHEDULE_STATUS_CANCELLED).await?;
            ScheduleChangeRef {
                schedule_id: old.schedule_id.clone(),
                new_schedule_id: None,
                status: SCHEDULE_STATUS_CANCELLED.into(),
            }
        }
        ChangeAction::Replace => apply_replace(txn, repo, scope, &old, cmd).await?,
    };
    idempotency
        .store_schedule_change_result(
            txn,
            cmd.tenant_id,
            &cmd.change_id,
            &payload_hash,
            result.new_schedule_id.as_deref(),
        )
        .await?;
    publisher
        .publish_schedule_changed(
            ctx,
            txn,
            LedgerScheduleChanged {
                tenant_id: cmd.tenant_id,
                schedule_id: cmd.schedule_id.clone(),
                new_schedule_id: result.new_schedule_id.clone(),
                treatment: cmd.treatment.clone(),
                status: result.status.clone(),
            },
        )
        .await
        .map_err(|e| DomainError::Internal(format!("publish schedule_changed: {e}")))?;
    Ok(result)
}

/// A lost ACTIVE transition must restart the complete operation.
async fn transition(
    txn: &DbTx<'_>,
    repo: &RecognitionRepo,
    scope: &AccessScope,
    old: &ScheduleState,
    status: &str,
) -> Result<(), AttemptError> {
    let changed = repo
        .mark_schedule_status(
            txn,
            scope,
            old.tenant_id,
            &old.schedule_id,
            SCHEDULE_STATUS_ACTIVE,
            status,
        )
        .await
        .map_err(map_recognition_repo_err)?;
    if changed != 1 {
        return Err(AttemptError::Conflict);
    }
    Ok(())
}

/// Replace only the unrecognized exact remainder and retain all business dimensions.
async fn apply_replace(
    txn: &DbTx<'_>,
    repo: &RecognitionRepo,
    scope: &AccessScope,
    old: &ScheduleState,
    cmd: &ChangeRecognitionSchedule,
) -> Result<ScheduleChangeRef, AttemptError> {
    let segments = cmd.new_segments.as_deref().unwrap_or(&[]);
    check_segment_count(segments.len())?;
    // Validate every spec, including zero, before doing any arithmetic.
    for seg in segments {
        matching_spec(&old.total_deferred, &seg.money)?;
    }
    let remaining = ExactAmount::from_decimal(old.total_deferred.amount())
        .checked_sub(&ExactAmount::from_decimal(old.recognized.amount()))
        .map_err(map_exact_error)?;
    let supplied = sum_segments(segments)?;
    if supplied != remaining {
        return Err(DomainError::InvalidRequest(
            "replacement segments do not sum to the schedule's remaining deferred".into(),
        )
        .into());
    }
    let total_deferred = remaining
        .into_posted_exact(old.total_deferred.currency().clone())
        .map_err(map_exact_error)?;
    let floor = repo
        .max_done_business_period_in_txn(txn, scope, old)
        .await
        .map_err(map_recognition_repo_err)?;
    validate_replacement_periods(segments, floor.as_deref())?;
    let version = old
        .version
        .checked_add(1)
        .ok_or_else(|| DomainError::Internal("schedule version overflow".into()))?;
    transition(txn, repo, scope, old, SCHEDULE_STATUS_REPLACED).await?;
    let new_schedule_id = Uuid::now_v7().to_string();
    let replacement = ReplacementSchedule {
        tenant_id: old.tenant_id,
        schedule_id: new_schedule_id.clone(),
        payer_tenant_id: old.payer_tenant_id,
        source_invoice_id: old.source_invoice_id.clone(),
        source_invoice_item_ref: old.source_invoice_item_ref.clone(),
        po_allocation_group: old.po_allocation_group.clone(),
        subscription_ref: old.subscription_ref.clone(),
        revenue_stream: old.revenue_stream.clone(),
        total_deferred,
        policy_ref: old.policy_ref.clone(),
        ssp_snapshot_ref: old.ssp_snapshot_ref.clone(),
        vc_estimate_ref: old.vc_estimate_ref.clone(),
        vc_method_ref: old.vc_method_ref.clone(),
        version,
    };
    repo.insert_replacement_schedule(txn, scope, &replacement)
        .await
        .map_err(map_recognition_repo_err)?;
    let new_segments = segments
        .iter()
        .enumerate()
        .map(|(i, seg)| {
            Ok(NewSegment {
                tenant_id: old.tenant_id,
                schedule_id: new_schedule_id.clone(),
                segment_no: i32::try_from(i + 1)
                    .map_err(|_| DomainError::ScheduleTooLong("segment number overflow".into()))?,
                period_id: seg.period_id.clone(),
                amount: seg.money.clone(),
            })
        })
        .collect::<Result<Vec<_>, DomainError>>()?;
    repo.insert_segments(txn, scope, &new_segments)
        .await
        .map_err(map_recognition_repo_err)?;
    Ok(ScheduleChangeRef {
        schedule_id: old.schedule_id.clone(),
        new_schedule_id: Some(new_schedule_id),
        status: SCHEDULE_STATUS_REPLACED.into(),
    })
}

/// Replay immutable result evidence independently of successor money, version, or status.
async fn replay_result(
    txn: &DbTx<'_>,
    idempotency: &IdempotencyGate,
    cmd: &ChangeRecognitionSchedule,
    action: ChangeAction,
) -> Result<ScheduleChangeRef, AttemptError> {
    let successor = idempotency
        .read_schedule_change_result(txn, cmd.tenant_id, &cmd.change_id)
        .await?;
    // Exhaustive over (action, successor present), so a new action must choose
    // its replay rule instead of falling into the inconsistent-evidence arm.
    let status = match (action, successor.is_some()) {
        (ChangeAction::Cancel, false) => SCHEDULE_STATUS_CANCELLED,
        (ChangeAction::Replace, true) => SCHEDULE_STATUS_REPLACED,
        (ChangeAction::Cancel, true) | (ChangeAction::Replace, false) => {
            return Err(DomainError::Internal(
                "schedule change replay is missing or has inconsistent durable successor evidence"
                    .into(),
            )
            .into());
        }
    };
    Ok(ScheduleChangeRef {
        schedule_id: cmd.schedule_id.clone(),
        new_schedule_id: successor,
        status: status.into(),
    })
}

/// Keep intermediate sums exact until comparison with the exact remaining liability.
fn sum_segments(segments: &[ChangeSegment]) -> Result<ExactAmount, DomainError> {
    segments.iter().try_fold(
        ExactAmount::from_decimal(rust_decimal::Decimal::ZERO),
        |total, seg| {
            total
                .checked_add(&ExactAmount::from_decimal(seg.money.amount()))
                .map_err(map_exact_error)
        },
    )
}

/// Validate the replacement segments' `period_id`s (design §4.6), independent of
/// their amounts (summed by [`sum_segments`]): (1) each parses as a well-formed
/// `YYYYMM`; (2) the supplied periods are strictly ascending + distinct (a
/// schedule lays its segments out in increasing period order, 1:1 with
/// `segment_no`); (3) the FIRST replacement period is strictly greater than
/// `max_done_period` — the highest period the OLD schedule has already recognized
/// (its max-`DONE`-segment period, `None` ⇒ nothing recognized yet, no floor) —
/// so a replacement can never re-target an already-recognized period
/// (cross-version double-recognition). `period_id` is the `YYYYMM`
/// lexical-sortable string, so a `<=` string compare is the period-order compare
/// once each value is confirmed well-formed. Pure (no I/O); the caller reads
/// `max_done_period` in-txn.
///
/// # Errors
/// [`DomainError::InvalidRequest`] (400) for a malformed period, a
/// non-ascending / duplicate period, or a first period that does not clear the
/// already-recognized floor.
fn validate_replacement_periods(
    segments: &[ChangeSegment],
    max_done_period: Option<&str>,
) -> Result<(), DomainError> {
    let mut prev: Option<&str> = None;
    for (i, seg) in segments.iter().enumerate() {
        let pid = seg.period_id.as_str();
        if !is_well_formed_period(pid) {
            return Err(DomainError::InvalidRequest(format!(
                "replacement segment {i} period {pid:?} is not a valid YYYYMM"
            )));
        }
        // Strictly ascending + distinct (a `<=` against the predecessor catches
        // both a descending and a duplicate period).
        if let Some(prev) = prev
            && pid <= prev
        {
            return Err(DomainError::InvalidRequest(format!(
                "replacement segment periods must be strictly ascending and distinct, but \
                 {pid:?} does not follow {prev:?}"
            )));
        }
        prev = Some(pid);
    }
    // The first replacement period must clear the already-recognized floor.
    if let (Some(first), Some(floor)) = (segments.first(), max_done_period)
        && first.period_id.as_str() <= floor
    {
        return Err(DomainError::InvalidRequest(format!(
            "first replacement period {:?} must be strictly after the schedule's last \
             already-recognized period {floor:?} (a replacement cannot re-recognize a period \
             that already posted)",
            first.period_id
        )));
    }
    Ok(())
}

/// `true` iff `period_id` is a well-formed `YYYYMM` (6 chars, month `1..=12`) —
/// the same shape [`crate::domain::period`] / the runner's `parse_period`
/// validate. A well-formed 6-char period is lexically sortable, which the period
/// ordering checks above rely on.
fn is_well_formed_period(period_id: &str) -> bool {
    if period_id.len() != 6 || !period_id.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Some(month) = period_id.get(4..6).and_then(|m| m.parse::<u32>().ok()) else {
        return false;
    };
    period_id
        .get(0..4)
        .is_some_and(|y| y.parse::<i32>().is_ok())
        && (1..=12).contains(&month)
}

#[cfg(test)]
#[path = "change_service_tests.rs"]
mod tests;
