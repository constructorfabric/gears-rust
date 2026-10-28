//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Date-capture policy: the platform default row or one resource tenant's override.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__date_policy")]
// `resource_tenant_id` addresses a tenant's override without knowing its fresh `policy_id`
// (DESIGN 02 §3.7); the platform default is addressed by its immutable identity.
#[secure(
    no_tenant,
    resource_col = "policy_id",
    no_owner,
    no_type,
    pep_prop(resource_tenant_id = "resource_tenant_id")
)]
pub struct Model {
    /// Policy identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub policy_id: Uuid,
    /// Resource tenant the row governs; NULL for the platform default.
    pub resource_tenant_id: Option<Uuid>,
    /// Whether a service-activation date must be authored rather than defaulted.
    pub service_activation_required: bool,
    /// Whether an acceptance-due date must be authored rather than defaulted.
    pub acceptance_due_required: bool,
    /// Positive, monotonic revision bumped on every promoted change.
    pub revision: i64,
    /// When the promoted change landed.
    pub updated_at: TimeDateTimeWithTimeZone,
}
/// No `SeaORM` relations are declared for this table.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
