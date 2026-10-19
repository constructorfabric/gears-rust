//! `bss_rating__operation`: settled `Idempotency-Key`s, keyed `(tenant_id, actor, key)`.

use sea_orm::{ColumnTrait, Condition, EntityTrait, Set};
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, SecureDeleteExt, SecureEntityExt, SecureInsertExt,
};

use crate::domain::digest::Sha256Hex;
use crate::domain::error::DomainError;
use crate::domain::operation::OperationRepo;
use crate::domain::operation::{IdempotencyKey, OperationKey, StoredOperation};
use crate::infra::storage::entity::operation::{ActiveModel, Column, Entity, Model};
use crate::infra::storage::{corrupt, scope_failure};

const TABLE: &str = "bss_rating__operation";

/// The stored operation of `key`, live or expired.
///
/// # Errors
///
/// A storage failure or a corrupt row.
pub async fn find(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &OperationKey,
) -> Result<Option<StoredOperation>, DomainError> {
    Entity::find()
        .secure()
        .scope_with(scope)
        .filter(key_condition(key))
        .one(runner)
        .await
        .map_err(|e| scope_failure("find operation key", e))?
        .map(to_domain)
        .transpose()
}

/// Records a settled operation.
///
/// # Errors
///
/// [`DomainError::Concurrent`] when the key was recorded concurrently; a storage failure.
pub async fn insert(
    runner: &impl DBRunner,
    scope: &AccessScope,
    stored: &StoredOperation,
) -> Result<(), DomainError> {
    let active = ActiveModel {
        tenant_id: Set(stored.key.tenant_id),
        actor: Set(stored.key.actor),
        idempotency_key: Set(stored.key.idempotency_key.as_str().to_owned()),
        operation_id: Set(stored.operation_id),
        request_digest: Set(stored.request_digest.as_str().to_owned()),
        response: Set(stored.response.clone()),
        created_at: Set(stored.created_at),
    };
    Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| scope_failure("record operation key", e))?
        .exec(runner)
        .await
        .map_err(|e| scope_failure("record operation key", e))?;
    Ok(())
}

/// Deletes the (expired) operation of `key`.
///
/// # Errors
///
/// A storage failure.
pub async fn delete(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &OperationKey,
) -> Result<(), DomainError> {
    Entity::delete_many()
        .secure()
        .scope_with(scope)
        .filter(key_condition(key))
        .exec(runner)
        .await
        .map_err(|e| scope_failure("delete expired operation key", e))?;
    Ok(())
}

fn key_condition(key: &OperationKey) -> Condition {
    Condition::all()
        .add(Column::TenantId.eq(key.tenant_id))
        .add(Column::Actor.eq(key.actor))
        .add(Column::IdempotencyKey.eq(key.idempotency_key.as_str()))
}

fn to_domain(row: Model) -> Result<StoredOperation, DomainError> {
    let idempotency_key = IdempotencyKey::parse(&row.idempotency_key)
        .map_err(|_| corrupt(TABLE, "idempotency_key", "invalid"))?;
    let request_digest = Sha256Hex::parse(&row.request_digest)
        .map_err(|_| corrupt(TABLE, "request_digest", &row.request_digest))?;
    Ok(StoredOperation {
        key: OperationKey {
            tenant_id: row.tenant_id,
            actor: row.actor,
            idempotency_key,
        },
        operation_id: row.operation_id,
        request_digest,
        response: row.response,
        created_at: row.created_at,
    })
}

/// The `SecureORM` implementation of the domain's [`OperationRepo`] port.
#[derive(Debug, Default, Clone, Copy)]
pub struct OrmOperationRepo;

#[async_trait::async_trait]
impl OperationRepo for OrmOperationRepo {
    async fn find(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &OperationKey,
    ) -> Result<Option<StoredOperation>, DomainError> {
        find(tx, scope, key).await
    }

    async fn insert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        stored: &StoredOperation,
    ) -> Result<(), DomainError> {
        insert(tx, scope, stored).await
    }

    async fn delete(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        key: &OperationKey,
    ) -> Result<(), DomainError> {
        delete(tx, scope, key).await
    }
}
