//! Infrastructure-layer wiring for the Resource Group SDK.
//!
//! Holds the [`RgResourceOwnershipChecker`] adapter that backs the
//! production resource-ownership probe consumed by
//! [`crate::domain::tenant::service::TenantService::delete_tenant`].
//!
//! The `ClientHub` binding is wired in the AM module entry-point
//! ([`crate::module::AccountManagementModule`]): `resource-group` is
//! declared in `#[modkit::module(deps = [...])]`, so the runtime
//! guarantees its init runs first; the entry-point hard-resolves
//! `resource_group_sdk::ResourceGroupClient` and propagates a fatal
//! error from `init` if the client cannot be obtained — soft-delete
//! safety (DESIGN §3.5) is contract-load-bearing, so we fail closed
//! rather than admit-everything via an inert fallback.
//! [`crate::domain::tenant::resource_checker::InertResourceOwnershipChecker`]
//! is reserved for unit tests, which construct `TenantService`
//! directly and bypass this init path.

pub(crate) mod checker;

#[cfg(test)]
pub(crate) mod test_helpers;

// `RgResourceOwnershipChecker` is wiring-only: the AM module entry-
// point constructs it from `ClientHub` and hands it to
// `TenantService::new`. It is **not** part of the AM module's external
// API surface — outside consumers go through `account-management-sdk`.
pub(crate) use checker::RgResourceOwnershipChecker;
