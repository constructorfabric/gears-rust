use std::sync::Arc;
use std::time::Duration;

use crate::config::{StorageBackend, StorageConfig};
use crate::directory_backend::DirectoryBackend;

/// No shared storage plugins are available in this build.
pub enum StoragePluginHandle {}

impl StoragePluginHandle {
    /// Stops the selected storage plugin.
    pub async fn stop(self) {
        match self {}
    }
}

/// Selects in-memory storage or reports that the requested backend is unavailable.
///
/// # Errors
/// Returns an error when a shared storage backend is selected.
pub async fn build_selected_backend(
    config: StorageConfig,
    _grace: Duration,
) -> anyhow::Result<Option<(Arc<dyn DirectoryBackend>, StoragePluginHandle)>> {
    let _ = (&config.k8s, &config.postgres, &config.redis);
    match config.backend {
        StorageBackend::Memory => Ok(None),
        StorageBackend::K8s => {
            anyhow::bail!("storage backend 'k8s' is unavailable; enable storage-k8s")
        }
        StorageBackend::Postgres => {
            anyhow::bail!("storage backend 'postgres' is unavailable; enable storage-postgres")
        }
        StorageBackend::Redis => {
            anyhow::bail!("storage backend 'redis' is unavailable; enable storage-redis")
        }
    }
}
