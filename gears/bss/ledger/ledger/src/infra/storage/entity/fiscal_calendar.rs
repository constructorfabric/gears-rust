//! `SeaORM` entity for `bss.ledger_fiscal_calendar` (per-legal-entity calendar config).

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_fiscal_calendar")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "legal_entity_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub legal_entity_id: Uuid,
    pub fiscal_tz: String,
    pub granularity: String,
    pub fy_start_month: i16,
    /// The legal entity's functional (books) currency, ISO-4217 (Slice 5 / S5-F3).
    /// NULL → the tenant is treated as single-currency (no FX translation).
    pub functional_currency: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
