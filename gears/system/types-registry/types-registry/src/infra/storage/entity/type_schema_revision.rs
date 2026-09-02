//! `types_registry__type_schema_revision` — the immutable Type Schema admission
//! snapshot: authored document, hash and engine provenance (ADR-0005).
//!
//! Mirror of the table in `docs/database.sql`.
//!
//! Neither the effective artifacts nor the dependency revision vector are kept.
//! Nothing reads the admission-time resolution: compatibility compares a
//! candidate against its baseline, and no P0 operation looks backwards. The
//! vector exists only for the duration of one validation attempt, as the
//! concurrency control the commit re-checks; a redelivered outbox message
//! revalidates from scratch.
//!
//! `gts_spec_version` / `gts_impl_version` identify the admission engine for
//! **every** revision, including those with no compatibility comparison at all —
//! a first admission, an `M.0` opening a minor-bearing major, and a candidate
//! whose own last segment carries major 0. Where a comparison did happen they
//! identify the rules that produced the verdict, which is exactly what a checker
//! upgrade can change for an unchanged pair of schemas. This provenance cannot be
//! reconstructed later (ADR-0003).
//!
//! The natural `(entity_id, revision_no)` key is also the fact every dependent
//! row needs, and it clusters one entity's history; a surrogate would add a
//! lookup without replacing those two values.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

// ponytail: ceiling C6 — no PDP, as on `entity`. This table has no owner column:
// ownership is the parent entity's, reached through `entity_id`, so `unrestricted`
// is the only honest marker today. The P1 upgrade — copy the owner onto this row or
// scope-read the parent — belongs with the `PolicyEnforcer` work (SPEC §9 C6, §12).
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "types_registry__type_schema_revision")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub entity_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub revision_no: i32,
    /// The authored document as submitted, canonical UTF-8 text.
    pub raw_schema: String,
    pub content_hash: Vec<u8>,
    pub gts_spec_version: String,
    pub gts_impl_version: String,
    /// True when ADR-0004 `force` waived ADR-0003 cross-minor compatibility.
    /// Always false for major-only entities and the first minor of a major; a
    /// safe upgrade across several minors requires every traversed value to be
    /// false.
    pub compat_forced: bool,
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
