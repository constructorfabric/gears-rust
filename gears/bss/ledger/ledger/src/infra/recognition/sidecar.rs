//! [`ScheduleBuilderSidecar`] — the in-transaction [`PostSidecar`] that
//! materializes ASC 606 recognition schedules in the SAME serializable
//! transaction as the invoice post's `CR CONTRACT_LIABILITY` credit (design
//! §4.2 / Group C2/C3). Mirrors the payment
//! [sidecars](crate::infra::payment::sidecar): its writes commit atomically with
//! the journal entry or roll back with it (a derivation that produced a schedule
//! whose insert fails — or a duplicate-build collision — rolls the whole post
//! back, so a deferred Contract-liability balance never exists without its
//! schedule, and a schedule never exists without the balance).
//!
//! For each [`BuiltSchedule`] the pure derivation produced (one per deferred
//! item-stream), [`run`](ScheduleBuilderSidecar::run):
//!
//! 1. **Claims `SCHEDULE_BUILD` idempotency** keyed
//!    `business_id = source_invoice_id:source_invoice_item_ref:revenue_stream`.
//!    `SCHEDULE_BUILD` posts NO journal entry of its own (the invoice post is the
//!    entry); the claim is purely the at-most-once build guard. On a **replay**
//!    (same key and canonical plan) it **skips** materialization, even if the
//!    original schedule has since completed. Changed plans conflict. It does NOT mint a second
//!    `schedule_id`. The claim is never `finalize`d (there is no result entry to
//!    stamp); it stays `CLAIMED` as a permanent build marker.
//! 2. On a **fresh claim**, mints a fresh `schedule_id` (`UUIDv7` string), projects
//!    the [`BuiltSchedule`] into the [`NewSchedule`] + [`NewSegment`] insert
//!    shapes (supplying the posting-context identity it holds — `tenant_id`,
//!    `payer_tenant_id`, `source_invoice_id`, and the schedule's
//!    `source_invoice_item_ref`), and inserts both via [`RecognitionRepo`].
//!
//! A deferred item MUST carry an `invoice_item_ref` (`source_invoice_item_ref`
//! is `NOT NULL` and must resolve to the Contract-liability line this very post
//! created, §4.7) — the orchestrator blocks a deferred item that lacks one
//! BEFORE the post, so every [`PlannedScheduleMaterialization`] here already
//! carries a non-empty ref.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::repo_errors::map_recognition_repo_err;
use crate::domain::canonical::{digest32_hex, put_i32, put_money, put_opt_str, put_str, put_uuid};
use bss_ledger_sdk::{PostedMoney, SourceDocType};
use toolkit_db::secure::{AccessScope, DbTx};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::model::RepoError;
use crate::domain::recognition::builder::BuiltSchedule;
use crate::domain::status::{
    SCHEDULE_STATUS_ACTIVE, SEGMENT_STATUS_DONE, SEGMENT_STATUS_PENDING, SEGMENT_STATUS_QUEUED,
};
use crate::infra::events::payloads::{LedgerRevenueRecognitionReversed, LedgerRevenueRecognized};
use crate::infra::events::publisher::LedgerEventPublisher;
use crate::infra::posting::idempotency::{ClaimOutcome, IdempotencyGate};
use crate::infra::posting::service::{PostSidecar, PostedFacts};
use crate::infra::storage::repo::recognition_repo::{
    NewSchedule, NewSegment, RecognitionRepo, ScheduleState, SegmentState,
};
use time::OffsetDateTime;

/// One schedule to materialize: the pure [`BuiltSchedule`] plan plus the
/// `source_invoice_item_ref` it draws down (the Contract-liability line this post
/// created, §4.7). The orchestrator pairs each derived schedule with its item's
/// ref (asserted non-empty before the post) and the sidecar projects the pair
/// into the storage rows.
#[derive(Clone, Debug)]
pub struct PlannedScheduleMaterialization {
    /// The derived schedule plan (deferred amount + segments + stamped refs).
    pub schedule: BuiltSchedule,
    /// The deferred item's `invoice_item_ref` — the `recognition_schedule`
    /// `source_invoice_item_ref` (NOT NULL); non-empty by orchestrator invariant.
    pub source_invoice_item_ref: String,
}

