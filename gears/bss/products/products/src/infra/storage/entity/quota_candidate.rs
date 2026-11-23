//! Tenant-scoped storage model for `products_quota_candidate` (P-D-268, P-D-269, P-D-274): a
//! candidate's content, never changed, and its review columns. The table's guard refuses every
//! `DELETE` and any `UPDATE` of a content column.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_quota_candidate")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub target_id: Uuid,
    /// `discovery` or `manual`.
    pub origin: String,
    /// Set exactly when the origin is `discovery`.
    pub snapshot_row_id: Option<Uuid>,
    pub record_id: Uuid,
    pub capability_key: String,
    pub record_json: Json,
    pub record_digest: String,
    pub evidence_json: Json,
    /// `staged`, `submitted`, `approved` or `rejected`.
    pub state: String,
    /// The optimistic-concurrency tag, from 1.
    pub version: i64,
    pub proposal_json: Option<Json>,
    pub proposal_evidence_json: Option<Json>,
    pub created_by: Uuid,
    pub created_at: TimeDateTimeWithTimeZone,
    pub submitted_by: Option<Uuid>,
    pub submitted_at: Option<TimeDateTimeWithTimeZone>,
    pub submit_note: Option<String>,
    pub decided_by: Option<Uuid>,
    pub decided_at: Option<TimeDateTimeWithTimeZone>,
    pub decided_note: Option<String>,
    pub definition_id: Option<Uuid>,
    pub definition_version: Option<i64>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
