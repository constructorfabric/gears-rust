//! `SeaORM` entity for `bss.ledger_unallocated_balance` (unapplied-cash cache).

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_unallocated_balance")]
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
    pub payer_tenant_id: Uuid,
    // The grain is one account per (tenant, payer, currency); `account_id` is a
    // resolved attribute (and the SecureORM `resource_col`), NOT a key dimension.
    pub account_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub currency: String,
    pub balance_minor: i64,
    pub functional_balance_minor: Option<i64>,
    pub functional_currency: Option<String>,
    pub last_entry_seq: Option<i64>,
    pub version: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
