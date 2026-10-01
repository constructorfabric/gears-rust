//! Builds memory backends from registry entries.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use event_broker_sdk::GtsTypeId;
use event_broker_sdk::backend::{Backend, BackendError, Provider};
use tokio_util::sync::CancellationToken;

use crate::backend::MemoryBackend;
use crate::settings::MemorySettings;
use crate::store::Store;

/// The GTS backend type this plugin serves.
pub const BACKEND_TYPE: &str = "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~";

/// Builds a [`MemoryBackend`] per registry entry. Entries naming the same
/// `store` share one, for as long as this provider lives.
pub struct MemoryBackendProvider {
    backend_type: GtsTypeId,
    stores: Mutex<HashMap<String, Arc<Store>>>,
}

impl Default for MemoryBackendProvider {
    fn default() -> Self {
        Self {
            backend_type: GtsTypeId::new(BACKEND_TYPE),
            stores: Mutex::default(),
        }
    }
}

#[async_trait]
impl Provider for MemoryBackendProvider {
    fn backend_type(&self) -> &GtsTypeId {
        &self.backend_type
    }

    async fn build(
        &self,
        settings: &serde_json::Map<String, serde_json::Value>,
        stop: CancellationToken,
    ) -> Result<Arc<dyn Backend>, BackendError> {
        let settings = MemorySettings::parse(settings)?;
        let store = Arc::clone(
            self.stores
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(settings.store)
                .or_default(),
        );
        let backend = MemoryBackend::over(store);
        backend.spawn_retention(settings.retention_interval, stop);
        Ok(Arc::new(backend))
    }
}
