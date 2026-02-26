// Test file for DE0701: .secure() without .scope_with() should still trigger
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

// Simulate the secure() method (without scope_with)
trait SecureExt {
    fn secure(self) -> Self;
}

impl SecureExt for sea_orm::Select<Entity> {
    fn secure(self) -> Self { self }
}

impl SecureExt for sea_orm::UpdateMany<Entity> {
    fn secure(self) -> Self { self }
}

impl SecureExt for sea_orm::DeleteMany<Entity> {
    fn secure(self) -> Self { self }
}

/// `.secure()` without `.scope_with()` on `.all()` — should trigger DE0701.
async fn bad_secure_without_scope_all(conn: &impl ConnectionTrait) {
    // Should trigger DE0701 - unscoped query
    let _ = Entity::find().secure().all(conn).await;
}

/// `.secure()` without `.scope_with()` on `.one()` — should trigger DE0701.
async fn bad_secure_without_scope_one(conn: &impl ConnectionTrait) {
    // Should trigger DE0701 - unscoped query
    let _ = Entity::find().secure().one(conn).await;
}

/// `.secure()` without `.scope_with()` on `.exec()` — should trigger DE0701.
async fn bad_secure_without_scope_exec(conn: &impl ConnectionTrait) {
    // Should trigger DE0701 - unscoped query
    let _ = Entity::update_many().secure().exec(conn).await;
}

/// Test entry point.
fn main() {}
