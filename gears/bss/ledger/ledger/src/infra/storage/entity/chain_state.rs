//! `SeaORM` entity for `bss.chain_state` (per-tenant tamper-evidence chain
//! tip). One row per tenant pins the last sealed `row_hash`, entry id, period,
//! and sequence so the next seal links onto it.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "chain_state")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    pub last_row_hash: Vec<u8>,
    pub last_entry_id: Uuid,
    pub last_period_id: String,
    pub last_seq: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
