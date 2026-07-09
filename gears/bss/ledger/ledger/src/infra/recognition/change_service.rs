//! `RecognitionChangeService` — the Group H schedule change / cancel path
//! (design §3.6 / §4.6). It marks an ACTIVE recognition schedule terminal
//! (`CANCELLED` or `REPLACED`) and, on a `replace`, mints a fresh ACTIVE version
//! that re-plans the REMAINING deferred — all in ONE serializable transaction,
//! emitting `billing.ledger.schedule.changed` in-txn. Mirrors the construction of
//! [`RecognitionRunService`](super::run_service::RecognitionRunService) (a
//! `DBProvider` + the publisher), but the change posts **no journal entry** (it is
//! a pure schedule-lifecycle transition — the `CONTRACT_LIABILITY` balance is
//! already correct), so it brackets a bare `transaction_with_retry` rather than a
//! [`PostingService`](crate::infra::posting::service::PostingService) post.
//!
//! **Order of operations** (each gate documented):
//!
//! 1. **Treatment gate FIRST** ([`gate_treatment`], design §3.6) — `prospective`
//!    / `separate_contract` proceed; `catch_up` / unknown ⇒
//!    [`DomainError::ModificationTreatmentReview`] with NO state change. Run before
//!    the transaction so a review never opens a txn or mutates a schedule.
//! 2. **Action parse** ([`ChangeAction::parse`]) — `cancel` / `replace`; and, for
//!    a `replace`, the `new_segments` are validated to sum to the schedule's
//!    REMAINING deferred (`total_deferred − recognized`) before any write.
//! 3. **One serializable txn (with retry)** — claim `(tenant, SCHEDULE_CHANGE,
//!    change_id)` (a `Replay` short-circuits to the prior result, recomputed from
//!    durable state — no second schedule minted, never `finalize`d); read the ACTIVE
//!    `(tenant, schedule_id)` schedule in-txn (missing / non-ACTIVE ⇒
//!    [`DomainError::InvalidRequest`] 400 — the gear's only `NotFound` variant is
//!    `fiscal_period`-tagged, so a generic 400 is used, not a misattributed period
//!    error); apply the transition (`cancel` flips `ACTIVE → CANCELLED`; `replace`
//!    flips `ACTIVE → REPLACED` then inserts the new ACTIVE version — `version =
//!    old + 1`, same business key, remaining deferred — + its PENDING segments,
//!    old-first so the partial one-live UNIQUE holds); then emit
//!    `billing.ledger.schedule.changed` in-txn (atomic with the transition).

use std::sync::Arc;

use bss_ledger_sdk::{ChangeRecognitionSchedule, ChangeSegment, ScheduleChangeRef};
use toolkit_db::secure::{AccessScope, DbTx, TxConfig};
use toolkit_db::{DBProvider, DbError};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::recognition::change::{ChangeAction, gate_treatment};
use crate::domain::status::{
    SCHEDULE_STATUS_ACTIVE, SCHEDULE_STATUS_CANCELLED, SCHEDULE_STATUS_REPLACED,
};
use crate::infra::events::payloads::LedgerScheduleChanged;
use crate::infra::events::publisher::LedgerEventPublisher;
use crate::infra::posting::idempotency::{ClaimOutcome, IdempotencyGate};
use crate::infra::storage::entity::recognition_schedule;
use crate::infra::storage::repo::RecognitionRepo;
use crate::infra::storage::repo::recognition_repo::{NewSegment, ReplacementSchedule};

/// The idempotency-dedup `flow` literal for a schedule change. `idempotency_dedup`
/// `flow` is a free-text column (no CHECK), so a Group H change reuses the
/// [`IdempotencyGate`] with this literal + `business_id = change_id` WITHOUT a new
/// `SourceDocType` variant (the SDK enum is declared final). The claim is the
/// at-most-once change marker.
const FLOW_SCHEDULE_CHANGE: &str = "SCHEDULE_CHANGE";

