//! `bss_rating__operator_audit`: insert-only. No update or delete is offered here.

use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, DBRunner, DbTx, SecureEntityExt, SecureInsertExt};
use uuid::Uuid;

use crate::domain::audit::AuditEntry;
use crate::domain::audit::AuditRepo;
use crate::domain::error::DomainError;
use crate::infra::storage::entity::operator_audit::{ActiveModel, Column, Entity, Model};
use crate::infra::storage::scope_failure;

/// Inserts one audit row.
///
/// # Errors
///
/// A storage failure, or the entry's tenant outside `scope`.
pub async fn insert(
    runner: &impl DBRunner,
    scope: &AccessScope,
    audit_id: Uuid,
    entry: &AuditEntry,
    at: OffsetDateTime,
) -> Result<(), DomainError> {
    let active = ActiveModel {
        tenant_id: Set(entry.tenant_id),
        audit_id: Set(audit_id),
        actor: Set(entry.actor),
        operation_id: Set(entry.operation.operation_id().to_owned()),
        target_kind: Set(entry.target_kind.to_owned()),
        target_id: Set(entry.target_id.clone()),
        request_digest: Set(entry.request_digest.as_str().to_owned()),
        reason: Set(entry.reason.as_str().to_owned()),
        approved_by: Set(entry.approved_by),
        request_id: Set(entry.request_id.clone()),
        at: Set(at),
    };
    Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| scope_failure("record operator audit", e))?
        .exec(runner)
        .await
        .map_err(|e| scope_failure("record operator audit", e))?;
    Ok(())
}

/// The audit rows of one target within `scope`, oldest first.
///
/// # Errors
///
/// A storage failure.
pub async fn list_for_target(
    runner: &impl DBRunner,
    scope: &AccessScope,
    target_kind: &str,
    target_id: &str,
) -> Result<Vec<Model>, DomainError> {
    Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(Column::TargetKind.eq(target_kind))
                .add(Column::TargetId.eq(target_id)),
        )
        .order_by(Column::At, Order::Asc)
        .order_by(Column::AuditId, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| scope_failure("list operator audit", e))
}

/// The `SecureORM` implementation of the domain's [`AuditRepo`] port.
#[derive(Debug, Default, Clone, Copy)]
pub struct OrmAuditRepo;

#[async_trait::async_trait]
impl AuditRepo for OrmAuditRepo {
    async fn insert(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        audit_id: Uuid,
        entry: &AuditEntry,
        at: OffsetDateTime,
    ) -> Result<(), DomainError> {
        insert(tx, scope, audit_id, entry, at).await
    }
}
