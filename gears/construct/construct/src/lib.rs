//! Construct Gear Implementation
//!
//! The public API is defined in `construct_sdk` and re-exported here.

pub use construct_sdk::{ConstructClientV1, RecordOutcome};

pub mod gear;
pub use gear::ConstructGear;

#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;

#[cfg(test)]
mod test_support;
