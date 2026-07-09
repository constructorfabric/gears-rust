//! Inter-gear client trait for the file-storage control plane.

/// Public client trait other gears resolve from `ClientHub`.
///
/// The P1 operations (presign upload/download, bind, metadata CRUD, list) are
/// added incrementally; the trait is kept object-safe and registered so the
/// wiring is in place.
pub trait FileStorageClientV1: Send + Sync {
    /// Module name of the backing gear. Placeholder until the P1 operations land.
    fn module_name(&self) -> &'static str;
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod api_tests;
