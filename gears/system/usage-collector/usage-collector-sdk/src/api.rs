//! Consumer-facing SDK trait for the Usage Collector.

use async_trait::async_trait;
use toolkit_odata::{ODataQuery, Page as ODataPage};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::error::UsageCollectorError;
use crate::models::{
    AggregationResult, AggregationSpec, MetadataFilter, TimeWindow, UsageRecord, UsageType,
    UsageTypeGtsId,
};

/// Consumer-facing API for Usage Collector operations.
///
/// Obtained from `ClientHub` as `Arc<dyn UsageCollectorClientV1>`. The
/// canonical implementation performs PDP enforcement, usage-type validation,
/// and Plugin SPI dispatch — REST handlers are thin pass-throughs so
/// in-process and out-of-process callers share one authorization path.
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-entity-security-context:p1
#[async_trait]
pub trait UsageCollectorClientV1: Send + Sync + 'static {
    /// Create a single usage record.
    ///
    /// An exact-equality retry under the same idempotency key returns the
    /// previously persisted record; a canonical-field mismatch surfaces as
    /// [`UsageCollectorError::IdempotencyConflict`].
    async fn create_usage_record(
        &self,
        ctx: &SecurityContext,
        record: UsageRecord,
    ) -> Result<UsageRecord, UsageCollectorError>;

    /// Create a batch of usage records.
    ///
    /// Per-record outcomes are aligned with the input order.
    async fn create_usage_records(
        &self,
        ctx: &SecurityContext,
        records: Vec<UsageRecord>,
    ) -> Result<Vec<Result<UsageRecord, UsageCollectorError>>, UsageCollectorError>;

    /// Get a single usage record by its `uuid`.
    ///
    /// Returns the persisted record on `Ok`; an unknown `uuid` surfaces
    /// as [`UsageCollectorError::UsageRecordNotFound`].
    async fn get_usage_record(
        &self,
        ctx: &SecurityContext,
        uuid: Uuid,
    ) -> Result<UsageRecord, UsageCollectorError>;

    /// Aggregated query over usage records.
    async fn query_aggregated_usage_records(
        &self,
        ctx: &SecurityContext,
        gts_id: UsageTypeGtsId,
        window: TimeWindow,
        query: &ODataQuery,
        metadata_filter: &[MetadataFilter],
        aggregation: AggregationSpec,
    ) -> Result<AggregationResult, UsageCollectorError>;

    /// Keyset-paginated list of usage records.
    async fn list_usage_records(
        &self,
        ctx: &SecurityContext,
        gts_id: UsageTypeGtsId,
        window: TimeWindow,
        query: &ODataQuery,
        metadata_filter: &[MetadataFilter],
    ) -> Result<ODataPage<UsageRecord>, UsageCollectorError>;

    /// Deactivate a usage event.
    ///
    /// On success the targeted record and every active referencing
    /// compensation row are flipped to `inactive` atomically.
    // @cpt-begin:cpt-cf-usage-collector-state-event-deactivation-record-lifecycle:p1:inst-state-no-reactivation
    async fn deactivate_usage_record(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), UsageCollectorError>;
    // @cpt-end:cpt-cf-usage-collector-state-event-deactivation-record-lifecycle:p1:inst-state-no-reactivation

    /// Create a usage type.
    async fn create_usage_type(
        &self,
        ctx: &SecurityContext,
        usage_type: UsageType,
    ) -> Result<UsageType, UsageCollectorError>;

    /// Get a usage type by `gts_id`.
    async fn get_usage_type(
        &self,
        ctx: &SecurityContext,
        gts_id: UsageTypeGtsId,
    ) -> Result<UsageType, UsageCollectorError>;

    /// List usage types, paginated.
    async fn list_usage_types(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<ODataPage<UsageType>, UsageCollectorError>;

    /// Delete a usage type.
    async fn delete_usage_type(
        &self,
        ctx: &SecurityContext,
        gts_id: UsageTypeGtsId,
    ) -> Result<(), UsageCollectorError>;
}
