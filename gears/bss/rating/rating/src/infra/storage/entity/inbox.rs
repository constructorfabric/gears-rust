//! `bss_rating__inbox` (DESIGN §3.7 `cpt-cf-bss-rating-dbtable-inbox`).
use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__inbox")]
#[secure(tenant_col = "tenant_id", resource_col = "entry_id", no_owner, no_type)]
pub struct Model {
    /// `UUIDv5` of the key and digest (one arrival, one entry).
    #[sea_orm(primary_key, auto_increment = false)]
    pub entry_id: Uuid,
    /// The tenant that owns the payload's data.
    pub tenant_id: Uuid,
    /// `fact | segment | scope | coverage | subscription_version | billing_hint`.
    pub source: String,
    pub business_id: String,
    pub version: i64,
    /// Lowercase hex SHA-256 of the canonical payload.
    pub payload_digest: String,
    pub payload: JsonValue,
    /// `pull | event`.
    pub received_via: String,
    /// `accepted | applied | quarantined | discarded`; the only column ever updated.
    pub state: String,
    /// `digest_conflict | unsupported_schema_version`; kept after a release.
    pub quarantine_reason: Option<String>,
    /// The primary entry of the same key a digest conflict was raised against.
    pub conflict_of: Option<Uuid>,
    pub received_at: TimeDateTimeWithTimeZone,
    pub applied_at: Option<TimeDateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
