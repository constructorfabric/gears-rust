//! Append-only commercial command repository. Callers supply the authorized scope.
use super::driver_failure;
use crate::infra::storage::{RepoError, entity::commercial_command as e};
use sea_orm::{ColumnTrait, Condition, DbErr, EntityTrait, IntoActiveModel, sea_query::OnConflict};
use toolkit_db::secure::{AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureInsertExt};
use uuid::Uuid;
/// Complete authenticated command identity, available before a receipt exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandScope {
    /// Authorized catalog tenant.
    pub tenant_id: Uuid,
    /// Authenticated caller tenant.
    pub caller_tenant_id: Uuid,
    /// Authenticated caller subject.
    pub caller_id: Uuid,
    /// Commercial operation (`check` or `hold`).
    pub operation: String,
    /// Client's durable command key.
    pub idempotency_key: String,
}
impl From<&e::Model> for CommandScope {
    fn from(row: &e::Model) -> Self {
        Self {
            tenant_id: row.tenant_id,
            caller_tenant_id: row.caller_tenant_id,
            caller_id: row.caller_id,
            operation: row.operation.clone(),
            idempotency_key: row.idempotency_key.clone(),
        }
    }
}
/// Insert one immutable row in the caller's transaction.
/// # Errors
/// Scope denial, duplicate keys, invalid receipt or database failure.
pub async fn insert(
    runner: &impl DBRunner,
    scope: &AccessScope,
    row: e::Model,
) -> Result<e::Model, RepoError> {
    validate(&row)?;
    let active = row.into_active_model();
    e::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| driver_failure("commercial insert scope".into(), e))?
        .exec_with_returning(runner)
        .await
        .map_err(|e| driver_failure("commercial insert".into(), e))
}
/// Read one immutable row within the catalog tenant and authorized scope.
/// # Errors
/// Database failure.
pub async fn find(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<Option<e::Model>, RepoError> {
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::Id.eq(id)),
        )
        .one(runner)
        .await
        .map_err(|e| driver_failure("commercial find".into(), e))
}
/// Read by the complete authenticated command scope before any receipt is known.
/// # Errors
/// Database failure.
pub async fn find_scope(
    runner: &impl DBRunner,
    scope: &AccessScope,
    key: &CommandScope,
) -> Result<Option<e::Model>, RepoError> {
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(key.tenant_id))
                .add(e::Column::CallerTenantId.eq(key.caller_tenant_id))
                .add(e::Column::CallerId.eq(key.caller_id))
                .add(e::Column::Operation.eq(&key.operation))
                .add(e::Column::IdempotencyKey.eq(&key.idempotency_key)),
        )
        .one(runner)
        .await
        .map_err(|e| driver_failure("commercial command scope lookup".into(), e))
}
fn validate(row: &e::Model) -> Result<(), RepoError> {
    let target = match row.receipt_kind.as_str() {
        "acceptance" => {
            row.operation == "check"
                && row.acceptance_id == Some(row.receipt_id)
                && row.hold_id.is_none()
        }
        "hold" => {
            row.operation == "hold"
                && row.hold_id == Some(row.receipt_id)
                && row.acceptance_id.is_none()
        }
        _ => false,
    };
    if !target || row.idempotency_key.is_empty() {
        return Err(RepoError::CorruptRow("commercial command target".into()));
    }
    Ok(())
}
/// Insert or reread the committed unique-key winner, never overwriting its receipt.
/// The enclosing transaction may need the existing bounded contention retry on either backend.
/// # Errors
/// Different semantic content conflicts; scope and driver failures retain their types.
pub async fn insert_or_get(
    runner: &impl DBRunner,
    scope: &AccessScope,
    row: e::Model,
) -> Result<e::Model, RepoError> {
    validate(&row)?;
    let active = row.clone().into_active_model();
    let result = e::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| driver_failure("commercial race scope".into(), e))?
        .on_conflict_raw(
            OnConflict::columns([
                e::Column::TenantId,
                e::Column::CallerTenantId,
                e::Column::CallerId,
                e::Column::Operation,
                e::Column::IdempotencyKey,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec(runner)
        .await;
    match result {
        Ok(_) | Err(ScopeError::Db(DbErr::RecordNotInserted)) => {}
        Err(e) => return Err(driver_failure("commercial race insert".into(), e)),
    }
    let winner = find_scope(runner, scope, &CommandScope::from(&row))
        .await?
        .ok_or_else(|| RepoError::CorruptRow("commercial winner absent from scope".into()))?;
    if winner.request_digest != row.request_digest
        || winner.receipt_kind != row.receipt_kind
        || winner.receipt_id != row.receipt_id
    {
        return Err(RepoError::Conflict {
            code: "IDEMPOTENCY_CONFLICT",
        });
    }
    Ok(winner)
}
