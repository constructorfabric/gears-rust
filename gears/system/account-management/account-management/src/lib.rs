//! Account Management — storage floor crate.
//!
//! This crate ships the persistence foundation for the AM gear:
//! the stable domain shapes (error taxonomy, idp contract, tenant
//! model / repo trait, retention types), the SeaORM-backed
//! `TenantRepoImpl` and migration set, the domain services
//! ([`crate::domain::tenant::service::TenantService`] with hooks,
//! retention + reaper pipelines), and the `ToolKit` gear entry-point
//! ([`AccountManagementGear`]) that wires everything together with
//! the `AuthZ` resolver, `IdP` provisioner, Resource Group and Types
//! Registry plugins resolved from `ClientHub`.
//!
//! REST wiring, the platform-bootstrap saga, and hierarchy-integrity
//! audit arrive in subsequent PRs.
//!
//! # Authorization posture
//!
//! The [`InTenantSubtree`](toolkit_security::ScopeFilter::in_tenant_subtree)
//! predicate (gears-rust#1813) provides the SQL-level subtree
//! clamp via a `tenant_closure` JOIN. AM consumes the predicate as
//! follows:
//!
//! * `tenants` and `tenant_closure` are declared
//!   `no_tenant, no_resource, no_owner, no_type` — the predicate has
//!   no resolvable property to clamp against on those entities, so
//!   reads stay scope-property-less and the service-layer PDP gate
//!   ([`crate::domain::tenant::service::TenantService`]) carries the
//!   authorization burden for the tenant CRUD surface.
//! * `tenant_metadata` is declared `Scopable(tenant_col = "tenant_id",
//!   ...)`. A caller-built `InTenantSubtree(root=subject.tenant_id)`
//!   scope therefore clamps `MetadataRepo` reads / writes via the
//!   secure-ORM closure subquery — no AM-side wiring required, the
//!   storage seam simply forwards the caller's [`AccessScope`].
//! * Conversion / lifecycle paths run as `actor=system` and pass
//!   [`AccessScope::allow_all`] explicitly; structural reads on the
//!   closure table use the same posture.
//!
//! REST handlers on top of `TenantRepo` MUST build the
//! `InTenantSubtree` constraint at the request-handler layer (from
//! the platform `AuthN` context) before invoking the service so the
//! PDP-narrowed scope flows into every downstream `MetadataRepo` /
//! `ConversionRepo` call.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

// AM publishes this crate's GTS declarations itself; see `gts`.
toolkit_gts::declare_gts_inventory!();

pub mod api;
pub mod client;
pub mod config;
pub mod domain;
pub mod gear;
pub(crate) mod gts;
pub mod infra;
pub(crate) mod tr_plugin;

pub use domain::error::DomainError;
pub use domain::metrics::{
    AM_BOOTSTRAP_LIFECYCLE, AM_CONVERSION_LIFECYCLE, AM_CROSS_TENANT_DENIAL, AM_DEPENDENCY_HEALTH,
    AM_HIERARCHY_DEPTH_EXCEEDANCE, AM_HIERARCHY_INTEGRITY_DURATION,
    AM_HIERARCHY_INTEGRITY_LAST_SUCCESS, AM_HIERARCHY_INTEGRITY_REPAIRED,
    AM_HIERARCHY_INTEGRITY_RUNS, AM_HIERARCHY_INTEGRITY_VIOLATIONS, AM_METADATA_RESOLUTION,
    AM_RETENTION_INVALID_WINDOW, AM_TENANT_RETENTION, MetricKind, emit_metric,
};
pub use domain::tenant::{
    ChildCountFilter, ClosureRow, HardDeleteOutcome, HardDeleteResult, NewTenant, ReaperResult,
    TenantModel, TenantProvisioningRow, TenantRepo, TenantRetentionRow, TenantStatus,
};

pub use infra::storage::migrations::Migrator;
// `AmDbProvider` and `TenantRepoImpl` are crate-internal: external
// consumers depend on `account-management-sdk` (the trait surface) and
// resolve a live instance through `ClientHub`, never on the impl crate's
// concrete storage types. Re-exporting them here would re-open the
// impl/SDK boundary documented in `account-management-sdk/src/lib.rs`
// (every SeaORM bump or schema change in `infra::storage::repo_impl`
// would then break every dependent crate). Kept reachable only via the
// `infra::storage::repo_impl` path inside the crate.

pub use client::AccountManagementClientImpl;
pub use gear::AccountManagementGear;
