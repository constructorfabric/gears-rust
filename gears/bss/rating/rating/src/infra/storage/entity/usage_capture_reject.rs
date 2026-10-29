//! `bss_rating__usage_capture_reject` (DESIGN §3.7 `cpt-cf-bss-rating-dbtable-usage-capture-reject`):
//! entries whose id or tenant cannot be read, or that lie outside the source's subscription;
//! platform-scoped.
use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__usage_capture_reject")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub source_id: String,
    /// The digest of the whole entry.
    #[sea_orm(primary_key, auto_increment = false)]
    pub raw_sha256: String,
    /// The entry, or its stub when larger than the row bound.
    pub raw_entry: JsonValue,
    pub captured_at: TimeDateTimeWithTimeZone,
    /// `unidentifiable | outside_subscription`.
    pub reason: String,
    /// The `unidentifiable_entry` loss an unidentifiable entry opened.
    pub loss_id: Option<Uuid>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
