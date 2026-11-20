//! RG Tenant Resolver Plugin
//!
//! Resolves tenant hierarchy via the Resource Group gear's
//! `ResourceGroupReadHierarchy` trait. Production replacement for
//! `static-tr-plugin`: tenants are RG groups whose GTS type code is one of
//! the configured `tenant_types` (default: `TENANT_GROUP_TYPE_ID`, see
//! `resource_group_sdk`). Metadata contains `status` and `self_managed`
//! fields.
//!
//! ## Configuration
//!
//! ```yaml
//! gears:
//!   rg_tr_plugin:
//!     config:
//!       vendor: "constructorfabric"
//!       priority: 50
//!       tenant_types:
//!         - "gts.cf.core.rg.group.v1~cf.core.rg.tenant.v1~"
//! ```
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod gear;

pub use gear::RgTrPlugin;
