//! Capability experiments only; production adapters belong to S2-03.
use super::{entity, history, provider};
use authz_resolver_sdk::{AccessRequest, EnforcerError};
use sea_orm::{ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QuerySelect};
use toolkit_db::secure::{SecureEntityExt, SecureInsertExt, secure_update_with_scope};
use toolkit_db::{Db, secure::TxConfig};
use toolkit_security::{
    AccessScope,
    access_scope::{ScopeFilter, ScopeValue},
};
use uuid::Uuid;

/// Accept a complete model, so no authorization field can be `NotSet`.
pub fn validate_proposed(model: &entity::Model, scope: &AccessScope) -> anyhow::Result<()> {
    let active = model.clone().into_active_model();
    let _validated = entity::Entity::insert(active.clone())
        .secure()
        .scope_with_model(scope, &active)?;
    Ok(())
}

/// Both scopes must come from separate current/proposed PDP evaluations.
pub async fn guarded_update(
    db: &Db,
    observed: entity::Model,
    proposed: entity::Model,
    current_scope: AccessScope,
    proposed_scope: AccessScope,
) -> anyhow::Result<()> {
    anyhow::ensure!(observed.order_id == proposed.order_id, "immutable order ID");
    anyhow::ensure!(
        observed.seller_tenant_id == proposed.seller_tenant_id,
        "immutable seller"
    );
    db.transaction_ref_mapped_with_config(TxConfig::default(), |tx| {
        Box::pin(async move {
            let locked = entity::Entity::find_by_id(observed.order_id)
                .lock_exclusive()
                .secure()
                .scope_with(&current_scope)
                .one(tx)
                .await?;
            anyhow::ensure!(
                locked.as_ref() == Some(&observed),
                "authorization facts changed or hidden"
            );
            validate_proposed(&proposed, &proposed_scope)?;
            let mut active = proposed.clone().into_active_model();
            active.resource_tenant_id = sea_orm::Set(proposed.resource_tenant_id);
            active.payer_tenant_id = sea_orm::Set(proposed.payer_tenant_id);
            active.version = sea_orm::Set(proposed.version);
            secure_update_with_scope::<entity::Entity>(
                active,
                &current_scope,
                observed.order_id,
                tx,
            )
            .await?;
            Ok(())
        })
    })
    .await
}

/// Validate scope shape, never make a local permission decision.
pub fn finite_ids(scope: &AccessScope) -> bool {
    !scope.is_unconstrained()
        && !scope.constraints().is_empty()
        && scope.constraints().iter().all(|branch| {
            branch.filters().iter().any(|filter| match filter {
                ScopeFilter::Eq(eq) => {
                    eq.property() == "id" && matches!(eq.value(), ScopeValue::Uuid(_))
                }
                ScopeFilter::In(set) => {
                    set.property() == "id"
                        && !set.values().is_empty()
                        && set
                            .values()
                            .iter()
                            .all(|v| matches!(v, ScopeValue::Uuid(_)))
                }
                _ => false,
            })
        })
}

pub fn properties(row: &entity::Model) -> AccessRequest {
    AccessRequest::new()
        .require_constraints(true)
        .resource_property("id", row.order_id)
        .resource_property("resource_tenant_id", row.resource_tenant_id)
        .resource_property("seller_tenant_id", row.seller_tenant_id)
        .resource_property("payer_tenant_id", row.payer_tenant_id)
}

#[derive(Debug, PartialEq, Eq)]
pub enum PointResult {
    Found(entity::Model),
    HiddenOrMissing,
    Unavailable,
}

/// Private prefetch is never returned; every path performs a constrained reread.
pub async fn point_read(
    db: &Db,
    id: Uuid,
    pdp: &provider::Recorded,
) -> anyhow::Result<PointResult> {
    let observed = super::capabilities::prefetch(db, id).await?;
    let request = observed.as_ref().map_or_else(
        || AccessRequest::new().require_constraints(true),
        properties,
    );
    let decision = pdp
        .enforcer()
        .access_scope_with(
            &provider::caller(),
            &bss_orders_lifecycle::gts::ORDER,
            "read",
            Some(id),
            &request,
        )
        .await;
    let (scope, unavailable) = match decision {
        Ok(scope) => (scope, false),
        Err(EnforcerError::Denied { .. }) => (AccessScope::deny_all(), false),
        Err(_) => (AccessScope::deny_all(), true),
    };
    let reread = entity::Entity::find_by_id(id)
        .secure()
        .scope_with(&scope)
        .one(&db.conn()?)
        .await?;
    if unavailable {
        return Ok(PointResult::Unavailable);
    }
    // A changed prefetched fact requires a fresh evaluation; do not return stale authorization.
    if reread.is_some() && reread != observed {
        return Ok(PointResult::Unavailable);
    }
    Ok(reread.map_or(PointResult::HiddenOrMissing, PointResult::Found))
}

/// The same SQL statement authorizes the CURRENT parent of each historical row.
pub async fn historical(
    db: &Db,
    id: Uuid,
    scope: &AccessScope,
) -> anyhow::Result<Vec<history::Model>> {
    Ok(history::Entity::find()
        .inner_join(entity::Entity)
        .filter(history::Column::OrderId.eq(id))
        .secure()
        // This ID filter is only narrowing; authority is the joined parent's PDP scope.
        .scope_with(&AccessScope::for_resources(vec![id]))
        .and_scope_for::<entity::Entity>(scope)
        .all(&db.conn()?)
        .await?)
}
