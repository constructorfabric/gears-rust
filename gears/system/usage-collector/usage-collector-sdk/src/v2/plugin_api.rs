//! Storage Plugin SPI for the Usage Collector.

use async_trait::async_trait;
use toolkit_odata::{ODataQuery, ast};
use uuid::Uuid;

use crate::v2::aggregation::{AggregationDimension, AggregationFold, AggregationResult};
use crate::v2::error::UsageCollectorPluginError;
use crate::v2::feed::{FeedPage, FeedPosition, FeedStart, SubscribedMeters};
use crate::v2::filter::MetadataFilter;
use crate::v2::keyset::{Keyset, RecordPage};
use crate::v2::reconciliation::ReconciliationMetadata;
use crate::v2::stored::{MeterRef, MeteredRecord, StoredUsageRecord};
use crate::v2::time_range::TimeRange;

/// The consistency floor every Usage Collector read surface publishes
/// (DESIGN §3.10).
///
/// One copy, read by this SPI's own contract and by the gear's REST route
/// descriptions, because a floor stated twice drifts once.
pub const CONSISTENCY_FLOOR_STATEMENT: &str = "Consistency floor (DESIGN.md Section \
    3.10): after an ingestion call returns the persisted entry, that entry is durable. Under \
    eventual dedup level, an acknowledged entry can still lose a race before convergence \
    (DESIGN.md Section 3.1, Dedup level). This read surface is eventually consistent with no \
    upper bound relative to a same-tenant ingestion ack. No monotonic-reads guarantee at the \
    floor. The floor is per (tenant_id, gts_type_id). The floor claims no ordering of entries. \
    A consumer depending on a tighter bound than this floor must record that dependency in \
    its own design document, naming the plugin, the dimension, and the value; weakening a \
    published bound is a breaking change for every coupled consumer, and the Plugin SPI \
    publishes no runtime method for discovering a plugin's ceiling in v1.";

