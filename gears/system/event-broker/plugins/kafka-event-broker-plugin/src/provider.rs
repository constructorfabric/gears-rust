//! Builds Kafka backends from registry entries.

use std::sync::Arc;

use async_trait::async_trait;
use event_broker_sdk::GtsTypeId;
use event_broker_sdk::backend::{Backend, BackendError, BuildContext, Provider};

use crate::backend::KafkaBackend;
use crate::settings::KafkaSettings;
use crate::shared::Shared;

/// The GTS backend type this plugin serves.
pub const BACKEND_TYPE: &str =
    toolkit_gts::gts_id!("cf.core.events.backend.v1~cf.core.backend.kafka.v1~");

pub struct KafkaBackendProvider {
    backend_type: GtsTypeId,
}

impl Default for KafkaBackendProvider {
    fn default() -> Self {
        Self {
            backend_type: GtsTypeId::new(BACKEND_TYPE),
        }
    }
}

#[async_trait]
impl Provider for KafkaBackendProvider {
    fn backend_type(&self) -> &GtsTypeId {
        &self.backend_type
    }

    /// Validates the settings; nothing connects until an operation needs the
    /// cluster (`docs/DESIGN.md` §1).
    async fn build(&self, context: &BuildContext<'_>) -> Result<Arc<dyn Backend>, BackendError> {
        // Every role does the same: the brokers run retention, so no
        // instance has background work to place.
        let shared = Shared::new(KafkaSettings::parse(context.settings)?);
        Ok(Arc::new(KafkaBackend::new(shared)))
    }
}
