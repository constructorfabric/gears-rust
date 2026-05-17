//! Storage plugin trait for usage-collector backends.

use async_trait::async_trait;
use modkit_odata::Page;

use crate::error::UsageCollectorError;
use crate::models::{AggregationQuery, AggregationResult, RawQuery, UsageRecord};

/// Backend storage adapter for usage records.
///
/// Plugins register via GTS; the gateway resolves the active instance and delegates writes
/// as well as aggregated/raw read queries.
///
/// All query operations take the PDP-compiled [`modkit_security::AccessScope`] embedded
/// in [`AggregationQuery::scope`] / [`RawQuery::scope`]; the plugin contract intentionally
/// takes no separate `SecurityContext` — the gateway is the authorization gateway.
#[async_trait]
pub trait UsageCollectorPluginClientV1: Send + Sync {
    /// Create one usage record in storage (idempotent upsert where applicable).
    async fn create_usage_record(&self, record: UsageRecord) -> Result<(), UsageCollectorError>;

    // @cpt-begin:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-8
    /// Execute an aggregated usage query.
    ///
    /// The gateway has already compiled the PDP-derived [`modkit_security::AccessScope`]
    /// into [`AggregationQuery::scope`]; the plugin MUST apply that scope as a mandatory
    /// row filter (per `inst-plugin-contract-2`). If the result set would exceed the
    /// gateway-configured `MAX_AGG_ROWS`, the plugin MUST return
    /// `Err(UsageCollectorError::ResourceExhausted)` (built via
    /// [`crate::UsageRecordError::resource_exhausted`] with detail `"query result too large"`)
    /// instead of silently truncating.
    async fn query_aggregated(
        &self,
        query: AggregationQuery,
    ) -> Result<Vec<AggregationResult>, UsageCollectorError>;
    // @cpt-end:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-8

    // @cpt-begin:cpt-cf-usage-collector-algo-query-api-sdk-types:p2:inst-sdk-9
    /// Execute a raw paginated usage record query.
    ///
    /// The gateway has already compiled the PDP-derived [`modkit_security::AccessScope`]
    /// into [`RawQuery::scope`]. Returns at most `query.page_size` records ordered
    /// ascending by `(timestamp, id)` strictly greater than `query.cursor` when present.
    async fn query_raw(&self, query: RawQuery) -> Result<Page<UsageRecord>, UsageCollectorError>;
    // @cpt-end:cpt-cf-usage-collector-algo-query-api-sdk-types:p2:inst-sdk-9
}
