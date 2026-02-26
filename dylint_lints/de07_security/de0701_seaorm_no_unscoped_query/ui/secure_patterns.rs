// Test file for DE0701: Secure patterns that should NOT trigger the lint
#![allow(unused_imports)]
#![allow(dead_code)]
#![allow(unused_variables)]

use sea_orm::entity::prelude::*;

// Minimal entity definition for testing
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub name: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

// ── Test-only shim ──────────────────────────────────────────────────────
// The real secure extension traits (SecureEntityExt, SecureUpdateExt, …)
// live in libs/modkit-db which belongs to the main workspace and cannot be
// imported here.  This no-op shim provides `.secure()` and `.scope_with()`
// method names so the code below compiles and the lint's name-based chain
// detection sees them.
//
// Because DE0701 uses method-name matching (not DefId / type resolution),
// any trait that exposes identically-named methods will satisfy the lint.
// This is a documented known limitation (see lib.rs "Known Limitations").
// The shim therefore correctly exercises the positive path of the lint —
// if name-based detection ever changes to type-aware detection, this test
// must be updated to use the real traits or a more faithful mock.
// ────────────────────────────────────────────────────────────────────────
trait SecureExt {
    fn secure(self) -> Self;
    fn scope_with(self, scope: &str) -> Self;
}

impl SecureExt for sea_orm::Select<Entity> {
    fn secure(self) -> Self { self }
    fn scope_with(self, _scope: &str) -> Self { self }
}

impl SecureExt for sea_orm::UpdateMany<Entity> {
    fn secure(self) -> Self { self }
    fn scope_with(self, _scope: &str) -> Self { self }
}

impl SecureExt for sea_orm::DeleteMany<Entity> {
    fn secure(self) -> Self { self }
    fn scope_with(self, _scope: &str) -> Self { self }
}

/// Scoped `Entity::find().secure().scope_with().all()` — should NOT trigger DE0701.
async fn good_find_all_secure(conn: &impl ConnectionTrait) {
    // Should not trigger DE0701 - unscoped query
    let _ = Entity::find().secure().scope_with("scope").all(conn).await;
}

/// Scoped `Entity::find().secure().scope_with().one()` — should NOT trigger DE0701.
async fn good_find_one_secure(conn: &impl ConnectionTrait) {
    // Should not trigger DE0701 - unscoped query
    let _ = Entity::find().secure().scope_with("scope").one(conn).await;
}

/// Scoped `Entity::find().secure().scope_with().count()` — should NOT trigger DE0701.
async fn good_find_count_secure(conn: &impl ConnectionTrait) {
    // Should not trigger DE0701 - unscoped query
    let _ = Entity::find().secure().scope_with("scope").count(conn).await;
}

/// Scoped `Entity::update_many().secure().scope_with().exec()` — should NOT trigger DE0701.
async fn good_update_many_secure(conn: &impl ConnectionTrait) {
    // Should not trigger DE0701 - unscoped query
    let _ = Entity::update_many().secure().scope_with("scope").exec(conn).await;
}

/// Scoped `Entity::delete_many().secure().scope_with().exec()` — should NOT trigger DE0701.
async fn good_delete_many_secure(conn: &impl ConnectionTrait) {
    // Should not trigger DE0701 - unscoped query
    let _ = Entity::delete_many().secure().scope_with("scope").exec(conn).await;
}

/// Test entry point.
fn main() {}
