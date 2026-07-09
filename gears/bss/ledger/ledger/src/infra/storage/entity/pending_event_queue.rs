//! `SeaORM` entity for `bss.ledger_pending_event_queue` (durable queued /
//! quarantined deferred-apply work items, keyed by `(tenant_id, flow,
//! business_id)`). Tenant-scoped via `SecureORM`; the `payload` is a PII-free
//! JSON snapshot of the financial keys the apply path needs.

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_pending_event_queue")]
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
    pub payload: JsonValue,
    pub queued_at: DateTime<Utc>,
    pub apply_after: Option<DateTime<Utc>>,
    pub status: String,
    pub attempts: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
