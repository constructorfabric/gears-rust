//! `SeaORM` entity for `bss.pricing_bundle` — a bundle's identity, its price
//! basis and its invoice itemization (`design/08-bundles.md` §6, D-105).
//!
//! Keyed `bundle_id` alone. The composition itself does **not** live here: it
//! lives in the three revision-scoped child tables, which is what lets a draft
//! recomposition edit its own copies (D-92). What is here is what a bundle *is*
//! rather than what it *contains* — plus, for now, two columns that are content
//! and are not revision-scoped; `pricing_bundle`'s migration doc carries that
//! hazard and it is in the owed register.
//!
//! `plan_id` is the join D-92's *"a bundle rides its plan's revisions"* needs,
//! and it carries no database foreign key — `pricing_plan` is keyed
//! `(plan_id, revision)` and has only partial uniqueness on `plan_id`, which
//! Postgres refuses as a referent. The child tables enforce the reference
//! transitively through their own append-only triggers.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_bundle")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "bundle_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub bundle_id: Uuid,
    pub tenant_id: Uuid,
    /// The `bundle`-type plan whose revisions this bundle's composition rides.
    /// Unique across the table: two bundles on one plan would give one revision
    /// sequence two composition chains.
    pub plan_id: Uuid,
    /// `sum_of_parts | own_price` (`inst-bb-declared`), constrained by
    /// `chk_pricing_bundle_price_basis`.
    pub price_basis: String,
    /// `aggregate | itemize` (B3), constrained by
    /// `chk_pricing_bundle_invoice_itemization`. Either layout preserves the
    /// per-SKU rev-share Marketplace accrues from.
    pub invoice_itemization: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
