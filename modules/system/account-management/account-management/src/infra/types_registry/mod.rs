//! Infrastructure-layer wiring for the GTS Types Registry SDK.
//!
//! Holds the [`GtsTenantTypeChecker`] adapter that connects the AM
//! [`crate::domain::tenant_type::TenantTypeChecker`] domain trait to
//! `types_registry_sdk::TypesRegistryClient` resolved from `ClientHub`
//! (FEATURE 2.3 `tenant-type-enforcement`).
//!
//! The `ClientHub` binding is wired in the AM module entry-point
//! ([`crate::module::AccountManagementModule`]): `types-registry` is
//! a hard `deps` of AM, so the runtime guarantees its init runs first
//! and the entry-point fail-closes `init` if `TypesRegistryClient`
//! cannot be resolved. There is no production fallback to
//! [`crate::domain::tenant_type::InertTenantTypeChecker`]; that
//! checker is reserved for unit tests.

pub(crate) mod checker;
pub(crate) mod metadata_schema_registry;

#[cfg(test)]
pub(crate) mod test_helpers;

// `GtsTenantTypeChecker` is wiring-only: the AM module entry-point
// constructs it from `ClientHub` and hands it to `TenantService::new`.
// It is **not** part of the AM module's external API surface — outside
// consumers go through `account-management-sdk`.
pub(crate) use checker::GtsTenantTypeChecker;
pub(crate) use metadata_schema_registry::GtsMetadataSchemaRegistry;
