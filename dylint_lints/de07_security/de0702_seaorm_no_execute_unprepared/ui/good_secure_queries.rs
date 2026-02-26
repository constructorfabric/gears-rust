// Test file for DE0702: Secure patterns that should NOT trigger the lint
#![allow(unused_imports)]
#![allow(dead_code)]
#![allow(unused_variables)]

use sea_orm::entity::prelude::*;
use sea_orm::ConnectionTrait;

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

// Regular SeaORM methods that are NOT execute_unprepared or Statement::from_string
// should not trigger. The unscoped query lint (DE0701) handles those separately.

/// Test entry point.
fn main() {}
