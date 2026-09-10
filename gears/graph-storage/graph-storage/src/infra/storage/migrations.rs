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
pub mod m0003_embedding_space;
pub mod m0004_element_envelope;
pub mod m0005_payload_index;
pub mod m0006_type_revision;

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
            Box::new(m0003_embedding_space::Migration),
            Box::new(m0004_element_envelope::Migration),
            Box::new(m0005_payload_index::Migration),
            Box::new(m0006_type_revision::Migration),
        ]
    }
}
