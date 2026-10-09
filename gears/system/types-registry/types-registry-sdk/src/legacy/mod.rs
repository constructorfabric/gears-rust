//! The old SDK surface: `TypesRegistryClient`, its models and its mock.
//!
//! Deleted in T31, when every consumer moves onto `PlatformTypesRegistryApi`; there is no shim.
//! New code does not depend on anything here. The crate root re-exports its items, and the mock
//! stays at `types_registry_sdk::testing`; `models` names the new surface.

pub mod api;
pub mod models;

#[cfg(feature = "test-util")]
pub mod testing;
