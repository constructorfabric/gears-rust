//! Migrations of the built-in `PostgreSQL` store.
//!
//! Raw SQL is permitted here and nowhere else: `tsvector` generation, partial
//! indexes, HNSW, identity columns and `CREATE PROPERTY GRAPH` are all beyond
//! what `sea-orm-migration`'s schema builder models. The property-graph DDL
//! itself is still *generated* — from the one declaration `MATCH` uses
//! (Policy 3) — never hand-written.

use sea_orm_migration::prelude::*;

pub mod m0001_initial_schema;
pub mod m0002_property_graph;

/// Text-search configuration used for both the GIN index expression and every
/// lexical query. One constant, so predicate and index cannot drift apart —
/// a mismatch would silently stop using the index rather than fail.
pub const FTS_CONFIG: &str = "simple";

pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0001_initial_schema::Migration),
            Box::new(m0002_property_graph::Migration),
        ]
    }
}
