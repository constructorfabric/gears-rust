//! Builds `MySQL`/`MariaDB` backends from registry entries.

use std::sync::Arc;

use async_trait::async_trait;
use event_broker_sdk::GtsTypeId;
use event_broker_sdk::backend::{Backend, BackendError, BuildContext, Provider};

use crate::backend::MysqlBackend;
use crate::settings::MysqlSettings;
use crate::shared::Shared;

/// The GTS backend type this plugin serves, for `MySQL` and `MariaDB` alike.
pub const BACKEND_TYPE: &str =
    toolkit_gts::gts_id!("cf.core.events.backend.v1~cf.core.backend.mysql.v1~");

pub struct MysqlBackendProvider {
    backend_type: GtsTypeId,
}

impl Default for MysqlBackendProvider {
    fn default() -> Self {
        Self {
            backend_type: GtsTypeId::new(BACKEND_TYPE),
        }
    }
}

#[async_trait]
impl Provider for MysqlBackendProvider {
    fn backend_type(&self) -> &GtsTypeId {
        &self.backend_type
    }

    /// Validates the settings; nothing connects until `start`
    /// (`docs/DESIGN.md` §3.1).
    async fn build(&self, context: &BuildContext<'_>) -> Result<Arc<dyn Backend>, BackendError> {
        let shared = Shared::new(MysqlSettings::parse(context.settings)?, context.role);
        Ok(Arc::new(MysqlBackend::new(shared)))
    }
}
