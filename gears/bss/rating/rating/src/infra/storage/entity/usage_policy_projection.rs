//! `bss_rating__usage_policy_projection` (DESIGN §3.7
//! `cpt-cf-bss-rating-dbtable-usage-policy-projection`): what ingestion reads to place a record
//! into its window and granule; insert-only, one row per fact version.
use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__usage_policy_projection")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "subscription_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub subscription_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub item_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub valid_from: TimeDateTimeWithTimeZone,
    #[sea_orm(primary_key, auto_increment = false)]
    pub fact_version: i64,
    pub valid_to: TimeDateTimeWithTimeZone,
    pub fact_id: Uuid,
    pub sub_line_key: String,
    pub policy_id: Uuid,
    pub policy_version: i64,
    pub policy_digest: String,
    pub rating_window: String,
    pub aggregation_scope: String,
    pub meter: String,
    /// The declaration's inputs: names, raw GTS ids and folds.
    pub inputs: JsonValue,
    pub binding_digest: String,
    pub created_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
