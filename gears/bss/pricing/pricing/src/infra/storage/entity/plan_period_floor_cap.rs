//! `SeaORM` entity for `bss.pricing_plan_period_floor_cap` — the plan-level
//! **period floor and cap** of **one plan revision** in **one market**
//! (`design/02-plan-definition.md` §6, **D-319**), keyed
//! `(plan_id, plan_revision, currency, region)`.
//!
//! The market pair is in the key because the bound is money and money here is
//! denominated by the market: `pricing_plan` has no `currency` and no `region`
//! column, those axes living on the price row's canonical scope key, so a
//! plan-level bound *per market* has to be a row keyed on the market rather
//! than a column on the plan.
//!
//! This is not a price. Nothing in this gear evaluates it: Rating reads it out
//! of the pinned snapshot and emits a `PeriodFloorCapObligation`, and
//! **Billing** applies `max(total, floor)` / `min(total, cap)` to the period's
//! aggregated total after step 9. It is likewise not a quantity floor —
//! `min_qty_purchase` / `min_qty_usage` are on `pricing_price` and mean
//! something else (rating §6.2 forbids the conflation).
//!
//! There is no `lifecycle_state` here. A bound row is frozen when **its**
//! revision publishes, so the parent `pricing_plan` row is the referent and the
//! table's append-only triggers read it — the same arrangement
//! `pricing_plan_phase` has.

use sea_orm::entity::prelude::*;
use uuid::Uuid;

use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_plan_period_floor_cap")]
#[secure(tenant_col = "tenant_id", resource_col = "plan_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub plan_id: Uuid,
    /// The revision this copy belongs to — the row it is frozen with (D-83).
    #[sea_orm(primary_key, auto_increment = false)]
    pub plan_revision: i64,
    /// ISO 4217, the first half of the market the bound is denominated in. It
    /// is also the currency of [`Model::floor_minor`] and [`Model::cap_minor`]:
    /// the amount carries no currency column of its own, because a second
    /// spelling of the denomination is a second thing to disagree.
    #[sea_orm(primary_key, auto_increment = false)]
    pub currency: String,
    /// The second half of the market pair.
    #[sea_orm(primary_key, auto_increment = false)]
    pub region: String,
    /// Copied from the parent revision by the repository, never taken from a
    /// request: the foreign key covers `(plan_id, plan_revision)` alone, so a row
    /// claiming a tenant its parent revision does not belong to is refused by the
    /// append-only trigger's parent-tenant arm rather than by the key.
    pub tenant_id: Uuid,
    /// The period floor in ISO 4217 minor units of [`Model::currency`].
    /// `CHECK`-constrained strictly positive when present — `0` is refused
    /// rather than admitted as a second spelling of absence, the per-line
    /// non-negative guard already making `max(total, 0)` a no-op.
    pub floor_minor: Option<i64>,
    /// The period cap, same denomination and same positivity rule. At least
    /// one of the two is present, and the floor never exceeds the cap; both are
    /// `CHECK`s in the `CREATE TABLE`.
    pub cap_minor: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
