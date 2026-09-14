//! `SeaORM` entity for `bss.pricing_bundle_component` — one component of one
//! bundle revision (`design/08-bundles.md` §6, D-92 + D-105), keyed
//! `(bundle_id, plan_revision, component_plan_id)`.
//!
//! The third key column is D-105 and it is load-bearing: without it a revision
//! holds **one** component, and `inst-bc-coverage`'s *"every referenced
//! component"*, `inst-bc-frequency`'s cross-component comparison and
//! `COMPONENT_IS_BUNDLE` are all rules over data the key cannot represent.
//! `plan_revision` is the copy-on-new-revision half (D-92).
//!
//! There is no `lifecycle_state` here. A component row is frozen when **its**
//! revision publishes, so the owning `pricing_plan` row is the referent and the
//! table's append-only triggers resolve it through `pricing_bundle` — the same
//! arrangement `pricing_plan_addon_rule` has with its parent, one join longer.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_bundle_component")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "bundle_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub bundle_id: Uuid,
    /// The revision this copy belongs to — the row it is frozen with.
    #[sea_orm(primary_key, auto_increment = false)]
    pub plan_revision: i64,
    /// The component's **plan** (B1: bare SKU ids are ambiguous per
    /// `(currency, region)`), and the discriminator that lets one revision hold
    /// several components (D-105).
    #[sea_orm(primary_key, auto_increment = false)]
    pub component_plan_id: Uuid,
    /// Copied from the parent bundle by the repository, never taken from a
    /// request: the foreign key covers `bundle_id` alone, so nothing in the
    /// schema stops a child carrying a foreign tenant.
    pub tenant_id: Uuid,
    /// The registry SKU this component is published under — the
    /// `includedSkuIds` half of the composition. It points outside this gear,
    /// which is why it carries no foreign key.
    pub included_sku_id: Uuid,
    /// Selection-time lower bound, §6's "constraints (min/max qty)".
    pub min_qty: Option<i32>,
    /// Selection-time upper bound.
    pub max_qty: Option<i32>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
