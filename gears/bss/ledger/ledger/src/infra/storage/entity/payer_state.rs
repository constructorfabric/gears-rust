//! `SeaORM` entity for `bss.ledger_payer_state` (per-payer lifecycle).

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_payer_state")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "payer_tenant_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub payer_tenant_id: Uuid,
    pub lifecycle_state: String,
    pub closed_with_open_balance: bool,
    pub approved_by: Option<Uuid>,
    pub changed_at: Option<DateTime<Utc>>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
