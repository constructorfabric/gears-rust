//! `bss_rating__source_loss` (DESIGN §3.7 `cpt-cf-bss-rating-dbtable-source-loss`): one row per
//! loss against the source scope, owned by the scope's owner root.
use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__source_loss")]
#[secure(tenant_col = "tenant_id", resource_col = "loss_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub loss_id: Uuid,
    pub source_id: String,
    /// The root tenants the loss is against.
    pub tenant_scope: JsonValue,
    /// `cursor_beyond_retention | unidentifiable_entry | scope_widened_beyond_retention`.
    pub kind: String,
    pub gts_type_ids: JsonValue,
    pub detected_at: TimeDateTimeWithTimeZone,
    pub last_good_cursor: Option<String>,
    pub last_good_at: Option<TimeDateTimeWithTimeZone>,
    /// `NULL` restarts at `Oldest`.
    pub restart_cursor: Option<String>,
    /// `open | narrowed | pending_approval | resolved`.
    pub state: String,
    pub tainted_days: JsonValue,
    pub resolution: Option<String>,
    pub evidence_ref: Option<String>,
    pub resolved_by: Option<Uuid>,
    pub approved_by: Option<Uuid>,
    pub resolved_at: Option<TimeDateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
