//! Domain layer: the store port the SQL adapter implements.

pub mod ports;

pub use ports::{FoundationStore, SeedReport, StoreError};
