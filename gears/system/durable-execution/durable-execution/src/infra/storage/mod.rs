//! Secure journal persistence and runtime-managed migrations.
/// Forward-only schema migrations for the durable journal.
pub mod migrations;
pub use migrations::migration;
mod outbox;
/// Journal store with compare-and-swap revisions and outbox delivery.
pub mod repository;
/// `SeaORM` entity for the `durable_runs` table.
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
pub(super) mod normalized;
pub use migrations::normalized_migration;

mod event;
/// Run event history recording and cursor reads.
pub mod events;
pub use migrations::events_migration;

mod probe;

mod progress;

#[cfg(all(test, feature = "integration"))]
#[expect(
    clippy::unwrap_used,
    reason = "Crash fixture setup must fail immediately when its provider or disposable database is unavailable."
)]
#[path = "../../../tests/integration/provider_stages_tests.rs"]
mod provider_stages_tests;
