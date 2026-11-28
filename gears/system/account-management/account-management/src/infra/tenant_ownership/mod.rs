//! Infrastructure-layer glue for the optional
//! [`account_management_sdk::TenantOwnershipPluginClient`] plugin.
//!
//! When `tenant_ownership.vendor` is configured, the gear wires
//! [`LazyTenantOwnershipChecker`] as the soft-delete ownership check and
//! registers its [`LazyTenantOwnershipChecker::cleanup_hook`] in the
//! hard-delete pipeline. Without a vendor neither is wired and AM keeps the
//! built-in [`crate::infra::rg::RgResourceOwnershipChecker`].

mod lazy;
pub use lazy::LazyTenantOwnershipChecker;
