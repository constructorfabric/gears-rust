//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__order")]
#[secure(
    no_tenant,
    resource_col = "order_id",
    no_owner,
    no_type,
    pep_prop(
        resource_tenant_id = "resource_tenant_id",
        seller_tenant_id = "seller_tenant_id",
        payer_tenant_id = "payer_tenant_id",
        audit_tenant_id = "audit_tenant_id"
    )
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    pub order_number: String,
    pub category: String,
    pub resource_tenant_id: Uuid,
    pub audit_tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub seller_tenant_id: Uuid,
    pub initiating_actor: String,
    pub sales_path: String,
    pub contract_id: Option<Uuid>,
    pub state: String,
    pub state_entered_at: TimeDateTimeWithTimeZone,
    pub current_version: i32,
    pub version_allocation_high_water: i32,
    pub draft_revision: i64,
    pub pre_hold_state: Option<String>,
    pub resume_count: i32,
    pub amendment_count: i32,
    pub fulfillment_control_generation: i64,
    pub fulfillment_control_pending: Option<Uuid>,
    pub spawn_signal_at: Option<TimeDateTimeWithTimeZone>,
    pub authorization_failure_tolerated_at: Option<TimeDateTimeWithTimeZone>,
    pub compensation_evidence: Option<Json>,
    pub audit_sequence: i64,
    pub created_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
