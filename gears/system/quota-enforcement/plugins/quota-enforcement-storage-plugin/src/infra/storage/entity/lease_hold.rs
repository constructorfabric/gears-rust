//! `qe_lease_holds`: one row per Quota in a lease's plan.
//!
//! Separate rows rather than an array on the lease, so a Quota's holds are
//! reachable from the Quota side: the deactivation cascade and the
//! expired-hold reconciliation both start there.
//!
//! `returned_at` is that reconciliation's arbiter. An expired lease is released
//! the moment its TTL passes (I4), but its capacity sits in the counter until
//! someone gives it back. Whichever writer locks the counter row first does
//! that and stamps the hold, so the sweeper that arrives later moves nothing a
//! second time and no credit can floor away capacity still owed.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qe_lease_holds")]
#[secure(tenant_col = "tenant_id", resource_col = "quota_id", no_owner, no_type)]
pub struct Model {
    /// The lease this hold belongs to.
    #[sea_orm(primary_key, auto_increment = false)]
    pub lease_token: Uuid,
    /// The held Quota.
    #[sea_orm(primary_key, auto_increment = false)]
    pub quota_id: Uuid,
    /// The Quota's tenant, denormalized so `SecureORM` can scope this row the
    /// way it scopes the counter tables.
    pub tenant_id: Uuid,
    /// What this Quota holds for the lease.
    pub held_amount: i64,
    /// The acquisition period of a consumption Quota, fixed here and never
    /// recomputed: every settlement of this hold lands on it (I5).
    pub period_id: Option<Uuid>,
    /// When the capacity was given back, by whichever writer or sweep got here
    /// first. `NULL` while the counter still carries it.
    pub returned_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
