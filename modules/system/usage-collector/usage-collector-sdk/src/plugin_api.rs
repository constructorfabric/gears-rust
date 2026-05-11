//! Storage plugin trait for usage-collector backends.

use async_trait::async_trait;

use crate::error::UsageCollectorError;
use crate::models::UsageRecord;

/// Backend storage adapter for usage records.
///
/// Plugins register via GTS; the gateway resolves the active instance and delegates writes.
#[async_trait]
pub trait UsageCollectorPluginClientV1: Send + Sync {
    /// Create one usage record in storage (idempotent upsert where applicable).
    async fn create_usage_record(&self, record: UsageRecord) -> Result<(), UsageCollectorError>;
}