/// Carries the in-transaction change decision out of the `SERIALIZABLE` body on
/// the commit path. Both arms are `Ok(_)` (the txn commits); infrastructure /
/// serialization faults are the closure's `Err(DbError)`. A `DomainError` raised
/// inside the body (e.g. a missing ACTIVE schedule) is wrapped into `DbError`
/// (via [`domain_to_db`]) so it propagates out of the retry helper and is
/// re-projected by [`db_to_domain`] — mirroring the `period_close` pattern.
enum ChangeTxnResult {
    /// The change applied (fresh claim): the resulting ref.
    Applied(ScheduleChangeRef),
    /// The change was an idempotent replay (the `change_id` claim already
    /// existed): the prior ref, recomputed from durable schedule state.
    Replayed(ScheduleChangeRef),
}

/// Applies Group H schedule changes (cancel / replace) over one serializable
/// transaction. Holds the [`IdempotencyGate`] (the `change_id` claim), the event
/// publisher (the in-txn `schedule.changed` emit), and the `DBProvider` (to open
/// the change txn). The schedule reads/writes go through [`RecognitionRepo`]'s
/// in-txn associated functions (which take the `DbTx`), so no repo instance is
/// held. Same `db`/`publisher` deps as the peer recognition / payment services.
pub struct RecognitionChangeService {
    db: DBProvider<DbError>,
    idempotency: IdempotencyGate,
    publisher: Arc<LedgerEventPublisher>,
}

impl RecognitionChangeService {
    /// Build the change-service over one database provider + the event publisher
    /// (the in-txn `schedule.changed` emit). Mirrors
    /// [`RecognitionRunService::new`](super::run_service::RecognitionRunService::new)
    /// minus the runner/lease (a change posts no journal entry).
    #[must_use]
    pub fn new(db: DBProvider<DbError>, publisher: Arc<LedgerEventPublisher>) -> Self {
        Self {
            db,
            idempotency: IdempotencyGate::new(),
            publisher,
        }
    }

    /// Change or cancel an ACTIVE recognition schedule (design §3.6 / §4.6).
    /// Gates the treatment first (a `catch_up`/unknown treatment is a review with
    /// NO state change), validates the action + (for a `replace`) the segments,
    /// then applies the transition in one serializable transaction and emits
    /// `schedule.changed` in-txn. Idempotent on `cmd.change_id`.
    ///
    /// # Errors
    /// [`DomainError::ModificationTreatmentReview`] for a `catch_up`/unknown
    /// treatment; [`DomainError::InvalidRequest`] for an unknown action, a
    /// `replace` whose `new_segments` are missing/empty or do not sum to the
    /// remaining deferred, OR when no ACTIVE schedule exists for
    /// `(tenant, schedule_id)`; [`DomainError::Internal`] on a storage fault.
    pub async fn change(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        cmd: ChangeRecognitionSchedule,
    ) -> Result<ScheduleChangeRef, DomainError> {
        // 1. Treatment gate FIRST (design §3.6) — before any txn / state read. A
        //    catch_up / unknown treatment surfaces for review and changes nothing.
        gate_treatment(&cmd.treatment)?;

        // 2. Parse the action; pre-validate a replace's segments against the
        //    schedule's remaining deferred is deferred to the in-txn body (it
        //    needs the read schedule), but the *shape* (segments present for a
        //    replace) is checked here for a clean 400.
        let action = ChangeAction::parse(&cmd.action)?;
        if action == ChangeAction::Replace {
            let segs = cmd.new_segments.as_deref().unwrap_or(&[]);
            if segs.is_empty() {
                return Err(DomainError::InvalidRequest(
                    "schedule replace requires at least one replacement segment".to_owned(),
                ));
            }
            for (i, seg) in segs.iter().enumerate() {
                if seg.amount_minor < 0 {
                    return Err(DomainError::InvalidRequest(format!(
                        "replacement segment {i} amount must be >= 0, got {}",
                        seg.amount_minor
                    )));
                }
            }
        }

        // 3. One serializable transaction (with retry): claim → read → transition
        //    → emit. The body returns a `ChangeTxnResult` on the commit path; a
        //    business precondition (missing ACTIVE schedule, bad segment sum) is
        //    wrapped into `DbError` so it leaves the retry helper and is
        //    re-projected below. A serialization conflict (a concurrent release /
        //    change) retries (SSI).
        let cmd = cmd.clone();
        let scope_owned = scope.clone();
        let ctx_owned = ctx.clone();
        let publisher = Arc::clone(&self.publisher);
        let idempotency = self.idempotency.clone();

        let result: Result<ChangeTxnResult, DbError> = self
            .db
            .db()
            .transaction_with_retry(TxConfig::serializable(), as_db_err, move |txn| {
                let cmd = cmd.clone();
                let scope = scope_owned.clone();
                let ctx = ctx_owned.clone();
                let publisher = Arc::clone(&publisher);
                let idempotency = idempotency.clone();
                Box::pin(async move {
                    change_in_txn(txn, &idempotency, &publisher, &ctx, &scope, &cmd, action).await
                })
            })
            .await;

        match result {
            Ok(ChangeTxnResult::Applied(r) | ChangeTxnResult::Replayed(r)) => Ok(r),
            Err(e) => Err(db_to_domain(&e)),
        }
    }
}

