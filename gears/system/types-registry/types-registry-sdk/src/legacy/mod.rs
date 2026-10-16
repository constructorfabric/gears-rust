//! Legacy client, models and test support, re-exported at the crate root.
//! New consumers should use [`crate::PlatformTypesRegistryApi`] or [`crate::TypesRegistryApi`].

pub mod api;
pub mod models;

#[cfg(feature = "test-util")]
pub mod testing;
