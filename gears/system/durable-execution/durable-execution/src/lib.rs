//! Durable execution. Journal checkpoints and fencing are authoritative.
#![forbid(unsafe_code)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
pub use durable_execution_sdk as sdk;
pub use gear::DurableExecutionGear;
pub mod config;
mod domain;
pub mod gear;
mod infra;

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/integration/postgres.rs"]
mod test_postgres;

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/integration/typed_runtime_tests.rs"]
mod typed_runtime_tests;
