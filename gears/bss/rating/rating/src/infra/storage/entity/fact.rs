//! `bss_rating__fact` (DESIGN §3.7 `cpt-cf-bss-rating-dbtable-fact`): one row per fact version,
//! insert-only; the shape of Subscriptions' `subscriptions-rating/1` profile (T-D-87).
use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__fact")]
#[secure(tenant_col = "tenant_id", resource_col = "fact_id", no_owner, no_type)]
pub struct Model {
    /// The seller tenant.
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub fact_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub fact_version: i64,
    /// The inbox entry the version arrived as.
    pub entry_id: Uuid,
    /// `recurring | usage | one_time`.
    pub kind: String,
    /// `active | zeroed`.
    pub obligation: String,
    pub predecessor_version: Option<i64>,
    pub subscription_id: Uuid,
    pub sub_line_key: String,
    pub item_id: Uuid,
    pub billing_group_id: Uuid,
    pub seller_tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub resource_tenant_id: Uuid,
    pub currency: String,
    pub period_start: TimeDateTimeWithTimeZone,
    pub period_end: TimeDateTimeWithTimeZone,
    pub served_from: TimeDateTimeWithTimeZone,
    pub served_to: TimeDateTimeWithTimeZone,
    /// `advance | arrears`.
    pub timing: String,
    pub occurrence_id: Option<String>,
    pub billing_terms: JsonValue,
    pub suspensions: JsonValue,
    /// The slices with their coverage, terms and lots.
    pub timeline: JsonValue,
    /// Per `terms_ref`: the plan, revision, cell and `binding_digest`.
    pub accepted_terms: JsonValue,
    /// Subscriptions' payload digest.
    pub producer_digest: String,
    /// Rating's digest of the payload (the inbox's).
    pub payload_digest: String,
    pub payload: JsonValue,
    pub received_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