/// In-transaction change body: claim → read ACTIVE schedule → cancel/replace →
/// emit `schedule.changed`. Runs under the caller's `SERIALIZABLE` transaction so
/// the claim, the status flip, and any replacement insert share one snapshot and
/// conflict with a concurrent release. A business precondition is raised as a
/// `DomainError` wrapped into `DbError` (so it propagates out of the retry helper);
/// a serialization conflict surfaces as a retryable `DbErr`.
async fn change_in_txn(
    txn: &DbTx<'_>,
    idempotency: &IdempotencyGate,
    publisher: &Arc<LedgerEventPublisher>,
    ctx: &SecurityContext,
    scope: &AccessScope,
    cmd: &ChangeRecognitionSchedule,
    action: ChangeAction,
) -> Result<ChangeTxnResult, DbError> {
    let tenant = cmd.tenant_id;

    // a. Idempotency claim on the change_id. SCHEDULE_CHANGE posts no journal
    //    entry, so the payload hash is over the change_id (stable across retries).
    //    A `Replay` means the change already applied — recompute the prior result
    //    from durable schedule state (no second schedule minted).
    let payload_hash = IdempotencyGate::content_hash(&cmd.change_id);
    match idempotency
        .claim(
            txn,
            tenant,
            FLOW_SCHEDULE_CHANGE,
            &cmd.change_id,
            &payload_hash,
        )
        .await
        .map_err(|e| repo_to_db(&e))?
    {
        ClaimOutcome::Claimed => {}
        ClaimOutcome::Replay(_) => {
            let replayed = replay_result(txn, scope, cmd).await?;
            return Ok(ChangeTxnResult::Replayed(replayed));
        }
    }

    // b. Read the ACTIVE schedule in-txn (joins the serializable snapshot). A
    //    missing / non-ACTIVE target ⇒ InvalidRequest (400 — the change cannot be
    //    satisfied against current state; the gear's only NotFound variant is
    //    fiscal-period-tagged, so we do not misattribute it).
    let schedule = RecognitionRepo::read_schedule_in_txn(txn, scope, tenant, &cmd.schedule_id)
        .await?
        .filter(|s| s.status == SCHEDULE_STATUS_ACTIVE)
        .ok_or_else(|| {
            domain_to_db(DomainError::InvalidRequest(format!(
                "no ACTIVE recognition schedule {} for tenant {tenant} to change",
                cmd.schedule_id
            )))
        })?;

    // c. Apply the transition.
    let result = match action {
        ChangeAction::Cancel => apply_cancel(txn, scope, tenant, &cmd.schedule_id).await?,
        ChangeAction::Replace => apply_replace(txn, scope, &schedule, cmd).await?,
    };

    // d. Emit billing.ledger.schedule.changed in-txn (transactional outbox): the
    //    event row commits atomically with the transition, or rolls back with it.
    //    `treatment` is the upstream code that let the change proceed; `status` is
    //    the original schedule's resulting terminal status.
    publisher
        .publish_schedule_changed(
            ctx,
            txn,
            LedgerScheduleChanged {
                tenant_id: tenant,
                schedule_id: cmd.schedule_id.clone(),
                new_schedule_id: result.new_schedule_id.clone(),
                treatment: cmd.treatment.clone(),
                status: result.status.clone(),
            },
        )
        .await
        .map_err(|e| {
            domain_to_db(DomainError::Internal(format!(
                "publish schedule_changed: {e}"
            )))
        })?;

    Ok(ChangeTxnResult::Applied(result))
}

