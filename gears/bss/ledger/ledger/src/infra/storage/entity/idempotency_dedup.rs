//! `SeaORM` entity for `bss.ledger_idempotency_dedup` (per-flow request dedup).

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_idempotency_dedup")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "business_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub flow: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub business_id: String,
    pub payload_hash: String,
    pub result_entry_id: Option<Uuid>,
    pub posted_at_utc: Option<DateTime<Utc>>,
    pub status: String,
    pub retain_until: Option<DateTime<Utc>>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
