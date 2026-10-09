//! Inter-gear client trait for the file-storage control plane.

/// Public client trait other gears resolve from `ClientHub`.
pub trait FileStorageClientV1: Send + Sync {
    /// Module name of the backing gear. Placeholder until real operations land.
    fn module_name(&self) -> &'static str;
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod api_tests;
