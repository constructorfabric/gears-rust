use async_trait::async_trait;

use crate::error::UsageCollectorError;
use crate::models::{ModuleConfig, UsageRecord};

/// Gateway-facing API trait for the usage collector.
///
/// Implemented by the gateway module's local client (`UsageCollectorLocalClient`)
/// and by remote client modules (e.g. `usage-collector-rest-client`).
///
/// # Security invariant
///
/// This trait is **never** registered in `ClientHub`. Passing it through the hub
/// would allow any module to push unvalidated records directly to the collector,
/// bypassing the authorized-emitter path. Always supply it by constructor argument
/// to the emitter.
#[async_trait]
pub trait UsageCollectorClientV1: Send + Sync {
    /// Create one usage record at the collector gateway (ingest).
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError`] on transient or permanent delivery failure.
    async fn create_usage_record(&self, record: UsageRecord) -> Result<(), UsageCollectorError>;

    /// Retrieve per-module configuration from the collector.
    ///
    /// Returns the set of metrics `module_name` is allowed to emit.
    /// Extensible: future versions may include rate limit config, max metadata size, etc.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError`] if the module is not configured or the call fails.
    async fn get_module_config(
        &self,
        module_name: &str,
    ) -> Result<ModuleConfig, UsageCollectorError>;
}
