use async_trait::async_trait;
use usage_collector_sdk::models::{AggregationQuery, AggregationResult, RawQuery, UsageRecord};
use usage_collector_sdk::{Page, UsageCollectorError};

#[async_trait]
pub trait QueryPort: Send + Sync {
    async fn query_aggregated(
        &self,
        query: AggregationQuery,
    ) -> Result<Vec<AggregationResult>, UsageCollectorError>;

    async fn query_raw(&self, query: RawQuery) -> Result<Page<UsageRecord>, UsageCollectorError>;
}
