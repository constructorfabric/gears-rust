//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__transition_audit")]
#[secure(
    no_tenant,
    resource_col = "order_id",
    no_owner,
    no_type,
    pep_prop(
        subject_tenant_id = "subject_tenant_id",
        audit_tenant_id = "audit_tenant_id"
    )
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub audit_id: Uuid,
    pub hash_version: i16,
    pub audit_tenant_id: Option<Uuid>,
    pub subject_tenant_id: Uuid,
    pub resource_tenant_id: Option<Uuid>,
    pub order_id: Option<Uuid>,
    pub requested_order_ref: Option<Uuid>,
    pub sequence: Option<i64>,
    pub prev_hash: Option<Vec<u8>>,
    pub entry_hash: Vec<u8>,
    pub from_state: Option<String>,
    pub to_state: Option<String>,
    pub trigger: String,
    pub outcome: String,
    pub actor: String,
    pub actor_class: String,
    pub delegation_proof_ref: Option<String>,
    pub reason: String,
    pub caller_reason: Option<String>,
    pub force_request_observation: Option<Json>,
    pub changed_field: Option<String>,
    pub prior_value: Option<String>,
    pub new_value: Option<String>,
    pub idempotency_key: String,
    pub correlation_id: Option<Uuid>,
    pub version: Option<i32>,
    pub created_at: TimeDateTimeWithTimeZone,
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
