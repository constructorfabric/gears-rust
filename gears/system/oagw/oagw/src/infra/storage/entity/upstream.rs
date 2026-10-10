//! The `oagw_upstream` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// An upstream row. JSON columns hold storage-local DTOs; each `*_sharing`
/// column is the source of truth for its section's sharing mode.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "oagw_upstream")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    /// Upstream ID.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// Owning tenant.
    pub tenant_id: Uuid,
    /// Unique per tenant (`uq_oagw_upstream_tenant_alias`).
    pub alias: String,
    /// GTS protocol identifier.
    pub protocol: String,
    /// Whether the upstream accepts traffic.
    pub enabled: bool,
    /// `api` or `registry`; written on create only.
    pub managed_by: String,
    /// Version of the JSON layout in this row's JSON columns.
    pub schema_version: i32,
    /// JSON: endpoints.
    #[sea_orm(column_type = "JsonBinary")]
    pub server: Json,
    /// Canonical auth plugin identifier; `NULL` when the upstream has no auth.
    pub auth_plugin_ref: Option<String>,
    /// UUID parsed from a custom auth plugin reference.
    pub auth_plugin_uuid: Option<Uuid>,
    /// JSON: auth plugin config only (no plugin id).
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub auth_config: Option<Json>,
    /// Sharing mode of the auth config (`private`, `inherit` or `enforce`).
    pub auth_sharing: String,
    /// JSON: header rules.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub headers: Option<Json>,
    /// JSON: CORS config without `sharing`.
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub cors: Option<Json>,
    /// Sharing mode of the CORS config (`private`, `inherit` or `enforce`).
    pub cors_sharing: String,
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

/// Relations of the `oagw_upstream` entity (none).
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
