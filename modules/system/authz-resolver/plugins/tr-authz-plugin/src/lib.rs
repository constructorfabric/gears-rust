//! TR `AuthZ` Resolver Plugin
//!
//! `AuthZ` plugin that resolves tenant hierarchy via `TenantResolverClient`.
//! Produces `In(owner_tenant_id)` predicates for tenant scoping and
//! optional `InGroup`/`InGroupSubtree` predicates from request context.
//!
//! ## Configuration
//!
//! ```yaml
//! modules:
//!   tr_authz_plugin:
//!     config:
//!       vendor: "hyperspot"
//!       priority: 50
//! ```
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod module;

pub use module::TrAuthZPlugin;
