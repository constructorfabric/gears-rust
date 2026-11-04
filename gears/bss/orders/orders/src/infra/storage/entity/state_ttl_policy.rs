//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Dwell-time bound for one bounded order state at platform or seller scope.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__state_ttl_policy")]
#[secure(no_tenant, resource_col = "policy_id", no_owner, no_type)]
pub struct Model {
    /// Policy identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub policy_id: Uuid,
    /// `platform` or `seller`.
    pub scope: String,
    /// Seller the override applies to; NULL for platform scope.
    pub seller_tenant_id: Option<Uuid>,
    /// Bounded state: `draft`, `submitted`, `pending_approval`, `approved` or `on_hold`.
    pub state: String,
    /// Positive bound as PostgreSQL interval text; NULL means unset (platform row only).
    #[sea_orm(
        column_type = "custom(\"interval\")",
        select_as = "text",
        save_as = "interval"
    )]
    pub ttl_duration: Option<String>,
    /// Whether the platform row still carries the design-seeded provisional value.
    pub provisional: bool,
    /// Positive revision, incremented on every change.
    pub policy_revision: i64,
    /// Who made the last change.
    pub updated_by: String,
    /// Instant of the last change.
    pub updated_at: TimeDateTimeWithTimeZone,
}
/// No `SeaORM` relations are declared for this table.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
