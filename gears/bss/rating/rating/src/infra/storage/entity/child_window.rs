//! `bss_rating__child_window` (DESIGN §3.7 `cpt-cf-bss-rating-dbtable-child-window`): the current
//! pointer and status of each child; its history is in the results.
use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__child_window")]
#[secure(tenant_col = "tenant_id", resource_col = "child_id", no_owner, no_type)]
pub struct Model {
    /// `UUIDv5` of the `wk1` name.
    #[sea_orm(primary_key, auto_increment = false)]
    pub child_id: Uuid,
    pub tenant_id: Uuid,
    pub fact_id: Uuid,
    pub shard: i16,
    /// `usage_window | period_line | one_time`.
    pub child_kind: String,
    pub subscription_id: Uuid,
    pub item_id: Uuid,
    pub dimension_value: Option<String>,
    pub aggregation_key: JsonValue,
    pub agg_key_digest: String,
    pub window_group_id: Option<Uuid>,
    pub window_start: TimeDateTimeWithTimeZone,
    pub window_end: TimeDateTimeWithTimeZone,
    pub served_from: TimeDateTimeWithTimeZone,
    pub served_to: TimeDateTimeWithTimeZone,
    /// `pending | provisional | final | final_stale | failed`.
    pub status: String,
    pub pending_reason: Option<String>,
    pub failure_reason: Option<String>,
    pub next_check_at: Option<TimeDateTimeWithTimeZone>,
    pub input_generation: i64,
    pub current_revision: Option<i64>,
    pub current_covered_generation: Option<i64>,
    pub current_input_digest: Option<String>,
    pub binding_of_record: Option<String>,
    pub binding_resolves: Option<JsonValue>,
    pub engine_of_record: Option<i32>,
    pub requested_rebind: bool,
    pub requested_engine: Option<i32>,
    pub requested_run_id: Option<Uuid>,
    pub provisional_outcome: Option<JsonValue>,
    pub last_reason_code: Option<String>,
    pub queued_at: Option<TimeDateTimeWithTimeZone>,
    pub created_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
