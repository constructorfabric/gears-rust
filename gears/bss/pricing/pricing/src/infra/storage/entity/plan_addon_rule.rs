//! `SeaORM` entity for `bss.pricing_plan_addon_rule` — one add-on composition
//! rule of **one plan revision** (`design/02-plan-definition.md` §6, D-105),
//! keyed `(plan_id, plan_revision, addon_sku_id)`.
//!
//! The third key column is D-105 and it is load-bearing: without it a revision
//! holds **one** add-on rule, and the `depends_on` cycle walk, the symmetric
//! conflict normalization and "two required conflicting add-ons fail publish"
//! are all rules over data the key cannot represent. `plan_revision` is the
//! copy-on-new-revision half (D-83) — a new revision copies these rows under its
//! own number and the open draft edits its own copies.
//!
//! There is no `lifecycle_state` here. An add-on rule row is frozen when **its**
//! revision publishes, so the parent `pricing_plan` row is the referent and the
//! table's append-only triggers read it — the same arrangement
//! `pricing_plan_phase` and `pricing_price_tier_band` have with their parents.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_plan_addon_rule")]
#[secure(tenant_col = "tenant_id", resource_col = "plan_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub plan_id: Uuid,
    /// The revision this copy belongs to — the row it is frozen with.
    #[sea_orm(primary_key, auto_increment = false)]
    pub plan_revision: i64,
    /// The add-on SKU this rule is about, and the discriminator that lets one
    /// revision hold several rules (D-105).
    #[sea_orm(primary_key, auto_increment = false)]
    pub addon_sku_id: Uuid,
    /// Copied from the parent revision by the repository, never taken from a
    /// request: the foreign key covers `(plan_id, plan_revision)` alone, so a row
    /// claiming a tenant its parent revision does not belong to is refused by the
    /// append-only trigger's parent-tenant arm rather than by the key.
    pub tenant_id: Uuid,
    /// Whether the add-on must be taken. `chk_..._required_max_qty` is what
    /// keeps a required add-on's [`Model::max_qty`] admitting a selection.
    pub required: bool,
    /// Selection-time lower bound.
    pub min_qty: Option<i32>,
    /// Selection-time upper bound. `>= 1` where [`Model::required`], per §6.
    pub max_qty: Option<i32>,
    /// Selection-time quantity step.
    pub step_qty: Option<i32>,
    /// An optional alternative price for this add-on when taken with this plan.
    /// It resolves to a published `priceId` on a plan of the **add-on SKU
    /// itself** (`inst-cmp-override-home`, D-97/D-116) — a row this schema does
    /// not hold, which is why there is no foreign key on it.
    pub price_override_ref: Option<Uuid>,
    /// The plan-authored `depends_on` edges (D-16), as a JSON array of uuid
    /// strings: `jsonb` on Postgres, `text` on `SQLite`. §6 writes `uuid[]`,
    /// which `SQLite` has no type for; see the migration's module doc.
    ///
    /// **Directed.** The `ADDON_CYCLE` walk runs over it, and symmetrizing it
    /// would make every dependency its own two-cycle.
    pub depends_on_addon_sku_id: Json,
    /// The plan-authored `conflicts_with` edges (D-16), same encoding.
    ///
    /// **Stored normalized symmetric**, by the repository: a conflict authored
    /// on one side is a conflict on both, so no validator can reach two verdicts
    /// on one plan depending on which row it started from.
    pub conflicts_with_addon_sku_id: Json,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