/// Apply a `cancel`: flip the ACTIVE schedule `→ CANCELLED` (bump `version`). The
/// unreleased deferred remainder stays as `CONTRACT_LIABILITY` (no auto-reversal,
/// v1). A `rows_affected == 0` (a concurrent change already moved it) is folded
/// into the same terminal ref — the change is idempotent on its effect.
async fn apply_cancel(
    txn: &DbTx<'_>,
    scope: &AccessScope,
    tenant: Uuid,
    schedule_id: &str,
) -> Result<ScheduleChangeRef, DbError> {
    RecognitionRepo::mark_schedule_status(
        txn,
        scope,
        tenant,
        schedule_id,
        SCHEDULE_STATUS_ACTIVE,
        SCHEDULE_STATUS_CANCELLED,
    )
    .await?;
    Ok(ScheduleChangeRef {
        schedule_id: schedule_id.to_owned(),
        new_schedule_id: None,
        status: SCHEDULE_STATUS_CANCELLED.to_owned(),
    })
}

/// Apply a `replace`: validate the new segments sum to the remaining deferred,
/// flip the old schedule `→ REPLACED`, then insert the new ACTIVE version
/// (`version = old + 1`, same business key, remaining deferred) + its PENDING
/// segments — all in this txn. Already-DONE segments on the old schedule stay; no
/// compensating journal entry (the `CONTRACT_LIABILITY` balance already equals the
/// remaining deferred).
async fn apply_replace(
    txn: &DbTx<'_>,
    scope: &AccessScope,
    old: &recognition_schedule::Model,
    cmd: &ChangeRecognitionSchedule,
) -> Result<ScheduleChangeRef, DbError> {
    let tenant = old.tenant_id;
    let segments = cmd.new_segments.as_deref().unwrap_or(&[]);

    // The remaining deferred = what was deferred minus what has already been
    // recognized. The new version re-plans exactly this remainder (prospective).
    let remaining = old
        .total_deferred_minor
        .checked_sub(old.recognized_minor)
        .ok_or_else(|| {
            domain_to_db(DomainError::Internal(format!(
                "schedule {} remaining deferred underflow (total={}, recognized={})",
                old.schedule_id, old.total_deferred_minor, old.recognized_minor
            )))
        })?;
    let supplied: i64 = sum_segments(segments).map_err(domain_to_db)?;
    if supplied != remaining {
        return Err(domain_to_db(DomainError::InvalidRequest(format!(
            "replacement segments sum to {supplied} but the schedule's remaining deferred is \
             {remaining} (total={}, recognized={})",
            old.total_deferred_minor, old.recognized_minor
        ))));
    }

    // Validate the replacement PERIODS (design §4.6) BEFORE the status flip: each
    // `period_id` must parse as `YYYYMM`, the supplied periods must be strictly
    // ascending + distinct, and the FIRST replacement period must be strictly
    // greater than the highest period the OLD schedule has already recognized
    // (its max-`DONE`-segment period) — so a replacement can never re-target an
    // already-recognized period (cross-version double-recognition). The max DONE
    // period is read IN-TXN (the change claim holds the conn-bypass guard).
    let max_done =
        RecognitionRepo::max_done_segment_period_in_txn(txn, scope, tenant, &old.schedule_id)
            .await?;
    validate_replacement_periods(segments, max_done.as_deref()).map_err(domain_to_db)?;

    // Flip the OLD schedule REPLACED FIRST (same txn) so the partial one-live
    // UNIQUE frees before the new ACTIVE inserts.
    RecognitionRepo::mark_schedule_status(
        txn,
        scope,
        tenant,
        &old.schedule_id,
        SCHEDULE_STATUS_ACTIVE,
        SCHEDULE_STATUS_REPLACED,
    )
    .await?;

    // Mint the successor id + version, carrying the SAME business-key dims.
    let new_schedule_id = Uuid::now_v7().to_string();
    let new_version = old.version.checked_add(1).ok_or_else(|| {
        domain_to_db(DomainError::Internal(format!(
            "schedule {} version overflow",
            old.schedule_id
        )))
    })?;
    let replacement = ReplacementSchedule {
        tenant_id: tenant,
        schedule_id: new_schedule_id.clone(),
        payer_tenant_id: old.payer_tenant_id,
        source_invoice_id: old.source_invoice_id.clone(),
        source_invoice_item_ref: old.source_invoice_item_ref.clone(),
        po_allocation_group: old.po_allocation_group.clone(),
        subscription_ref: old.subscription_ref.clone(),
        revenue_stream: old.revenue_stream.clone(),
        currency: old.currency.clone(),
        total_deferred_minor: remaining,
        policy_ref: old.policy_ref.clone(),
        ssp_snapshot_ref: old.ssp_snapshot_ref.clone(),
        vc_estimate_ref: old.vc_estimate_ref.clone(),
        vc_method_ref: old.vc_method_ref.clone(),
        version: new_version,
    };
    RecognitionRepo::insert_replacement_schedule(txn, scope, &replacement).await?;

    // Insert the successor's PENDING segments (segment_no 1..).
    let new_segments: Vec<NewSegment> = segments
        .iter()
        .enumerate()
        .map(|(i, seg)| {
            let segment_no = i32::try_from(i + 1).map_err(|_| {
                domain_to_db(DomainError::InvalidRequest(
                    "too many replacement segments (segment number overflow)".to_owned(),
                ))
            })?;
            Ok(NewSegment {
                tenant_id: tenant,
                schedule_id: new_schedule_id.clone(),
                segment_no,
                period_id: seg.period_id.clone(),
                amount_minor: seg.amount_minor,
            })
        })
        .collect::<Result<Vec<_>, DbError>>()?;
    RecognitionRepo::insert_segments(txn, scope, &new_segments).await?;

    Ok(ScheduleChangeRef {
        schedule_id: old.schedule_id.clone(),
        new_schedule_id: Some(new_schedule_id),
        status: SCHEDULE_STATUS_REPLACED.to_owned(),
    })
}

