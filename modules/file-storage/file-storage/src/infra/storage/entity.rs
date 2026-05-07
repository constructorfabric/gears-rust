//! SeaORM entity for `file_storage.files`.
//!
//! Mirrors the DDL in `modules/file-storage/docs/migration.sql`.

use modkit_db_macros::Scopable;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "files")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub backend_id: Uuid,
    pub file_path: String,
    pub owner_id: Uuid,
    pub name: String,
    pub gts_file_type: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub etag: String,
    pub meta_revision: i64,
    /// `"pending_upload"` | `"uploaded"`.
    pub status: String,
    /// JSON-serialised `BTreeMap<String, String>`.
    pub custom_metadata: String,
    pub upload_expires_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub modified_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
