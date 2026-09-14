//! `types_registry__instance_revision` — the immutable Registered Instance
//! admission snapshot: authored value, hash, the schema revision that validated
//! it, and engine provenance (ADR-0005).
//!
//! Mirror of the table in `docs/database.sql`.
//!
//! **The schema-revision pair is the point of this table.** An Instance is valid
//! against one exact schema *revision*, not against its identifier — the schema's
//! current revision can move under it, and an admission recording only the
//! identifier could not say afterwards which rules it passed. `ON DELETE RESTRICT`
//! keeps that revision alive as long as the Instance.
//!
//! No `compat_forced` counterpart to [`super::type_schema_revision`]: an Instance is
//! either valid or refused, so `force` has nothing to waive. The engine versions are
//! recorded for the reason they are there — a checker upgrade can change the verdict
//! for an unchanged pair.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

// ponytail: ceiling C6 — no PDP, as on `entity`. This table has no owner column:
// ownership is the parent entity's, reached through `entity_id`, so `unrestricted`
// is the only honest marker today. The P1 upgrade — copy the owner onto this row or
// scope-read the parent — belongs with the `PolicyEnforcer` work (SPEC §9 C6, §12).
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "types_registry__instance_revision")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub entity_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub revision_no: i32,
    /// The authored value as submitted, canonical UTF-8 text.
    pub canonical_value: String,
    pub content_hash: Vec<u8>,
    /// Entity half of the exact Type Schema revision this value was validated
    /// against.
    pub type_schema_entity_id: i64,
    /// Revision half of the same pair. Pinned by `ON DELETE RESTRICT`.
    pub type_schema_revision_no: i32,
    pub gts_spec_version: String,
    pub gts_impl_version: String,
    /// Reaches the operation and the admitting principal. `ON DELETE RESTRICT`
    /// pins that provenance until the revision is purged.
    pub operation_item_id: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// No relations declared — see the note on [`super::version_family`].
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
