// Created: 2026-08-19 by Constructor Tech
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "resource_membership_tenant")]
#[secure(no_tenant, no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub gts_type_id: i16,
    #[sea_orm(primary_key, auto_increment = false)]
    pub resource_id: String,
    pub tenant_id: Uuid,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
