//! The `oagw_upstream_plugin` table.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// One plugin binding of an upstream. Primary key `(upstream_id, position)`.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "oagw_upstream_plugin")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    /// The upstream this binding belongs to.
    #[sea_orm(primary_key, auto_increment = false)]
    pub upstream_id: Uuid,
    /// Equals the parent upstream's tenant (composite FK).
    pub tenant_id: Uuid,
    /// Zero-based position in the plugin chain.
    #[sea_orm(primary_key, auto_increment = false)]
    pub position: i32,
    /// Canonical plugin identifier.
    pub plugin_ref: String,
    /// UUID parsed from a custom plugin reference.
    pub plugin_uuid: Option<Uuid>,
    /// Version of the JSON layout in `config`.
    pub schema_version: i32,
    /// JSON: plugin config.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub config: Option<Json>,
}

/// Relations of the `oagw_upstream_plugin` entity.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// The alias lookup joins upstreams with their bindings on the composite
    /// foreign key.
    /// The tenant in this key confines the joined rows to the parent's
    /// scoped tenant: keep it (`query_count_tests` pins it in the SQL).
    #[sea_orm(
        belongs_to = "super::upstream::Entity",
        from = "(Column::TenantId, Column::UpstreamId)",
        to = "(super::upstream::Column::TenantId, super::upstream::Column::Id)"
    )]
    Upstream,
}

impl ActiveModelBehavior for ActiveModel {}
