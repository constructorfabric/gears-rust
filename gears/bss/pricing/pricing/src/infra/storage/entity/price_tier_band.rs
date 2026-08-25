//! `SeaORM` entity for `bss.pricing_price_tier_band` — the **authored** tier
//! bands of a `graduated` / `volume` price row
//! (`design/03-price-structure.md` §6).
//!
//! Authored bands only (D-130): the D-45 allowance compile is a projection into
//! the read model and never writes back here, so what this entity reads is
//! always what the operator wrote.
//!
//! Band quantities are **billable units** — the units that exist after
//! `billingGranularity` quantization (`inst-tb-units`), not raw metered ones.
//! The columns carry no unit of their own; the parent row's granularity is what
//! says what a `1` means.
//!
//! There is no ordinal column. A band's identity is its lower bound
//! (`UNIQUE (price_id, from_qty)`), which is why
//! `domain::rules::tier_bands::BandGeometry` judges a set sorted by `from_qty`
//! rather than in authored order: authored order does not survive this table.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_price_tier_band")]
#[secure(tenant_col = "tenant_id", resource_col = "price_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub band_id: Uuid,
    pub tenant_id: Uuid,
    /// The `graduated` / `volume` row these bands price. A band on any other
    /// kind is refused by the table's structural-exclusivity trigger.
    pub price_id: Uuid,
    /// Inclusive lower bound, in billable units.
    pub from_qty: i64,
    /// Exclusive upper bound, in billable units. `None` is the **open top** —
    /// a state of the band, not an absent value, and the state the top band
    /// always carries (D-17). It is the storage spelling of
    /// [`crate::domain::price_row::BandTop::Open`].
    pub to_qty: Option<i64>,
    /// The band's rate in 10^-9 minor units (D-311) — a multiplier, not an
    /// amount, so it is not bounded by the currency's ISO-4217 scale. `0` is
    /// valid: a free first band is a normal way to author "N included" (Q5).
    pub unit_price_nano: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
