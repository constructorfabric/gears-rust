//! Append-only commercial acceptance repository. Callers supply the authorized scope.
use super::driver_failure;
use crate::infra::storage::{RepoError, entity::acceptance as e};
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
/// Read the single order line/version acceptance, using the order-prefix unique index.
/// # Errors
/// Database failure.
pub async fn find_business(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    order: Uuid,
    version: &str,
    line: Uuid,
) -> Result<Option<e::Model>, RepoError> {
    e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::OrderId.eq(order))
                .add(e::Column::OrderVersion.eq(version))
                .add(e::Column::LineId.eq(line)),
        )
        .one(runner)
        .await
        .map_err(|e| driver_failure("acceptance business lookup".into(), e))
}
/// Whether a consumer holds a binding on `price`: an acceptance of the catalog tenant whose
/// receipt's bindings name it (D-520). These receipts are every binding pricing stores. A hold
/// freezes its acceptance's bindings and needs that acceptance; a consumer's pins are its own and
/// pricing stores none.
///
/// The receipt is text, so the database narrows the tenant's acceptances to the receipts that
/// mention the id. Each one is then decoded, and only its bindings count: the same id elsewhere
/// in a receipt is not a binding. An id is hexadecimal and hyphens, so it holds no `LIKE`
/// wildcard.
/// # Errors
/// Database failure, or a stored receipt that does not decode.
pub async fn binds_price(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    price: Uuid,
) -> Result<bool, RepoError> {
    let mentions = e::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(e::Column::TenantId.eq(tenant))
                .add(e::Column::ReceiptJson.contains(price.to_string())),
        )
        .all(runner)
        .await
        .map_err(|e| driver_failure("acceptance binding lookup".into(), e))?;
    for row in mentions {
        let receipt = crate::infra::commercial_terms::wire::decode_acceptance(&row.receipt_json)?;
        if receipt.bindings.iter().any(|b| b.price.price_id == price) {
            return Ok(true);
        }
    }
    Ok(false)
}
/// Build indexed fields from the typed receipt without rehashing issued content.
/// # Errors
/// Unsupported version or invalid exact scalar.
pub fn from_receipt(
    r: &bss_pricing_sdk::acceptance::AcceptanceReceipt,
    actor: Uuid,
) -> Result<e::Model, RepoError> {
    use crate::infra::{commercial_terms::wire, usage_policy_wire::digest_text};
    Ok(e::Model {
        id: r.acceptance_id,
        tenant_id: r.query.tenant_axes.seller_tenant_id,
        order_id: r.query.order_id,
        order_version: r.query.order_version.to_string(),
        line_id: r.query.line_id,
        request_digest: digest_text(r.request_digest),
        terms_digest: digest_text(r.terms_digest),
        receipt_json: wire::encode_acceptance(r)?,
        accepted_at: wire::timestamp(r.accepted_at)?,
        hold_until: wire::timestamp(r.hold_until)?,
        created_by: actor,
    })
}
fn validate(row: &e::Model) -> Result<(), RepoError> {
    let receipt = crate::infra::commercial_terms::wire::decode_acceptance(&row.receipt_json)?;
    if from_receipt(&receipt, row.created_by)? != *row {
        return Err(RepoError::CorruptRow(
            "acceptance index/receipt mismatch".into(),
        ));
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
                e::Column::OrderId,
                e::Column::OrderVersion,
                e::Column::LineId,
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
    let winner = find_business(
        runner,
        scope,
        row.tenant_id,
        row.order_id,
        &row.order_version,
        row.line_id,
    )
    .await?
    .ok_or_else(|| RepoError::CorruptRow("commercial winner absent from scope".into()))?;
    if winner.request_digest != row.request_digest {
        return Err(RepoError::Conflict {
            code: "ACCEPTANCE_MISMATCH",
        });
    }
    Ok(winner)
}
