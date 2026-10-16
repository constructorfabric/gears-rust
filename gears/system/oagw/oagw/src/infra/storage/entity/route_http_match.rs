//! The `oagw_route_http_match` table.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// The HTTP match of a route. Primary key `route_id`.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "oagw_route_http_match")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    /// The route this match belongs to.
    #[sea_orm(primary_key, auto_increment = false)]
    pub route_id: Uuid,
    /// Equals the parent route's tenant (composite FK).
    pub tenant_id: Uuid,
    /// Path prefix the request path must start with.
    pub path_prefix: String,
}

/// Relations of the `oagw_route_http_match` entity.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// The proxy's candidate load joins routes with their HTTP match on the
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
