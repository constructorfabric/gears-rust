//! `TimescaleDB` storage plugin client.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use modkit_macros::domain_model;
use usage_collector_sdk::error::UsageRecordError;
use usage_collector_sdk::models::{
    AggregationQuery, AggregationResult, RawQuery, UsageKind, UsageRecord,
};
use usage_collector_sdk::{Page, UsageCollectorError, UsageCollectorPluginClientV1};

use crate::domain::insert_port::InsertPort;
use crate::domain::metrics::PluginMetrics;
use crate::domain::query_port::QueryPort;

/// Storage plugin client backed by a `TimescaleDB` connection pool.
#[domain_model]
pub struct TimescaleDbPluginClient {
    insert_port: Arc<dyn InsertPort>,
    query_port: Arc<dyn QueryPort>,
    metrics: Arc<dyn PluginMetrics>,
}

impl TimescaleDbPluginClient {
    /// Creates a new client wrapping the given insert port, query port, and metrics port.
    pub fn new(
        insert_port: Arc<dyn InsertPort>,
        query_port: Arc<dyn QueryPort>,
        metrics: Arc<dyn PluginMetrics>,
    ) -> Self {
        Self {
            insert_port,
            query_port,
            metrics,
        }
    }
}

#[async_trait]
impl UsageCollectorPluginClientV1 for TimescaleDbPluginClient {
    // @cpt-algo:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1
    // @cpt-flow:cpt-cf-usage-collector-flow-production-storage-plugin-storage-backend-ingest:p1
    async fn create_usage_record(&self, record: UsageRecord) -> Result<(), UsageCollectorError> {
        let start = Instant::now();

        // @cpt-begin:cpt-cf-usage-collector-flow-production-storage-plugin-storage-backend-ingest:p1:inst-flow-ing-1
        // Plugin entry point; called by the gateway when delegating record storage.
        // @cpt-end:cpt-cf-usage-collector-flow-production-storage-plugin-storage-backend-ingest:p1:inst-flow-ing-1

        // @cpt-begin:cpt-cf-usage-collector-flow-production-storage-plugin-storage-backend-ingest:p1:inst-flow-ing-2
        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1:inst-cur-1
        if !record.value.is_finite() {
            self.metrics.record_schema_validation_error();
            return Err(UsageRecordError::invalid_argument()
                .with_constraint(format!("value must be finite, got: {}", record.value))
                .create());
        }
        if record.kind == UsageKind::Counter && record.value < 0.0 {
            self.metrics.record_schema_validation_error();
            return Err(UsageRecordError::invalid_argument()
                .with_constraint(format!(
                    "counter value must be non-negative, got: {}",
                    record.value
                ))
                .create());
        }
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1:inst-cur-1

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1:inst-cur-2
        if record.kind == UsageKind::Counter && record.idempotency_key.trim().is_empty() {
            self.metrics.record_schema_validation_error();
            return Err(UsageRecordError::invalid_argument()
                .with_constraint("counter records require a non-empty idempotency_key")
                .create());
        }
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1:inst-cur-2
        // @cpt-end:cpt-cf-usage-collector-flow-production-storage-plugin-storage-backend-ingest:p1:inst-flow-ing-2

        // @cpt-begin:cpt-cf-usage-collector-flow-production-storage-plugin-storage-backend-ingest:p1:inst-flow-ing-3
        let result = self.insert_port.insert_usage_record(&record).await;
        // @cpt-end:cpt-cf-usage-collector-flow-production-storage-plugin-storage-backend-ingest:p1:inst-flow-ing-3

        let rows_affected = match result {
            Ok(n) => n,
            Err(e) => {
                // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1:inst-cur-4
                // @cpt-begin:cpt-cf-usage-collector-flow-production-storage-plugin-storage-backend-ingest:p1:inst-flow-ing-4
                // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1:inst-cur-5
                self.metrics.record_ingestion_error();
                self.metrics
                    .record_ingestion_latency_ms(start.elapsed().as_secs_f64() * 1000.0);
                return Err(e.into());
                // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1:inst-cur-5
                // @cpt-end:cpt-cf-usage-collector-flow-production-storage-plugin-storage-backend-ingest:p1:inst-flow-ing-4
                // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1:inst-cur-4
            }
        };

        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        self.metrics.record_ingestion_latency_ms(elapsed_ms);
        if rows_affected == 0 {
            self.metrics.record_dedup();
        } else {
            self.metrics.record_ingestion_success();
        }

        // @cpt-begin:cpt-cf-usage-collector-flow-production-storage-plugin-storage-backend-ingest:p1:inst-flow-ing-5
        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1:inst-cur-7
        Ok(())
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1:inst-cur-7
        // @cpt-end:cpt-cf-usage-collector-flow-production-storage-plugin-storage-backend-ingest:p1:inst-flow-ing-5
    }

    // @cpt-algo:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1
    async fn query_aggregated(
        &self,
        query: AggregationQuery,
    ) -> Result<Vec<AggregationResult>, UsageCollectorError> {
        let start = Instant::now();
        let result = self.query_port.query_aggregated(query).await;
        self.metrics
            .record_query_latency_ms("aggregated", start.elapsed().as_secs_f64() * 1000.0);
        if result.is_ok() {
            self.metrics.record_query_success("aggregated");
        } else {
            self.metrics.record_query_error("aggregated");
        }
        result
    }

    // @cpt-algo:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1
    async fn query_raw(&self, query: RawQuery) -> Result<Page<UsageRecord>, UsageCollectorError> {
        let start = Instant::now();
        let result = self.query_port.query_raw(query).await;
        self.metrics
            .record_query_latency_ms("raw", start.elapsed().as_secs_f64() * 1000.0);
        if result.is_ok() {
            self.metrics.record_query_success("raw");
        } else {
            self.metrics.record_query_error("raw");
        }
        result
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod client_tests;
