//! `bss_rating__source_checkpoint` (DESIGN §3.7 `cpt-cf-bss-rating-dbtable-source-checkpoint`):
//! platform-scoped, as a source spans tenants; written only by the gear's service identity.
use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__source_checkpoint")]
#[secure(unrestricted)]
pub struct Model {
    /// `usage:{uuid}` (or `segments:…`, `period_facts:…` for later features).
    #[sea_orm(primary_key, auto_increment = false)]
    pub source_id: String,
    /// `usage | segments | period_facts`.
    pub source_kind: String,
    /// The canonical definition the id is derived from.
    pub source_definition: JsonValue,
    /// The collector's opaque cursor; `NULL` reads from `Oldest`.
    pub cursor: Option<String>,
    pub applied_seq: Option<i64>,
    /// `active | draining | retired`.
    pub state: String,
    pub updated_at: TimeDateTimeWithTimeZone,
    /// The `usage_feed.config_generation` the usage sources were last reconciled under.
    pub config_generation: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
