//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// One predicate result of a Preview, submit or amendment gate assessment run.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__gate_outcome")]
#[secure(
    tenant_col = "subject_tenant_id",
    resource_col = "run_id",
    no_owner,
    no_type
)]
pub struct Model {
    /// Outcome identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub outcome_id: Uuid,
    /// Assessment run identity shared by every result of one run; returned as `assessmentId`.
    pub run_id: Uuid,
    /// Caller home tenant from the security context.
    pub subject_tenant_id: Uuid,
    /// Opaque caller principal from the security context.
    pub subject_id: String,
    /// Resource tenant of the assessed scope snapshot.
    pub resource_tenant_id: Uuid,
    /// Seller tenant of the assessed scope snapshot.
    pub seller_tenant_id: Uuid,
    /// Payer tenant of the assessed scope snapshot.
    pub payer_tenant_id: Uuid,
    /// Request correlation for diagnostics, if any; not the run key.
    pub correlation_id: Option<Uuid>,
    /// Assessed order; NULL for a Preview run.
    pub order_id: Option<Uuid>,
    /// Order version the run admitted, if any.
    pub version: Option<i32>,
    /// Assessed line; NULL for an order-level predicate.
    pub line_id: Option<Uuid>,
    /// Predicate identity.
    pub predicate: String,
    /// Pricing revision-item identity; NULL for a line- or order-level result.
    pub item_id: Option<Uuid>,
    /// Whether the result carries a catalog selection, including a selected default dimension.
    pub has_catalog_selection: bool,
    /// Raw selected dimension value (NULL for the selected default); NULL without a selection.
    pub catalog_scope_key: Option<String>,
    /// `passed`, `failed` or `unevaluable`.
    pub verdict: String,
    /// Registered reason on failure or unevaluability.
    pub reason: Option<String>,
    /// Orders mapping/coverage registry version used for the run (D-195).
    pub mapping_version: String,
    /// `required`, `tcv-only` or `preview-forecast`.
    pub applicability: String,
    /// Ordered typed producer subresults behind this result; empty for a wholly local result.
    pub producer_results: Json,
    /// Allowlisted local dependency and evidence identifiers, if any.
    pub upstream_detail: Option<Json>,
    /// Run instant.
    pub evaluated_at: TimeDateTimeWithTimeZone,
}
/// `SeaORM` relations of this table.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// Belongs to the assessed order in `bss_orders__order`.
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
