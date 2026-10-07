pub mod definition_migration;
pub mod delivery_migration;
pub mod events_migration;
pub mod migration;
pub mod normalized_migration;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../../../tests/unit/mod_tests.rs"]
mod tests;
