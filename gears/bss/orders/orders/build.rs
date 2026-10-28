//! TEMPORARY (D-229): declares the custom `orders_event_broker_test_support` cfg so the
//! `unexpected_cfgs` lint accepts it. The crate inherits the workspace lint table
//! (`[lints] workspace = true`), so it cannot add a `check-cfg` entry of its own the way crates
//! with their own `[lints.rust]` table do (event-broker-sdk's `cfg(coverage_nightly)`).
//!
//! The cfg compiles in the in-crate tests that need the split-out Event Broker test support
//! (PR #5381). It is a cfg rather than a Cargo feature so that `--all-features` (CI clippy)
//! never turns it on. Nothing sets it; after #5381 merges it can be tried with
//! `RUSTFLAGS="--cfg orders_event_broker_test_support"`, and then this file, the `build` key
//! in Cargo.toml and every `cfg(orders_event_broker_test_support)` gate are deleted.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(orders_event_broker_test_support)");
}
