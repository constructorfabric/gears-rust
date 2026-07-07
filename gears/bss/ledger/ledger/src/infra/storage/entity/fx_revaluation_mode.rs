//! `SeaORM` entity for `bss.ledger_tenant_fx_revaluation_mode` — per-tenant,
//! append-only effective-dated FX revaluation-mode versions (VHP-1986): whether
//! BSS runs the period-end unrealized revaluation (`MODE_B` = ledger of record)
//! or defers to the tenant's ERP (`MODE_A`, the fail-safe default). The
//! revaluation job / period-close pick the row in effect at decision time (latest
//! `effective_from <= now`, highest `version` on a tie); absent a row, the gear
//! default (`MODE_A`) applies. Mirrors the `tenant_posting_policy` append-only
//! shape.

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_tenant_fx_revaluation_mode")]
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
    /// Revaluation mode: `MODE_A` (defer to the tenant's ERP — the fail-safe
    /// default) or `MODE_B` (BSS = ledger of record, runs the period-end
    /// unrealized revaluation). DB CHECK-constrained to those two literals.
    pub revaluation_mode: String,
    pub created_at_utc: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
