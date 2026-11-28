//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-intake:p1
//! Staging one verified snapshot of a target (P-D-268) in ONE transaction that reads only stored
//! rows: no registry or plugin call happens inside it.
//!
//! The target's snapshot at the revision decides first: the same digest answers it and its
//! candidates (a replay, nothing written), another digest is 409 `QUOTA_SOURCE_CONFLICT`. Without
//! one, a revision below the target's highest stored is 409 `QUOTA_SOURCE_STALE`. Else the
//! snapshot, its candidates and one audit row are written, and the answer is read back from the
//! store, so a first staging and every replay of it answer the same rows. A staging that loses
//! the insert race to a concurrent one of the same revision reads the winner from a fresh
//! transaction (Postgres aborts the losing transaction) and answers it as a replay or a conflict:
//! the discovery's `stage_snapshot` runs both.
use std::cmp::Ordering;

use time::UtcOffset;
use time::format_description::well_known::Rfc3339;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::secure::{AccessScope, DBRunner};
use uuid::Uuid;

use super::On;
use crate::api::rest::TxError;
use crate::domain::error::DomainError;
use crate::domain::quota::evidence::EvidenceSet;
use crate::domain::quota::intake::{IntakePlan, review_json, revision_order};
use crate::domain::quota::snapshot::VerifiedSnapshot;
use crate::domain::quota::{
    CandidateOrigin, NewCandidate, QuotaCandidate, QuotaTarget, StoredSnapshot,
};
use crate::infra::storage::repo::quota_intake_repo as store;
use crate::infra::storage::repo::{self, AuditCommon, LifecycleMove};
use crate::infra::storage::{RepoError, stored_instant};

/// The audit action of one staged intake.
pub const ACTION_INTAKE: &str = "quota_discovery.intake";
/// The audit subject kind of a staged snapshot.
pub const SUBJECT_SNAPSHOT: &str = "quota_snapshot";
/// Another snapshot is stored at the revision.
pub const QUOTA_SOURCE_CONFLICT: &str = "QUOTA_SOURCE_CONFLICT";
/// The revision is below the target's highest stored.
pub const QUOTA_SOURCE_STALE: &str = "QUOTA_SOURCE_STALE";

/// A stored snapshot and its candidates, by capability key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedIntake {
    pub snapshot: StoredSnapshot,
    pub candidates: Vec<QuotaCandidate>,
}

/// What staging did: wrote the snapshot, or found it stored with the same digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageOutcome {
    Created(StagedIntake),
    Replayed(StagedIntake),
}

/// What a staging writes: the snapshot row and one discovery candidate per valid record, all
/// written at `now` by `actor`.
///
/// # Errors
/// 500 when the verified snapshot has no UTC observation time.
pub(crate) fn prepare(
    target: &QuotaTarget,
    snapshot: &VerifiedSnapshot,
    plan: &IntakePlan,
    actor: Uuid,
    now: time::OffsetDateTime,
) -> Result<(StoredSnapshot, Vec<NewCandidate>), CanonicalError> {
    let row = stored_snapshot(target, snapshot, plan, actor, now)?;
    let candidates = plan
        .candidates
        .iter()
        .map(|judged| NewCandidate {
            target_id: target.id,
            origin: CandidateOrigin::Discovery,
            snapshot_row_id: Some(row.id),
            record_id: judged.record.id,
            capability_key: judged.record.capability_key.clone(),
            record_json: judged.raw.clone(),
            record_digest: judged.record_digest.clone(),
            evidence_json: judged.evidence.to_json(),
            created_by: actor,
            created_at: now,
        })
        .collect();
    Ok((row, candidates))
}

/// The staging's statements, inside the caller's transaction: the stored revision decides first
/// (a replay or a conflict), then the stale rule; else the snapshot, its candidates and the audit
/// row are inserted and read back, so a first staging answers what its replay answers.
///
/// # Errors
/// 409 `QUOTA_SOURCE_CONFLICT` / `QUOTA_SOURCE_STALE` as refusals; `QuotaSnapshotTaken` when a
/// concurrent staging of the revision committed first (Postgres then aborts the transaction: the
/// discovery's staging reads the winner from a fresh transaction); storage failures.
pub(crate) async fn stage_in(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    row: &StoredSnapshot,
    candidates: &[NewCandidate],
) -> Result<StageOutcome, TxError> {
    let (revision, digest) = (row.source_revision.as_str(), row.content_digest.as_str());
    if let Some(answer) = settled(tx, scope, tenant, row.target_id, revision, digest).await? {
        return Ok(answer);
    }
    store::insert_snapshot(tx, scope, tenant, row)
        .await
        .map_err(TxError::Repo)?;
    store::insert_candidates(tx, scope, tenant, candidates)
        .await
        .map_err(TxError::Repo)?;
    audit(tx, scope, tenant, row).await?;
    let revision = row.source_revision.as_str();
    let stored = store::snapshot_at(tx, scope, tenant, row.target_id, revision)
        .await
        .map_err(TxError::Repo)?
        .ok_or_else(|| {
            TxError::Repo(RepoError::Db(format!(
                "quota snapshot {} did not read back",
                row.id
            )))
        })?;
    let candidates = store::candidates_of_snapshot(tx, scope, tenant, row.id)
        .await
        .map_err(TxError::Repo)?;
    Ok(StageOutcome::Created(StagedIntake {
        snapshot: stored,
        candidates,
    }))
}

