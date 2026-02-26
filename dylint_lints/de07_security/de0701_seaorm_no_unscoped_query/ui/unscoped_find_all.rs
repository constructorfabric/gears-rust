// Test file for DE0701: Unscoped SeaORM .find().all() detection
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

/// Unscoped `Entity::find().all()` — should trigger DE0701.
async fn bad_find_all(conn: &impl ConnectionTrait) {
    // Should trigger DE0701 - unscoped query
    let _ = Entity::find().all(conn).await;
}

/// Unscoped `Entity::find().filter().all()` — should trigger DE0701.
async fn bad_find_all_with_filter(conn: &impl ConnectionTrait) {
    // Should trigger DE0701 - unscoped query
    let _ = Entity::find().filter(Column::Name.eq("test")).all(conn).await;
}

/// Test entry point.
fn main() {}
