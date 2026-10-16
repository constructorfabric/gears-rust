//! The `oagw_route_plugin` table.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// One plugin binding of a route. Primary key `(route_id, position)`.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "oagw_route_plugin")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    /// The route this binding belongs to.
    #[sea_orm(primary_key, auto_increment = false)]
    pub route_id: Uuid,
    /// Equals the parent route's tenant (composite FK).
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

/// Relations of the `oagw_route_plugin` entity.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// The proxy's winner load joins the route with its bindings on the
    /// composite foreign key.
    /// The tenant in this key confines the joined rows to the parent's
    /// scoped tenant: keep it (`query_count_tests` pins it in the SQL).
    #[sea_orm(
        belongs_to = "super::route::Entity",
        from = "(Column::TenantId, Column::RouteId)",
        to = "(super::route::Column::TenantId, super::route::Column::Id)"
    )]
    Route,
}

impl ActiveModelBehavior for ActiveModel {}
