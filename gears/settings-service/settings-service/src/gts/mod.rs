//! The gear's link-time GTS content.
//!
//! Everything declared here lands in this crate's collector
//! (`gts_declarations()`), which the gear's `gts_publication(...)` merges with
//! the SDK's into one manifest: the runtime publishes it to `types-registry`
//! and waits for its admission before any application `init`. `init`
//! registers nothing for it.

mod permissions;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "permissions_tests.rs"]
mod permissions_tests;
