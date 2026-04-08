// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-persistence:p1
use modkit_db_macros::Scopable;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "gts_type")]
#[secure(no_tenant, no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i16,
    #[sea_orm(unique)]
    pub schema_id: String,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub metadata_schema: Option<serde_json::Value>,
    pub created_at: OffsetDateTime,
    #[sea_orm(nullable)]
    pub updated_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