/// Recompute the result of an already-applied change (the idempotent replay
/// path) from durable schedule state — no second schedule is minted. Reads the
/// original schedule's current terminal status: `CANCELLED` ⇒ no successor;
/// `REPLACED` ⇒ resolve the ACTIVE successor (same business key, `version =
/// old.version + 1`). A still-ACTIVE schedule under a claimed `change_id` is an
/// invariant breach (the claim implies the change committed) — surfaced as
/// `Internal`.
async fn replay_result(
    txn: &DbTx<'_>,
    scope: &AccessScope,
    cmd: &ChangeRecognitionSchedule,
) -> Result<ScheduleChangeRef, DbError> {
    let tenant = cmd.tenant_id;
    let schedule = RecognitionRepo::read_schedule_in_txn(txn, scope, tenant, &cmd.schedule_id)
        .await?
        .ok_or_else(|| {
            domain_to_db(DomainError::Internal(format!(
                "schedule {} vanished on change replay",
                cmd.schedule_id
            )))
        })?;
    match schedule.status.as_str() {
        SCHEDULE_STATUS_CANCELLED => Ok(ScheduleChangeRef {
            schedule_id: cmd.schedule_id.clone(),
            new_schedule_id: None,
            status: SCHEDULE_STATUS_CANCELLED.to_owned(),
        }),
        SCHEDULE_STATUS_REPLACED => {
            // The successor is the ACTIVE schedule at version old+1 with the same
            // business key — read IN-TXN (the claim guard holds the conn-bypass
            // guard, so an out-of-txn read would fail).
            let successor = RecognitionRepo::read_active_successor_in_txn(
                txn,
                scope,
                tenant,
                &schedule.source_invoice_id,
                &schedule.source_invoice_item_ref,
                &schedule.revenue_stream,
                schedule.version,
            )
            .await?;
            Ok(ScheduleChangeRef {
                schedule_id: cmd.schedule_id.clone(),
                new_schedule_id: successor.map(|s| s.schedule_id),
                status: SCHEDULE_STATUS_REPLACED.to_owned(),
            })
        }
        other => Err(domain_to_db(DomainError::Internal(format!(
            "change replay found schedule {} in unexpected status {other:?}",
            cmd.schedule_id
        )))),
    }
}

