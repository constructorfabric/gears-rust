//! Builds `PostgreSQL` backends from registry entries.

use std::sync::Arc;

use async_trait::async_trait;
use event_broker_sdk::GtsTypeId;
use event_broker_sdk::backend::{Backend, BackendError, Provider};
use tokio_util::sync::CancellationToken;

use crate::backend::PostgresBackend;
use crate::settings::PostgresSettings;
use crate::shared::Shared;
use crate::startup;

/// The GTS backend type this plugin serves.
pub const BACKEND_TYPE: &str = "gts.cf.core.events.backend.v1~cf.core.backend.postgres.v1~";

pub struct PostgresBackendProvider {
    backend_type: GtsTypeId,
}

impl Default for PostgresBackendProvider {
    fn default() -> Self {
        Self {
            backend_type: GtsTypeId::new(BACKEND_TYPE),
        }
    }
}

#[async_trait]
impl Provider for PostgresBackendProvider {
    fn backend_type(&self) -> &GtsTypeId {
        &self.backend_type
    }

    /// Validates the settings and returns at once: the startup task connects
    /// in the background, so an unreachable database is `Unavailable` per
    /// operation, never a failed build (`docs/DESIGN.md` §3.1).
    async fn build(
        &self,
        settings: &serde_json::Map<String, serde_json::Value>,
        stop: CancellationToken,
    ) -> Result<Arc<dyn Backend>, BackendError> {
        let shared = Shared::new(PostgresSettings::parse(settings)?);
        startup::spawn(Arc::clone(&shared), stop);
        Ok(Arc::new(PostgresBackend::new(shared)))
    }
}
