//! `SeaORM` entity for the `files` table (logical file identity + content pointer).

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "files")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "file_id",
    owner_col = "owner_id",
    type_col = "gts_file_type"
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub file_id: Uuid,
    pub tenant_id: Uuid,
    pub owner_kind: String,
    pub owner_id: Uuid,
    pub name: String,
    pub gts_file_type: String,
    pub content_id: Option<Uuid>,
    pub meta_version: i64,
    pub created_at: OffsetDateTime,
    pub last_modified_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
