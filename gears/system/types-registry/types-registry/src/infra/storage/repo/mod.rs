//! Repositories over the managed-state schema.
//!
//! One file per repository (`02_gear_layout_and_sdk_pattern.md`). The shared pieces
//! live here: the `IN_CHUNK` batch size, and the re-exports that keep
//! `storage::repo::EntityRepo` the import path callers already use.
//!
//! # These repositories speak the domain's row types, not `SeaORM`'s
//!
//! Every method takes and returns the types in [`crate::domain::ports`], and each
//! file maps its own `SeaORM` models at the edge. The alternative — repositories
//! returning `Model` and an adapter translating — was what this gear did until the
//! duplication showed the seam was one layer too high: four of seven input types
//! existed twice with identical fields, and the adapter had to rebuild an
//! `operation::Model` from a domain row to satisfy `insert_items`. `credstore` and
//! `mini-chat` both put the mapping in the repository for the same reason.
//!
//! [`super::store`] therefore holds no mapping at all — only the `&DbTx` port
//! signatures the domain's dyn-safe traits need. What still lives here and not
//! there is [`EntityPage`] and [`PageRequest`]: paging is not a port yet.
//!
//! Every method takes `runner: &impl DBRunner` rather than a connection, so one
//! body serves both a pooled connection and a transaction — which is what lets
//! the admission worker run a read outside its commit transaction and the same
//! read inside it. No method takes `&SecureConn`.
//!
//! Two rules shape the read primitives here, and both are load-bearing rather
//! than stylistic.
//!
//! # GTS matching is never translated into SQL
//!
//! `constraint-gts-implementation` makes `gts-rust` the sole source of GTS
//! semantics: a missing behaviour is *"a change request against `gts-rust`, not a
//! local approximation."* Compiling the pattern grammar into `LIKE` or a regex
//! would be exactly such an approximation, and it would drift the moment the
//! grammar gained a rule.
//!
//! So SQL only ever **narrows**: [`EntityRepo::list_page`] applies a prefix range
//! over `gts_id`, which is exact on all three backends because the column carries
//! binary collation, and then [`gts::GtsId::matches_pattern`] decides. The range
//! is deliberately wider than the pattern — see `prefilter_prefix` in
//! [`entity_repo`] — because a range that were too tight would silently *drop*
//! real matches, which is a far worse failure than loading a few rows the pattern
//! then rejects.
//!
//! # The dependency walk is iterative, not a recursive CTE
//!
//! `11_database_patterns.md` forbids raw SQL outside migration definitions, and a
//! recursive CTE cannot be expressed through `SeaORM`'s typed builder. The
//! worklist in [`DependencyRepo::closure`] is therefore the only shape available
//! here, not merely the cheaper one (SPEC D5).

pub mod dependency_repo;
pub mod entity_repo;
pub mod instance_repo;
pub mod operation_repo;
pub mod type_schema_repo;
pub mod version_family_repo;

pub use dependency_repo::DependencyRepo;
pub use entity_repo::{EntityPage, EntityRepo, PageRequest};
pub use instance_repo::InstanceRepo;
pub use operation_repo::OperationRepo;
pub use type_schema_repo::TypeSchemaRepo;
pub use version_family_repo::VersionFamilyRepo;

/// `ON CONFLICT DO NOTHING`, spelled portably, for the two inserts that race.
///
/// # Why not catch the unique violation and re-read
///
/// Because a raised unique violation **aborts the transaction** on `PostgreSQL`:
/// every later statement fails with *"current transaction is aborted"*, so the
/// recovering re-read fails for a second, unrelated reason. Both racing inserts run
/// inside the admission commit transaction ([`crate::domain::admission::unit`]), so
/// that recovery could never have worked there — `SQLite` and `MySQL` tolerate it,
/// which is why a pooled-connection test passes while the production path does not.
/// An insert whose conflict writes nothing is a statement that *succeeded*: nothing
/// is aborted, on any backend, and the caller decides what the absence means.
///
/// # Why the column argument
///
/// Untargeted on `PostgreSQL` and `SQLite` — plain `ON CONFLICT DO NOTHING`, which
/// covers *every* unique key on the table rather than one named index. `MySQL` has no
/// `DO NOTHING`, so `sea-query` polyfills it with an `ON DUPLICATE KEY UPDATE` that
/// assigns a column to itself, and that needs a column whose self-assignment changes
/// nothing: pass the primary key.
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
/// `SQLite`'s default `SQLITE_MAX_VARIABLE_NUMBER` is 999 on older builds, and a
/// prepared statement here carries the scope predicate's parameters too. 200 is
/// comfortably inside every backend's limit and large enough that a realistic
/// closure needs a handful of round trips, not hundreds.
const IN_CHUNK: usize = 200;