/// Sum the replacement segment amounts (i128 intermediate to dodge an i64
/// overflow on a pathological set), returning the i64 total.
fn sum_segments(segments: &[ChangeSegment]) -> Result<i64, DomainError> {
    let mut total: i128 = 0;
    for seg in segments {
        total += i128::from(seg.amount_minor);
    }
    i64::try_from(total).map_err(|_| {
        DomainError::InvalidRequest("replacement segment amounts overflow i64".to_owned())
    })
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
    if period_id.len() != 6 {
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

/// Extractor for the retry helper: a wrapped `DbErr` (so a serialization failure
/// surfaced at a statement or COMMIT is recognised as retryable). Mirrors
/// `period_close::as_db_err`.
fn as_db_err(e: &DbError) -> Option<&sea_orm::DbErr> {
    match e {
        DbError::Sea(db_err) => Some(db_err),
        _ => None,
    }
}

/// Wrap a [`DomainError`] into a `DbError` so it propagates out of the retry
/// helper unchanged (NOT retryable — `DbError::Other` carries no `sea_orm::DbErr`,
/// so [`as_db_err`] returns `None`). Re-projected by [`db_to_domain`].
fn domain_to_db(e: DomainError) -> DbError {
    DbError::Other(anyhow::anyhow!(DomainErrorCarrier(e)))
}

/// Re-project the closure's `DbError` back into a [`DomainError`]: a wrapped
/// `DomainError` (raised in the body via [`domain_to_db`]) is unwrapped verbatim;
/// any other `DbError` (a genuine storage / serialization fault that exhausted
/// retries) is an [`DomainError::Internal`].
fn db_to_domain(e: &DbError) -> DomainError {
    if let DbError::Other(err) = e
        && let Some(carrier) = err.downcast_ref::<DomainErrorCarrier>()
    {
        return carrier.0.clone();
    }
    DomainError::Internal(format!("schedule change txn: {e}"))
}

/// Map the idempotency-gate [`RepoError`] (the `SCHEDULE_CHANGE` claim) into the
/// change txn's `DbError` as a non-retryable `Internal`. The schedule
/// reads/writes go through the in-txn [`RecognitionRepo`] helpers, which now
/// return `DbError` directly and PRESERVE the inner `sea_orm::DbErr` (so a
/// serialization conflict at the contended `ACTIVE` row stays retryable via
/// [`as_db_err`] — see `recognition_repo::scope_to_db`); only the claim still
/// surfaces a `RepoError`, and a claim fault is treated as an infrastructure
/// `Internal`, not a retryable conflict.
fn repo_to_db(e: &crate::domain::model::RepoError) -> DbError {
    domain_to_db(DomainError::Internal(format!("recognition repo: {e}")))
}

/// Wrapper so a [`DomainError`] can ride inside `anyhow::Error` (and thus
/// `DbError::Other`) and be downcast back out by [`db_to_domain`]. Mirrors the
/// `period_close::scope_to_db` round-trip intent without needing the inner
/// `sea_orm::DbErr` (a change conflict retries at COMMIT, not on the carried
/// business error).
#[derive(Debug)]
struct DomainErrorCarrier(DomainError);

impl std::fmt::Display for DomainErrorCarrier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DomainErrorCarrier {}
