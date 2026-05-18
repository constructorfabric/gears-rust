//! Account Management — storage floor crate.
//!
//! This crate ships the persistence foundation for the AM module:
//! the stable domain shapes (error taxonomy, idp contract, tenant
//! model / repo trait, retention types), the SeaORM-backed
//! `TenantRepoImpl` and migration set, the domain services
//! ([`crate::domain::tenant::service::TenantService`] with hooks,
//! retention + reaper pipelines), and the `ModKit` module entry-point
//! ([`AccountManagementModule`]) that wires everything together with
//! the `AuthZ` resolver, `IdP` provisioner, Resource Group and Types
//! Registry plugins resolved from `ClientHub`.
//!
//! REST wiring, the platform-bootstrap saga, and hierarchy-integrity
//! audit arrive in subsequent PRs.
//!
//! # Authorization posture
//!
//! The [`InTenantSubtree`](modkit_security::ScopeFilter::in_tenant_subtree)
//! predicate (cyberware-rust#1813) provides the SQL-level subtree
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

pub mod client;
pub mod config;
pub mod domain;
pub mod infra;
pub mod module;

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
pub use infra::storage::repo_impl::{AmDbProvider, TenantRepoImpl};

pub use client::AccountManagementClientImpl;
pub use module::AccountManagementModule;
