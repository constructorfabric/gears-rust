//! No-op Usage Collector Storage Plugin
//!
//! Registers a [`usage_collector_sdk::UsageCollectorPluginClientV1`] implementation that
//! accepts `create_usage_record` calls and drops all data. Use when the usage-collector gateway should run
//! end-to-end without a real storage backend.
//!
//! ## Configuration
//!
//! ```yaml
//! modules:
//!   noop_usage_collector_storage_plugin:
//!     config:
//!       vendor: "cyberfabric"
//!       priority: 100
//! ```
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod config;
mod domain;
mod module;
