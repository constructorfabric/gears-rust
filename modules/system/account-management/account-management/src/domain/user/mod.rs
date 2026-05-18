//! IdpUser-operations domain module.
//!
//! Implements FEATURE `idp-user-operations-contract` (see
//! `modules/system/account-management/docs/features/feature-idp-user-operations-contract.md`).
//!
//! AM owns the contract surface and the AM-side service that validates
//! tenant scope and forwards every call to the resolved
//! [`account_management_sdk::IdpPluginClient`] plugin via
//! `ClientHub`. There is no AM-side user table, projection cache, or
//! membership cache: every read and write is a live pass-through to
//! the `IdP` per
//! `cpt-cf-account-management-constraint-no-user-storage` and
//! `cpt-cf-account-management-dod-idp-user-operations-contract-no-local-user-storage`.
//!
//! Layering:
//!
//! * [`service`] -- [`service::UserService`] resolves the target tenant
//!   via [`crate::domain::tenant::TenantRepo`], validates `Active`
//!   status, and forwards `create_user` / `delete_user` /
//!   `list_users` to the configured plugin. Maps SDK
//!   `IdpUserOperationFailure` variants onto [`crate::domain::error::DomainError`]
//!   through the boundary helper in [`crate::domain::idp`].
//!
//! The REST surface for `/tenants/{id}/users` and
//! `/tenants/{id}/users/{user_id}` is intentionally not wired in this
//! crate yet -- it lands in a follow-up PR once the `InTenantSubtree`
//! predicate (`cyberfabric-core#1813`) makes the storage-level subtree
//! clamp safe for cross-barrier authorization. The domain types here
//! are REST-ready so that drop-in is a thin handler wiring step.

pub mod service;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
