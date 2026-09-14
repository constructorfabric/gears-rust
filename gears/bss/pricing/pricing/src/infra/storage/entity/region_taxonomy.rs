//! `SeaORM` entity for `bss.pricing_region_taxonomy` — the region value universe
//! (`design/04-currency-tax.md` §6, D-01).
//!
//! **Slice 4's table**, carried on the Slice 9 chain because `inst-plv-scope`
//! validates against it and it did not exist; `pricing_region_taxonomy`'s migration doc
//! carries the whole argument.
//!
//! The one of the four that carries the `tax_*` pair, and §6 is explicit that it
//! is the only one: a tax category rides the price row's `tax_category_ref`
//! (D-110) or this table's default, and a third place for one to live is the
//! cardinality error D-110 removed from `pricing_plan_descriptor_set`.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_region_taxonomy")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "tenant_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    /// The declared region code. Never blank — the empty string is
    /// [`super::price_overlay`]'s sentinel for the classless scope.
    #[sea_orm(primary_key, auto_increment = false)]
    pub value: String,
    /// The operator's label for the region.
    pub display_name: String,
    /// `active` | `retired`. Retirement is guarded while the value is
    /// referenced, and `retired -> active` re-activation is legal and audited.
    pub state: String,
    /// The region's default tax category (D-01). `None` is an undeclared
    /// default, not an empty one: a row's own `tax_category_ref` is the source
    /// of truth and this is the fallback D-154 resolves at publish.
    pub tax_category: Option<String>,
    /// The tenant-declared *"a tax rate is configured for this region"* — the
    /// MVP `RegionTaxReadiness` source, reconciled against the Tax Engine
    /// post-GA. Defaults to **false**, which is the fail-closed reading: a
    /// region nobody has declared a rate for is a region with no rate, not one
    /// with an unknown rate.
    pub tax_rate_present: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
