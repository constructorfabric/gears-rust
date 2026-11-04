//! Builds `SQLite` backends from registry entries.

use std::sync::Arc;

use async_trait::async_trait;
use event_broker_sdk::GtsTypeId;
use event_broker_sdk::backend::{Backend, BackendError, BuildContext, Provider};

use crate::backend::SqliteBackend;
use crate::settings::SqliteSettings;
use crate::shared::Shared;

/// The GTS backend type this plugin serves.
pub const BACKEND_TYPE: &str =
    toolkit_gts::gts_id!("cf.core.events.backend.v1~cf.core.backend.sqlite.v1~");

pub struct SqliteBackendProvider {
    backend_type: GtsTypeId,
}

impl Default for SqliteBackendProvider {
    fn default() -> Self {
        Self {
            backend_type: GtsTypeId::new(BACKEND_TYPE),
        }
    }
}

#[async_trait]
impl Provider for SqliteBackendProvider {
    fn backend_type(&self) -> &GtsTypeId {
        &self.backend_type
    }

    /// Validates the settings; nothing opens the file until `start`
    /// (`docs/DESIGN.md` §3.1).
    async fn build(&self, context: &BuildContext<'_>) -> Result<Arc<dyn Backend>, BackendError> {
        let shared = Shared::new(SqliteSettings::parse(context.settings)?, context.role);
        Ok(Arc::new(SqliteBackend::new(shared)))
    }
}
