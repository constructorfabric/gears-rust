//! Static `AuthZ` Resolver Plugin
//!
//! This plugin provides a static authorization policy for development and testing.
//!
//! - Valid tenant → `decision: true` with `in` predicate on `owner_tenant_id`
//! - Nil or missing tenant → `decision: false`
//!
//! ## Configuration
//!
//! ```yaml
//! modules:
//!   static_authz_plugin:
//!     config:
//!       vendor: "cyberfabric"
//!       priority: 100
//! ```
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod module;

pub use module::StaticAuthZPlugin;
