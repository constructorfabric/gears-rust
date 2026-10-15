//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__fulfillment_control")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub control_id: Uuid,
    pub order_id: Uuid,
    pub idempotency_execution_id: Uuid,
    pub operation: String,
    pub request_fingerprint: String,
    pub expected_version: i32,
    pub fulfillment_attempt_id: Uuid,
    pub generation: i64,
    pub original_actor: Json,
    pub proof_reference: Option<String>,
    pub authorization_fact_fingerprint: String,
    pub roster: Json,
    pub roster_digest: String,
    pub created_at: TimeDateTimeWithTimeZone,
    pub status: String,
    pub owner_token: Uuid,
    pub fencing_generation: i64,
    pub lease_until: Option<TimeDateTimeWithTimeZone>,
    pub receiver_commands: Json,
    pub receiver_evidence: Json,
    pub error_classification: Option<String>,
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
