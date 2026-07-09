//! `SeaORM` entity for the `files_custom_metadata` table (user key/value pairs).
//!
//! Reached through the parent `files` row (FK); tenant scoping is on the file.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "files_custom_metadata")]
#[secure(no_tenant, no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false, column_type = "Text")]
    pub file_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    pub value: String,
    pub set_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
