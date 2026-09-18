//! Compile-fail test: an entity cannot declare a dimension column beside its
//! `SCOPE_PROPERTIES` table.
//!
//! `tenant_col`, `resource_col` and `owner_col` are not members of
//! `ScopableEntity` any more. They are the well-known properties of the table,
//! read back out of it by `ScopeProperties`, so an entity has nowhere to write
//! a second answer.
//!
//! Security: written by hand they were the same column in two places, and
//! nothing checked that the two agreed. A table saying `owner_tenant_id` means
//! one column while `tenant_col()` returned another would filter a PDP
//! constraint on one column while stamping and checking the tenant on the
//! other, with no error on either path (issue #4726).

use toolkit_db::secure::ScopableEntity;

mod test_entity {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "dimension_from_table")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub legacy_tenant_id: Uuid,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl toolkit_db::secure::ScopableEntity for Entity {
        const SCOPE_PROPERTIES: &'static [(&'static str, Self::Column)] =
            &[("owner_tenant_id", Column::TenantId)];

        fn type_col() -> Option<Column> {
            None
        }

        // ERROR: not a member of the trait. The table above already said which
        // column `owner_tenant_id` means; this is the second, disagreeing place.
        fn tenant_col() -> Option<Column> {
            Some(Column::LegacyTenantId)
        }
    }
}

fn main() {
    let _ = <test_entity::Entity as ScopableEntity>::SCOPE_PROPERTIES;
}
