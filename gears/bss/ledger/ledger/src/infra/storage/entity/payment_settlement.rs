//! `SeaORM` entity for `bss.ledger_payment_settlement` (per-payment money-out
//! serialization counters).

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_payment_settlement")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "tenant_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub payment_id: String,
    pub currency: String,
    pub currency_scale: i16,
    pub settled: String,
    pub fee: String,
    pub allocated: String,
    pub refunded: String,
    pub refunded_unallocated: String,
    pub clawed_back: String,
    pub version: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