/// In-transaction sidecar that materializes the derived recognition schedules.
/// Holds the plans + the posting-context identity common to all of them (the
/// per-schedule identity lives on each [`PlannedScheduleMaterialization`]).
pub struct ScheduleBuilderSidecar {
    /// The seller tenant whose ledger this posts into (`= entry.tenant_id`).
    pub tenant_id: Uuid,
    /// The tenant that pays the invoice (the schedule's `payer_tenant_id`).
    pub payer_tenant_id: Uuid,
    /// The external invoice id (the schedule's `source_invoice_id` + the first
    /// segment of the `SCHEDULE_BUILD` dedup business id).
    pub source_invoice_id: String,
    /// The schedules to materialize (one per deferred item-stream).
    pub schedules: Vec<PlannedScheduleMaterialization>,
    /// The at-most-once build gate (claims `SCHEDULE_BUILD`).
    pub idempotency: IdempotencyGate,
    /// Backend-aware repository on the caller transaction.
    pub recognition_repo: Arc<RecognitionRepo>,
    /// Existing configured builder ceiling for each supplied plan, not accumulated extension rows.
    pub max_segments_per_schedule: usize,
    /// Discriminates a later EXTEND build (a debit note adding deferred to a live
    /// schedule) from the FIRST build (invoice-post): `None` for the invoice-post
    /// (mints the schedule), `Some(note_id)` for a debit note — so its
    /// `SCHEDULE_BUILD` claim does not collide with (and replay → skip) the base
    /// build, and it EXTENDS the live schedule instead of minting a second one the
    /// partial UNIQUE would reject.
    pub build_discriminator: Option<String>,
}

impl ScheduleBuilderSidecar {
    /// The `idempotency_dedup` business id for one schedule build:
    /// `source_invoice_id:source_invoice_item_ref:revenue_stream` (design §3.2),
    /// suffixed with the `build_discriminator` (a debit note's id) when set so an
    /// EXTEND build does not collide with (and replay → skip) the base build. One
    /// schedule per stream, so the stream tail keeps a multi-stream invoice's
    /// builds distinct.
    fn build_business_id(&self, item_ref: &str, revenue_stream: &str) -> String {
        match &self.build_discriminator {
            Some(d) => format!("{}:{item_ref}:{revenue_stream}:{d}", self.source_invoice_id),
            None => format!("{}:{item_ref}:{revenue_stream}", self.source_invoice_id),
        }
    }

    /// Closed typed framing, independent of JSON and amount spelling.
    fn plan_hash(&self, plan: &PlannedScheduleMaterialization) -> String {
        let mut bytes = Vec::new();
        put_str(&mut bytes, "ledger.schedule-build.v1");
        put_uuid(&mut bytes, self.tenant_id);
        put_uuid(&mut bytes, self.payer_tenant_id);
        put_str(&mut bytes, &self.source_invoice_id);
        put_opt_str(&mut bytes, self.build_discriminator.as_deref());
        put_str(&mut bytes, &plan.source_invoice_item_ref);
        let s = &plan.schedule;
        put_money(&mut bytes, &s.deferred);
        put_str(&mut bytes, &s.revenue_stream);
        put_str(&mut bytes, &s.policy_ref);
        for field in [
            &s.ssp_snapshot_ref,
            &s.po_allocation_group,
            &s.subscription_ref,
            &s.vc_estimate_ref,
            &s.vc_method_ref,
        ] {
            put_opt_str(&mut bytes, field.as_deref());
        }
        for segment in &s.segments {
            put_i32(&mut bytes, segment.segment_no);
            put_str(&mut bytes, &segment.period_id);
            put_money(&mut bytes, &segment.amount);
        }
        digest32_hex(&bytes)
    }

