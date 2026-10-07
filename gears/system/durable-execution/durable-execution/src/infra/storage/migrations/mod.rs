/// Definition catalog table and run registration generations.
pub mod definition_migration;
pub mod delivery_migration;
/// Per-run event history table.
pub mod events_migration;
/// Initial runs, outbox and start-key schema.
pub mod migration;
/// Normalized activity, attempt and epoch tables.
pub mod normalized_migration;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../../../tests/unit/mod_tests.rs"]
mod tests;
