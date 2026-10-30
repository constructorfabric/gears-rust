//! The `oagw_route_tag` table.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// One tag of a route. Primary key `(route_id, tag)`.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "oagw_route_tag")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    /// The route this tag belongs to.
    #[sea_orm(primary_key, auto_increment = false)]
    pub route_id: Uuid,
    /// Equals the parent route's tenant (composite FK).
    pub tenant_id: Uuid,
    /// Tag value.
    #[sea_orm(primary_key, auto_increment = false)]
    pub tag: String,
}

/// Relations of the `oagw_route_tag` entity (none).
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
