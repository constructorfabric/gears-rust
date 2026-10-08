#[path = "../capability_support/entity.rs"]
mod entity;
#[path = "../capability_support/capabilities.rs"]
mod capabilities;
use capabilities::{DiscoveryScope, DiscoveredOrder, TargetScope};
use toolkit_security::AccessScope;

fn raw_scope_escape(discovery: &DiscoveryScope) -> &AccessScope {
    discovery
}
fn main() {
    // Callers cannot manufacture a discovery capability or a persisted result.
    let _ = DiscoveryScope(AccessScope::deny_all());
    let _ = DiscoveredOrder(entity::Model {
        order_id: uuid::Uuid::nil(), resource_tenant_id: uuid::Uuid::nil(),
        seller_tenant_id: uuid::Uuid::nil(), payer_tenant_id: uuid::Uuid::nil(),
        version: 1, state: "draft".into(),
    });
    // A caller-supplied UUID/scope is not a discovered target.
    let _ = TargetScope::from_discovered(uuid::Uuid::nil());
    let _ = TargetScope::from_discovered(AccessScope::deny_all());
}

fn worker_rejects_discovery(db: &toolkit_db::Db, discovery: DiscoveryScope) {
    let _ = capabilities::recheck_target(db, discovery);
}
fn writes_reject_discovery(discovery: &DiscoveryScope) {
    use sea_orm::EntityTrait;
    use toolkit_db::secure::{SecureDeleteExt, SecureInsertExt, SecureUpdateExt};
    let _ = entity::Entity::delete_many().secure().scope_with(discovery);
    let _ = entity::Entity::update_many().secure().scope_with(discovery);
    let model = entity::ActiveModel::default();
    let _ = entity::Entity::insert(model.clone()).secure().scope_with_model(discovery, &model);
}
