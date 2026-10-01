//! Persistence for the policy engine.
//!
//! The gear's stable database namespace is `policy_engine`: every table, index
//! and constraint it creates is prefixed `policy_engine__` per the platform's
//! object-namespacing decision (`cpt-cf-database-adr-object-namespacing`).
//!
//! - [`entity`] — one `SeaORM` entity per table, each deriving `Scopable` and
//!   declaring its secure-data-layer dimensions.
//! - [`migrations`] — the schema.
//!
//! All access goes through the secure data layer (`.secure().scope_with(..)`,
//! `secure_insert`, ...); raw SQL exists only inside the migrations.

pub mod content_repo;
pub mod entity;
pub mod mapper;
pub mod migrations;
pub mod odata_mapper;

pub use migrations::Migrator;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "migrations_tests.rs"]
mod migrations_tests;
