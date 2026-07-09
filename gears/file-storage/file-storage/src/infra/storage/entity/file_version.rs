//! `SeaORM` entity for the `file_versions` table (immutable content versions).
//!
//! No `tenant_id` column: versions are reached through the parent `files` row
//! (FK), so tenant scoping is enforced on the file, not re-declared here.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "file_versions")]
#[secure(no_tenant, resource_col = "version_id", no_owner, no_type)]
pub struct Model {
    // `version_id` is globally unique, so it is the sole entity primary key
    // (the DB table keeps the composite `(file_id, version_id)` PK). This keeps
    // updates/deletes keyed off a single PK column.
    pub file_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub version_id: Uuid,
    pub mime_type: String,
    pub size: i64,
    pub hash_algorithm: String,
    pub hash_value: Vec<u8>,
    pub status: String,
    pub is_current: bool,
    pub backend_id: String,
    pub backend_path: String,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
