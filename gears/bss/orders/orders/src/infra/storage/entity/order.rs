//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Order aggregate root: identity, tenant axes, current state and version pointers.
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
    /// Aggregate identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    /// Human-readable order number, unique per seller.
    pub order_number: String,
    /// GTS well-known instance identifier of the order category.
    pub category: String,
    /// Resource recipient tenant.
    pub resource_tenant_id: Uuid,
    /// Immutable audit-chain namespace copied from the resource tenant at creation.
    pub audit_tenant_id: Uuid,
    /// Billing party tenant.
    pub payer_tenant_id: Uuid,
    /// Selling party tenant; immutable from creation.
    pub seller_tenant_id: Uuid,
    /// Actor that initiated the order, recorded for audit and delegation proof.
    pub initiating_actor: String,
    /// `self_service` or `partner_placed`; written once at creation.
    pub sales_path: String,
    /// Governing contract, where one is referenced.
    pub contract_id: Option<Uuid>,
    /// Current lifecycle state, denormalized for read latency.
    pub state: String,
    /// When the current state was entered; the dwell input for every sweep.
    pub state_entered_at: TimeDateTimeWithTimeZone,
    /// Current committed version in the version chain.
    pub current_version: i32,
    /// Greatest permanently reserved candidate version; monotonic (D-188).
    pub version_allocation_high_water: i32,
    /// Draft concurrency token, incremented by every commercial draft edit.
    pub draft_revision: i64,
    /// State before a hold; set by a hold and cleared by a resume.
    pub pre_hold_state: Option<String>,
    /// Resumes taken on this order; never decremented.
    pub resume_count: i32,
    /// Amendments appended to this order; never decremented.
    pub amendment_count: i32,
    /// Monotonic generation for fulfillment dispatch and control grants (D-198).
    pub fulfillment_control_generation: i64,
    /// Staged fulfillment control that blocks new activation grants until settled.
    pub fulfillment_control_pending: Option<Uuid>,
    /// When the spawn-signal transition was recorded; never cleared.
    pub spawn_signal_at: Option<TimeDateTimeWithTimeZone>,
    /// When an authorization failure was tolerated for this order; never cleared.
    pub authorization_failure_tolerated_at: Option<TimeDateTimeWithTimeZone>,
    /// Workflow-supplied compensation evidence under a closed schema, if recorded.
    pub compensation_evidence: Option<Json>,
    /// Per-order committed audit counter; the first committed entry takes sequence 1.
    pub audit_sequence: i64,
    /// Creation instant.
    pub created_at: TimeDateTimeWithTimeZone,
}
/// No `SeaORM` relations are declared from the order table.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
