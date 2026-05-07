//! SeaORM entity for `file_storage.files`.

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
    /// Raw S3 ETag (unquoted) of the current bytes.
    pub etag: String,
    /// Raw S3 VersionId of the current generation. `Some` when bucket has
    /// S3 versioning enabled.
    pub version_id: Option<String>,
    /// `pending_upload | completing | uploaded | meta_updating | deleting`.
    pub status: String,
    /// JSON-serialised `BTreeMap<String, String>`.
    pub custom_metadata: String,
    pub upload_expires_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