// @cpt-begin:cpt-cf-bss-products-algo-quota-catalog-snapshot:p1:inst-quota-replay
/// The target's snapshot at `revision` answers as a replay (the same `digest`) or a conflict;
/// without one, a revision below the target's highest stored is stale. `None`: the revision is
/// new. The discovery asks it before any record is judged and the staging again inside its
/// transaction.
///
/// # Errors
/// 409 `QUOTA_SOURCE_CONFLICT` / `QUOTA_SOURCE_STALE` as refusals; storage failures.
pub(crate) async fn settled(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    target_id: Uuid,
    revision: &str,
    digest: &str,
) -> Result<Option<StageOutcome>, TxError> {
    if let Some(stored) = store::snapshot_at(runner, scope, tenant, target_id, revision)
        .await
        .map_err(TxError::Repo)?
    {
        return answer_stored(runner, scope, tenant, stored, digest)
            .await
            .map(Some);
    }
    if let Some(highest) = store::highest_revision(runner, scope, tenant, target_id)
        .await
        .map_err(TxError::Repo)?
        && revision_order(revision, &highest) == Ordering::Less
    {
        return Err(TxError::Refused(DomainError::Conflict {
            code: QUOTA_SOURCE_STALE,
            detail: format!(
                "revision {revision} is below the target's highest stored revision {highest}"
            ),
        }));
    }
    Ok(None)
}
// @cpt-end:cpt-cf-bss-products-algo-quota-catalog-snapshot:p1:inst-quota-replay

/// The snapshot row a staging writes: the verified snapshot as JSON (records sorted by id, the
/// observation time in the digest's UTC form), its candidates' evidence and its notes.
fn stored_snapshot(
    target: &QuotaTarget,
    snapshot: &VerifiedSnapshot,
    plan: &IntakePlan,
    actor: Uuid,
    now: time::OffsetDateTime,
) -> Result<StoredSnapshot, CanonicalError> {
    let unwritable = || {
        CanonicalError::internal("bss-products: a verified snapshot has no UTC observation time")
            .create()
    };
    let observed_at = snapshot
        .observed_at
        .checked_to_offset(UtcOffset::UTC)
        .ok_or_else(unwritable)?;
    let observed_text = observed_at.format(&Rfc3339).map_err(|_| unwritable())?;
    let mut evidence = EvidenceSet::default();
    for judged in &plan.candidates {
        evidence.extend(&judged.evidence);
    }
    Ok(StoredSnapshot {
        id: Uuid::now_v7(),
        target_id: target.id,
        snapshot_id: snapshot.snapshot_id,
        source_revision: snapshot.source_revision.clone(),
        observed_at: stored_instant(observed_at),
        item_count: snapshot.item_count,
        content_digest: snapshot.content_digest.clone(),
        snapshot_json: serde_json::json!({
            "snapshot_id": snapshot.snapshot_id.to_string(),
            "source_revision": snapshot.source_revision,
            "observed_at": observed_text,
            "item_count": snapshot.item_count,
            "content_digest": snapshot.content_digest,
            "items": snapshot.items,
        }),
        evidence_json: evidence.to_json(),
        review_json: review_json(plan),
        created_by: actor,
        created_at: now,
    })
}

/// The stored snapshot at the revision: its candidates when `digest` is its digest, else the
/// conflict.
async fn answer_stored(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    stored: StoredSnapshot,
    digest: &str,
) -> Result<StageOutcome, TxError> {
    if stored.content_digest != digest {
        return Err(TxError::Refused(DomainError::Conflict {
            code: QUOTA_SOURCE_CONFLICT,
            detail: format!(
                "the target holds other content at revision {}",
                stored.source_revision
            ),
        }));
    }
    let candidates = store::candidates_of_snapshot(runner, scope, tenant, stored.id)
        .await
        .map_err(TxError::Repo)?;
    Ok(StageOutcome::Replayed(StagedIntake {
        snapshot: stored,
        candidates,
    }))
}

/// A staging lost the insert race, yet no snapshot is stored at its revision: a 409 to retry, on
/// the adapter target.
pub(crate) fn unsettled() -> CanonicalError {
    On::TARGET.refuse(DomainError::Conflict {
        code: "CONTENDED",
        detail: "a concurrent staging of this revision did not settle; retry".into(),
    })
}

/// The intake's audit row, naming the snapshot row, in the staging's transaction.
async fn audit(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    row: &StoredSnapshot,
) -> Result<(), TxError> {
    repo::write_eventless_act_audit(
        tx,
        scope,
        AuditCommon {
            audit_id: Uuid::now_v7(),
            tenant_id: tenant,
            actor_ref: row.created_by,
            action: ACTION_INTAKE.to_owned(),
            subject_kind: SUBJECT_SNAPSHOT.to_owned(),
            reason: None,
            correlation_id: None,
            written_at: row.created_at,
            lifecycle: LifecycleMove::NONE,
        },
        row.id,
        None,
    )
    .await
    .map_err(TxError::Repo)
}

#[cfg(test)]
#[path = "intake_tests.rs"]
mod intake_tests;
