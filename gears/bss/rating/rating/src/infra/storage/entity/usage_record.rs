//! `bss_rating__usage_record` (DESIGN §3.7 `cpt-cf-bss-rating-dbtable-usage-record`): the raw
//! capture, the normalized columns and the contribution state of every feed entry. Feature 05 adds
//! the attribution and counted-coordinate columns.
use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__usage_record")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "usage_record_id",
    no_owner,
    no_type
)]
pub struct Model {
    /// The entry's resource tenant.
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    /// The collector's entry id.
    #[sea_orm(primary_key, auto_increment = false)]
    pub usage_record_id: Uuid,
    pub source_id: String,
    /// The complete entry as received; never changed.
    pub raw_entry: JsonValue,
    pub raw_sha256: String,
    pub raw_digest_version: i16,
    pub captured_at: TimeDateTimeWithTimeZone,
    /// The meter. Normalized columns are `NULL` on a rejected row.
    pub gts_type: Option<String>,
    pub idempotency_key: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub subject_ref: Option<JsonValue>,
    pub interval_start: Option<TimeDateTimeWithTimeZone>,
    pub interval_end: Option<TimeDateTimeWithTimeZone>,
    /// The entry's canonical decimal text.
    pub quantity: Option<String>,
    pub metadata: Option<JsonValue>,
    /// `record | invalidation`.
    pub entry_type: Option<String>,
    pub invalidates_id: Option<Uuid>,
    pub reason_code: Option<String>,
    /// `live | backfill`.
    pub origin: Option<String>,
    pub accepted_at: Option<TimeDateTimeWithTimeZone>,
    /// The Flow A contribution state.
    pub contribution_state: String,
    pub reject_reason: Option<String>,
    pub withdrawn_by: Option<Uuid>,
    pub withdrawn_at: Option<TimeDateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
