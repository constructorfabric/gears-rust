//! Verification transactions after an ambiguous commit (ADR-0006,
//! "Verification after an ambiguous commit"): one for a secret write's tx1
//! (create and overwrite) and one for a record delete.
//!
//! Each starts with a LOCKING read, so that it waits for the ambiguous
//! transaction to resolve instead of racing a commit that is merely delayed:
//!
//! * a write locks its own intent row by `attempt_id` (`SELECT … FOR UPDATE`;
//!   tx1 deletes that row first, so a pending tx1 holds the lock);
//! * a delete locks the record row by id (a pending delete holds the lock).
//!
//! `SQLite` has no row locks (`lock_exclusive` is a no-op there), and none
//! are needed: it serializes writers, so a read inside a writing transaction
//! cannot interleave with another writer's commit.
//!
//! Every body is SQL only and idempotent, and runs through
//! [`SecretRepoImpl::run_tx`] like every other transaction of the protocol.

use credstore_sdk::{SharingMode, StoreKey, ValueVersion};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QuerySelect};
use time::OffsetDateTime;
use toolkit_db::secure::{DbTx, SecureEntityExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{
    CleanupDebt, DeleteVerification, Fallback, NewSecret, SecretRow, WriteAttempt,
    WriteVerification,
};
use crate::infra::storage::entity;
use crate::infra::storage::repo_impl::helpers::{
    SecretRepoImpl, TxFuture, entity_to_model, map_scope_err,
};
use crate::infra::storage::repo_impl::intents::{lost_write_tasks, record_debts};
use crate::infra::storage::repo_impl::writes::{
    active_model, delete_row_tx, insert_active_tx, switch_value_tx,
};

/// What the locking read of a write's verification saw.
enum Probe {
    /// The own intent is still there (and locked by this transaction).
    IntentPresent,
    /// The own intent is gone and the row points at the attempt's version.
    Committed(SecretRow),
    /// The own intent is gone and no row points at the attempt's version.
    NotApplied,
}

/// The locking read of a write's verification: the attempt's own intent by
/// `attempt_id`, then (only if it is gone) the record row by `(tenant_id,
/// id)`. The row is read after the lock was granted, so it reflects the
/// resolved state of any ambiguous transaction that held the lock.
async fn probe_attempt_tx(
    tx: &DbTx<'_>,
    attempt: &WriteAttempt,
    value_version: &ValueVersion,
) -> Result<Probe, DomainError> {
    let intent = entity::write_intents::Entity::find()
        .lock_exclusive()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(
            Condition::all().add(entity::write_intents::Column::AttemptId.eq(attempt.attempt_id)),
        )
        .one(tx)
        .await
        .map_err(map_scope_err)?;
    if intent.is_some() {
        return Ok(Probe::IntentPresent);
    }
    let row = entity::secrets::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(
            Condition::all()
                .add(entity::secrets::Column::TenantId.eq(attempt.key.tenant_id.0))
                .add(entity::secrets::Column::Id.eq(attempt.key.record_id)),
        )
        .one(tx)
        .await
        .map_err(map_scope_err)?;
    match row {
        // A pointer at the attempt's version can only have been set by this
        // attempt's tx1: versions are unique per key.
        Some(row) if row.value_version.as_deref() == Some(value_version.0.as_str()) => {
            Ok(Probe::Committed(entity_to_model(row)))
        }
        _ => Ok(Probe::NotApplied),
    }
}

/// The attempt took no effect: records the cleanup of its version, in this
/// transaction. Reached only when [`probe_attempt_tx`] saw no row pointing at
/// the version, and the intent a late tx1 would need is gone, so nothing can
/// ever point at it.
async fn not_applied_tx(
    tx: &DbTx<'_>,
    attempt: &WriteAttempt,
    value_version: &ValueVersion,
) -> Result<Vec<CleanupDebt>, DomainError> {
    let tasks =
        lost_write_tasks(tx, &attempt.key, value_version, attempt.destroy_supported).await?;
    record_debts(tx, tasks).await
}

/// Verification of a create's tx1 ([`super::writes::insert_active`]).
pub(super) async fn verify_insert_active(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    new: &NewSecret,
    attempt: &WriteAttempt,
) -> Result<WriteVerification<()>, DomainError> {
    let am = active_model(new);
    let scope = scope.clone();
    let attempt = attempt.clone();
    let value_version = new.value_version.clone();
    repo.run_tx(move |tx: &DbTx<'_>| {
        let (scope, am, attempt) = (scope.clone(), am.clone(), attempt.clone());
        let value_version = value_version.clone();
        Box::pin(async move {
            match probe_attempt_tx(tx, &attempt, &value_version).await? {
                Probe::IntentPresent => Ok(WriteVerification::Retried(
                    insert_active_tx(tx, &scope, am, &attempt, &value_version).await?,
                )),
                Probe::Committed(row) => Ok(WriteVerification::Committed { row }),
                Probe::NotApplied => Ok(WriteVerification::NotApplied {
                    debts: not_applied_tx(tx, &attempt, &value_version).await?,
                }),
            }
        }) as TxFuture<'_, WriteVerification<()>>
    })
    .await
}

