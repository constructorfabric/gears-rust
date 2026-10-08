//! Scoped persistence model, following Products.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_approval_unit")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub kind: String,
    pub ref_type: String,
    pub ref_id: Uuid,
    pub state: String,
    pub common_effective_date: Option<TimeDate>,
    pub quorum_required: i32,
    pub generation: i32,
    pub submitted_by: Uuid,
    pub submitted_at: TimeDateTimeWithTimeZone,
    pub decided_at: Option<TimeDateTimeWithTimeZone>,
    pub decided_note: Option<String>,
    pub snapshot: Json,
    pub snapshot_hash: String,
    pub version: i64,
    /// The submitter's note (products P-D-219, pricing D-445), added by a forward migration after
    /// `version`; null on every unit written before it.
    pub submit_note: Option<String>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
