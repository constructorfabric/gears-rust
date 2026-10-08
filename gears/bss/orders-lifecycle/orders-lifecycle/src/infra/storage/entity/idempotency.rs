//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__idempotency")]
#[secure(
    no_tenant,
    resource_col = "execution_id",
    no_owner,
    no_type,
    pep_prop(
        principal_scope = "principal_scope",
        operation = "operation",
        idempotency_key = "idempotency_key"
    )
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub operation: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub principal_scope: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub idempotency_key: String,
    pub order_id: Option<Uuid>,
    pub request_fingerprint: String,
    pub status: String,
    pub execution_id: Uuid,
    pub attempt_id: Option<Uuid>,
    pub fulfillment_control_id: Option<Uuid>,
    pub owner_token: Option<Uuid>,
    pub fencing_generation: i64,
    pub lease_expires_at: Option<TimeDateTimeWithTimeZone>,
    pub outcome: Option<String>,
    pub outcome_reason: Option<String>,
    pub audit_id: Option<Uuid>,
    pub settled_response: Option<Json>,
    pub created_at: TimeDateTimeWithTimeZone,
    pub expires_at: TimeDateTimeWithTimeZone,
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
