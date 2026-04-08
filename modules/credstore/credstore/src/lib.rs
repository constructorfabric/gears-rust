//! `CredStore` Gateway Module
//!
//! Implements the `CredStore` gateway module that:
//! 1. Registers the `CredStorePluginSpecV1` schema in types-registry
//! 2. Discovers plugin instances via types-registry (lazy, first-use)
//! 3. Routes `get`/`put`/`delete` calls through the selected plugin
//! 4. Registers `Arc<dyn CredStoreClientV1>` in `ClientHub` for consumers
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod module;
