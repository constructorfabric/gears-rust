//! Account Management SDK — public contract surface.
//!
//! This crate publishes the inter-module client trait
//! ([`AccountManagementClient`]) and its data types. The public error
//! envelope is [`AccountManagementError`] — a flat enum mirroring the
//! AIP-193 categories AM raises; this SDK does **not** depend on
//! `modkit-canonical-errors`. The impl crate
//! (`cyberware-account-management`) owns the mapping between AM's
//! internal `DomainError` vocabulary and [`AccountManagementError`]
//! at this boundary, and the further lift to
//! `modkit_canonical_errors::CanonicalError` at the REST boundary.
//!
//! External consumers — plugin authors, dashboards, integration tests,
//! sibling modules calling AM via `ClientHub` — depend on **this**
//! crate, never on the impl crate, so impl-side churn (sea-orm
//! migrations, axum wiring, tokio runtime) does not propagate as a
//! contract break.
//!
//! # Mapping summary (AIP-193)
//!
//! Every AM domain failure surfaces in one of the
//! [`AccountManagementError`] variants below; the HTTP status column
//! is the AIP-193 mapping the REST handler applies through the
//! impl-side `account_management_error_to_canonical` lift.
//!
//! | Variant | AIP-193 category | HTTP |
//! |---------|-----------------|------|
//! | `Validation` | `InvalidArgument` | 400 |
//! | `NotFound` | `NotFound` | 404 |
//! | `AlreadyExists` | `AlreadyExists` | 409 |
//! | `FailedPrecondition` | `FailedPrecondition` | 400 |
//! | `Aborted` | `Aborted` | 409 |
//! | `CrossTenantDenied` | `PermissionDenied` | 403 |
//! | `IntegrityCheckInProgress` | `ResourceExhausted` | 429 |
//! | `UnsupportedOperation` | `Unimplemented` | 501 |
//! | `ServiceUnavailable` | `ServiceUnavailable` | 503 |
//! | `Internal` | `Internal` | 500 |
//!
//! `ServiceUnavailable` carries `retry_after_seconds`; `Aborted`
//! carries `reason = "SERIALIZATION_CONFLICT"` for retry-exhausted
//! serializable conflicts; resource-scoped variants (`NotFound`,
//! `FailedPrecondition`) carry the GTS resource type from [`gts`] —
//! e.g. `gts.cf.core.am.{tenant|tenant_metadata|conversion_request}.v1~`.
//! Those strings live in [`gts`] as `pub const` so consumers can
//! match on them by typed reference instead of stringly-typed
//! comparison.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod client;
pub mod error;
pub mod gts;
pub mod idp;
pub mod idp_user;
pub mod metadata;
pub mod tenant;

pub use client::AccountManagementClient;
pub use error::AccountManagementError;
pub use gts::{
    CONVERSION_REQUEST_RESOURCE_TYPE, IdpPluginSpecV1, TENANT_METADATA_RESOURCE_TYPE,
    TENANT_RESOURCE_TYPE, USER_RESOURCE_TYPE,
};
pub use idp::{
    IdpDeprovisionFailure, IdpDeprovisionTenantRequest, IdpPluginClient, IdpProvisionFailure,
    IdpProvisionResult, IdpProvisionTarget, IdpProvisionTenantRequest,
};
pub use idp_user::{
    IdpDeprovisionUserRequest, IdpListUsersRequest, IdpNewUser, IdpProvisionUserRequest,
    IdpTenantContext, IdpUser, IdpUserOperationFailure, IdpUserPagination, IdpUserPaginationError,
    ListUsersQuery,
};
pub use metadata::{
    MetadataEntry, MetadataEntryFilterField, MetadataEntryQuery, UpsertMetadataRequest,
};
pub use tenant::{
    CreateTenantRequest, Tenant, TenantId, TenantInfoFilterField, TenantInfoQuery, TenantStatus,
    UpdateTenantRequest,
};
