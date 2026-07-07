//! `SeaORM` entity for `bss.ledger_tenant_precedence_policy` (per-tenant,
//! append-only effective-dated allocation precedence policy versions).

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_tenant_precedence_policy")]
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
    pub version: i64,
    pub effective_from: DateTime<Utc>,
    pub strategy: String,
    pub created_at_utc: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
