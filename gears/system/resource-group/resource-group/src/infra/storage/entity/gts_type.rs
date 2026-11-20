// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-persistence:p1
//! RG-local GTS type identity dictionary: maps the `SMALLINT` key groups and
//! memberships store to the full GTS type identifier.
//!
//! A row carries no definition and proves nothing about the type in the types
//! registry (existence, admission, lifecycle). Rows are never updated or
//! deleted, so reads and cleanup of old groups work without the registry.
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "gts_type")]
#[secure(no_tenant, no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i16,
    #[sea_orm(unique)]
    pub schema_id: String,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
