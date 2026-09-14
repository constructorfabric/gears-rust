//! `SeaORM` entity for `bss.pricing_bundle_revshare_group` — one rev-share group
//! per included vendor SKU within one bundle revision (`design/08-bundles.md`
//! §6, D-07 + D-55 + D-92 + D-105), keyed
//! `(bundle_id, plan_revision, vendor_sku_id)`.
//!
//! The group exists because D-07's tolerance and exact-sum rule is **per
//! `(bundle, vendor SKU)`**: *"sum to 100% per included vendor SKU"*. Before
//! D-55's correction the platform cut was a per-party column used once per group
//! (nothing stopped two parties disagreeing about one group's cut) and the
//! absorber was a bundle-level column typed as a `vendor_sku_id`, which names a
//! group and not a resolvable party.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_bundle_revshare_group")]
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
    /// The included vendor SKU this group reconciles, and the discriminator
    /// D-105 restored: one group per vendor SKU within a revision.
    #[sea_orm(primary_key, auto_increment = false)]
    pub vendor_sku_id: Uuid,
    /// Copied from the parent bundle by the repository, never from a request.
    pub tenant_id: Uuid,
    /// This group's explicit platform cut, in basis points
    /// (`inst-rs-sum`: *"with an explicit per-group platform cut"*).
    pub platform_cut_bp: i32,
    /// The party of this group that absorbs the publish-time residual, or
    /// [`PLATFORM_SENTINEL`](crate::domain::bundle::PLATFORM_SENTINEL) (the
    /// default, D-07). `text` rather than a `Uuid` because one column holds both
    /// inhabitants and the reconciler compares it against
    /// `pricing_bundle_revshare.party`.
    ///
    /// **The sentinel's one declaration is the domain's**, and this entity used
    /// to carry a second under the name `PLATFORM_ABSORBER` — same value, no
    /// reader, declared in the storage layer where the token is a *column
    /// default* rather than a domain fact. `Party::new` refuses it and
    /// `Party::as_str` renders it, so a copy here could only ever have gone stale
    /// against the enum that owns it.
    pub residual_absorber_party: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
