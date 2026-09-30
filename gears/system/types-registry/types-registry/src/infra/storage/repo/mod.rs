//! Repositories over the managed-state schema. One file per repository
//! (`02_gear_layout_and_sdk_pattern.md`); the shared `IN_CHUNK` and the re-exports
//! live here.
//!
//! # These repositories speak the domain's row types, not `SeaORM`'s
//!
//! Every method takes and returns the types in [`crate::domain::ports`], mapping
//! its own `SeaORM` models at the edge — as `credstore` and `mini-chat` do.
//! [`super::store`] therefore holds no mapping, only the `&DbTx` port signatures
//! the domain's dyn-safe traits need. [`EntityPage`] and [`PageRequest`] are port
//! types too since T22a made discovery a port; they are re-exported here so a
//! repository caller names one module.
//!
//! Every method takes `runner: &impl DBRunner`, never `&SecureConn`, so one body
//! serves both a pooled connection and a transaction — which is how the admission
//! worker runs the same read inside and outside its commit transaction.
//!
//! # GTS patterns are compiled, never approximated
//!
//! `gts-rust` parses both the stored identifier and the pattern. Admission stores
//! the parsed segments in `entity_gts_segment`, and [`segment_filter`] compiles a
//! parsed pattern into exact per-segment predicates, so
//! [`EntityRepo::list_page`] decides a page in SQL before `LIMIT`. No `LIKE`, no
//! regex, no Rust post-filter; differential tests pin the compiler to
//! [`gts::GtsId::matches_pattern`] on every backend.
//!
//! Dependency walks use `ToolKit`'s scoped recursive CTE builder, without raw SQL.

pub mod coordination_state_repo;
pub mod dependency_repo;
pub mod entity_repo;
pub mod instance_repo;
pub mod operation_repo;
pub mod segment_filter;
pub mod type_schema_repo;
pub mod version_family_repo;

pub use crate::domain::ports::{EntityPage, PageRequest};
pub use coordination_state_repo::CoordinationStateRepo;
pub use dependency_repo::DependencyRepo;
pub use entity_repo::EntityRepo;
pub use instance_repo::InstanceRepo;
pub use operation_repo::OperationRepo;
pub use type_schema_repo::TypeSchemaRepo;
pub use version_family_repo::VersionFamilyRepo;

/// `ON CONFLICT DO NOTHING`, spelled portably, for the two inserts that race.
///
/// # Why not catch the unique violation and re-read
///
/// A raised unique violation **aborts the transaction** on `PostgreSQL`, so the
/// recovering re-read fails for a second, unrelated reason. Both racing inserts run
/// inside the admission commit transaction ([`crate::domain::admission::unit`]);
/// `SQLite` and `MySQL` tolerate the recovery, which is why a pooled-connection test
/// passes while the production path does not. An insert whose conflict writes
/// nothing *succeeded* on every backend, and the caller decides what the absence
/// means.
///
/// # Why the column argument
///
/// Untargeted on `PostgreSQL` and `SQLite` — plain `ON CONFLICT DO NOTHING` covers
/// every unique key on the table. `MySQL` has no `DO NOTHING`; `sea-query` polyfills
/// it with an `ON DUPLICATE KEY UPDATE` that assigns a column to itself, which needs
/// a column whose self-assignment changes nothing: the primary key.
fn conflict_do_nothing<C>(pk: C) -> sea_orm::sea_query::OnConflict
where
    C: sea_orm::sea_query::IntoIden,
{
    sea_orm::sea_query::OnConflict::new()
        .do_nothing_on([pk])
        .to_owned()
}

/// Chunk size for `IN (…)` lists.
///
/// `SQLite`'s default `SQLITE_MAX_VARIABLE_NUMBER` is 999 on older builds, and the
/// statement carries the scope predicate's parameters too. 200 is inside every
/// backend's limit and large enough that a realistic closure needs a handful of
/// round trips, not hundreds.
const IN_CHUNK: usize = 200;
