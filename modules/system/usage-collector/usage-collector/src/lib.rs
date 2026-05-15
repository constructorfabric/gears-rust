//! Usage Collector gateway.
//!
//! Centralized ingest for usage records from the SDK outbox pipeline and
//! delegation to the GTS-selected storage plugin.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod api;
pub mod config;
pub mod domain;
mod module;
