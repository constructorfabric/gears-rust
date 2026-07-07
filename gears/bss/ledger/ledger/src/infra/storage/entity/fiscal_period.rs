//! `SeaORM` entity for `bss.ledger_fiscal_period` (per-legal-entity period status).

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_fiscal_period")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "period_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub legal_entity_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub period_id: String,
    pub fiscal_tz: String,
    pub status: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
