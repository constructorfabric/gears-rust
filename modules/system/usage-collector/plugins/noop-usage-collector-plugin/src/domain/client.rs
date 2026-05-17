use async_trait::async_trait;

use usage_collector_sdk::models::{AggregationQuery, AggregationResult, RawQuery, UsageRecord};
use usage_collector_sdk::{Page, PageInfo, UsageCollectorError, UsageCollectorPluginClientV1};

use super::service::Service;

#[async_trait]
impl UsageCollectorPluginClientV1 for Service {
    async fn create_usage_record(&self, _record: UsageRecord) -> Result<(), UsageCollectorError> {
        Ok(())
    }

    // @cpt-begin:cpt-cf-usage-collector-algo-query-api-noop-stubs:p1:inst-noop-1
    // Stub: noop storage never holds records; return an empty aggregation set.
    // There is no error path here because the implementation does not access
    // storage and does not validate the query — Feature 3 explicitly defines
    // the noop semantics as "ignore the query, return empty, no error".
    async fn query_aggregated(
        &self,
        _query: AggregationQuery,
    ) -> Result<Vec<AggregationResult>, UsageCollectorError> {
        Ok(vec![])
    }
    // @cpt-end:cpt-cf-usage-collector-algo-query-api-noop-stubs:p1:inst-noop-1

    // @cpt-begin:cpt-cf-usage-collector-algo-query-api-noop-stubs:p2:inst-noop-2
    // Stub: noop storage never holds records; return an empty page that still
    // echoes the caller-requested `page_size` in `PageInfo::limit` so callers
    // can rely on the response shape even against the noop backend. No error
    // path: storage is intentionally absent and the query is ignored.
    async fn query_raw(&self, query: RawQuery) -> Result<Page<UsageRecord>, UsageCollectorError> {
        Ok(Page::new(
            vec![],
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: u64::from(query.page_size),
            },
        ))
    }
    // @cpt-end:cpt-cf-usage-collector-algo-query-api-noop-stubs:p2:inst-noop-2
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod client_tests;
