//! Local (in-process) client for the usage-collector module.
//!
//! Registered in `ClientHub` during `init()` as the consumer-facing
//! `UsageCollectorClientV1`. The REST surface goes straight through the
//! domain [`Service`] and does not pass through this client.
//!
//! `list_usage_records` and `query_aggregated_usage_records` are both
//! realized by the `usage-query` feature and delegate to the
//! same-named methods on [`Service`] (PDP authorization, PDP constraint
//! composition into the `OData` filter, plugin SPI dispatch).

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page as ODataPage};
use toolkit_security::SecurityContext;
use usage_collector_sdk::{
    AggregationResult, AggregationSpec, MetadataFilter, UsageCollectorClientV1,
    UsageCollectorError, UsageRecord, UsageType, UsageTypeGtsId,
};
use uuid::Uuid;

use super::Service;

/// Local client wrapping the usage-collector service.
#[domain_model]
pub struct UsageCollectorLocalClient {
    svc: Arc<Service>,
}

impl UsageCollectorLocalClient {
    #[must_use]
    pub fn new(svc: Arc<Service>) -> Self {
        Self { svc }
    }
}

#[async_trait]
impl UsageCollectorClientV1 for UsageCollectorLocalClient {
    async fn create_usage_record(
        &self,
        ctx: &SecurityContext,
        record: UsageRecord,
    ) -> Result<UsageRecord, UsageCollectorError> {
        self.svc.create_usage_record(ctx, record).await
    }

    async fn create_usage_records(
        &self,
        ctx: &SecurityContext,
        records: Vec<UsageRecord>,
    ) -> Result<Vec<Result<UsageRecord, UsageCollectorError>>, UsageCollectorError> {
        self.svc.create_usage_records(ctx, records).await
    }

    async fn get_usage_record(
        &self,
        ctx: &SecurityContext,
        uuid: Uuid,
    ) -> Result<UsageRecord, UsageCollectorError> {
        self.svc.get_usage_record(ctx, uuid).await
    }

    // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-submit
    async fn query_aggregated_usage_records(
        &self,
        ctx: &SecurityContext,
        gts_id: UsageTypeGtsId,
        query: &ODataQuery,
        metadata_filter: &[MetadataFilter],
        aggregation: AggregationSpec,
    ) -> Result<AggregationResult, UsageCollectorError> {
        self.svc
            .query_aggregated_usage_records(ctx, gts_id, query, metadata_filter, aggregation)
            .await
    }
    // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-aggregated:p1:inst-aggregated-submit

    // @cpt-begin:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-submit
    async fn list_usage_records(
        &self,
        ctx: &SecurityContext,
        gts_id: UsageTypeGtsId,
        query: &ODataQuery,
        metadata_filter: &[MetadataFilter],
    ) -> Result<ODataPage<UsageRecord>, UsageCollectorError> {
        self.svc
            .list_usage_records(ctx, gts_id, query, metadata_filter)
            .await
    }
    // @cpt-end:cpt-cf-usage-collector-flow-usage-query-query-raw:p1:inst-raw-submit

    async fn deactivate_usage_record(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), UsageCollectorError> {
        self.svc.deactivate_usage_record(ctx, id).await
    }

    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-submit
    async fn create_usage_type(
        &self,
        ctx: &SecurityContext,
        usage_type: UsageType,
    ) -> Result<UsageType, UsageCollectorError> {
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-service-call
        self.svc.create_usage_type(ctx, usage_type).await
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-service-call
    }
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-submit

    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-submit
    async fn get_usage_type(
        &self,
        ctx: &SecurityContext,
        gts_id: UsageTypeGtsId,
    ) -> Result<UsageType, UsageCollectorError> {
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-service-call
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-return
        self.svc.get_usage_type(ctx, gts_id).await
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-return
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-service-call
    }
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-submit

    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-submit
    async fn list_usage_types(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<ODataPage<UsageType>, UsageCollectorError> {
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-service-call
        // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-return
        self.svc.list_usage_types(ctx, query).await
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-return
        // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-service-call
    }
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-submit

    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-pdp-authorize
    async fn delete_usage_type(
        &self,
        ctx: &SecurityContext,
        gts_id: UsageTypeGtsId,
    ) -> Result<(), UsageCollectorError> {
        self.svc.delete_usage_type(ctx, gts_id).await
    }
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-pdp-authorize
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "local_client_tests.rs"]
mod local_client_tests;
