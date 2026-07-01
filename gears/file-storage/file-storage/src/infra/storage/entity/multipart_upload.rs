//! `SeaORM` entity for the `multipart_uploads` table.
//!
//! No `tenant_id` column — tenant boundary is enforced through the parent
//! `files` row. All queries use `AccessScope::allow_all()`.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// A multipart upload session row.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "multipart_uploads")]
#[secure(no_tenant, resource_col = "upload_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub upload_id: Uuid,
    pub file_id: Uuid,
    pub version_id: Uuid,
    pub backend_upload_handle: String,
    pub state: String,
    pub declared_mime: String,
    pub mime_validated: bool,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