/// Backend storage adapter trait implemented by
/// `usage-collector-plugin-<backend>` crates.
///
/// # Consistency floor parity (DESIGN §3.10)
///
/// The same floor binds this SPI; nothing an implementation does relaxes or
/// strengthens it. [`CONSISTENCY_FLOOR_STATEMENT`] is the one copy, published
/// here so a plugin author depending on this crate alone can read it and so
/// the REST route descriptions render it rather than restating it.
///
/// # Obligations (DESIGN §3.3)
///
/// - **Pure persistence.** Authorization, type resolution, shape and quantity
///   validation and the invalidation copy rules are the gateway's. A plugin
///   that observes a violation of one has observed a host-contract breach and
///   returns [`UsageCollectorPluginError::Internal`]; it does not re-validate.
/// - **Key on the reference, never the identifier.** A meter crosses this
///   SPI as a [`MeterRef`]: `uuid` is the identity a plugin persists,
///   indexes and partitions by, and `id` is diagnostic — loggable, never
///   persisted and never derived from `uuid` or vice versa.
/// - **Acknowledge only what is durable.** A persist call returns only after
///   every entry it reports accepted is durable. A plugin may buffer and
///   coalesce calls, but a buffer holds only unacknowledged entries.
/// - **Declare a dedup level and meet it** (DESIGN §3.1 "Dedup level", published
///   in the plugin's deployment guide). A collision on the dedup identity
///   resolves by [`StoredUsageRecord::caller_supplied_eq`]: equal is absorbed and
///   returns the stored entry, different is
///   [`UsageCollectorPluginError::IdempotencyConflict`] carrying it.
/// - **Decide converged-only lookups.** See [`Self::get_usage_record`].
/// - **Be cancel-safe.** See below.
///
/// # Cancellation
///
/// The host dispatches every method under a deadline of its own, so a
/// future is dropped at whatever `await` it sits on when that expires.
/// **Cancellation is routine here, not an exceptional path**: a merely slow
/// plugin is cancelled in normal operation, and the host reports the drop as
/// a retryable failure. Two obligations follow.
///
/// *Nothing may depend on code after an `await` running* — there is no
/// cancellation callback and no guaranteed cleanup path, so state a method
/// must unwind cannot live across an `await` (an index updated after the
/// commit it describes, a buffer drained after the write that filled it).
///
/// *A coalesced flush acknowledges nobody on its own.* The buffering permission
/// above lets one in-flight call's future drive a flush holding another call's
/// entries. If that future is dropped, the flush is abandoned wherever it sits,
/// so a call whose entries it carried MUST NOT report them accepted — the
/// "acknowledge only what is durable" obligation binds each call on its own
/// terms, and a flush it did not observe complete is not durability it may
/// report.
///
/// *A cancelled write may already have committed.* Reads are side-effect
/// free, so a drop is unobservable on them;
/// [`Self::create_usage_records`] can be dropped between the backend
/// committing and the plugin observing it, which neither side can tell from
/// a write that never landed. The deterministic identity is what makes that
/// recoverable: a retry derives the same `id`s, so a
/// committed-but-unacknowledged entry is absorbed by the dedup rule above.
/// **A plugin whose persist path is not idempotent under the dedup identity
/// double-counts on every cancelled write.**
///
/// The storage plugin interface carries no erasure or redaction method
/// (`cpt-cf-usage-collector-dod-no-gear-local-privacy-workflow`).
// @cpt-dod:cpt-cf-usage-collector-dod-plugin-spi-sole-seam:p1
// @cpt-dod:cpt-cf-usage-collector-dod-plugin-spi-compat-rule:p1
// @cpt-dod:cpt-cf-usage-collector-dod-no-gear-local-privacy-workflow:p3
// @cpt-dod:cpt-cf-usage-collector-dod-consistency-floor-published:p1
// @cpt-dod:cpt-cf-usage-collector-dod-staleness-coupling-recorded:p1
#[async_trait]
pub trait UsageCollectorPluginV2: Send + Sync + 'static {
    /// Persist a batch of usage records.
    ///
    /// Per-record outcomes are aligned with the input order, and each is
    /// resolved as follows.
    ///
    /// A collision on the dedup identity (and so on `id`, derived from it)
    /// resolves by comparing caller-supplied fields
    /// ([`StoredUsageRecord::caller_supplied_eq`]). An equal submission is absorbed
    /// and the **stored** entry is returned, its `accepted_at` and `origin`
    /// included. A different one is
    /// [`UsageCollectorPluginError::IdempotencyConflict`] carrying the stored
    /// entry. The rule covers invalidations too: every invalidation of one
    /// record repeats that record's tenant, type, key and period and reads
    /// `entry_type = invalidation`, so all of them share one dedup identity —
    /// a second one with the same reason code is absorbed and one with another
    /// reason code conflicts. There is no separate at-most-one check.
    ///
    /// In each [`MeteredRecord`], `record().gts_type_uuid` is always
    /// `meter().uuid`; a plugin may key on either. That is a property of the
    /// type rather than an obligation on the host:
    /// [`MeteredRecord::new`] refuses a disagreeing pair, so there is no
    /// batch element here whose two meter identities differ, and a plugin
    /// needs no reconciliation of its own.
    ///
    /// Two same-identity entries in one call resolve later against earlier:
    /// the later is absorbed when its caller-supplied fields equal the earlier
    /// accepted entry's, and conflicts otherwise.
    async fn create_usage_records(
        &self,
        records: Vec<MeteredRecord>,
    ) -> Result<Vec<Result<StoredUsageRecord, UsageCollectorPluginError>>, UsageCollectorPluginError>;

    /// Get a single usage record by its `id`.
    ///
    /// `scope` is the caller's compiled PDP scope, projected into a
    /// `toolkit_odata` filter expression by
    /// `authz::scope_to_odata_filter` (gateway-side; never a plugin
    /// concern). The point lookup carries no caller-supplied filter of its
    /// own, so `scope` is the *whole* filter the row must satisfy — a row
    /// whose attribution tuple falls outside it MUST NOT be returned; the
    /// plugin reports `UsageRecordNotFound` exactly as it would for an
    /// `id` that does not exist at all. This is what keeps the by-id
    /// surface from acting as an existence oracle: the caller cannot tell
    /// "exists but not yours" apart from "does not exist".
    ///
    /// A withdrawn pair MUST be returned **as persisted** — both entries,
    /// the invalidation naming its target
    /// (`cpt-cf-usage-collector-adr-append-only-invalidation`). A plugin
    /// MUST NOT withhold a withdrawn entry from this path; the exclusion
    /// belongs to the fold instead
    /// ([`Self::query_aggregated_usage_records`]). A consumer folding
    /// entries it read here leaves a withdrawn pair out on its own side.
    ///
    /// **`converged_only`.** The gateway passes `true` when it resolves an
    /// invalidation's target and `false` on the caller-facing point read.
    /// With `true` the plugin applies `scope` first, returns the survivor once
    /// the identity has converged, never reports an acknowledged, retained
    /// entry missing, and answers
    /// [`UsageCollectorPluginError::UsageRecordNotConverged`] only until it can
    /// decide: within its convergence bound plus its query-path lag bound it
    /// returns the entry or [`UsageCollectorPluginError::UsageRecordNotFound`].
    /// A plugin whose every read is converged (a single linearizable primary)
    /// ignores the flag.
    async fn get_usage_record(
        &self,
        id: Uuid,
        scope: &ast::Expr,
        converged_only: bool,
    ) -> Result<StoredUsageRecord, UsageCollectorPluginError>;

    /// Compute the given fold over the authorized scope and `time_range`.
    ///
    /// `scope` is the caller's compiled PDP scope (gateway-side; never a
    /// plugin concern). **It is also AND-merged into `query.filter` before
    /// dispatch**, so applying `scope`, `query.filter`, or both is
    /// scope-correct either way — the conjunction is idempotent. Honouring
    /// `query.filter` alone is also *complete*, since it carries the
    /// caller's own conjuncts too. A plugin MUST NOT widen either, and MUST
    /// NOT serve a row that fails `scope`.
    ///
    /// `time_range` is a typed parameter and never appears in
    /// `query.filter`. The plugin MUST select an entry when
    /// `from <= window_end < to`
    /// (`cpt-cf-usage-collector-adr-window-end-selection`): the predicate
    /// reads the period end alone, needs no case for a point event
    /// (`window_start == window_end`), and MUST NOT match by overlap or by
    /// containment — those make adjacent ranges double count or drop
    /// entries. No selection predicate reads `window_start`.
    /// [`TimeRange::contains_window_end`] is the reference spelling for an
    /// in-process implementation; a SQL-backed plugin restates the same
    /// boundary in its own `WHERE` clause.
    ///
    /// **A withdrawn pair contributes nothing.** Two obligations rather
    /// than one conditional
    /// (`cpt-cf-usage-collector-adr-append-only-invalidation`), and the
    /// plugin MUST honour both:
    ///
    /// 1. An **invalidation entry** contributes nothing to any fold,
    ///    whether or not its target is in the selection.
    /// 2. A **record an accepted invalidation names** contributes nothing
    ///    either.
    ///
    /// The reference an entry carries ([`StoredUsageRecord::invalidation`]) is
    /// both what makes it an invalidation, for the first, and what names
    /// the record to leave out with it, for the second.
    ///
    /// A materialised aggregate MUST **recompute** over the affected range
    /// rather than absorb an appended term: no further term reverses `MAX`,
    /// `MIN` or `LATEST`. Append-only is a property of the ledger, not of a
    /// derived view.
    ///
    /// **What the gateway guarantees about `group_by`**, on every surface —
    /// REST, the in-process client and a direct service call alike:
    ///
    /// * every dimension is either one of the fixed
    ///   [`AggregationDimension`] variants or a metadata key the queried
    ///   meter's resolved declaration actually declares, checked fresh per
    ///   request against that declaration (Spec §3.11), so a plugin never
    ///   has to decide what an undeclared key means; and
    /// * **no dimension appears more than once**, by exact equality. A
    ///   bucket carries one value per `group_by` entry, in the caller's
    ///   order.
    #[allow(
        clippy::too_many_arguments,
        reason = "each parameter is a typed SPI input the fold needs; bundling them \
                  into one struct would hide which of them a plugin must honour"
    )]
    async fn query_aggregated_usage_records(
        &self,
        meter: &MeterRef,
        time_range: TimeRange,
        fold: AggregationFold,
        query: &ODataQuery,
        scope: &ast::Expr,
        metadata_filter: &[MetadataFilter],
        group_by: &[AggregationDimension],
    ) -> Result<AggregationResult, UsageCollectorPluginError>;

    /// Keyset-paginated ledger read over the authorized scope and
    /// `time_range`.
    ///
    /// `scope` is the caller's compiled PDP scope (gateway-side; never a
    /// plugin concern). **It is also AND-merged into `query.filter` before
    /// dispatch**, so applying `scope`, `query.filter`, or both is
    /// scope-correct either way — the conjunction is idempotent. Honouring
    /// `query.filter` alone is also *complete*, since it carries the
    /// caller's own conjuncts too. A plugin MUST NOT widen either, and MUST
    /// NOT serve a row that fails `scope`.
    ///
    /// `time_range` selects on the covered-period end under the same
    /// obligation as [`Self::query_aggregated_usage_records`], and is
    /// likewise absent from `query.filter`.
    ///
    /// A withdrawn pair MUST likewise be returned as persisted here, under
    /// the ledger obligation [`Self::get_usage_record`] states — both
    /// entries, though **not necessarily on one page**: the pair shares a
    /// `window_end` but not an `id`, and an admissible order names both, so
    /// whichever of the two it leads with, a page boundary can fall between
    /// them. A consumer leaving a pair out folds over a range it has read
    /// whole rather than over a single page.
    ///
    /// "No filter that selects them in or out" means none this method
    /// applies on its own initiative. A caller's `$filter` may name
    /// `invalidates` or `entry_type` — the filterable schema declares both,
    /// the former precisely so a consumer folding entries itself can find a
    /// withdrawn pair.
    ///
    /// `query.order` MUST be honoured: it is the keyset the page
    /// continuation is built from, so ignoring it drops rows across a page
    /// boundary. The gateway guarantees the slot is usable on every
    /// surface — REST, the in-process client, and a direct service call
    /// alike: `query.order` is non-empty, uses one sort direction
    /// throughout, names only never-null record attributes, names no key
    /// more than once, and names both `window_end` and `id`, so the sort
    /// tuple is globally unique. A plugin therefore needs no fallback
    /// keyset of its own, and an empty order is a gateway breach rather
    /// than a case to paper over.
    ///
    /// Those two names are guaranteed to be *present*, not to be last: a
    /// caller ordering by `id` is handed on as `(id, window_end)`. A
    /// plugin MUST read the order it is given rather than assume a
    /// position for either key.
    ///
    /// Every key in an admissible order is one of
    /// [`crate::v2::KEYSET_SAFE_RECORD_FIELDS`], and a plugin MUST resolve
    /// each of those names to a column — all of them are
    /// [`crate::v2::UsageRecordFilterField`] variants, so the field-to-column
    /// map `$filter` already needs covers them. A name it cannot resolve is a
    /// 500 on an order this SDK blessed.
    ///
    /// `keyset` is the continuation, and it is the gateway's to decode. A
    /// first page carries `None` and MUST be served from the start of the
    /// selected range with no seek predicate; a continuation carries the
    /// boundary values of the last row of the previous page, one per key of
    /// `query.order` and in the same sequence, in that order's single
    /// direction. A plugin builds its seek from `keyset` and `query.order`
    /// together and MUST NOT read `query.order`'s position for either
    /// canonical field — see the paragraphs above.
    ///
    /// `query.limit` is the page size, already bounded by
    /// [`crate::v2::MAX_PAGE_SIZE`] (the gateway refuses more and defaults
    /// an absent `$top` to it), so a plugin never sees an unbounded limit on
    /// a well-behaved dispatch. Clamp to it anyway: that is what stops a
    /// different or buggy caller from provoking a full-result-set read.
    ///
    /// `query.cursor` is `None` on every dispatch and MUST NOT be read.
    /// Encoding, decoding, signing or validating a wire cursor is the
    /// gateway's alone
    /// (`cpt-cf-usage-collector-dod-gateway-owned-cursor`,
    /// `cpt-cf-uc-plugin-dod-no-filter-widening-no-offset`). The gateway
    /// verifies a returned keyset's arity and direction against the order
    /// it dispatched, so a keyset of the wrong width is refused as a
    /// host-contract breach rather than served.
    ///
    /// `query.filter_hash` carries no guarantee on this method and MUST NOT
    /// be read: the gateway mints the token, holds its own fingerprint, and
    /// needs nothing from a plugin to bind a token to a query.
    ///
    /// [`Self::query_aggregated_usage_records`] is assigned no fingerprint
    /// either, and MUST NOT read the slot.
    async fn list_usage_records(
        &self,
        meter: &MeterRef,
        time_range: TimeRange,
        query: &ODataQuery,
        scope: &ast::Expr,
        metadata_filter: &[MetadataFilter],
        keyset: Option<&Keyset>,
    ) -> Result<RecordPage, UsageCollectorPluginError>;

    /// Snapshot-consistent feed page in feed order (§3.1).
    ///
    /// `start` names where the page begins. `FeedStart::After(position)` and
    /// `until` carry back positions this plugin issued; `FeedStart::Oldest`
    /// means the oldest position this plugin still serves for `subscription`
    /// under `scope` — never the head. `FeedStart` is open, so a plugin
    /// matches it with a wildcard arm and returns `Internal(detail)` there.
    /// `scope` is the compiled PDP scope. An entry outside it is absent.
    ///
    /// # Errors
    ///
    /// [`UsageCollectorPluginError::CursorBeyondRetention`] when retention has
    /// removed an entry of a subscribed type after `start`'s position, decided
    /// from what this plugin still holds rather than from the position's age.
    /// [`UsageCollectorPluginError::Transient`] on a retryable backend
    /// failure, and [`UsageCollectorPluginError::Internal`] otherwise — which
    /// is where a host-contract breach lands: a `start` mode this plugin does
    /// not know (the wildcard arm above), a position it did not issue, or a
    /// `limit` outside the published bound [`crate::v2::MAX_PAGE_SIZE`] —
    /// refused rather than clamped, for the reason that const records.
    ///
    /// Every entry on the returned page MUST carry a `gts_type_uuid` naming
    /// one of the references in `subscription`. A page is bounded by the
    /// subscription, so an entry outside it has no meter the caller asked
    /// for; the gear refuses such a page as a host-contract breach rather
    /// than serving it.
    ///
    /// `subscription` is non-empty and carries no repeated reference —
    /// [`SubscribedMeters::new`] decides both, so a plugin needs no
    /// defensive deduplication of its own and has no empty subscription to
    /// interpret.
    async fn read_feed_page(
        &self,
        subscription: &SubscribedMeters,
        scope: &ast::Expr,
        start: FeedStart<FeedPosition>,
        until: Option<FeedPosition>,
        limit: u64,
    ) -> Result<FeedPage<FeedPosition, StoredUsageRecord>, UsageCollectorPluginError>;

    /// Per-scope ingestion counters and watermarks.
    ///
    /// The counters cover the range; the watermarks do not.
    ///
    /// `fold` is the queried meter's **declared** fold, and it is what
    /// decides the branch of [`crate::v2::reconciliation::QuantitySummary`] this
    /// method returns — never the caller, and never this plugin's own
    /// judgment. A summing meter (`fold` is `Sum`) reports
    /// [`crate::v2::reconciliation::QuantitySummary::Accrued`]; every other fold
    /// reports [`crate::v2::reconciliation::QuantitySummary::Observations`].
    /// `Observations::latest` follows DESIGN §3.1's `LATEST` total order —
    /// greatest `window_end`, then greatest `accepted_at`, then greatest
    /// `id` — regardless of the declared fold's own ordering; `MAX` and
    /// `MIN` select the branch, never the ordering within it. An empty
    /// selection is a complete answer rather than an absent one and still
    /// splits by fold: `Accrued(0)` under `Sum`, `Observations(None)` under
    /// every other fold — both of which
    /// [`crate::v2::reconciliation::QuantitySummary::empty_for`] builds.
    /// A returned branch that disagrees with `fold` is a host-contract
    /// breach the gear answers with an internal error, not a caller error.
    ///
    /// # Errors
    ///
    /// [`UsageCollectorPluginError::Transient`] on a retryable backend
    /// failure, [`UsageCollectorPluginError::Internal`] otherwise. A tenant
    /// the compiled `scope` excludes answers exactly as one holding no
    /// entries, never as an error.
    async fn get_reconciliation_metadata(
        &self,
        tenant_id: Uuid,
        meter: &MeterRef,
        time_range: TimeRange,
        fold: AggregationFold,
        scope: &ast::Expr,
    ) -> Result<ReconciliationMetadata, UsageCollectorPluginError>;
}
