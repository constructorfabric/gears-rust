//! `bss_rating__fact_head` (DESIGN §3.7 `cpt-cf-bss-rating-dbtable-fact-head`): one row per fact,
//! updated under its row lock.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__fact_head")]
#[secure(tenant_col = "tenant_id", resource_col = "fact_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub fact_id: Uuid,
    /// `hash(fact_id) mod S`, stored at insert.
    pub shard: i16,
    pub kind: String,
    pub current_version: i64,
    /// `pending | bound | failed`.
    pub binding_state: String,
    pub binding_reason: Option<String>,
    pub bound_version: Option<i64>,
    pub binding_next_check_at: Option<TimeDateTimeWithTimeZone>,
    pub expected_manifest_digest: Option<String>,
    pub rollup_due_at: Option<TimeDateTimeWithTimeZone>,
    pub currency: String,
    pub received_at: TimeDateTimeWithTimeZone,
    pub updated_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
