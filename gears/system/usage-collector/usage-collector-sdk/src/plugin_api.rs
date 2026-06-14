//! Storage Plugin SPI for the Usage Collector.

use async_trait::async_trait;
use toolkit_odata::{ODataQuery, Page as ODataPage};
use uuid::Uuid;

use crate::error::UsageCollectorPluginError;
use crate::models::{
    AggregationResult, AggregationSpec, MetadataFilter, TimeWindow, UsageRecord, UsageType,
    UsageTypeGtsId,
};

/// Backend storage adapter trait implemented by
/// `usage-collector-plugin-<backend>` crates.
///
/// Plugins are pure persistence: authorization and shape validation are
/// the gateway's responsibility.
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-contract-storage-plugin:p1
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-nfr-plugin-contract-stability:p1
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-principle-contract-stability:p1
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-adr-contract-stability:p1
#[async_trait]
pub trait UsageCollectorPluginV1: Send + Sync + 'static {
    /// Persist a single usage record.
    ///
    /// An exact-equality retry under the same idempotency key returns
    /// the previously persisted row.
    async fn create_usage_record(
        &self,
        record: UsageRecord,
    ) -> Result<UsageRecord, UsageCollectorPluginError>;

    /// Persist a batch of usage records.
    ///
    /// Per-record outcomes are aligned with the input order.
    async fn create_usage_records(
        &self,
        records: Vec<UsageRecord>,
    ) -> Result<Vec<Result<UsageRecord, UsageCollectorPluginError>>, UsageCollectorPluginError>;

    /// Get a single usage record by its `uuid`.
    async fn get_usage_record(&self, uuid: Uuid) -> Result<UsageRecord, UsageCollectorPluginError>;

    /// Aggregated query over usage records.
    async fn query_aggregated_usage_records(
        &self,
        gts_id: UsageTypeGtsId,
        window: TimeWindow,
        query: &ODataQuery,
        metadata_filter: &[MetadataFilter],
        aggregation: AggregationSpec,
    ) -> Result<AggregationResult, UsageCollectorPluginError>;

    /// Keyset-paginated list of usage records.
    async fn list_usage_records(
        &self,
        gts_id: UsageTypeGtsId,
        window: TimeWindow,
        query: &ODataQuery,
        metadata_filter: &[MetadataFilter],
    ) -> Result<ODataPage<UsageRecord>, UsageCollectorPluginError>;

    /// Deactivate a usage record.
    ///
    /// On `Ok(())`, the targeted record and every active record that
    /// compensates it are atomically flipped to `inactive`.
    async fn deactivate_usage_record(&self, id: Uuid) -> Result<(), UsageCollectorPluginError>;

    /// Create a usage type.
    async fn create_usage_type(
        &self,
        usage_type: UsageType,
    ) -> Result<UsageType, UsageCollectorPluginError>;

    /// Get a usage type by `gts_id`.
    async fn get_usage_type(
        &self,
        gts_id: UsageTypeGtsId,
    ) -> Result<UsageType, UsageCollectorPluginError>;

    /// List usage types ordered by `gts_id` ascending.
    async fn list_usage_types(
        &self,
        query: &ODataQuery,
    ) -> Result<ODataPage<UsageType>, UsageCollectorPluginError>;

    /// Delete a usage type.
    async fn delete_usage_type(
        &self,
        gts_id: UsageTypeGtsId,
    ) -> Result<(), UsageCollectorPluginError>;
}
