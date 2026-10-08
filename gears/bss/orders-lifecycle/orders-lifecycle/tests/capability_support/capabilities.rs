//! D-184 type-boundary prototype. Not exported by the runtime crate.
use super::entity;
use sea_orm::{EntityTrait, QuerySelect};
use toolkit_db::secure::SecureEntityExt;
use toolkit_db::{Db, secure::TxConfig};
use toolkit_security::{
    AccessScope,
    access_scope::{ScopeConstraint, ScopeFilter},
};
use uuid::Uuid;

pub struct DiscoveryScope(AccessScope);
pub struct DiscoveredOrder(entity::Model);
pub struct TargetScope {
    scope: AccessScope,
    observed: entity::Model,
}

impl TargetScope {
    pub fn from_discovered(row: DiscoveredOrder) -> Self {
        let row = row.0;
        Self {
            scope: AccessScope::single(ScopeConstraint::new(vec![
                ScopeFilter::eq("id", row.order_id),
                ScopeFilter::eq("resource_tenant_id", row.resource_tenant_id),
                ScopeFilter::eq("seller_tenant_id", row.seller_tenant_id),
                ScopeFilter::eq("payer_tenant_id", row.payer_tenant_id),
            ])),
            observed: row,
        }
    }
}

// D-184: the S1-03 prototype's discovery constructor, mirroring the runtime exception.
#[allow(clippy::disallowed_methods)]
fn discovery() -> DiscoveryScope {
    DiscoveryScope(AccessScope::allow_all())
}

pub async fn discover(db: &Db) -> anyhow::Result<Vec<DiscoveredOrder>> {
    db.transaction_ref_mapped_with_config(TxConfig::read_only(), |tx| {
        Box::pin(async move {
            Ok(entity::Entity::find()
                .limit(10)
                .secure()
                .scope_with(&discovery().0)
                .all(tx)
                .await?
                .into_iter()
                .map(DiscoveredOrder)
                .collect())
        })
    })
    .await
}

/// Separate point-prefetch exception, returning private authorization facts only.
pub async fn prefetch(db: &Db, id: Uuid) -> anyhow::Result<Option<entity::Model>> {
    db.transaction_ref_mapped_with_config(TxConfig::read_only(), |tx| {
        Box::pin(async move {
            Ok(entity::Entity::find_by_id(id)
                .secure()
                .scope_with(&discovery().0)
                .one(tx)
                .await?)
        })
    })
    .await
}

/// A bounded worker entry point consumes only the persisted target capability.
pub async fn recheck_target(db: &Db, target: TargetScope) -> anyhow::Result<bool> {
    db.transaction_ref_mapped_with_config(TxConfig::default(), |tx| {
        Box::pin(async move {
            let row = entity::Entity::find_by_id(target.observed.order_id)
                .lock_exclusive()
                .secure()
                .scope_with(&target.scope)
                .one(tx)
                .await?;
            Ok(row.as_ref() == Some(&target.observed))
        })
    })
    .await
}
