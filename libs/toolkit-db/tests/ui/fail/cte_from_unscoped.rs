//! Compile-fail test: a CTE cannot be built from an unscoped query.
//!
//! ADR-0001's central claim is that tenant isolation for CTEs is a *compile-time*
//! guarantee: `with_ctes` is reachable only from `SecureSelect<E, Scoped>`, so a
//! CTE body cannot exist without a scope to embed. If `with_ctes` were ever added
//! to the `Unscoped` impl block, every CTE body would silently lose its scope
//! predicate and the `WITH` clause would read unfiltered tables.
//!
//! Security: this is the guard that makes "embed scope inside the body of every
//! CTE" structural rather than a convention a reviewer has to enforce.

use sea_orm::entity::prelude::*;
use toolkit_db::secure::SecureEntityExt;

mod test_entity {
    use super::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "cte_unscoped_table")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

use test_entity::Entity;

fn attempt_cte_without_scope() {
    // ERROR: `with_ctes` is only available on the Scoped state.
    // `.scope_with(&scope)` must come first.
    let _cte_query = Entity::find().secure().with_ctes();
}

fn main() {}
