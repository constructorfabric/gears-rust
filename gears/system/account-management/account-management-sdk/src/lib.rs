//! Account Management SDK — public contract surface.
//!
//! This crate publishes the inter-gear client trait
//! ([`AccountManagementClient`]) and its data types. Per
//! [ADR 0005][adr] the trait boundary is
//! [`toolkit_canonical_errors::CanonicalError`]; the SDK additionally
//! ships [`AccountManagementError`] as an **opt-in** typed projection
//! (`From<CanonicalError>`) for consumers that want flat dispatch on
//! AM's emission set. The single authoritative AIP-193 ladder
//! (`From<DomainError> for CanonicalError`) lives in the impl crate
//! (`cf-gears-account-management`); this SDK never re-derives that
//! classification — it only projects the finished `CanonicalError`.
//!
//! External consumers — plugin authors, dashboards, integration tests,
//! sibling gears calling AM via `ClientHub` — depend on **this**
//! crate, never on the impl crate, so impl-side churn (sea-orm
//! migrations, axum wiring, tokio runtime) does not propagate as a
//! contract break.
//!
//! # Error surface
//!
//! Trait methods return `Result<_, CanonicalError>`. Consumers may
//! propagate the canonical error, or project it into the typed
//! [`AccountManagementError`] view via
//! `.map_err(AccountManagementError::from)` (or a
//! `From<CanonicalError> for OwnError` chain). The projection models
//! the ten canonical categories AM emits, plus a catch-all
//! `Other { canonical }`; see [`error`] for the full dispatch table,
//! the three consumer integration patterns, and the co-located wire
//! vocabulary ([`field`], [`precondition`], [`reason`], [`quota`],
//! [`gts`]).
//!
//! [adr]: https://github.com/constructorfabric/gears-rust/blob/main/docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod client;
pub mod error;
pub mod field;
pub mod gts;
pub mod idp;
pub mod idp_user;
pub mod metadata;
pub mod precondition;
pub mod quota;
pub mod reason;
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
    IdpTenantContext, IdpUpdateUserRequest, IdpUser, IdpUserDuplicateField, IdpUserFilterField,
    IdpUserOperationFailure, IdpUserPagination, IdpUserPaginationError, IdpUserPatch, IdpUserQuery,
    ListUsersQuery, NewUserPassword,
};
pub use metadata::{
    MetadataEntry, MetadataEntryFilterField, MetadataEntryQuery, UpsertMetadataRequest,
};
pub use tenant::{
    CreateTenantRequest, Tenant, TenantId, TenantInfoFilterField, TenantInfoQuery, TenantStatus,
    UpdateTenantRequest,
};
