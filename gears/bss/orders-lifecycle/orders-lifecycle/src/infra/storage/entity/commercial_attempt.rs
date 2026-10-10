//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__commercial_attempt")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub attempt_id: Uuid,
    pub order_id: Uuid,
    pub candidate_version: i32,
    pub previous_committed_version: i32,
    pub idempotency_execution_id: Uuid,
    pub operation: String,
    pub principal_scope: String,
    pub request_fingerprint: String,
    pub prepared_draft_revision: Option<i64>,
    pub proposed_arrangement: Json,
    pub authorization_fact_fingerprint: String,
    pub original_principal: Json,
    pub proof_reference: Option<String>,
    pub line_requests: Json,
    pub date_policy_basis: Json,
    pub commercial_subject_id: Uuid,
    pub commercial_subject_type: String,
    pub commercial_subject_tenant_id: Uuid,
    pub status: String,
    pub owner_token: Uuid,
    pub fencing_generation: i64,
    pub lease_until: Option<TimeDateTimeWithTimeZone>,
    pub receipt_results: Json,
    pub created_at: TimeDateTimeWithTimeZone,
    pub terminal_at: Option<TimeDateTimeWithTimeZone>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::order::Entity",
        from = "Column::OrderId",
        to = "super::order::Column::OrderId"
    )]
    Order,
}
impl Related<super::order::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Order.def()
    }
}
impl ActiveModelBehavior for ActiveModel {}
