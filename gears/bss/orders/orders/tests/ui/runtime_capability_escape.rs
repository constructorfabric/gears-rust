// D-184 blocking gate on the RUNTIME capability module (src/infra/maintenance/scope.rs), included
// unchanged with the production entities. Every misuse below must fail to compile.
#![allow(dead_code, unused)]
#[path = "../../src/infra/storage/entity/mod.rs"]
mod entity;
#[path = "../../src/infra/maintenance/scope.rs"]
mod scope;
use scope::{DiscoveredOrder, DiscoveryScope, TargetScope};
use toolkit_security::AccessScope;

// A discovery capability never dereferences or converts to a raw scope.
fn deref_escape(discovery: &DiscoveryScope) -> &AccessScope {
    discovery
}
fn worker_entry(_: &TargetScope) {}


fn main() {
    // Outside its module a discovery capability cannot be constructed.
    let _ = DiscoveryScope(AccessScope::deny_all());
    let _ = DiscoveryScope::new();
    // A target scope has no raw constructor: not from a UUID, an AccessScope or a literal.
    let _ = TargetScope::from_discovered_order(&uuid::Uuid::nil());
    let _: TargetScope = AccessScope::deny_all().into();
    // A discovered row cannot be forged from caller values.
    let _ = DiscoveredOrder {
        order_id: uuid::Uuid::nil(),
    };
}

fn discovery_never_reaches_writes_or_worker_entries(discovery: DiscoveryScope) {
    use sea_orm::EntityTrait;
    use toolkit_db::secure::{SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt};
    worker_entry(&discovery);
    let _ = TargetScope::from_discovered_order(&discovery);
    let _ = entity::order::Entity::delete_many().secure().scope_with(&discovery);
    let _ = entity::order::Entity::update_many().secure().scope_with(&discovery);
    let model = entity::order::ActiveModel::default();
    let _ = entity::order::Entity::insert(model.clone())
        .secure()
        .scope_with_model(&discovery, &model);
    // Even its own select builder is private to the discovery module.
    let _ = discovery.select(entity::order::Entity::find());
}
