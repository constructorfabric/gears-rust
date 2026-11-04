//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Hash-chained audit entry of a committed or refused order transition.
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
    /// Entry identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub audit_id: Uuid,
    /// Audit hash encoding version (new writers emit 3); not the order version.
    pub hash_version: i16,
    /// Immutable chain namespace of the resolved order; NULL for an unresolved refusal.
    pub audit_tenant_id: Option<Uuid>,
    /// Caller home tenant from the trusted security context.
    pub subject_tenant_id: Uuid,
    /// Resource-tenant snapshot of the resolved order; NULL for an unresolved refusal.
    pub resource_tenant_id: Option<Uuid>,
    /// Resolved order; NULL on a refusal with no safely resolved order.
    pub order_id: Option<Uuid>,
    /// Order identifier the caller supplied, without an FK.
    pub requested_order_ref: Option<Uuid>,
    /// Per-order chain sequence of a committed entry; NULL on a refusal.
    pub sequence: Option<i64>,
    /// Hash of the preceding committed entry; NULL on a refusal.
    pub prev_hash: Option<Vec<u8>>,
    /// 32-byte SHA-256 digest over the entry's canonical fields and predecessor.
    pub entry_hash: Vec<u8>,
    /// Observed state before the transition; NULL on create or when unresolved.
    pub from_state: Option<String>,
    /// Observed state after the transition; NULL when unresolved.
    pub to_state: Option<String>,
    /// Transition trigger.
    pub trigger: String,
    /// `committed` or `refused`.
    pub outcome: String,
    /// Caller subject UUID rendered as lowercase hyphenated text.
    pub actor: String,
    /// Closed actor class: `system`, `service` or `user`.
    pub actor_class: String,
    /// Delegation proof reference the PDP accepted, else the one the caller supplied.
    pub delegation_proof_ref: Option<String>,
    /// Registered reason: the trigger's token if committed, the refusal reason if refused.
    pub reason: String,
    /// Caller-supplied explanation, stored as received and never interpreted.
    pub caller_reason: Option<String>,
    /// Locked `{audit_sequence, state, version}` seen by a resolved force/second-approver refusal.
    pub force_request_observation: Option<Json>,
    /// Administrative field changed by this entry, if any.
    pub changed_field: Option<String>,
    /// Minimized prior value of `changed_field`.
    pub prior_value: Option<String>,
    /// Minimized new value of `changed_field`.
    pub new_value: Option<String>,
    /// Idempotency key in force.
    pub idempotency_key: String,
    /// Sibling gear's process correlation identifier, if any.
    pub correlation_id: Option<Uuid>,
    /// Observed order version in force; NULL when unresolved.
    pub version: Option<i32>,
    /// Append instant.
    pub created_at: TimeDateTimeWithTimeZone,
}
/// `SeaORM` relations of this table.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// Belongs to the audited order in `bss_orders__order`.
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
