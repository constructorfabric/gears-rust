//! Compile-fail test: a hand-written scope that leaves out a dimension.
//!
//! `Dimensions` has no default, so every dimension must be written out and a
//! forgotten one is a missing-field error at the entity, in `cargo check`.

use sea_orm::entity::prelude::*;
use toolkit_db::secure::{Dimensions, EntityScope, ScopableEntity};

mod test_entity {
    use super::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "forgotten_dimension")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub tenant_id: Uuid,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    // ERROR: `type_col` is not decided.
    impl ScopableEntity for Entity {
        const SCOPE: EntityScope<Column> = EntityScope::Scoped(Dimensions {
            tenant: Some(Column::TenantId),
            resource: Some(Column::Id),
            owner: None,
            custom: &[],
        });
    }
}

fn main() {}
