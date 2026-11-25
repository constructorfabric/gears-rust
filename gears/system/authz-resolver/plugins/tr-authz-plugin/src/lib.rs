//! TR `AuthZ` Resolver Plugin
//!
//! `AuthZ` plugin that resolves tenant hierarchy via `TenantResolverClient`.
//! Produces `In(owner_tenant_id)` predicates for tenant scoping,
//! plus `InGroup`/`InGroupSubtree` predicates from the `group_ids` /
//! `ancestor_group_ids` resource properties when the PEP advertises the
//! matching group capabilities (otherwise, or on empty/malformed input, it denies).
//!
//! ## Configuration
//!
//! ```yaml
//! gears:
//!   tr-authz-plugin:
//!     config:
//!       vendor: "constructorfabric"
//!       priority: 50
//! ```
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod gear;

pub use gear::TrAuthZPlugin;