    /// Reject all malformed direct plans before claiming or writing any schedule.
    /// The identity rules (non-empty refs, one schedule per item and stream) are
    /// the sidecar's; the plan layout rules are [`BuiltSchedule::validate_plan`].
    fn validate(&self) -> Result<(), DomainError> {
        let mut keys = HashSet::new();
        for plan in &self.schedules {
            let s = &plan.schedule;
            let item = plan.source_invoice_item_ref.as_str();
            if self.source_invoice_id.is_empty()
                || item.is_empty()
                || s.revenue_stream.is_empty()
                || s.policy_ref.is_empty()
            {
                return Err(DomainError::RecognitionPolicyConflict(format!(
                    "item {item:?}: invoice id, item ref, revenue stream and policy ref are required"
                )));
            }
            if !keys.insert(self.build_business_id(item, &s.revenue_stream)) {
                return Err(DomainError::RecognitionPolicyConflict(format!(
                    "item {item}: duplicate schedule for revenue stream {}",
                    s.revenue_stream
                )));
            }
            s.validate_plan(item, self.max_segments_per_schedule)?;
        }
        Ok(())
    }

    /// Project one [`BuiltSchedule`] + its `source_invoice_item_ref` into the
    /// repo insert shapes, minting the supplied `schedule_id`. Pure (no I/O); the
    /// caller runs the inserts.
    fn project(
        &self,
        schedule_id: &str,
        plan: &PlannedScheduleMaterialization,
    ) -> (NewSchedule, Vec<NewSegment>) {
        let s = &plan.schedule;
        let new_schedule = NewSchedule {
            tenant_id: self.tenant_id,
            schedule_id: schedule_id.to_owned(),
            payer_tenant_id: self.payer_tenant_id,
            source_invoice_id: self.source_invoice_id.clone(),
            source_invoice_item_ref: plan.source_invoice_item_ref.clone(),
            po_allocation_group: s.po_allocation_group.clone(),
            subscription_ref: s.subscription_ref.clone(),
            revenue_stream: s.revenue_stream.clone(),
            total_deferred: s.deferred.clone(),
            policy_ref: s.policy_ref.clone(),
            ssp_snapshot_ref: s.ssp_snapshot_ref.clone(),
            vc_estimate_ref: s.vc_estimate_ref.clone(),
            vc_method_ref: s.vc_method_ref.clone(),
        };
        let segments: Vec<NewSegment> = s
            .segments
            .iter()
            .map(|seg| NewSegment {
                tenant_id: self.tenant_id,
                schedule_id: schedule_id.to_owned(),
                segment_no: seg.segment_no,
                period_id: seg.period_id.clone(),
                amount: seg.amount.clone(),
            })
            .collect();
        (new_schedule, segments)
    }

