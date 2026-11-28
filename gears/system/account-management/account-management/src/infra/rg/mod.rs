//! Infrastructure-layer wiring for the Resource Group SDK.
//!
//! Holds the [`RgResourceOwnershipChecker`] adapter, the built-in
//! resource-ownership probe consumed by
//! [`crate::domain::tenant::service::TenantService::delete_tenant`] when no
//! tenant-ownership plugin vendor is configured.
//!
//! The `ClientHub` binding is wired in the AM gear entry-point
//! ([`crate::gear::AccountManagementGear`]): `resource-group` is declared
//! in `#[toolkit::gear(deps = [...])]`, so the runtime guarantees its init
//! runs first; the entry-point hard-resolves
//! `resource_group_sdk::ResourceGroupReadHierarchy` and propagates a fatal
//! error from `init` if it cannot be obtained — soft-delete safety
//! (DESIGN §3.5) is contract-load-bearing, so we fail closed rather than
//! admit-everything via an inert fallback.
//! [`crate::domain::tenant::resource_checker::InertResourceOwnershipChecker`]
//! is reserved for unit tests, which construct `TenantService` directly and
//! bypass this init path.

pub(crate) mod checker;

#[cfg(test)]
pub(crate) mod test_helpers;

// `RgResourceOwnershipChecker` is wiring-only: the AM gear entry-
// point constructs it from `ClientHub` and hands it to
// `TenantService::new`. It is **not** part of the AM gear's external
// API surface — outside consumers go through `account-management-sdk`.
pub(crate) use checker::RgResourceOwnershipChecker;
