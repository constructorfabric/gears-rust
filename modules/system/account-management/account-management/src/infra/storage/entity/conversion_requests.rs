//! `SeaORM` entity for the AM-owned `conversion_requests` table.
//!
//! Mirrors the schema declared by `m0004_create_conversion_requests`
//! column-for-column. The state-machine encodings (`status`,
//! `initiator_side`, `target_mode`) are stored as `SMALLINT` at the DB
//! layer and surfaced through the domain layer via
//! [`crate::domain::conversion::model::ConversionStatus`] /
//! [`crate::domain::conversion::model::ConversionSide`] /
//! [`crate::domain::conversion::model::TargetMode`].
//!
//! `Scopable(tenant_col = "tenant_id", resource_col = "id", no_owner,
//! no_type)` — the entity declares BOTH the row's owner tenant (via
//! `tenant_id`) AND the row's own primary key (via `id`) as resolvable
//! secured properties, so a compiled
//! [`InTenantSubtree`](modkit_security::ScopeFilter::in_tenant_subtree)
//! predicate has a property to clamp against on either column. A scope
//! of shape `InTenantSubtree(OWNER_TENANT_ID, root, respect_barriers
//! = b)` materialises as
//! `tenant_id IN (SELECT descendant_id FROM tenant_closure
//!   WHERE ancestor_id = :root [AND barrier = 0 if b])`;
//! `InTenantSubtree(RESOURCE_ID, root, …)` clamps `conversion_requests.id`
//! the same way (forward-compat for the future REST PR that will let the
//! PDP emit identity-based subtree clamps on this entity).
//!
//! **Service-side posture:**
//! [`crate::domain::conversion::service::ConversionService::cancel`] /
//! `reject` / `approve` / `list_inbound_for_parent` build a side-specific
//! [`modkit_security::AccessScope`] before calling the repo:
//!
//! * Child-side caller: `for_tenant(child_id)` — clamps
//!   `tenant_id = child_id` so the URL-bound child cannot see a request
//!   on any other tenant.
//! * Parent-side caller: `InTenantSubtree(OWNER_TENANT_ID, parent_id,
//!   respect_barriers = false)` — clamps `tenant_id IN closure(parent_id)`
//!   with barrier penetration, so a parent acting as counterparty on a
//!   self-managed child whose barrier is `1` still sees the row (the
//!   request authorisation is on `parent`, the converting child is
//!   inside the parent's subtree by hierarchy even when invisible to a
//!   barrier-respecting scope).
//!
//! INSERT paths stay `scope_unchecked` — the Scopable INSERT-time clamp
//! is not the right model for inserts (the row is being created and
//! cannot yet be filtered against the caller's scope).

use modkit_db_macros::Scopable;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

// @cpt-begin:cpt-cf-account-management-dbtable-conversion-requests:p1:inst-dbtable-conversion-requests-entity
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Scopable)]
#[sea_orm(table_name = "conversion_requests")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    #[sea_orm(nullable)]
    pub parent_id: Option<Uuid>,
    pub child_tenant_name: String,
    /// `0=child, 1=parent` — encodes which side of the dual-consent
    /// pair initiated this request. Matches the
    /// `CHECK (initiator_side IN (0, 1))` constraint.
    pub initiator_side: i16,
    /// `0=managed, 1=self_managed` — the mode the tenant will move to
    /// if the request is approved. Matches the
    /// `CHECK (target_mode IN (0, 1))` constraint.
    pub target_mode: i16,
    /// `0=pending, 1=approved, 2=cancelled, 3=rejected, 4=expired` —
    /// matches the `CHECK (status IN (0, 1, 2, 3, 4))` constraint and
    /// the encoding pinned by
    /// [`crate::domain::conversion::model::ConversionStatus::as_smallint`].
    pub status: i16,
    pub requested_by: Uuid,
    #[sea_orm(nullable)]
    pub approved_by: Option<Uuid>,
    #[sea_orm(nullable)]
    pub cancelled_by: Option<Uuid>,
    #[sea_orm(nullable)]
    pub rejected_by: Option<Uuid>,
    pub requested_at: OffsetDateTime,
    #[sea_orm(nullable)]
    pub resolved_at: Option<OffsetDateTime>,
    pub expires_at: OffsetDateTime,
    #[sea_orm(nullable)]
    pub deleted_at: Option<OffsetDateTime>,
}
// @cpt-end:cpt-cf-account-management-dbtable-conversion-requests:p1:inst-dbtable-conversion-requests-entity

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
