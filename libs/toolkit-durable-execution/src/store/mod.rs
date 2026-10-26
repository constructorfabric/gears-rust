//! Secure journal persistence and runtime-managed migrations.
/// Forward-only schema migrations for the durable journal.
pub mod migrations;
mod outbox;
/// Journal store with compare-and-swap revisions and outbox delivery.
pub mod repository;
/// `SeaORM` entity for the `durable_execution__runs` table.
pub mod run;
pub use repository::{JournalStore, StoreError};
mod catalog;
mod coalescing;
mod definition;
mod start;
mod start_key;

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/integration/process_tests.rs"]
mod process_tests;

mod activity;
mod attempt;
mod epoch;
/// Normalized run, activity and attempt rows behind the journal.
pub mod normalized;

mod event;
/// Run event history recording and cursor reads.
pub mod events;

mod probe;

mod progress;

#[cfg(all(test, feature = "integration"))]
#[expect(
    clippy::unwrap_used,
    reason = "Crash fixture setup must fail immediately when its provider or disposable database is unavailable."
)]
#[path = "../../tests/integration/provider_stages_tests.rs"]
mod provider_stages_tests;
