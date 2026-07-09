//! No-op storage backend for the Usage Collector storage Plugin SPI.
//!
//! [`NoopBackend`] implements [`usage_collector_sdk::UsageCollectorPluginV1`]
//! and persists nothing. It exists so the plugin-host binding resolves
//! end-to-end in development and testing without a real database backend:
//! the plugin still performs the full GTS registration handshake and
//! registers its scoped client in `ClientHub`, but every SPI operation
//! returns a well-formed default response. MUST NOT be used in production.

use async_trait::async_trait;
use toolkit_odata::{ODataQuery, Page as ODataPage};
use uuid::Uuid;

use usage_collector_sdk::{
    AggregationResult, AggregationSpec, MetadataFilter, UsageCollectorPluginError,
    UsageCollectorPluginV1, UsageRecord, UsageType, UsageTypeGtsId,
};

#[derive(Debug, Default)]
pub struct NoopBackend;

impl NoopBackend {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl UsageCollectorPluginV1 for NoopBackend {
    async fn create_usage_record(
        &self,
        record: UsageRecord,
    ) -> Result<UsageRecord, UsageCollectorPluginError> {
        Ok(record)
    }

    async fn create_usage_records(
        &self,
        records: Vec<UsageRecord>,
    ) -> Result<Vec<Result<UsageRecord, UsageCollectorPluginError>>, UsageCollectorPluginError>
    {
        if records.is_empty() {
            return Err(UsageCollectorPluginError::internal(
                "create_usage_records called with an empty batch (host-contract breach)",
            ));
        }
        Ok(records.into_iter().map(Ok).collect())
    }

    async fn get_usage_record(&self, uuid: Uuid) -> Result<UsageRecord, UsageCollectorPluginError> {
        Err(UsageCollectorPluginError::UsageRecordNotFound { id: uuid })
    }

    async fn query_aggregated_usage_records(
        &self,
        _gts_id: UsageTypeGtsId,
        _query: &ODataQuery,
        _metadata_filter: &[MetadataFilter],
        _aggregation: AggregationSpec,
    ) -> Result<AggregationResult, UsageCollectorPluginError> {
        Ok(AggregationResult {
            buckets: Vec::new(),
        })
    }

    async fn list_usage_records(
        &self,
        _gts_id: UsageTypeGtsId,
        _query: &ODataQuery,
        _metadata_filter: &[MetadataFilter],
    ) -> Result<ODataPage<UsageRecord>, UsageCollectorPluginError> {
        Ok(ODataPage::empty(0))
    }

    async fn deactivate_usage_record(&self, id: Uuid) -> Result<(), UsageCollectorPluginError> {
        Err(UsageCollectorPluginError::UsageRecordNotFound { id })
    }

    // @cpt-flow:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1
    async fn create_usage_type(
        &self,
        usage_type: UsageType,
    ) -> Result<UsageType, UsageCollectorPluginError> {
        Ok(usage_type)
    }

    async fn get_usage_type(
        &self,
        gts_id: UsageTypeGtsId,
    ) -> Result<UsageType, UsageCollectorPluginError> {
        Err(UsageCollectorPluginError::UsageTypeNotFound { gts_id })
    }

    async fn list_usage_types(
        &self,
        _query: &ODataQuery,
    ) -> Result<ODataPage<UsageType>, UsageCollectorPluginError> {
        Ok(ODataPage::empty(0))
    }

    async fn delete_usage_type(
        &self,
        gts_id: UsageTypeGtsId,
    ) -> Result<(), UsageCollectorPluginError> {
        Err(UsageCollectorPluginError::UsageTypeNotFound { gts_id })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "plugin_tests.rs"]
mod plugin_tests;
