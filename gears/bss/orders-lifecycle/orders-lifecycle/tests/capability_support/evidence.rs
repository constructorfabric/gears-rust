//! Separate test tables let PostgreSQL enforce each role's privileges.
macro_rules! evidence_entity {
    ($module:ident, $table:literal) => {
        pub mod $module {
            use sea_orm::entity::prelude::*;
            use toolkit_db_macros::Scopable;
            #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
            #[sea_orm(table_name = $table)]
            #[secure(no_tenant, resource_col = "id", no_owner, no_type)]
            pub struct Model {
                #[sea_orm(primary_key, auto_increment = false)]
                pub id: Uuid,
                pub value: String,
            }
            #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
            pub enum Relation {}
            impl ActiveModelBehavior for ActiveModel {}
        }
    };
}
evidence_entity!(audit, "orders_capability_audit");
evidence_entity!(private, "orders_capability_private");
evidence_entity!(checkpoint, "orders_capability_checkpoint");

/// Test-only storage oracle: PostgreSQL recomputes frozen SHA-256 bytes independently.
pub mod vector {
    use sea_orm::entity::prelude::*;
    use toolkit_db_macros::Scopable;
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "orders_conformance_vector")]
    #[secure(no_tenant, resource_col = "id", no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub captured_at: DateTimeUtc,
        pub preimage: Vec<u8>,
        pub digest: Vec<u8>,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
