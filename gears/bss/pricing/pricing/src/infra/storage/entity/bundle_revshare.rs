//! `SeaORM` entity for `bss.pricing_bundle_revshare` — one rev-share **party**
//! row within one group of one bundle revision (`design/08-bundles.md` §6,
//! D-07 + D-92 + D-105), keyed
//! `(bundle_id, plan_revision, vendor_sku_id, party)`.
//!
//! Two share columns, and the difference between them is the whole of D-07:
//! [`Model::share_bp`] is what the operator typed, [`Model::effective_share_bp`]
//! is what publish normalized. The typed values are retained for audit, and
//! downstream consumers read only the effective ones.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_bundle_revshare")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "bundle_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub bundle_id: Uuid,
    /// The revision this copy belongs to (D-92).
    #[sea_orm(primary_key, auto_increment = false)]
    pub plan_revision: i64,
    /// The group this party's share is a share **of** — a foreign key onto
    /// `pricing_bundle_revshare_group`, which is what makes a share authored
    /// against no explicit platform cut unrepresentable.
    #[sea_orm(primary_key, auto_increment = false)]
    pub vendor_sku_id: Uuid,
    /// The party taking the share. `text` for the reason
    /// `residual_absorber_party` is: the two are compared to each other and one
    /// of them must also hold the `platform` sentinel.
    #[sea_orm(primary_key, auto_increment = false)]
    pub party: String,
    /// Copied from the parent bundle by the repository, never from a request.
    pub tenant_id: Uuid,
    /// The **typed** share in basis points — what the operator authored, kept
    /// for audit after normalization (D-07).
    pub share_bp: i32,
    /// The **published** share in basis points, absorber-adjusted at publish so
    /// the group sums to exactly 10000 bp.
    ///
    /// `None` until publish normalizes it. Defaulting it to the typed value
    /// would make "not yet reconciled" indistinguishable from "reconciled to
    /// exactly what was typed", which is the common case for every party that is
    /// not the absorber.
    pub effective_share_bp: Option<i32>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
