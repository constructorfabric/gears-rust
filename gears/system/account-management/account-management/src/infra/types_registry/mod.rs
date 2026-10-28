//! Production GTS adapters, including [`GtsTenantTypeChecker`] for Feature 2.3.
//!
//! [`crate::gear::AccountManagementGear`] resolves `TypesRegistryApi` from the
//! host-linked database-backed registry, initialized first as a system gear.
//! A missing client fails init; [`crate::domain::tenant_type::InertTenantTypeChecker`]
//! is for unit tests only.

pub(crate) mod checker;
pub(crate) mod metadata_schema_registry;
pub(crate) mod root_type;

#[cfg(test)]
pub(crate) mod test_helpers;

// `GtsTenantTypeChecker` is wiring-only: the AM gear entry-point
// constructs it from `ClientHub` and hands it to `TenantService::new`.
// It is **not** part of the AM gear's external API surface — outside
// consumers go through `account-management-sdk`.
pub(crate) use checker::GtsTenantTypeChecker;
pub(crate) use metadata_schema_registry::GtsMetadataSchemaRegistry;
pub(crate) use root_type::register_root_type;
