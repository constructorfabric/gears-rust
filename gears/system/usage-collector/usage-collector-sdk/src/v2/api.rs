//! Consumer-facing SDK trait for the Usage Collector.
//!
//! Reconciliation metadata is REST-only for consumers, permanently: no
//! method here reports it, and the Plugin SPI's
//! [`get_reconciliation_metadata`](crate::v2::plugin_api::UsageCollectorPluginV2::get_reconciliation_metadata)
//! exists only because the gear is stateless and holds no counters of its
//! own.

use async_trait::async_trait;
use toolkit_odata::{CursorV1, ODataQuery, Page as ODataPage};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::v2::aggregation::{AggregationDimension, AggregationResult};
use crate::v2::error::UsageCollectorError;
use crate::v2::feed::{FeedPage, FeedStart, FeedSubscription};
use crate::v2::filter::MetadataFilter;
use crate::v2::models::{CreateUsageRecord, MeterTypeId, UsageRecord};
use crate::v2::time_range::TimeRange;

/// Consumer-facing API for Usage Collector operations.
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-entity-security-context:p1
#[async_trait]
pub trait UsageCollectorClientV2: Send + Sync + 'static {
    /// Create a batch of usage records.
    ///
    /// Takes identity-free [`CreateUsageRecord`] submissions: the returned
    /// record's `id` is derived deterministically from the dedup identity (see
    /// [`crate::v2::id::derive_usage_record_id`]), never supplied by the caller.
    /// An exact-equality retry under the same dedup identity returns the
    /// previously persisted record; a canonical-field mismatch surfaces as
    /// [`UsageCollectorError::Conflict`].
    ///
    /// A covered period that is inverted, or that carries a bound finer than
    /// microsecond precision, surfaces as
    /// [`UsageCollectorError::InvalidArgument`] — rejected before the identity
    /// derivation runs, never truncated.
    ///
    /// Per-record outcomes are aligned with the input order.
    async fn create_usage_records(
        &self,
        ctx: &SecurityContext,
        records: Vec<CreateUsageRecord>,
    ) -> Result<Vec<Result<UsageRecord, UsageCollectorError>>, UsageCollectorError>;

    /// Bulk historical import on its own route.
    ///
    /// Stamps [`RecordOrigin::Backfill`] and admits the covered periods the
    /// live past tolerance rejects. Validation is otherwise identical to
    /// [`Self::create_usage_records`], the future tolerance included: this
    /// route lifts the past bound and nothing else. Batch-only, because an
    /// import is a batch.
    ///
    /// An invalidation whose covered period is older than the live past
    /// tolerance is refused on the live route and belongs here, which makes
    /// correcting closed history the ordinary use of this route.
    ///
    /// An entry whose covered period ends further back than the deployment's
    /// configured backfill window is authorized against the `usage_record`
    /// `backfill` action instead of `create` — the elevated grant an import
    /// job holds and a plain emitter does not. One batch may mix the two.
    ///
    /// *Workload* isolation is a gear-level obligation and is **not
    /// implemented**: this route shares the live path's runtime, connection
    /// pool and fan-out budget, so a bulk import can still degrade live
    /// ingestion latency. What it does own is its origin marker, its
    /// covered-period bounds and its own PDP labelling.
    ///
    /// Per-record outcomes are aligned with the input order.
    ///
    /// [`RecordOrigin::Backfill`]: crate::v2::RecordOrigin::Backfill
    async fn backfill_usage_records(
        &self,
        ctx: &SecurityContext,
        records: Vec<CreateUsageRecord>,
    ) -> Result<Vec<Result<UsageRecord, UsageCollectorError>>, UsageCollectorError>;

    /// Get a single usage record by its `id`.
    ///
    /// Returns the persisted record on `Ok`; an unknown `id` surfaces
    /// as [`UsageCollectorError::NotFound`]. The read runs under the
    /// caller's compiled PDP scope, so a record outside it is
    /// [`UsageCollectorError::NotFound`] too — this surface is never an
    /// existence oracle.
    async fn get_usage_record(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<UsageRecord, UsageCollectorError>;

    /// Aggregated query over one meter and one range.
    ///
    /// Carries no aggregation parameter: the fold is resolved from the
    /// queried type's declaration, so no request is well-formed and
    /// semantically wrong. A withdrawn record and its invalidation each
    /// contribute nothing.
    ///
    /// `time_range` is mandatory and typed — never a `$filter` conjunct, and a
    /// predicate naming either covered-period bound is rejected rather than
    /// honoured. Selection reads the period end alone; see
    /// [`TimeRange::contains_window_end`](crate::v2::TimeRange::contains_window_end).
    async fn query_aggregated_usage_records(
        &self,
        ctx: &SecurityContext,
        gts_type_id: MeterTypeId,
        time_range: TimeRange,
        query: &ODataQuery,
        metadata_filter: &[MetadataFilter],
        group_by: &[AggregationDimension],
    ) -> Result<AggregationResult, UsageCollectorError>;

    /// Keyset-paginated ledger read over one meter and one range.
    ///
    /// `time_range` is mandatory and typed, selecting on the covered-period
    /// end exactly as on [`Self::query_aggregated_usage_records`]. Entries
    /// are returned as persisted — a withdrawn record and its invalidation
    /// both appear, because this is a ledger path rather than a derived
    /// view; excluding them from a locally computed fold is the reader's
    /// obligation.
    async fn list_usage_records(
        &self,
        ctx: &SecurityContext,
        gts_type_id: MeterTypeId,
        time_range: TimeRange,
        query: &ODataQuery,
        metadata_filter: &[MetadataFilter],
    ) -> Result<ODataPage<UsageRecord>, UsageCollectorError>;

    /// Replay-safe feed page in feed order.
    ///
    /// Snapshot-consistent, unlike the query paths: a consumer that must not
    /// miss entries reads this and not `list_usage_records`. `start` names
    /// where the read begins — `FeedStart::Oldest` for a first connect, which
    /// is the oldest entry the subscription retains, and
    /// `FeedStart::After(cursor)` to resume. `until`, a later cursor, bounds a
    /// replay.
    ///
    /// `limit` is bounded by [`MAX_PAGE_SIZE`](crate::v2::MAX_PAGE_SIZE): the
    /// gear refuses `Some(0)` and anything above it as
    /// [`UsageCollectorError::InvalidArgument`] on `limit`, and `None` takes
    /// the gear's default.
    async fn read_usage_feed(
        &self,
        ctx: &SecurityContext,
        subscription: &FeedSubscription,
        start: FeedStart<&CursorV1>,
        until: Option<&CursorV1>,
        limit: Option<u64>,
    ) -> Result<FeedPage<CursorV1>, UsageCollectorError>;
}
