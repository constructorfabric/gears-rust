//! Storage plugin trait for usage-collector backends.

use async_trait::async_trait;

use crate::error::UsageCollectorError;
use crate::models::UsageRecord;

/// Backend storage adapter for usage records.
///
/// Plugins register via GTS; the gateway resolves the active instance and delegates writes.
///
/// # Batch semantics
///
/// Override [`create_usage_records_batch`] when the backend supports bulk inserts
/// (e.g. TimescaleDB `COPY` or a multi-row `INSERT`). The default sequentially calls
/// [`create_usage_record`] and returns on the first error, leaving the remaining
/// records unwritten — acceptable for small batches but inefficient at scale.
///
/// [`create_usage_records_batch`]: Self::create_usage_records_batch
#[async_trait]
pub trait UsageCollectorPluginClientV1: Send + Sync {
    /// Store one usage record (idempotent upsert where applicable).
    async fn create_usage_record(&self, record: UsageRecord) -> Result<(), UsageCollectorError>;

    /// Store multiple records in one call where the backend supports it.
    ///
    /// The default implementation calls [`Self::create_usage_record`] sequentially.
    /// Backends that support bulk inserts should override this for efficiency.
    async fn create_usage_records_batch(
        &self,
        records: Vec<UsageRecord>,
    ) -> Result<(), UsageCollectorError> {
        for record in records {
            self.create_usage_record(record).await?;
        }
        Ok(())
    }
}