/// Verification of an overwrite's tx1 ([`super::writes::switch_value`]).
#[allow(
    clippy::too_many_arguments,
    reason = "the same CAS as switch_value, plus nothing: it may have to run it again"
)]
pub(super) async fn verify_switch_value(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    id: Uuid,
    expected_version: i64,
    sharing: SharingMode,
    fallback: Fallback,
    expires_at: Option<OffsetDateTime>,
    new_value_version: ValueVersion,
    attempt: &WriteAttempt,
) -> Result<WriteVerification<SecretRow>, DomainError> {
    let scope = scope.clone();
    let attempt = attempt.clone();
    let backend = repo.db.db().backend();
    repo.run_tx(move |tx: &DbTx<'_>| {
        let (scope, attempt) = (scope.clone(), attempt.clone());
        let value_version = new_value_version.clone();
        Box::pin(async move {
            match probe_attempt_tx(tx, &attempt, &value_version).await? {
                Probe::IntentPresent => Ok(WriteVerification::Retried(
                    switch_value_tx(
                        backend,
                        tx,
                        &scope,
                        id,
                        expected_version,
                        sharing,
                        fallback,
                        expires_at,
                        value_version,
                        &attempt,
                    )
                    .await?,
                )),
                Probe::Committed(row) => Ok(WriteVerification::Committed { row }),
                Probe::NotApplied => Ok(WriteVerification::NotApplied {
                    debts: not_applied_tx(tx, &attempt, &value_version).await?,
                }),
            }
        }) as TxFuture<'_, WriteVerification<SecretRow>>
    })
    .await
}

/// Verification of a record delete ([`super::writes::delete_by_id`]): locks
/// the record row by id. No row: the delete committed. A row: it did not, so
/// the delete runs again here with the same precondition.
pub(super) async fn verify_delete(
    repo: &SecretRepoImpl,
    scope: &AccessScope,
    key: &StoreKey,
    expected_version: Option<i64>,
) -> Result<DeleteVerification, DomainError> {
    let scope = scope.clone();
    let key = key.clone();
    repo.run_tx(move |tx: &DbTx<'_>| {
        let (scope, key) = (scope.clone(), key.clone());
        Box::pin(async move {
            let row = entity::secrets::Entity::find()
                .lock_exclusive()
                .secure()
                .scope_with(&AccessScope::allow_all())
                .filter(
                    Condition::all()
                        .add(entity::secrets::Column::TenantId.eq(key.tenant_id.0))
                        .add(entity::secrets::Column::Id.eq(key.record_id)),
                )
                .one(tx)
                .await
                .map_err(map_scope_err)?;
            let Some(row) = row else {
                return Ok(DeleteVerification::Committed);
            };
            if expected_version.is_some_and(|expected| row.version != expected) {
                return Ok(DeleteVerification::PreconditionFailed);
            }
            match delete_row_tx(tx, &scope, &key, expected_version).await {
                Ok(debts) => Ok(DeleteVerification::Retried { debts }),
                // The row is locked and its version was just checked: 0 rows
                // can only mean the caller's scope no longer covers it.
                Err(DomainError::NotFound) => Ok(DeleteVerification::PreconditionFailed),
                Err(e) => Err(e),
            }
        }) as TxFuture<'_, DeleteVerification>
    })
    .await
}
