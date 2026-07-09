//! Usage Collector Gear
//!
//! Implements the `usage-collector` gear host that:
//! 1. Reads the `[usage-collector]` configuration once at `init` (vendor
//!    binding only — the usage-type catalog is plugin-owned per ADR-0012).
//! 2. Constructs the domain [`domain::Service`] carrying an embedded
//!    `GtsPluginSelector` (lazy storage-plugin resolution via
//!    `ClientHub::try_get_scoped::<dyn UsageCollectorPluginV1>`).
//! 3. Wires the [`authz_resolver_sdk::PolicyEnforcer`] (PDP) onto the service
//!    as a hard dependency per
//!    `cpt-cf-usage-collector-adr-pdp-centric-authorization`.
//! 4. Registers `Arc<dyn UsageCollectorClientV1>` in `ClientHub` for
//!    in-process consumers.
//!
//! The usage-type catalog itself is plugin-owned per ADR-0012; the
//! foundation host carries no gateway-local catalog repository, no host-side
//! `usage_type_catalog` migration, and no host-local catalog table.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod api;
pub mod config;
pub mod domain;
pub(crate) mod gts;
pub mod infra;
pub mod module;
