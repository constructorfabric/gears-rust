//! `SeaORM` entity for `bss.pricing_brand_taxonomy` — the brand value universe (`inst-plv-scope` sends the `brand` scope class here)
//! (`design/04-currency-tax.md` §6).
//!
//! **Slice 4's table**, carried on the Slice 9 chain because `inst-plv-scope`
//! validates against it and it did not exist; `pricing_brand_taxonomy`'s sibling
//! migrations carry the whole argument. It is the region taxonomy's shape minus
//! the two `tax_*` columns D-01 puts on that one alone.
//!
//! `resource_col` is the tenant, as on [`super::policy_object`]: a taxonomy is
//! per-tenant configuration and has no row-level resource id for a single-row
//! gate to pin.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_brand_taxonomy")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "tenant_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    /// The declared value. Never blank — the empty string is
    /// [`super::price_overlay`]'s sentinel for the classless scope, so a blank
    /// value here would make that sentinel forgeable.
    #[sea_orm(primary_key, auto_increment = false)]
    pub value: String,
    /// The operator's label for the value.
    pub display_name: String,
    /// `active` | `retired`. Retirement is guarded while the value is
    /// referenced, and `retired -> active` re-activation is legal and audited.
    pub state: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
