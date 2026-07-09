//! `SeaORM` entity for `bss.ledger_tax_subbalance` (per-jurisdiction tax cache).

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_tax_subbalance")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "account_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub account_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub tax_jurisdiction: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub tax_filing_period: String,
    pub balance_minor: i64,
    pub last_entry_seq: Option<i64>,
    pub version: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
