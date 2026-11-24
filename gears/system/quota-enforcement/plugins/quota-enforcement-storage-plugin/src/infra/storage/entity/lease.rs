//! `qe_leases`: the lease state machine and the attribution digest of the
//! acquisition, which the charge of its commit takes (ADR-0011).

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
    /// The attribution digest of the acquisition.
    pub attribution_digest: Vec<u8>,
    /// The acquisition's own idempotency key, for the audit trail.
    pub idem_key: String,
    /// One of `active`, `committed`, `released`, `auto_released`,
    /// `resolved_by_deactivation`. Every value but the first is terminal.
    pub state: String,
    /// What the acquisition requested; the holds of every chain of the plan
    /// add up to it, and a commit unwinds what it does not keep of it.
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
