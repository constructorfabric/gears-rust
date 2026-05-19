//! Insert port abstraction for testable DB writes.
//!
//! Decouples `create_usage_record` from `PgPool` so unit tests can inject
//! a mock without a live database.

use async_trait::async_trait;
use usage_collector_sdk::models::UsageRecord;

use crate::domain::error::StoragePluginError;

#[async_trait]
pub trait InsertPort: Send + Sync {
    async fn insert_usage_record(&self, record: &UsageRecord) -> Result<u64, StoragePluginError>;
}
