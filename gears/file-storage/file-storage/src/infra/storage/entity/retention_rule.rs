//! `SeaORM` entity for the `retention_rules` table (per-tenant / per-user / per-file
//! retention criteria).
//!
//! `tenant_id` provides the tenant boundary; `scope_target_id` is the target id
//! (`user_id` when `scope = "user"`, `file_id` when `scope = "file"`, NULL when
//! `scope = "tenant"`).

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "retention_rules")]
#[secure(tenant_col = "tenant_id", resource_col = "rule_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub rule_id: Uuid,
    pub tenant_id: Uuid,
    /// `"tenant"`, `"user"`, or `"file"`.
    pub scope: String,
    /// Target id -- NULL for tenant scope, `user_id` for user scope, `file_id` for file scope.
    pub scope_target_id: Option<Uuid>,
    /// Retention rule body serialized as JSON (see `RetentionRuleBody`).
    #[sea_orm(column_type = "Text")]
    pub body: String,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
