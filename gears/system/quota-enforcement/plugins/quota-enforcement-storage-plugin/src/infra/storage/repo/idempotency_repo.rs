//! `qe_idempotency_records`: replay records addressed by their scope
//! `(tenant_id, operation_type, idem_key)`.
//!
//! A key never cross-matches another tenant's or another operation kind's
//! record. Reads filter on `expires_at`, so a row the sweeper has not reclaimed
//! yet is already invisible and the same key is a new operation.

use std::collections::HashSet;

use sea_orm::sea_query::{LockBehavior, LockType};
use sea_orm::{
    ActiveValue, ColumnTrait, Condition, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect,
};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::RowWait;
use crate::infra::storage::entity::idempotency_record::{self, Column, Entity};
use crate::infra::storage::entity::idempotency_stripe::{self, STRIPES};

/// The three components that address a record, and a charge.
pub struct ScopeKey<'a> {
    /// Authorized target tenant.
    pub tenant_id: Uuid,
    /// Serialized operation kind.
    pub operation_type: &'a str,
    /// Client-supplied key.
    pub idem_key: &'a str,
}

fn addressed(key: &ScopeKey<'_>) -> Condition {
    Condition::all()
        .add(Column::TenantId.eq(key.tenant_id))
        .add(Column::OperationType.eq(key.operation_type))
        .add(Column::IdemKey.eq(key.idem_key))
}

/// The unexpired record under `key`: unlocked for `None`, otherwise locked for
/// update with the given behaviour on a held row.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn find(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &ScopeKey<'_>,
    now: OffsetDateTime,
    lock: Option<RowWait>,
) -> Result<Option<idempotency_record::Model>, ScopeError> {
    let select = Entity::find()
        .filter(addressed(key))
        .filter(Column::ExpiresAt.gt(now));
    let select = match lock {
        Some(wait) => wait.apply(select),
        None => select,
    };
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
        operation_type: ActiveValue::Set(record.key.operation_type.to_owned()),
        idem_key: ActiveValue::Set(record.key.idem_key.to_owned()),
        payload_hash: ActiveValue::Set(record.payload_hash.to_vec()),
        decision_blob: ActiveValue::Set(record.decision_blob.clone()),
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

/// The stripe that serializes writers of `key` (I8): every field of the
/// record's primary key, length-prefixed so no two keys feed the same bytes,
/// through 64-bit FNV-1a. The hash is fixed by its definition rather than by a
/// library, so every process and every version maps a scope to the same row.
#[must_use]
pub fn stripe_of(key: &ScopeKey<'_>) -> i32 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for field in [
        key.tenant_id.as_bytes().as_slice(),
        key.operation_type.as_bytes(),
        key.idem_key.as_bytes(),
    ] {
        let length = u64::try_from(field.len()).unwrap_or(u64::MAX).to_le_bytes();
        for byte in length.iter().chain(field) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    // Below `STRIPES`, which fits an `i32`.
    i32::try_from(hash % u64::from(STRIPES)).unwrap_or(0)
}

/// Lock `stripes` for update in one statement, in ascending order, refusing at
/// once if another transaction holds one of them. Returns the stripes that
/// exist; one that is missing is one only an unmigrated schema can lack.
///
/// # Errors
///
/// The scope or database error of the read; on `PostgreSQL` a held stripe is
/// `lock_not_available`.
pub async fn lock_stripes(runner: &impl DBRunner, stripes: &[i32]) -> Result<Vec<i32>, ScopeError> {
    if stripes.is_empty() {
        return Ok(Vec::new());
    }
    let select = RowWait::Nowait.apply(
        idempotency_stripe::Entity::find()
            .filter(idempotency_stripe::Column::Stripe.is_in(stripes.iter().copied()))
            .order_by(idempotency_stripe::Column::Stripe, Order::Asc),
    );
    Ok(select
        .secure()
        // A platform table: no tenant owns a stripe.
        .scope_with(&AccessScope::allow_all())
        .all(runner)
        .await?
        .into_iter()
        .map(|row| row.stripe)
        .collect())
}

/// Those of `keys` under `(tenant_id, operation_type)` that hold an unexpired
/// record. A platform read of existence only: the tenant is fixed by the
/// predicate, and the caller holds the keys' stripes.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn unexpired_keys(
    runner: &impl DBRunner,
    tenant_id: Uuid,
    operation_type: &str,
    keys: &[&str],
    now: OffsetDateTime,
) -> Result<Vec<String>, ScopeError> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    Ok(Entity::find()
        .filter(Column::TenantId.eq(tenant_id))
        .filter(Column::OperationType.eq(operation_type))
        .filter(Column::IdemKey.is_in(keys.iter().copied()))
        .filter(Column::ExpiresAt.gt(now))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(runner)
        .await?
        .into_iter()
        .map(|row| row.idem_key)
        .collect())
}

/// Lock those of `stripes` that no other transaction holds, in one
/// statement, and return them. A background pass skips the scopes of the
/// others rather than wait on, or refuse, a writer.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn try_lock_stripes(
    runner: &impl DBRunner,
    stripes: &[i32],
) -> Result<HashSet<i32>, ScopeError> {
    if stripes.is_empty() {
        return Ok(HashSet::new());
    }
    Ok(idempotency_stripe::Entity::find()
        .filter(idempotency_stripe::Column::Stripe.is_in(stripes.iter().copied()))
        .lock_with_behavior(LockType::Update, LockBehavior::SkipLocked)
        .secure()
        // A platform table: no tenant owns a stripe.
        .scope_with(&AccessScope::allow_all())
        .all(runner)
        .await?
        .into_iter()
        .map(|row| row.stripe)
        .collect())
}

/// A primary key of a stored record, owned.
pub type OwnedKey = (Uuid, String, String);

/// Up to `batch_size` keys whose records expired before `before`.
///
/// # Errors
///
/// The scope or database error of the read.
pub async fn select_expired(
    runner: &impl DBRunner,
    scope: &AccessScope,
    batch_size: u32,
    before: OffsetDateTime,
) -> Result<Vec<OwnedKey>, ScopeError> {
    Ok(Entity::find()
        .filter(Column::ExpiresAt.lt(before))
        .limit(u64::from(batch_size))
        .secure()
        .scope_with(scope)
        .all(runner)
        .await?
        .into_iter()
        .map(|row| (row.tenant_id, row.operation_type, row.idem_key))
        .collect())
}

/// Delete the records under `keys` that are still expired before `before`.
///
/// The expiry is part of the delete, not only of the selection that found the
/// keys: a writer may have replaced an expired record since, and its fresh
/// record must survive, or a replay of it would run a second time.
///
/// # Errors
///
/// The scope or database error of the delete.
pub async fn delete_if_expired(
    runner: &impl DBRunner,
    scope: &AccessScope,
    keys: &[ScopeKey<'_>],
    before: OffsetDateTime,
) -> Result<u64, ScopeError> {
    // SQLite refuses an expression nested deeper than 1000, and each key adds
    // one level to the OR chain.
    const KEYS_PER_DELETE: usize = 500;
    let mut deleted = 0;
    for keys in keys.chunks(KEYS_PER_DELETE) {
        deleted += Entity::delete_many()
            .filter(
                keys.iter()
                    .fold(Condition::any(), |any, key| any.add(addressed(key))),
            )
            .filter(Column::ExpiresAt.lt(before))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await?
            .rows_affected;
    }
    Ok(deleted)
}
