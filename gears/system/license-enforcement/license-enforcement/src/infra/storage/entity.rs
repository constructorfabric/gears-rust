//! `SeaORM` entities of the gear store (DESIGN §3.7).

/// One row per grant.
pub mod grant {
    use sea_orm::entity::prelude::*;
    use time::OffsetDateTime;
    use toolkit_db_macros::Scopable;
    use uuid::Uuid;

    #[allow(
        unknown_lints,
        de0309_must_have_domain_model,
        clippy::struct_excessive_bools
    )]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "license_enforcement__grants")]
    #[secure(
        tenant_col = "holder_tenant_id",
        resource_col = "id",
        no_owner,
        no_type
    )]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub holder_tenant_id: Uuid,
        pub resource_type: String,
        pub resource_type_key: String,
        pub resource_id: Option<String>,
        pub subject_type: String,
        pub binding_kind: String,
        pub binding_subject_type: Option<String>,
        pub binding_subject_type_key: Option<String>,
        pub binding_subject_id: Option<String>,
        pub binding_grant_id: Option<Uuid>,
        pub binding_fixed: bool,
        pub validity_kind: String,
        pub not_before: OffsetDateTime,
        pub expires_at: OffsetDateTime,
        pub grace_until: Option<OffsetDateTime>,
        pub effective_end: OffsetDateTime,
        #[sea_orm(column_type = "Json", nullable)]
        pub conditions: Option<Json>,
        #[sea_orm(column_type = "Json", nullable)]
        pub recipient_rules: Option<Json>,
        pub may_bind: bool,
        pub max_depth: i32,
        pub requires_acceptance: bool,
        pub root_grant_id: Uuid,
        pub parent_grant_id: Option<Uuid>,
        pub delegation_id: Option<Uuid>,
        pub pack_id: Option<Uuid>,
        pub evidence_id: Uuid,
        pub state: String,
        pub termination_reason: Option<String>,
        pub version: i64,
        pub created_at: OffsetDateTime,
        pub updated_at: OffsetDateTime,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// One row per pack.
pub mod pack {
    use sea_orm::entity::prelude::*;
    use time::OffsetDateTime;
    use toolkit_db_macros::Scopable;
    use uuid::Uuid;

    #[allow(unknown_lints, de0309_must_have_domain_model)]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "license_enforcement__packs")]
    #[secure(
        tenant_col = "holder_tenant_id",
        resource_col = "id",
        no_owner,
        no_type
    )]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub holder_tenant_id: Uuid,
        pub version: i64,
        pub created_at: OffsetDateTime,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// Evidence that names root grants, with the exact bytes that the plugin
/// admitted.
pub mod evidence {
    use sea_orm::entity::prelude::*;
    use time::OffsetDateTime;
    use toolkit_db_macros::Scopable;
    use uuid::Uuid;

    #[allow(unknown_lints, de0309_must_have_domain_model)]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "license_enforcement__evidence")]
    #[secure(
        tenant_col = "holder_tenant_id",
        resource_col = "id",
        no_owner,
        no_type
    )]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub holder_tenant_id: Uuid,
        pub plugin_id: String,
        pub intake: String,
        pub bytes: Vec<u8>,
        pub replay_key: Option<String>,
        pub external_id: Option<String>,
        pub replaces_evidence_id: Option<Uuid>,
        pub created_at: OffsetDateTime,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// The stored result of a mutating request, per tenant and idempotency token.
pub mod idempotency {
    use sea_orm::entity::prelude::*;
    use time::OffsetDateTime;
    use toolkit_db_macros::Scopable;
    use uuid::Uuid;

    #[allow(unknown_lints, de0309_must_have_domain_model)]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "license_enforcement__idempotency")]
    #[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub token: String,
        pub payload_hash: String,
        #[sea_orm(column_type = "Json")]
        pub response: Json,
        pub expires_at: OffsetDateTime,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// Key-value material of the linked plugin. Only the plugin interprets it.
pub mod plugin_material {
    use sea_orm::entity::prelude::*;
    use toolkit_db_macros::Scopable;

    #[allow(unknown_lints, de0309_must_have_domain_model)]
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
    #[sea_orm(table_name = "license_enforcement__plugin_material")]
    #[secure(no_tenant, no_resource, no_owner, no_type)]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub plugin_id: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub key: String,
        pub value: Vec<u8>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
