//! `SeaORM` entity for `bss.ledger_dispute` (chargeback dispute current-state:
//! the variant + cycle + last phase + disputed amount, keyed by
//! `(tenant_id, dispute_id)`). Tenant-scoped via `SecureORM`; the resource col
//! is the business `dispute_id` (mirrors `pending_event_queue`'s `business_id`).

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_dispute")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "dispute_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub dispute_id: String,
    pub payment_id: String,
    pub currency: String,
    pub variant: String,
    pub last_phase: String,
    pub cycle: i32,
    pub disputed_amount_minor: i64,
    /// The cash actually moved into `DISPUTE_HOLD` at `opened` for a `CASH_HOLD`
    /// dispute (`min(disputed, net)`, Model N) — the size the `won`/`lost`
    /// outcome releases / forfeits. Persisted at `opened` so a settlement-return
    /// that lowers the payment's `net` between `opened` and the outcome cannot
    /// strand the hold (the outcome sizes off THIS stored amount, not a re-read
    /// `settled − fee`). `0` for `AR_RECLASS` (no cash leg).
    pub cash_hold_minor: i64,
    pub version: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
