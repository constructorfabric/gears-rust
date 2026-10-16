//! Rules `AuthZ` Resolver Plugin
//!
//! A platform PDP provider that evaluates explicitly configured permission rules: per subject,
//! resource type and action, OR alternatives of AND predicate paths over named resource
//! properties (including finite resource-ID sets) and an optional delegated-path condition. It knows no consumer gear's types.
//!
//! Fail closed: no matching rule denies, a malformed configuration fails startup, and there is
//! no default policy, wildcard or allow-all form.
//!
//! ## Configuration
//!
//! ```yaml
//! gears:
//!   rules-authz-plugin:
//!     config:
//!       vendor: "constructorfabric"
//!       priority: 10
//!       policy_revision: "example-2026-10-06"
//!       rules:
//!         - id: "project-reader"
//!           subject: { id: "…", tenant_id: "…" }
//!           resource_type: "gts.cf.example.project.v1~"
//!           actions: ["read", "update"]
//!           paths:
//!             - predicates:
//!                 - { property: "owner_tenant_id", values: ["…"] }
//! ```
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod gear;

pub use gear::RulesAuthZPlugin;
