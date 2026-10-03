//! `qe_leases`: the lease state machine and the two values a settlement cannot
//! take from its caller.
//!
//! `subject_key` is the idempotency subject the acquisition fingerprinted, and
//! `attribution_hash` the attribution the PDP authorized it under. A commit or
//! release completes its idempotency scope from the first — the caller may
//! never supply one — and a rollback of a commit must present the second.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qe_leases")]
#[secure(tenant_col = "tenant_id", resource_col = "token", no_owner, no_type)]
pub struct Model {
    /// The opaque, server-issued token.
    #[sea_orm(primary_key, auto_increment = false)]
    pub token: Uuid,
    /// Owning tenant.
    pub tenant_id: Uuid,
    /// The metric the hold was acquired on, which the active-lease cap is
    /// keyed by together with the tenant (I7).
    pub metric: String,
    /// The subject key the acquisition recorded under. Commit and release
    /// reuse it rather than resolving against the current catalogue.
    pub subject_key: Vec<u8>,
    /// The authorized attribution digest of the acquisition.
    pub attribution_hash: Vec<u8>,
    /// The acquisition's own idempotency key, for the audit trail.
    pub idem_key: String,
    /// One of `active`, `committed`, `released`, `auto_released`,
    /// `resolved_by_deactivation`. Every value but the first is terminal.
    pub state: String,
    /// What the acquisition requested. The plan's holds need not sum to it, so
    /// the commit's share is measured against this, not against the holds.
    pub reserved_amount: i64,
    /// When the hold was taken.
    pub acquired_at: OffsetDateTime,
    /// When the hold lapses. Past this instant the lease is released
    /// semantically, whatever `state` still reads (I4).
    pub expiry_at: OffsetDateTime,
    /// When a transition left `active`.
    pub resolved_at: Option<OffsetDateTime>,
    /// Increments once per accepted mutation.
    pub record_version: i32,
    /// Row creation time.
    pub created_at: OffsetDateTime,
    /// Last write.
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
