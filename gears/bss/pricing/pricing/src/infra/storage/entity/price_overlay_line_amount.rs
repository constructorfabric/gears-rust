//! `SeaORM` entity for `bss.pricing_price_overlay_line_amount` — one line's value
//! in one currency (`design/09-price-overlays.md` §6, D-08).
//!
//! An amount-based magnitude is money and exists **only per currency**: the
//! catalog performs no FX, so a line whose magnitude is absolute carries one
//! value per currency its resolved target scope sells, and a missing one fails
//! save and publish (`ADJUSTMENT_CURRENCY_NOT_COVERED`, naming the line). A
//! percent line is currency-neutral and has no rows here at all.
//!
//! The key is `(tenant_id, overlay_revision, line_id, currency)`. §6 spells it
//! `UNIQUE (line_id, currency)`, which stops being a key once the line's own key
//! carries the revision — see `pricing_price_overlay_line_amount`'s migration doc. It is the
//! **primary key** rather than a surrogate plus a unique index, because the tuple
//! *is* the row's identity.
//!
//! `tenant_id` joined it under `pricing_price_overlay_line`, in the same statement list that
//! put it in the line's key: once two tenants may hold one
//! `(line_id, overlay_revision)`, a narrow key here collides on their amounts
//! instead — which is exactly the condition review A1-4 records as the one that
//! would arm this table's untyped insert catch-all.
//!
//! There is no `lifecycle_state` here. A value is frozen when the revision its
//! **line** belongs to publishes, so the reference is the overlay revision and
//! the table's append-only trigger resolves it through
//! [`super::price_overlay_line`] — [`super::bundle_revshare`]'s arrangement one
//! slice over.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_price_overlay_line_amount")]
#[secure(tenant_col = "tenant_id", resource_col = "line_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub line_id: Uuid,
    /// The revision this value rides — §6's *"the amount table rides the same
    /// revision through its line"*, which is only expressible once the line's
    /// own key carries the revision.
    #[sea_orm(primary_key, auto_increment = false)]
    pub overlay_revision: i64,
    /// ISO 4217.
    #[sea_orm(primary_key, auto_increment = false)]
    pub currency: String,
    /// Copied from the parent line by the repository, never taken from a request.
    ///
    /// **In the key, and in the foreign key, since `pricing_price_overlay_line_amount`**: the
    /// reference is now `(tenant_id, overlay_revision, line_id)`, so a child
    /// carrying a tenant its line does not have is refused by the schema rather
    /// than merely never written. The table's append-only trigger gained the same
    /// conjunct in that migration, for a sharper reason — see its module doc.
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    /// The magnitude, in the currency's ISO 4217 minor unit. `>= 0` (D-67), and
    /// **zero is admitted**: a `fixed 0` line is how a market is priced at
    /// nothing, which is a real authoring act — unlike a `markup` of 0 bp, which
    /// adjusts nothing and is refused one table up.
    pub value_minor: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
