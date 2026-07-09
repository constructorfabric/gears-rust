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
    pub settled_minor: i64,
    pub fee_minor: i64,
    pub allocated_minor: i64,
    pub refunded_minor: i64,
    pub refunded_unallocated_minor: i64,
    pub clawed_back_minor: i64,
    pub version: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
