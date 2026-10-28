//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Immutable line of one committed order version.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__order_line")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    /// Owning order.
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    /// Owning order version.
    #[sea_orm(primary_key, auto_increment = false)]
    pub version: i32,
    /// References the order-scoped line identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub line_id: Uuid,
    /// Commercial attempt that selected this line; NULL on the draft version.
    pub commercial_attempt_id: Option<Uuid>,
    /// Pricing receipt selected by the commercial transaction; not a cross-database FK.
    pub pricing_acceptance_id: Option<Uuid>,
    /// Producer digest of the stored per-line Pricing request.
    pub pricing_request_digest: Option<String>,
    /// Producer digest of the returned Pricing terms.
    pub pricing_terms_digest: Option<String>,
    /// Opaque authored plan reference.
    pub plan_id: Uuid,
    /// Opaque authored plan revision reference.
    pub plan_revision_id: Uuid,
    /// Selected items, optional choices, quantities and dimension values.
    pub selected_items: Json,
    /// The line's ISO 4217 currency code.
    pub currency: String,
    /// Contract-effective date.
    pub contract_effective_date: TimeDate,
    /// Requested service-activation date; retained even when deferred.
    pub service_activation_date: Option<TimeDate>,
    /// Acceptance-due calendar date; never recorded assent.
    pub acceptance_due_date: Option<TimeDate>,
    /// Finite term as PostgreSQL interval text; NULL for a rolling or missing term.
    #[sea_orm(
        column_type = "custom(\"interval\")",
        select_as = "text",
        save_as = "interval"
    )]
    pub term_duration: Option<String>,
    /// Term classification: `missing`, `rolling` or `finite`.
    pub term_kind: String,
    /// Authored term intent as JSON.
    pub authored_term: Json,
    /// Cycle the price was quoted against: `month` or `year`.
    pub billing_cycle: String,
    /// Versioned immutable `OrderPin` with the Pricing receipt; NULL only on the draft version.
    pub order_pin: Option<Json>,
    /// Subscriptions' `catalogSubscriptionProductKey` for the line, stored as received.
    pub overlap_scope_key: Option<String>,
    /// Snapshot of the effective date policy (switches, source scope, revision) used at capture.
    pub date_policy_switch_state: Json,
}
/// `SeaORM` relations of this table.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// Belongs to the owning order in `bss_orders__order`.
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
