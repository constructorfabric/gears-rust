//! Secure journal persistence and runtime-managed migrations.
pub mod migrations;
pub use migrations::migration;
mod outbox;
pub mod repository;
pub mod run;
pub use repository::{JournalStore, StoreError};
mod catalog;
mod coalescing;
mod definition;
mod start;
pub use migrations::definition_migration;
mod start_key;

pub use migrations::delivery_migration;

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../../tests/integration/process_tests.rs"]
mod process_tests;

mod activity;
mod attempt;
mod epoch;
mod normalized;
pub use migrations::normalized_migration;

mod event;
pub mod events;
pub use migrations::events_migration;

mod probe;
