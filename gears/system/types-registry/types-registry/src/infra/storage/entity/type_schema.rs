//! `types_registry__type_schema` — current Type Schema state: the
//! current-revision pointer plus the resolved artifacts.
//!
//! Mirror of the table in `docs/database.sql`. Authored content, hash and checker
//! versions stay in the referenced immutable [`super::type_schema_revision`].
//!
//! The resolved artifacts depend on current floating dependencies and may be
//! recomputed **without** creating an authored revision: they are current facts,
//! not history. Per-level content-model classification is derived from
//! `resolved_schema` and used only off the hot path, so it is not stored.
//!
//! `resolution_fingerprint` digests the canonical bytes of all three artifacts
//! and is rewritten on recompute. Its input MUST be canonical and independent of
//! the serializer's map iteration order, or the value flaps while the artifacts
//! stand still. It supports **equality only, never ordering**. A digest, unlike a
//! counter, stays stable when recomputation yields identical artifacts — which is
//! how a dependency-driven read change is detected without moving
//! `entity.resource_version`, reserved for optimistic writes.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

// ponytail: ceiling C6 — no PDP, as on `entity`. This table has no owner column:
// ownership is the parent entity's, reached through `entity_id`, so `unrestricted`
// is the only honest marker today. The P1 upgrade — copy the owner onto this row or
// scope-read the parent — belongs with the `PolicyEnforcer` work (SPEC §9 C6, §12).
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "types_registry__type_schema")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub entity_id: i64,
    /// Current-revision pointer. Together with `entity_id` it is the composite
    /// foreign key onto the immutable revision.
    pub revision_no: i32,
    pub resolved_schema: String,
    pub effective_traits: String,
    pub effective_traits_schema: String,
    pub resolution_fingerprint: Vec<u8>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// No relations declared — see the note on [`super::version_family`].
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
