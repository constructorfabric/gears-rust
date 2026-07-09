//! In-process adapter implementing the SDK client trait.
//!
//! @cpt-cf-file-storage-component-sdk-facade

use file_storage_sdk::FileStorageClientV1;

/// Local (same-process) implementation of [`FileStorageClientV1`].
///
/// Gains the real P1 operations in milestones M5+; M0 keeps it object-safe and
/// registered so consumers can already resolve the trait from `ClientHub`.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Default)]
pub struct FileStorageLocalClient;

impl FileStorageLocalClient {
    /// Create a new local client.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl FileStorageClientV1 for FileStorageLocalClient {
    fn module_name(&self) -> &'static str {
        "file-storage"
    }
}

#[cfg(test)]
#[path = "local_client_tests.rs"]
mod local_client_tests;
