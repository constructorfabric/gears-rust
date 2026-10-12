//! `SeaORM` entity for the `idempotency_keys` table.
//!
//! Composite PK: `(tenant_id, owner_kind, owner_id, idempotency_key)`; no single
//! `resource_col`, so queries use `allow_all()` scope.

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
    /// Subject that created this record. Not part of the PK: on replay the domain
    /// layer checks it against the caller and answers `Forbidden` on mismatch, so a
    /// caller cannot surface another caller's ticket by guessing the key tuple.
    pub subject_id: Uuid,
    pub file_id: Uuid,
    pub response_status: i32,
    #[sea_orm(column_type = "Text")]
    pub response_body: String,
    pub response_etag: String,
    /// SHA-256 of the identity-relevant request fields at insert time
    /// (`domain::idempotency::compute_request_hash`). A replay with a different
    /// hash is rejected with `409 Conflict`.
    pub request_hash: Vec<u8>,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
