//! `types_registry__dependency` — the direct dependency relation:
//! `from_entity_id` depends on `to_entity_id`.
//!
//! Mirror of the table in `docs/database.sql`. **Nothing transitive is stored.**
//! Transitive reachability is answered by walking these rows, and deletion safety
//! reads the direct rows and only those — a transitive-only dependent must not
//! block, because it would disappear the moment the intermediate entity did.
//!
//! Derivation and Instance conformance are materialized even though both are
//! derivable from the identifier, because mixing prefix derivation with stored
//! edges in one traversal would need either a second recursive branch or an
//! index-defeating `OR`. These edges are written once, from immutable
//! identifiers.
//!
//! Admission replaces only the admitted entity's **outgoing** rows.
//!
//! A materialized transitive closure was rejected: drift could silently skip
//! revalidation (ADR-0011). It may later be added only as a cache over these
//! rows.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

use super::enums::DependencyKind;

// ponytail: ceiling C6 — no PDP, as on `entity`. An edge carries no owner of its
// own; both endpoints are entities, and their ownership is reached through
// `from_entity_id` / `to_entity_id`. The P1 upgrade is a scoped read of the
// endpoints, which belongs with the `PolicyEnforcer` work (SPEC §9 C6, §12).
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "types_registry__dependency")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub from_entity_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub kind: DependencyKind,
    #[sea_orm(primary_key, auto_increment = false)]
    pub to_entity_id: i64,
}

/// No relations declared — see the note on [`super::version_family`].
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
