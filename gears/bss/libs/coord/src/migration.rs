//! The `coord_leases` table migration.
//!
//! A consuming gear adds [`Migration`] to its own `MigratorTrait::migrations()`
//! vec (migrations run in vec order, so place it after the gear's own). The
//! migration name is `m0001_create_coord_leases` (derived from the defining
//! module), stable regardless of where it sits in the host chain.

pub mod m0001_create_coord_leases;

pub use m0001_create_coord_leases::Migration;
