//! `StorageBackendRegistry` (`DESIGN.md:770-780`): GTS discovery +
//! `ClientHub` resolution of a topic's bound storage backend. Shell only.
//!
//! The plugin trait itself is mid-reconciliation between `DESIGN.md`'s
//! `StorageBackend` and the SDK's shipped `EventBrokerBackend`
//! (`DESIGN.md:782`'s "Deferred (M5b-flattening)" note) - this registry
//! does not take a position on that reconciliation; it lands with #4348.

#[derive(Debug, Default)]
pub struct StorageBackendRegistry;

impl StorageBackendRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
