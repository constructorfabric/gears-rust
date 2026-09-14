//! `SeaORM` entity for `bss.pricing_customer_group_taxonomy` — the BSS
//! customer-group value universe (`design/09-price-overlays.md` §3
//! `inst-cg-taxonomy`, §6).
//!
//! The four Slice 4 taxonomies' shape (`org_tier_taxonomy`'s sibling, minus the
//! two `tax_*` columns `region_taxonomy` alone carries), on its **own** table and
//! its own route: `05-governance.md`'s endpoint map is explicit that this
//! taxonomy is not filed under `config` with its four siblings, because
//! per-payer commercial data is more sensitive than plan/config authoring. See
//! `pricing_customer_group_taxonomy`'s migration doc for the argument in full.
//!
//! `resource_col` is the tenant, as on [`super::org_tier_taxonomy`]: a taxonomy
//! is per-tenant configuration and has no row-level resource id for a
//! single-row gate to pin.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_customer_group_taxonomy")]
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
