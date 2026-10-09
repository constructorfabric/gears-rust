//! `SeaORM` entity for the `version_hash_manifest` table (ADR-0006).
//!
//! One row per `multipart-composite-sha256` version: a self-contained record
//! (offsets + per-part SHA-256 digests) to re-verify `hash_value` independently of
//! `multipart_upload_parts`. No row exists for `whole-sha256` versions.
//!
//! No `tenant_id` column: tenant scoping is on the parent `file_versions` row
//! (FK, `ON DELETE CASCADE`).

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "version_hash_manifest")]
#[secure(no_tenant, resource_col = "version_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub version_id: Uuid,
    pub manifest: String,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
