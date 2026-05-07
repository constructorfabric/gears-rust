//! SeaORM entity for `chat_attachments` — the test microchat's row.
//!
//! The entity is declared `no_tenant, no_resource, no_owner, no_type`
//! because the test microchat does not use tenant scoping in its
//! repository (queries always run with `AccessScope::allow_all()`).

use modkit_db_macros::Scopable;
use sea_orm::entity::prelude::*;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "chat_attachments")]
#[secure(no_tenant, no_resource, no_owner, no_type)]
pub struct Model {
    /// `file_id` issued by `cf-file-storage` — the microchat's PK.
    #[sea_orm(primary_key, auto_increment = false)]
    pub file_id: Uuid,
    pub chat_id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub mime: String,
    /// `pending` | `active` | `deleted`.
    pub status: String,
    /// NULL until `complete_upload` returns the final ETag.
    #[sea_orm(nullable)]
    pub etag: Option<String>,
    /// NULL until `complete_upload` returns the final size.
    #[sea_orm(nullable)]
    pub size_bytes: Option<i64>,
    /// RFC3339 UTC string — kept as `String` so the schema is
    /// trivially portable across SQLite and PostgreSQL without
    /// fighting time-type round-trips in tests.
    pub created_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