    /// EXTEND a live ACTIVE schedule with a later note's deferred part: add to its
    /// `total_deferred` and MERGE the note's segments — fold the amount into
    /// an existing PENDING period, else append a fresh segment (continuing
    /// `segment_no` past the current max). One ACTIVE schedule per key is preserved
    /// (the partial UNIQUE), so the credit-note splitter + the recognition runner
    /// see ONE aggregate releasable balance, not a skipped second schedule.
    /// Extending a period already released / parked (non-`PENDING`) is rejected by
    /// `add_pending_segment_amount` (rolls the post back) — a debit note normally
    /// lands before the base schedule's first release.
    async fn extend(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        schedule_id: &str,
        plan: &PlannedScheduleMaterialization,
    ) -> Result<(), DomainError> {
        let s = &plan.schedule;
        self.recognition_repo
            .increase_total_deferred(txn, scope, self.tenant_id, schedule_id, &s.deferred)
            .await
            .map_err(map_recognition_repo_err)?;

        let existing = self
            .recognition_repo
            .list_segments_in_txn(txn, scope, self.tenant_id, schedule_id)
            .await
            .map_err(map_recognition_repo_err)?;
        let by_period: HashMap<&str, i32> = existing
            .iter()
            .map(|r| (r.period_id.as_str(), r.segment_no))
            .collect();
        let mut next_no = existing.iter().map(|r| r.segment_no).max().unwrap_or(0);

        for seg in &s.segments {
            if let Some(&segment_no) = by_period.get(seg.period_id.as_str()) {
                self.recognition_repo
                    .add_pending_segment_amount(
                        txn,
                        scope,
                        self.tenant_id,
                        schedule_id,
                        segment_no,
                        &seg.amount,
                    )
                    .await
                    .map_err(map_recognition_repo_err)?;
            } else {
                next_no = next_no.checked_add(1).ok_or_else(|| {
                    DomainError::ScheduleTooLong("segment number exhausted".into())
                })?;
                let appended = vec![NewSegment {
                    tenant_id: self.tenant_id,
                    schedule_id: schedule_id.to_owned(),
                    segment_no: next_no,
                    period_id: seg.period_id.clone(),
                    amount: seg.amount.clone(),
                }];
                self.recognition_repo
                    .insert_segments(txn, scope, &appended)
                    .await
                    .map_err(map_recognition_repo_err)?;
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl PostSidecar for ScheduleBuilderSidecar {
    async fn run(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        _posted: &PostedFacts,
    ) -> Result<(), DomainError> {
        self.validate()?;
        let flow = SourceDocType::ScheduleBuild.as_str();
        for plan in &self.schedules {
            let business_id = self
                .build_business_id(&plan.source_invoice_item_ref, &plan.schedule.revenue_stream);

            // The outer journal hash contains no schedule plan; bind this claim independently.
            let payload_hash = self.plan_hash(plan);
            match self
                .idempotency
                .claim(txn, self.tenant_id, flow, &business_id, &payload_hash)
                .await
                .map_err(map_recognition_repo_err)?
            {
                ClaimOutcome::Replay(row) => {
                    if row.payload_hash != payload_hash {
                        return Err(DomainError::IdempotencyConflict(
                            "schedule build payload differs".into(),
                        ));
                    }
                    continue;
                }
                ClaimOutcome::Claimed => {}
            }

            // Fresh claim: EXTEND the live schedule if one exists for this key (a
            // later deferring note — a debit note — adds its deferred part to it;
            // one ACTIVE schedule per key, the partial UNIQUE), else mint the FIRST
            // schedule (the invoice-post). A failure rolls the whole post back.
            if let Some(existing) = self
                .recognition_repo
                .read_active_schedule_in_txn(
                    txn,
                    scope,
                    self.tenant_id,
                    &self.source_invoice_id,
                    &plan.source_invoice_item_ref,
                    &plan.schedule.revenue_stream,
                )
                .await
                .map_err(map_recognition_repo_err)?
            {
                self.extend(txn, scope, &existing.schedule_id, plan).await?;
            } else {
                let schedule_id = Uuid::now_v7().to_string();
                let (new_schedule, segments) = self.project(&schedule_id, plan);
                self.recognition_repo
                    .insert_schedule(txn, scope, &new_schedule)
                    .await
                    .map_err(map_recognition_repo_err)?;
                self.recognition_repo
                    .insert_segments(txn, scope, &segments)
                    .await
                    .map_err(map_recognition_repo_err)?;
            }
        }
        Ok(())
    }
}

/// Release evidence rebuilt by the caller on the same posting attempt.
/// The caller must use this exact stored segment money for both journal legs.
pub struct RecognitionStampSidecar {
    pub tenant_id: Uuid,
    pub schedule_id: String,
    pub segment_no: i32,
    /// Actual posting period, including missed-close reassignment.
    pub period_id: String,
    pub amount: PostedMoney,
    pub revenue_stream: String,
    pub expected_schedule_version: i64,
    pub expected_segment_version: i64,
    pub run_id: Uuid,
    pub recognition_repo: Arc<RecognitionRepo>,
    /// Parked no-op publisher; call placement does not establish broker/outbox atomicity.
    pub publisher: Arc<LedgerEventPublisher>,
    pub ctx: SecurityContext,
}

#[async_trait::async_trait]
impl PostSidecar for RecognitionStampSidecar {
    async fn run(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        _: &PostedFacts,
    ) -> Result<(), DomainError> {
        let (schedule, segment) = check_observed(
            &self.recognition_repo,
            txn,
            scope,
            self.tenant_id,
            &ExpectedObservation {
                schedule_id: &self.schedule_id,
                segment_no: self.segment_no,
                amount: &self.amount,
                stream: &self.revenue_stream,
                schedule_version: self.expected_schedule_version,
                segment_version: self.expected_segment_version,
            },
            ReleaseMode::Release,
        )
        .await?;
        // Schedule before segment; all errors escape the caller's whole attempt.
        // Each write CASes on the state read once above (or just written), so
        // the release reads the schedule and the segment once each.
        let schedule = self
            .recognition_repo
            .add_recognized_to(txn, scope, &schedule, &self.amount)
            .await
            .map_err(map_recognition_repo_err)?;
        self.recognition_repo
            .stamp_observed_segment_done(
                txn,
                scope,
                &segment,
                self.run_id,
                OffsetDateTime::now_utc(),
            )
            .await
            .map_err(map_recognition_repo_err)?;
        self.recognition_repo
            .complete_observed_schedule_if_drained(txn, scope, &schedule)
            .await
            .map_err(map_recognition_repo_err)?;
        let amount_minor = crate::infra::v1_payload::v1_minor_units_for(
            &self.amount,
            "amount_minor",
            &format_args!(
                "revenue.recognized schedule {} segment {} period {} tenant {}",
                self.schedule_id, self.segment_no, self.period_id, self.tenant_id
            ),
        );
        self.publisher
            .publish_revenue_recognized(
                &self.ctx,
                txn,
                LedgerRevenueRecognized {
                    tenant_id: self.tenant_id,
                    schedule_id: self.schedule_id.clone(),
                    segment_no: self.segment_no,
                    period_id: self.period_id.clone(),
                    amount_minor,
                    revenue_stream: self.revenue_stream.clone(),
                    currency: self.amount.currency().code().to_owned(),
                },
            )
            .await
            .map_err(|e| DomainError::Internal(format!("publish revenue_recognized: {e}")))
    }
}

/// Historical reversal evidence. Caller must prove the original posted release
/// belongs to this DONE segment and restore its full stored lines through
/// post_reversal_once. `amount` is positive original release money, never current registry money.
/// Segment remains DONE and terminal schedules are not reopened/completed here.
pub struct RecognitionReversalSidecar {
    pub tenant_id: Uuid,
    pub schedule_id: String,
    pub segment_no: i32,
    pub period_id: String,
    pub amount: PostedMoney,
    pub revenue_stream: String,
    pub expected_schedule_version: i64,
    pub expected_segment_version: i64,
    pub recognition_repo: Arc<RecognitionRepo>,
    /// Parked no-op publisher, called within the attempt without delivery guarantees.
    pub publisher: Arc<LedgerEventPublisher>,
    pub ctx: SecurityContext,
}

#[async_trait::async_trait]
impl PostSidecar for RecognitionReversalSidecar {
    async fn run(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        _: &PostedFacts,
    ) -> Result<(), DomainError> {
        let (schedule, _) = check_observed(
            &self.recognition_repo,
            txn,
            scope,
            self.tenant_id,
            &ExpectedObservation {
                schedule_id: &self.schedule_id,
                segment_no: self.segment_no,
                amount: &self.amount,
                stream: &self.revenue_stream,
                schedule_version: self.expected_schedule_version,
                segment_version: self.expected_segment_version,
            },
            ReleaseMode::Reversal,
        )
        .await?;
        let delta = PostedMoney::try_new(-self.amount.amount(), self.amount.currency().clone())
            .map_err(|e| map_recognition_repo_err(RepoError::Money(e)))?;
        self.recognition_repo
            .add_recognized_to(txn, scope, &schedule, &delta)
            .await
            .map_err(map_recognition_repo_err)?;
        let amount_minor = crate::infra::v1_payload::v1_minor_units_for(
            &delta,
            "amount_minor",
            &format_args!(
                "revenue.recognition_reversed schedule {} segment {} period {} tenant {}",
                self.schedule_id, self.segment_no, self.period_id, self.tenant_id
            ),
        );
        self.publisher
            .publish_revenue_recognition_reversed(
                &self.ctx,
                txn,
                LedgerRevenueRecognitionReversed {
                    tenant_id: self.tenant_id,
                    schedule_id: self.schedule_id.clone(),
                    segment_no: self.segment_no,
                    period_id: self.period_id.clone(),
                    amount_minor,
                    revenue_stream: self.revenue_stream.clone(),
                    currency: delta.currency().code().to_owned(),
                },
            )
            .await
            .map_err(|e| {
                DomainError::Internal(format!("publish revenue_recognition_reversed: {e}"))
            })
    }
}

/// What the caller observed (and posts against): the segment, its money and
/// stream, and the two mutation tokens that must still hold. Named fields, so
/// the adjacent versions cannot be swapped silently.
struct ExpectedObservation<'a> {
    schedule_id: &'a str,
    segment_no: i32,
    amount: &'a PostedMoney,
    stream: &'a str,
    schedule_version: i64,
    segment_version: i64,
}

/// Which eligibility rule applies: a release needs an ACTIVE schedule and a
/// PENDING/QUEUED segment, a reversal needs a DONE segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReleaseMode {
    Release,
    Reversal,
}

/// Compare caller evidence with validated stored state before any counter
/// mutation. Returns the schedule and segment as read (the segment validated
/// against that schedule), so the release mutates them without reading again.
async fn check_observed(
    repo: &RecognitionRepo,
    txn: &DbTx<'_>,
    scope: &AccessScope,
    tenant: Uuid,
    expected: &ExpectedObservation<'_>,
    mode: ReleaseMode,
) -> Result<(ScheduleState, SegmentState), DomainError> {
    let ExpectedObservation {
        schedule_id,
        segment_no,
        amount,
        stream,
        schedule_version,
        segment_version,
    } = *expected;
    let schedule = repo
        .read_schedule_in_txn(txn, scope, tenant, schedule_id)
        .await
        .map_err(map_recognition_repo_err)?
        .ok_or_else(|| DomainError::RecognitionPolicyConflict("schedule unavailable".into()))?;
    let segment = repo
        .read_segment_of(txn, scope, &schedule, segment_no)
        .await
        .map_err(map_recognition_repo_err)?
        .ok_or_else(|| DomainError::RecognitionPolicyConflict("segment unavailable".into()))?;
    if schedule.version != schedule_version || segment.version != segment_version {
        return Err(DomainError::ConcurrentModification(
            "recognition observation changed".into(),
        ));
    }
    same_spec(amount, &segment.amount)?;
    if amount != &segment.amount
        || amount.amount().is_sign_negative()
        || stream != schedule.revenue_stream
    {
        return Err(DomainError::RecognitionPolicyConflict(
            "release money or stream differs from stored evidence".into(),
        ));
    }
    let eligible = match mode {
        ReleaseMode::Reversal => segment.status == SEGMENT_STATUS_DONE,
        ReleaseMode::Release => {
            schedule.status == SCHEDULE_STATUS_ACTIVE
                && (segment.status == SEGMENT_STATUS_PENDING
                    || segment.status == SEGMENT_STATUS_QUEUED)
        }
    };
    if !eligible {
        return Err(DomainError::RecognitionPolicyConflict(
            "recognition state is not eligible".into(),
        ));
    }
    Ok((schedule, segment))
}

/// Preserve currency and scale mismatch as distinct named errors.
fn same_spec(a: &PostedMoney, b: &PostedMoney) -> Result<(), DomainError> {
    a.currency()
        .ensure_same(b.currency())
        .map_err(|e| map_recognition_repo_err(RepoError::Money(e)))
}

#[cfg(test)]
#[path = "sidecar_tests.rs"]
mod tests;
