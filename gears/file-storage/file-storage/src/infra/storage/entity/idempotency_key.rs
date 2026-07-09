//! `SeaORM` entity for the `idempotency_keys` table.
//!
//! Composite PK: `(tenant_id, owner_kind, owner_id, idempotency_key)`.
//! Scoped by `tenant_id` but no single `resource_col` — use `allow_all()` scope.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// An idempotency key row for POST /files deduplication.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "idempotency_keys")]
#[secure(no_tenant, no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub owner_kind: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub owner_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub idempotency_key: String,
    pub file_id: Uuid,
    pub response_status: i32,
    #[sea_orm(column_type = "Text")]
    pub response_body: String,
    pub response_etag: String,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
