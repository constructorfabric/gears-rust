//! Static `AuthZ` Resolver Plugin
//!
//! This plugin provides a static authorization policy for development and testing.
//!
//! - Valid tenant → `decision: true` with an `in` predicate on `owner_tenant_id` if the
//!   PEP declares that property, plus, under `TenantHierarchy`, an `InTenantSubtree`
//!   on each declared of `owner_tenant_id` / `id`; no constraints if neither applies
//! - `group_ids` / `ancestor_group_ids` on the resource → `decision: false`
//! - Nil or missing tenant → `decision: false`
//!
//! ## Configuration
//!
//! ```yaml
//! gears:
//!   static-authz-plugin:
//!     config:
//!       vendor: "constructorfabric"
//!       priority: 100
//! ```
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod gear;

pub use gear::StaticAuthZPlugin;
