//! Tenant-type compatibility barrier (FEATURE 2.3 `tenant-type-enforcement`).
//!
//! Houses the [`TenantTypeChecker`] trait abstraction consumed by
//! [`crate::domain::tenant::service::TenantService::create_tenant`] at
//! saga step 3 (`inst-algo-saga-type-check`). Production wiring resolves
//! the trait via [`crate::infra::types_registry::GtsTenantTypeChecker`]
//! against `types_registry_sdk::TypesRegistryClient` (the `ClientHub`
//! binding is wired in the AM module entry-point
//! [`crate::module::AccountManagementModule`]); dev / test wiring uses
//! [`InertTenantTypeChecker`].

pub mod checker;

pub use checker::{InertTenantTypeChecker, TenantTypeChecker, inert_tenant_type_checker};
