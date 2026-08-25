//! SQLite-specific helpers and utilities.
//!
//! This gear contains SQLite-specific functionality including:
//! - DSN parsing and cleaning
//! - PRAGMA parameter handling with typed enums
//! - Path preparation for `SQLite` databases

pub mod dsn;
// Crate-internal path helpers. `pub(crate)` is intentional API hygiene even though the parent
// `sqlite` module is private (clippy::redundant_pub_crate would prefer `pub`).
#[allow(clippy::redundant_pub_crate)]
pub(crate) mod path;
pub mod pragmas;

pub use dsn::{extract_sqlite_pragmas, is_memory_dsn};
pub use path::prepare_sqlite_path;
pub use pragmas::Pragmas;
