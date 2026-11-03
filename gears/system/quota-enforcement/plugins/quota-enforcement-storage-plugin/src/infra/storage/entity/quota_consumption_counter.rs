//! `qe_quota_consumption_counters`: the per-`(Quota, period)` counter of a
//! consumption Quota, materialized lazily by the first operation that falls in
//! a period.
//!
//! `period_end` is never null: a one-time Quota stores the open-ended sentinel,
//! so "the period has closed" is `now >= period_end` for every row.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qe_quota_consumption_counters")]
#[secure(tenant_col = "tenant_id", resource_col = "quota_id", no_owner, no_type)]
pub struct Model {
    /// The period row.
    #[sea_orm(primary_key, auto_increment = false)]
    pub period_id: Uuid,
    /// The consumption Quota this period belongs to.
    pub quota_id: Uuid,
    /// The Quota's tenant, denormalized for scoping.
    pub tenant_id: Uuid,
    /// Period start, inclusive.
    pub period_start: OffsetDateTime,
    /// Period end, exclusive. The sentinel for a one-time Quota.
    pub period_end: OffsetDateTime,
    /// Amount consumed in this period.
    pub consumed: i64,
    /// Highest notification threshold already emitted for this period. `NULL`
    /// on a newly materialized row (I13).
    pub highest_crossed_threshold_pct: Option<i16>,
    /// Whether the rollover event has been emitted for this period. A settled
    /// period is final and refuses rollback.
    pub is_settled: bool,
    /// Increments once per accepted mutation.
    pub record_version: i32,
    /// Materialization time.
    pub created_at: OffsetDateTime,
    /// Last write.
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
