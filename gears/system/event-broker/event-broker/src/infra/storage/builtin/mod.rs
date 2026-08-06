//! Built-in storage backend bundled with the event broker: SQLite
//! (eb-single-process-implementation D3), replacing the in-memory shell
//! `DESIGN.md:602-603` originally sketched.

pub mod sqlite;

pub use sqlite::SqliteEventBackend;
