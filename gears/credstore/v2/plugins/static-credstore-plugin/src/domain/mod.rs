// Updated: 2026-10-06 by Constructor Tech
//! In-memory value-store implementation and SDK adapter.

mod client;
pub mod service;

pub use service::Service;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "conformance_tests.rs"]
mod conformance_tests;
