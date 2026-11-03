//! Builds memory backends from registry entries.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use event_broker_sdk::GtsTypeId;
use event_broker_sdk::backend::{Backend, BackendError, BuildContext, Provider};

use crate::backend::MemoryBackend;
use crate::settings::MemorySettings;
use crate::store::Store;

/// The GTS backend type this plugin serves.
pub const BACKEND_TYPE: &str =
    toolkit_gts::gts_id!("cf.core.events.backend.v1~cf.core.backend.memory.v1~");

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

    async fn build(&self, context: &BuildContext<'_>) -> Result<Arc<dyn Backend>, BackendError> {
        let settings = MemorySettings::parse(context.settings)?;
        let store = Arc::clone(
            self.stores
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(settings.store)
                .or_default(),
        );
        Ok(Arc::new(MemoryBackend::over(
            store,
            settings.retention_interval,
        )))
    }
}
