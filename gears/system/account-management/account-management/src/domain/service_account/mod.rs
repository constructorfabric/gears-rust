//! Service-account domain gear — tenant-scoped machine identities.
//!
//! Implements FEATURE `service-accounts` (see
//! `gears/system/account-management/docs/features/feature-service-accounts.md`).
//!
//! A service account is a confidential OAuth client that
//! authenticates with the `client_credentials` grant and carries its
//! owning `tenant_id` plus a service-subject `user_type` in issued
//! tokens. AM owns the contract surface and the AM-side service that
//! authorizes the caller, validates tenant scope, and forwards every
//! call to the resolved [`account_management_sdk::IdpPluginClient`]
//! plugin. There is no AM-side account table or credential cache:
//! every read and write is a live pass-through to the `IdP`, exactly as
//! for users per `cpt-cf-account-management-constraint-no-user-storage`
//! — a gear restart loses nothing because AM holds nothing.
//!
//! Layering:
//!
//! * [`service`] — [`service::ServiceAccountService`] runs the PEP
//!   gate, resolves the target tenant through
//!   [`crate::domain::tenant::resolve`] (shared with the user
//!   pass-through), forwards provision / list / rotate / revoke, and
//!   maps SDK `IdpServiceAccountFailure` variants onto
//!   [`crate::domain::error::DomainError`] through the boundary helper in
//!   [`crate::domain::idp`].
//!
//! # Why not the user surface
//!
//! Machine identities are a separate PEP resource type
//! ([`account_management_sdk::SERVICE_ACCOUNT_RESOURCE_TYPE`]) rather
//! than a flavour of `cf.core.am.user.v1~`. Folding them together would
//! let a "manage users" grant silently confer machine-credential
//! minting, and secret rotation has no user-side counterpart to inherit
//! a permission from.

// @cpt-begin:cpt-cf-account-management-dod-service-accounts-no-local-storage:p1:inst-dod-sa-no-local-storage-module
// This gear owns no account table, inventory projection, or credential
// cache: there is no `repo`, no migration, and no entity module here —
// only the pass-through service below.
pub mod service;
// @cpt-end:cpt-cf-account-management-dod-service-accounts-no-local-storage:p1:inst-dod-sa-no-local-storage-module

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
