//! The `oagw_route` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// A route row. The match itself lives in `oagw_route_http_match` (plus
/// `oagw_route_method`) or `oagw_route_grpc_match`, selected by `match_type`.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "oagw_route")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    /// Route ID.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// Owning tenant.
    pub tenant_id: Uuid,
    /// The owning upstream; composite FK `(tenant_id, upstream_id)`.
    pub upstream_id: Uuid,
    /// Whether the route is active.
    pub enabled: bool,
    /// Match priority; higher wins when several routes match.
    pub priority: i32,
    /// `http` or `grpc`.
    pub match_type: String,
    /// `api` or `registry`; written on create only.
    pub managed_by: String,
    /// Version of the JSON layout in this row's JSON columns.
    pub schema_version: i32,
    /// JSON: HTTP query allowlist and path suffix mode; `NULL` for gRPC.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub match_config: Option<Json>,
    /// JSON: CORS config **including** `sharing` (no column for it).
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub cors: Option<Json>,
    /// JSON: rate limit config without `sharing`.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub rate_limit: Option<Json>,
    /// Sharing mode of the rate limit (`private`, `inherit` or `enforce`).
    pub rate_limit_sharing: String,
    /// `NULL` when no plugins configuration exists (absent, not empty).
    pub plugins_sharing: Option<String>,
    /// When the row was created.
    pub created_at: OffsetDateTime,
    /// When the row was last updated.
    pub updated_at: OffsetDateTime,
}

/// Relations of the `oagw_route` entity (none).
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
