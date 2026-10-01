//! # Event-broker backend bench
//!
//! Performance workloads for the storage backend contract (feature 0007,
//! `event_broker_sdk::backend`), written once and run by every backend plugin
//! against its own medium from a `benches/backend.rs` of its own. A plugin
//! implements [`Harness`], parses its command line into [`Settings`] and calls
//! [`run_bench`].
//!
//! Every cell builds a fresh backend instance and provisions fresh topics, and
//! ends with a correctness check whose failure marks the cell failed in the
//! report rather than aborting the run. See `README.md` for the scenarios, the
//! matrix and how to read the results.

#![forbid(unsafe_code)]

mod args;
mod check;
mod consume;
mod harness;
mod matrix;
mod phase;
mod produce;
mod report;
mod run;
mod scenario;
mod stats;
mod window;
mod workload;

#[cfg(test)]
mod args_tests;
#[cfg(test)]
mod matrix_tests;
#[cfg(test)]
mod stats_tests;
#[cfg(test)]
mod workload_tests;

pub use args::{ScenarioId, Settings};
pub use harness::{Harness, Sizing};
pub use report::{CellReport, Report};
pub use run::run_bench;
pub use stats::{Histogram, Percentiles};
