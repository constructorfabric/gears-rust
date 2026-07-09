//! `SeaORM` entity for the `policies` table (per-tenant / per-user policy store).
//!
//! `tenant_id` is the tenant boundary column; `policy_id` is the sole PK.
//! `scope_owner_id` is NULL for tenant-scope rows and non-NULL for user-scope rows.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "policies")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "policy_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub policy_id: Uuid,
    pub tenant_id: Uuid,
    /// `"tenant"` or `"user"`.
    pub scope: String,
    /// `None` when `scope = "tenant"`; the user's `owner_id` when `scope = "user"`.
    pub scope_owner_id: Option<Uuid>,
    /// Policy body serialized as JSON (see `PolicyBody`).
    #[sea_orm(column_type = "Text")]
    pub body: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
