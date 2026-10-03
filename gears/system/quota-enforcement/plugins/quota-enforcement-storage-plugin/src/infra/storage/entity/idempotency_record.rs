//! `qe_idempotency_records`: one row per write operation, keyed by the full
//! four-component scope `(tenant_id, subject_key, operation_type, idem_key)`
//! (PRD section 5.8).
//!
//! The primary key is load-bearing twice over. It keeps the same key string
//! independent across tenants, subject sets, and operation kinds, and it
//! arbitrates two writers that share a scope while locking disjoint Quota
//! rows: the loser's insert violates it and its whole transaction rolls back.
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
    /// SHA-256 fingerprint of the applicable subject set.
    #[sea_orm(primary_key, auto_increment = false)]
    pub subject_key: Vec<u8>,
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
    /// Plugin-private movements of a committed debit: the per-Quota amounts and
    /// the periods they were attributed to, which is what a rollback reverses.
    /// `NULL` for an operation that moved no counter.
    pub applied_entries: Option<String>,
    /// Digest of the authorized attribution a debit was admitted under. A
    /// rollback must present the same one. `NULL` on credit and rollback rows.
    pub attribution_hash: Option<Vec<u8>>,
    /// Key of the rollback that reversed this debit, if any. The reverse-once
    /// arbiter.
    pub reversed_by_key: Option<String>,
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
