//! `policy_engine__bundle_version` - one version of a bundle's content.
//!
//! Rows are immutable once `state` leaves draft, except for the transition to
//! superseded. At most one version per bundle is active and at most one is a
//! draft, enforced by the partial unique indexes `..__one_active` (`state = 2`)
//! and `..__one_draft` (`state = 1`).

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

/// `state` code: editable draft (at most one per bundle).
pub const STATE_DRAFT: i16 = 1;
/// `state` code: the bundle's active version (at most one per bundle).
pub const STATE_ACTIVE: i16 = 2;
/// `state` code: formerly active, replaced by a later activation.
pub const STATE_SUPERSEDED: i16 = 3;

/// Scoped by the owning tenant; the version identity is the resource.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "policy_engine__bundle_version")]
#[secure(tenant_col = "owner_tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    /// Version identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// Owning bundle.
    pub bundle_id: Uuid,
    /// Owning tenant; the scope column.
    pub owner_tenant_id: Uuid,
    /// Monotonic version number within the bundle.
    pub ordinal: i32,
    /// Draft, active or superseded ([`STATE_DRAFT`], [`STATE_ACTIVE`], [`STATE_SUPERSEDED`]).
    pub state: i16,
    /// Creation time.
    pub created_at: OffsetDateTime,
    /// Activation time, null while draft.
    pub activated_at: Option<OffsetDateTime>,
    /// Activating identity, null while draft.
    pub activated_by: Option<Uuid>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// The owning bundle.
    #[sea_orm(
        belongs_to = "super::bundle::Entity",
        from = "Column::BundleId",
        to = "super::bundle::Column::Id"
    )]
    Bundle,
    /// The version's documents; deleted with the version.
    #[sea_orm(has_many = "super::document::Entity")]
    Document,
}

impl Related<super::bundle::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Bundle.def()
    }
}

impl Related<super::document::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Document.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
