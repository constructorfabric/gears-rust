//! Append-only commercial hold repository. Callers supply the authorized scope.
use super::driver_failure;
use crate::infra::storage::{RepoError, entity::hold as e};
use sea_orm::{ColumnTrait, Condition, DbErr, EntityTrait, IntoActiveModel, sea_query::OnConflict};
use toolkit_db::secure::{AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureInsertExt};
use uuid::Uuid;
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
/// Read the first hold for an acceptance within its catalog tenant.
/// # Errors
/// Database failure.
pub async fn find_acceptance(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    acceptance: Uuid,
) -> Result<Option<e::Model>, RepoError> {
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::AcceptanceId.eq(acceptance)),
        )
        .one(runner)
        .await
        .map_err(|e| driver_failure("hold acceptance lookup".into(), e))
}
/// Build relational columns from the exact held snapshot.
/// # Errors
/// Unrepresentable timestamp or receipt scalar.
pub fn from_receipt(
    tenant: Uuid,
    r: &bss_pricing_sdk::acceptance::HeldBindings,
    actor: Uuid,
    created_at: time::OffsetDateTime,
) -> Result<e::Model, RepoError> {
    use crate::infra::{commercial_terms::wire, usage_policy_wire::digest_text};
    Ok(e::Model {
        id: r.hold_id,
        tenant_id: tenant,
        acceptance_id: r.acceptance_id,
        activation_at: wire::timestamp(r.activation_at)?,
        terms_digest: digest_text(r.terms_digest),
        receipt_json: wire::encode_hold(r)?,
        created_by: actor,
        created_at: wire::timestamp(created_at)?,
    })
}
fn validate(row: &e::Model) -> Result<(), RepoError> {
    let receipt = crate::infra::commercial_terms::wire::decode_hold(&row.receipt_json)?;
    let at = time::OffsetDateTime::parse(
        &row.created_at,
        &time::format_description::well_known::Rfc3339,
    )
    .map_err(|e| RepoError::CorruptRow(e.to_string()))?;
    if from_receipt(row.tenant_id, &receipt, row.created_by, at)? != *row {
        return Err(RepoError::CorruptRow("hold index/receipt mismatch".into()));
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
            OnConflict::columns([e::Column::TenantId, e::Column::AcceptanceId])
                .do_nothing()
                .to_owned(),
        )
        .exec(runner)
        .await;
    match result {
        Ok(_) | Err(ScopeError::Db(DbErr::RecordNotInserted)) => {}
        Err(e) => return Err(driver_failure("commercial race insert".into(), e)),
    }
    let winner = find_acceptance(runner, scope, row.tenant_id, row.acceptance_id)
        .await?
        .ok_or_else(|| RepoError::CorruptRow("commercial winner absent from scope".into()))?;
    if winner.terms_digest != row.terms_digest || winner.activation_at != row.activation_at {
        return Err(RepoError::Conflict {
            code: "ACCEPTANCE_MISMATCH",
        });
    }
    Ok(winner)
}
