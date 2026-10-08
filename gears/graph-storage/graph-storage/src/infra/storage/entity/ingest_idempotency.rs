//! Ingest idempotency receipts
//! (`cpt-cf-graph-storage-dbtable-ingest-idempotency`).
//!
//! The receipt commits in the same transaction as the batch it records. A
//! receipt whose `source_epoch` is not the current one is treated exactly as
//! an expired receipt: the retry is `IDEMPOTENCY_KEY_EXPIRED` and requires
//! reconciliation, never automatic re-execution.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ingest_idempotency")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    /// Producer identity, from the security context.
    #[sea_orm(primary_key, auto_increment = false)]
    pub producer: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub idempotency_key: String,
    /// Canonical hash of the ingest request.
    pub request_hash: String,
    /// Epoch in force when the original request committed.
    pub source_epoch: i64,
    /// Revision committed by the original request.
    pub graph_revision: i64,
    /// Recorded outcome, returned to identical retries.
    pub response: Json,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
