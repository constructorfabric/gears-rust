//! `qe_idempotency_records`: one row per write operation other than an allowed
//! debit or a lease commit, whose charge is its record, keyed by the scope
//! `(tenant_id, operation_type, idem_key)` (PRD section 5.8).
//!
//! The primary key is load-bearing twice over. It keeps the same key string
//! independent across tenants and operation kinds, and it is the backstop
//! behind the scope's stripe lock: a writer that inserts under a scope without
//! holding that lock makes the other's insert violate the key, and the loser's
//! whole transaction rolls back.
//!
//! The row has no resource column. Records are addressed by their scope, never
//! by a Quota, and one record can span several Quotas.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qe_idempotency_records")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    /// Authorized target tenant.
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    /// Operation kind, as its serialized name.
    #[sea_orm(primary_key, auto_increment = false)]
    pub operation_type: String,
    /// Client-supplied idempotency key.
    #[sea_orm(primary_key, auto_increment = false)]
    pub idem_key: String,
    /// SHA-256 of the canonical request payload.
    pub payload_hash: Vec<u8>,
    /// The recorded decision, with its top-level schema version.
    pub decision_blob: String,
    /// Engine that produced the decision, when one was invoked.
    pub engine_id: Option<String>,
    /// Policy that produced it, when one was selected.
    pub policy_id: Option<String>,
    /// Version of that policy.
    pub policy_version: Option<i32>,
    /// Record creation time.
    pub created_at: OffsetDateTime,
    /// Retention deadline. A lookup past it finds nothing, so the same key is a
    /// new operation.
    pub expires_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
