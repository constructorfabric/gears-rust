//! `SeaORM` entity for `bss.pricing_composite_meter` — one derived-meter
//! definition of **one plan revision** (`design/10-advanced-primitives.md` §6),
//! keyed `(tenant_id, plan_id, plan_revision, composite_id)`.
//!
//! [`plan_phase`](super::plan_phase)'s arrangement exactly, and for D-106's
//! reason rather than by imitation: `plan_revision` says *which copy* of the
//! definition this row is, and `composite_id` is the composite itself, stable
//! for the life of the plan. Opening a draft revision copies the rows under its
//! own number without re-minting an id, so a formula edit on the draft leaves the
//! published revision's rows byte-identical.
//!
//! **`tenant_id` and `plan_id` are part of the key** (D-340). Without them it says
//! that one composite id belongs to one plan per revision *number* across the whole
//! table, every tenant's included — and `composite_id` is client-supplied, so any
//! `plan × write` holder meets the key simply by naming an id. The resemblance to
//! `plan_phase` above is to that table's key **as widened**; taking it as a licence
//! to narrow this one is how the twin gets left behind. What the widening leaves
//! standing is the half the composite rules rely on: one revision may not hold the
//! same composite id twice.
//!
//! The four attributes are in field order rather than in the physical key's order,
//! and that is not a divergence anything can observe: `SeaORM` names columns in
//! every statement it builds, and the one construct where key order is a signature
//! — `Entity::find_by_id`'s tuple — has no call site in this gear.
//!
//! There is no `lifecycle_state` here for the same reason there is none on a
//! phase row: the definition is frozen when **its** revision publishes, so the
//! parent `pricing_plan` row is the referent and the table's append-only
//! triggers read it.

use sea_orm::entity::prelude::*;
use uuid::Uuid;

use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_composite_meter")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "composite_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub composite_id: Uuid,
    /// The revision this copy belongs to — and the row it is frozen with.
    #[sea_orm(primary_key, auto_increment = false)]
    pub plan_revision: i64,
    /// Copied from the parent revision by the repository, never taken from a
    /// request: the foreign key covers `(plan_id, plan_revision)` alone, so a row
    /// claiming a tenant its parent revision does not belong to is refused by the
    /// append-only trigger's parent-tenant arm rather than by the key.
    ///
    /// In the key since `pricing_composite_meter`, which is what makes a composite id
    /// private to its tenant rather than a name in a deployment-wide namespace.
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    /// In the key since `pricing_composite_meter`: a composite id belongs to a **plan**,
    /// so two plans of one tenant may hold the same one — which D-19's clone remap
    /// and D-83's copy-forward both need, since both supply an id the server did
    /// not mint.
    #[sea_orm(primary_key, auto_increment = false)]
    pub plan_id: Uuid,
    /// The registry-declared `meteringUnit` this composite rates as
    /// (`inst-cm-output`). Declaring it is the registry's act and not this
    /// gear's (D-32) — what is persisted here is the id and the binding.
    pub output_unit: String,
    /// The constituent `meteringUnit` ids, as a JSON array of strings.
    ///
    /// `≥ 2` is a **publish rule** and not a column constraint
    /// (`COMPOSITE_TOO_FEW_CONSTITUENTS`); whether each is *published* is not
    /// checked at all, because this gear has no registry client — see the
    /// migration's module doc.
    pub constituent_units: Json,
    /// The formula **as data** (A4): operands plus operator/weights, a
    /// declarative schema and never executable code. The catalog persists and
    /// freezes it; Rating evaluates it (`inst-cm-frozen`).
    pub formula: Json,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
