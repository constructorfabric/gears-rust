//! `types_registry__instance` — current Registered Instance state: the
//! current-revision pointer, and nothing else.
//!
//! Mirror of the table in `docs/database.sql`.
//!
//! **No derived artifact here, and the asymmetry with [`super::type_schema`] is the
//! design.** A schema's current row materializes resolution because it depends on
//! *other* entities: a base revision moves and the resolution changes without the
//! schema being reauthored. An Instance has no such state — its value is authored
//! and its schema revision is immutable and pinned by `ON DELETE RESTRICT`, so there
//! is nothing that could change without a new revision, and nothing to fingerprint.
//! Materializing anything would store the authored value twice.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

// ponytail: ceiling C6 — no PDP, as on `entity`. This table has no owner column:
// ownership is the parent entity's, reached through `entity_id`, so `unrestricted`
// is the only honest marker today. The P1 upgrade — copy the owner onto this row or
// scope-read the parent — belongs with the `PolicyEnforcer` work (SPEC §9 C6, §12).
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "types_registry__instance")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub entity_id: i64,
    /// Current-revision pointer. Together with `entity_id` it is the composite
    /// foreign key onto the immutable revision.
    pub revision_no: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// No relations declared — see the note on [`super::version_family`].
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
