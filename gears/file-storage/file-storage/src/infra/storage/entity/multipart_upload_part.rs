//! `SeaORM` entity for the `multipart_upload_parts` table.
//!
//! Composite PK: `(upload_id, part_number)`. No `tenant_id` — queried via the
//! parent `multipart_uploads` row; all queries use `AccessScope::allow_all()`.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// One uploaded part of a multipart upload session.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "multipart_upload_parts")]
#[secure(no_tenant, no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub upload_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub part_number: i32,
    pub backend_etag: String,
    pub part_hash: Vec<u8>,
    pub size: i64,
    pub uploaded_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
