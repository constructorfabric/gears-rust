//! `bss_rating__window_schedule` (DESIGN §3.7 `cpt-cf-bss-rating-dbtable-window-schedule`): one
//! schedule per usage item. Fact intake creates it `policy_pending`; feature 06b pins it.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__window_schedule")]
#[secure(tenant_col = "tenant_id", resource_col = "fact_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub fact_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub item_id: Uuid,
    pub shard: i16,
    /// `policy_pending | scheduling | complete`.
    pub state: String,
    pub fact_version: i64,
    /// `billing_cycle | calendar_hour`.
    pub window_policy: String,
    pub usage_policy_id: Uuid,
    pub usage_policy_version: i64,
    pub usage_policy_digest: String,
    pub finalization_policy_id: Option<Uuid>,
    pub finalization_policy_version: Option<i64>,
    pub finalization_policy_owner_scope: Option<String>,
    pub delay_seconds: Option<i64>,
    pub evidence_mode: Option<String>,
    pub next_due_window_start: Option<TimeDateTimeWithTimeZone>,
    pub next_due_at: TimeDateTimeWithTimeZone,
    pub expected_count: i64,
    pub scheduler_version: i64,
    pub updated_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
