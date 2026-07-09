//! `SeaORM` entity for `bss.ledger_payment_allocation_refund` (per-`(payment,
//! invoice)` allocated/refunded counter feeding Slice 3's refund cap).

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_payment_allocation_refund")]
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
    #[sea_orm(primary_key, auto_increment = false)]
    pub invoice_id: String,
    pub allocated_minor: i64,
    pub refunded_minor: i64,
    pub version: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
