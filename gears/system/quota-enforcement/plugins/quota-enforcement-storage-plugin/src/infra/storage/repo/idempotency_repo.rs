//! `qe_idempotency_records`: replay records addressed by their full
//! four-component scope.
//!
//! Every read carries the whole scope, rollback's lookup of the original
//! included, so no key-only index exists and a key can never cross-match
//! another tenant's, another subject set's, or another operation kind's record.
//! Reads also filter on `expires_at`, so a row the sweeper has not reclaimed
//! yet is already invisible and the same key is a new operation.

use sea_orm::sea_query::LockType;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter, QuerySelect};
use time::OffsetDateTime;
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt, SecureUpdateExt, secure_insert,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::infra::storage::entity::idempotency_record::{self, Column, Entity};

/// The four components that address a record.
pub struct ScopeKey<'a> {
    /// Authorized target tenant.
    pub tenant_id: Uuid,
    /// Fingerprint of the applicable subject set.
    pub subject_key: &'a [u8],
    /// Serialized operation kind.
    pub operation_type: &'a str,
    /// Client-supplied key.
    pub idem_key: &'a str,
}

fn addressed(key: &ScopeKey<'_>) -> Condition {
    Condition::all()
        .add(Column::TenantId.eq(key.tenant_id))
        .add(Column::SubjectKey.eq(key.subject_key.to_vec()))
        .add(Column::OperationType.eq(key.operation_type))
        .add(Column::IdemKey.eq(key.idem_key))
}

/// The unexpired record under `key`, optionally locked.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn find(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &ScopeKey<'_>,
    now: OffsetDateTime,
    lock: bool,
) -> Result<Option<idempotency_record::Model>, ScopeError> {
    let mut select = Entity::find()
        .filter(addressed(key))
        .filter(Column::ExpiresAt.gt(now));
    if lock {
        select = select.lock(LockType::Update);
    }
    select.secure().scope_with(scope).one(runner).await
}

/// Row values of a new record.
pub struct NewRecord<'a> {
    /// What addresses it.
    pub key: ScopeKey<'a>,
    /// Canonical payload digest.
    pub payload_hash: &'a [u8],
    /// The decision blob, with its schema version.
    pub decision_blob: String,
    /// Plugin-private movements a rollback reverses.
    pub applied_entries: Option<String>,
    /// The authorized attribution of a debit.
    pub attribution_hash: Option<Vec<u8>>,
    /// Engine that produced the decision.
    pub engine_id: Option<String>,
    /// Policy that produced it.
    pub policy_id: Option<String>,
    /// Version of that policy.
    pub policy_version: Option<i32>,
    /// Record creation time.
    pub created_at: OffsetDateTime,
    /// Retention deadline.
    pub expires_at: OffsetDateTime,
}

/// Whether the record was inserted, or lost the race for its primary key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inserted {
    /// This transaction owns the key.
    Yes,
    /// Another transaction committed the same scope first. The caller rolls
    /// back and resolves the winner's record.
    Raced,
}

/// Insert a record, reporting a primary-key conflict as [`Inserted::Raced`]
/// rather than as an error.
///
/// # Errors
///
/// The scope or database error of the insert, other than a unique violation.
pub async fn insert(
    runner: &impl DBRunner,
    scope: &AccessScope,
    record: &NewRecord<'_>,
) -> Result<Inserted, ScopeError> {
    let row = idempotency_record::ActiveModel {
        tenant_id: ActiveValue::Set(record.key.tenant_id),
        subject_key: ActiveValue::Set(record.key.subject_key.to_vec()),
        operation_type: ActiveValue::Set(record.key.operation_type.to_owned()),
        idem_key: ActiveValue::Set(record.key.idem_key.to_owned()),
        payload_hash: ActiveValue::Set(record.payload_hash.to_vec()),
        decision_blob: ActiveValue::Set(record.decision_blob.clone()),
        applied_entries: ActiveValue::Set(record.applied_entries.clone()),
        attribution_hash: ActiveValue::Set(record.attribution_hash.clone()),
        reversed_by_key: ActiveValue::Set(None),
        engine_id: ActiveValue::Set(record.engine_id.clone()),
        policy_id: ActiveValue::Set(record.policy_id.clone()),
        policy_version: ActiveValue::Set(record.policy_version),
        created_at: ActiveValue::Set(record.created_at),
        expires_at: ActiveValue::Set(record.expires_at),
    };
    match secure_insert::<Entity>(row, scope, runner).await {
        Ok(_) => Ok(Inserted::Yes),
        Err(error) if error.is_unique_violation() => Ok(Inserted::Raced),
        Err(error) => Err(error),
    }
}

/// Delete an expired row under `key`, so that a new operation can take the key.
///
/// # Errors
///
/// The scope or database error of the delete.
pub async fn delete_expired_at_key(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &ScopeKey<'_>,
    now: OffsetDateTime,
) -> Result<u64, ScopeError> {
    let affected = Entity::delete_many()
        .filter(addressed(key))
        .filter(Column::ExpiresAt.lte(now))
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(affected.rows_affected)
}

/// Record which rollback reversed this debit. `false` when another one already
/// did, which makes the second an idempotent no-op.
///
/// # Errors
///
/// The scope or database error of the update.
pub async fn mark_reversed(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &ScopeKey<'_>,
    by_key: &str,
) -> Result<bool, ScopeError> {
    let affected = Entity::update_many()
        .col_expr(Column::ReversedByKey, by_key.into())
        .filter(addressed(key))
        .filter(Column::ReversedByKey.is_null())
        .secure()
        .scope_with(scope)
        .exec(runner)
        .await?;
    Ok(affected.rows_affected == 1)
}

/// Delete up to `batch_size` records that expired before `before`.
///
/// # Errors
///
/// The scope or database error of the delete.
pub async fn delete_expired(
    runner: &impl DBRunner,
    scope: &AccessScope,
    batch_size: u32,
    before: OffsetDateTime,
) -> Result<u64, ScopeError> {
    let doomed: Vec<(Uuid, Vec<u8>, String, String)> = Entity::find()
        .filter(Column::ExpiresAt.lt(before))
        .limit(u64::from(batch_size))
        .secure()
        .scope_with(scope)
        .all(runner)
        .await?
        .into_iter()
        .map(|row| {
            (
                row.tenant_id,
                row.subject_key,
                row.operation_type,
                row.idem_key,
            )
        })
        .collect();
    let mut deleted = 0;
    for (tenant_id, subject_key, operation_type, idem_key) in &doomed {
        let key = ScopeKey {
            tenant_id: *tenant_id,
            subject_key,
            operation_type,
            idem_key,
        };
        let affected = Entity::delete_many()
            .filter(addressed(&key))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await?;
        deleted += affected.rows_affected;
    }
    Ok(deleted)
}
