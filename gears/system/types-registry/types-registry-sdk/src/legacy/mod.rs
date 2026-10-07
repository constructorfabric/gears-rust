//! The old SDK surface: `TypesRegistryClient`, its models and its mock.
//!
//! Deleted in T29, when every consumer moves onto `PlatformTypesRegistryApi`; there is no shim.
//! New code does not depend on anything here. The crate root re-exports these modules, so the
//! public paths (`types_registry_sdk::api`, `::models`, `::testing`) are unchanged.

pub mod api;
pub mod models;

#[cfg(feature = "test-util")]
pub mod testing;
