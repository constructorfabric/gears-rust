//! Scoped dimension persistence with conditional versions.
use super::{driver_failure, map_unique, matched};
use crate::infra::storage::{RepoError, entity::dimension_key as e};
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order, Set};
use toolkit_db::secure::{
    AccessScope, DBRunner, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use uuid::Uuid;
fn key(tenant: Uuid, id: &str) -> Condition {
    Condition::all()
        .add(e::Column::TenantId.eq(tenant))
        .add(e::Column::Key.eq(id))
}
/// Insert a tenant-scoped row in the caller's transaction.
/// # Errors
/// Returns unique conflicts, parent ownership refusals or typed database failures.
pub async fn insert(
    runner: &impl DBRunner,
    scope: &AccessScope,
    m: e::Model,
) -> Result<e::Model, RepoError> {
    let active = e::ActiveModel {
        tenant_id: Set(m.tenant_id),
        key: Set(m.key),
        values: Set(m.values),
        version: Set(m.version),
    };
    e::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)
        .map_err(|e| driver_failure("insert scope".into(), e))?
        .exec_with_returning(runner)
        .await
        .map_err(|e| map_unique("insert dimension_key".into(), e))
}
/// Read by tenant and identity within the authorized scope.
/// # Errors
/// Returns typed database failures.
pub async fn find(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: &str,
) -> Result<Option<e::Model>, RepoError> {
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(key(tenant, id))
        .one(runner)
        .await
        .map_err(|e| driver_failure("find dimension_key".into(), e))
}
/// List tenant rows in stable identity order.
/// # Errors
/// Returns typed database failures.
pub async fn list(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
) -> Result<Vec<e::Model>, RepoError> {
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(e::Column::TenantId.eq(tenant)))
        .order_by(e::Column::Key, Order::Asc)
        .all(runner)
        .await
        .map_err(|e| driver_failure("list dimension_key".into(), e))
}
/// Change business columns only if the caller's version still owns the row.
/// # Errors
/// Zero matches is a typed version conflict; database failures preserve their type.
pub async fn update(
    runner: &impl DBRunner,
    scope: &AccessScope,
    m: e::Model,
) -> Result<(), RepoError> {
    let predicate = key(m.tenant_id, &m.key).add(e::Column::Version.eq(m.version));
    let result = e::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(e::Column::Values, Expr::value(m.values))
        .col_expr(e::Column::Version, Expr::col(e::Column::Version).add(1_i64))
        .filter(predicate)
        .exec(runner)
        .await
        .map_err(|e| map_unique("update dimension_key".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}

/// Remove an unused registry key at its observed version in the caller's transaction.
/// # Errors
/// Refuses concurrent edits and preserves typed database failures.
pub async fn delete(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: &str,
    version: i64,
) -> Result<(), RepoError> {
    use toolkit_db::secure::SecureDeleteExt;
    let result = e::Entity::delete_many()
        .secure()
        .scope_with(scope)
        .filter(key(tenant, id).add(e::Column::Version.eq(version)))
        .exec(runner)
        .await
        .map_err(|e| driver_failure("delete dimension key".into(), e))?;
    matched(result.rows_affected, "STALE_REVISION")
}

/// Whether a key is declared: stored, or the seed key while the tenant stores no registry.
/// # Errors
/// Returns typed database failures.
pub async fn declared(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    key: &str,
) -> Result<bool, RepoError> {
    if find(runner, scope, tenant, key).await?.is_some() {
        return Ok(true);
    }
    Ok(key == crate::domain::dimension::SEED_KEY && list(runner, scope, tenant).await?.is_empty())
}
/// [`declared`] for an entry that is about to name the key: the seed key of a tenant with no
/// stored registry is stored now, in the caller's transaction, so the entry's foreign key holds.
/// # Errors
/// Returns typed database failures.
pub async fn declare_for_entry(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    key: &str,
) -> Result<bool, RepoError> {
    if find(runner, scope, tenant, key).await?.is_some() {
        return Ok(true);
    }
    if key != crate::domain::dimension::SEED_KEY || !list(runner, scope, tenant).await?.is_empty() {
        return Ok(false);
    }
    insert(
        runner,
        scope,
        e::Model {
            tenant_id: tenant,
            key: key.to_owned(),
            values: serde_json::json!([]),
            version: 1,
        },
    )
    .await?;
    Ok(true)
}
