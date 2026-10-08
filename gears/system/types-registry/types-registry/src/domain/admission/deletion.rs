//! Deletion protocol (T20, SPEC §8.1 step 4, DESIGN §3.7).
//!
//! Tombstones remain exact-readable and serve as compatibility baselines until
//! purge (ADR-0013). Deletion allocates no revision and needs no evaluation.
//!
//! Claim `entity_write_order` as the transaction's first statement: adding a
//! dependency moves no target resource version, so CAS alone cannot serialize
//! the live-dependant check against concurrent admission.

use time::OffsetDateTime;
use toolkit_db::DbTx;
use toolkit_db::secure::AccessScope;
use toolkit_macros::domain_model;
use tracing::Span;
use uuid::Uuid;

use super::errors::{ItemFailure, WorkerError};
use crate::config::Limits;
use crate::domain::admission::AdmissionFailureReason;
use crate::domain::enums::LifecycleStatus;
use crate::domain::key::EntityKey;
use crate::domain::ports::{EntityRow, ItemSuccess, Stores};
use crate::observability;

/// What committing a deletion produced.
///
/// No `revision_no`: a deletion allocates none, and reporting one would name a
/// revision that does not exist (ADR-0005) — the same reason `unchanged` carries
/// none.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletionCommit {
    /// The identifier the target resolved to, whichever key named it.
    pub gts_id: String,
    pub gts_uuid: Uuid,
    /// The version the tombstone now carries.
    pub resource_version: i64,
}

impl DeletionCommit {
    /// What a deletion's item write records.
    ///
    /// One function rather than the same decision on the committing and the
    /// predicting path, which is how those two come to disagree.
    #[must_use]
    pub const fn item_outcome(&self, dry_run: bool) -> ItemSuccess {
        ItemSuccess::deletion(dry_run, self.resource_version)
    }
}

/// Tombstone one entity inside the caller's commit transaction, after claiming
/// write order. Resolve the key, then check lifecycle, version and live
/// registered dependants.
///
/// ponytail: ceiling C6 — P0 has no owner/principal check. Any caller reaching
/// the route can delete an eligible entity, including `cf.core.*`; mutation
/// routes remain internal-only (C8). See SPEC C6 for the authorization rollout.
///
/// # Errors
/// [`WorkerError`] for infrastructure failure; `Ok(Err(ItemFailure))` for refusal.
#[expect(
    clippy::too_many_arguments,
    reason = "the eight are the commit transaction's context: stores, tx, scope, the target and its precondition, limits, span and clock. Bundling them into a struct would rename the same values without removing one"
)]
pub async fn commit_deletion(
    stores: &dyn Stores,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    key: &EntityKey,
    expected_resource_version: i64,
    limits: &Limits,
    span: &Span,
    now: OffsetDateTime,
) -> Result<Result<DeletionCommit, ItemFailure>, WorkerError> {
    // First statement, nothing before it, reads included.
    stores.claim_entity_write_order(tx, scope, now).await?;

    let found = match key {
        EntityKey::GtsId(gts_id) => stores.find_by_gts_id(tx, scope, gts_id).await?,
        EntityKey::Uuid(gts_uuid) => stores.find_by_gts_uuid(tx, scope, *gts_uuid).await?,
    };
    let Some(entity) = found else {
        return Ok(Err(ItemFailure::new(
            AdmissionFailureReason::PreconditionFailed,
            format!("'{key}' names no entity, so there is nothing to delete"),
        )));
    };
    decide(
        stores,
        tx,
        scope,
        &entity,
        expected_resource_version,
        limits,
        span,
        now,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "`commit_deletion`'s context with the resolved entity in place of its key"
)]
async fn decide(
    stores: &dyn Stores,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    entity: &EntityRow,
    expected_resource_version: i64,
    limits: &Limits,
    span: &Span,
    now: OffsetDateTime,
) -> Result<Result<DeletionCommit, ItemFailure>, WorkerError> {
    let gts_id = &entity.gts_id;
    observability::record_resolved_gts_id(span, gts_id);
    // Asked **before** the version, deliberately: a tombstone must never suggest
    // retrying with a newer version, which is exactly what `precondition_failed`
    // would invite.
    if entity.lifecycle_status != LifecycleStatus::Active {
        return Ok(Err(ItemFailure::new(
            AdmissionFailureReason::NotActive,
            format!("'{gts_id}' is already deleted, so this deletion has nothing to do"),
        )));
    }

    if entity.resource_version != expected_resource_version {
        return Ok(Err(ItemFailure::new(
            AdmissionFailureReason::PreconditionFailed,
            format!(
                "'{gts_id}' is at resource_version {}, and this deletion expected {expected_resource_version}",
                entity.resource_version,
            ),
        )));
    }

    // Bounded by the same number that bounds a refresh: a refusal must not cost
    // more than the commit it refuses.
    let bound = limits.activation_write_set;
    let blocked = stores
        .live_direct_dependents(tx, scope, entity.id, bound)
        .await?;
    if blocked > 0 {
        observability::record_blocked_dependents(span, blocked);
        // A count, never the identities: the caller may not be entitled to read
        // them, and the set is unbounded in principle.
        let count = if blocked > bound {
            format!("more than {bound}")
        } else {
            blocked.to_string()
        };
        return Ok(Err(ItemFailure::new(
            AdmissionFailureReason::HasRegisteredDependents,
            format!(
                "'{gts_id}' has {count} live direct registered dependants; delete or revise \
                 them first"
            ),
        )));
    }
    observability::record_blocked_dependents(span, 0);

    // Both preconditions are in the statement's `WHERE`, so `None` is a race lost
    // to a concurrent write rather than a state this transaction misread.
    let Some(resource_version) = stores
        .mark_deleted(tx, scope, entity.id, expected_resource_version, now)
        .await?
    else {
        return Ok(Err(ItemFailure::new(
            AdmissionFailureReason::PreconditionFailed,
            format!("'{gts_id}' moved while this deletion was committing"),
        )));
    };

    Ok(Ok(DeletionCommit {
        gts_id: gts_id.clone(),
        gts_uuid: entity.gts_uuid,
        resource_version,
    }))
}
