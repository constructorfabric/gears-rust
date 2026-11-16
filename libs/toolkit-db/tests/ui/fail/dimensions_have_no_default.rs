//! Compile-fail test: `Dimensions` has no `Default`.
//!
//! With one, `Dimensions { tenant: .., ..Default::default() }` would compile
//! and a forgotten dimension would silently become `None`.

use sea_orm::entity::prelude::*;
use toolkit_db::secure::Dimensions;

mod test_entity {
    use super::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "dimensions_default")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

fn main() {
    // ERROR: no `Default` for `Dimensions`.
    let _dims: Dimensions<test_entity::Column> = Default::default();
}
