//! RG `AuthZ` Resolver Plugin
//!
//! Reference implementation of an `AuthZ` plugin that resolves tenant and group
//! hierarchy via the Resource Group module's `ResourceGroupReadHierarchy` trait.
//!
//! - Resolves tenant subtree via `get_group_descendants`
//! - Returns `InGroup` / `InGroupSubtree` predicates with resolved group IDs
//! - Filters barrier tenants from scope (barrier groups visible but excluded from `AccessScope`)
//!
//! ## Configuration
//!
//! ```yaml
//! modules:
//!   rg_authz_plugin:
//!     config:
//!       vendor: "hyperspot"
//!       priority: 50
//! ```
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod module;

pub use module::RgAuthZPlugin;
