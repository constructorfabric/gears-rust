//! What more than one test module in this crate needs, written once.
//!
//! # Why this module exists
//!
//! Three suites had built their own copy of the same flat-`In` PDP fake, and
//! the two door suites had built their own copy of ten database-introspection
//! helpers on top of that — **twelve** byte-identical functions across
//! `api::rest::products_tests` and `api::rest::skus_tests`, plus a third
//! `FlatInResolver` in `authz_tests`. `FlatInResolver`'s own doc named the
//! reason: *"`authz_tests` is a private `#[cfg(test)]` sibling module, not a
//! reusable test-support crate."* That was true, and this module is the thing
//! whose absence it recorded.
//!
//! It matters more here than duplication usually does. This gear's Product and
//! SKU doors have already drifted apart six times, and a helper copied into
//! both suites is one more surface on which a repair can land in one and not
//! the other — a fix to a `SELECT` here, a widened predicate there, and the two
//! halves are silently measuring different things while both stay green.
//!
//! # What belongs here, and what does not
//!
//! Only what is genuinely **suite-agnostic**: reading a value back out of a
//! test database, and standing up a permissive PDP. A seed, a harness or a
//! request builder stays with its own suite, because those encode what a
//! particular door is being asked and moving them would hide the thing a
//! reader of that suite most needs to see.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use sea_orm::{ConnectionTrait, Database, DbBackend, FromQueryResult, Statement};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_gts::gts_id;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::infra::events;
use time::OffsetDateTime;

/// Degraded flat-`In` PDP fake: permits and emits a single flat
/// `In([allowed])` constraint over `OWNER_TENANT_ID` — **the shape the
/// production PDP returns for a PEP that advertises no tenant-subtree
/// capability** (this gear: [`PolicyEnforcer::new`] with no
/// `with_capabilities`). The request is ignored: the fake models a subject
/// authorized only for the single `allowed` tenant.
///
/// That first clause is what makes this a measurement rather than a
/// convenience, and review wave D's extraction dropped it — the wave verified
/// the *bodies* were byte-identical and did not compare the docs.
struct FlatInResolver {
    allowed: Uuid,
}

#[async_trait]
impl AuthZResolverApi for FlatInResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        vec![self.allowed],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

/// A fixture instant: `2026-09-02` at `hour`, UTC.
///
/// One fixture epoch for every suite: a second epoch makes `at(9)` mean two
/// instants.
///
/// **The count was never the trigger.** `harness()` is copied five times too
/// and stays copied: its forms differ only in an `.expect()` message, so
/// unifying it would edit five files to agree on a panic string. This one
/// differed in **meaning**, and that is what a hoist is for.
///
/// `.single()` rather than `.unwrap()`, which is the form one of the four
/// already used and the only one that says what it is asserting: that the
/// One UTC instant from its civil components — the fixture spelling that
/// replaced `chrono`'s `crate::test_support::utc(..)` when the gear
/// moved to `time`.
///
/// A helper rather than seventy-two inline conversions: `time` builds an
/// instant through a `Date` and a civil time, so the inline form is four
/// calls where chrono's was one, and the suite would have carried the
/// arithmetic in seventy-two places.
///
/// # Panics
///
/// On components that name no real instant, which is a fixture typo rather
/// than a runtime case.
#[must_use]
pub fn utc(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> OffsetDateTime {
    time::Date::from_calendar_date(
        year,
        time::Month::try_from(month).expect("a month of the year"),
        day,
    )
    .expect("a real date")
    .with_hms(hour, minute, second)
    .expect("a real civil time")
    .assume_utc()
}

/// civil time names exactly one instant.
#[must_use]
///
/// # Panics
/// Panics if the hour is outside the fixture date range.
pub fn at(hour: u32) -> OffsetDateTime {
    utc(
        2026,
        9,
        2,
        u8::try_from(hour).expect("an hour of the day"),
        0,
        0,
    )
}

/// [`FlatInResolver`] that counts its evaluations.
struct CountingFlatIn {
    inner: FlatInResolver,
    asked: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl AuthZResolverApi for CountingFlatIn {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.asked
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.evaluate(ctx, req).await
    }
}

/// [`flat_in_enforcer`] with a count of the PDP evaluations it made.
#[must_use]
pub fn counting_flat_in_enforcer(
    allowed: Uuid,
) -> (PolicyEnforcer, Arc<std::sync::atomic::AtomicUsize>) {
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    (
        PolicyEnforcer::new(Arc::new(CountingFlatIn {
            inner: FlatInResolver { allowed },
            asked: asked.clone(),
        })),
        asked,
    )
}

/// A [`PolicyEnforcer`] over [`FlatInResolver`], scoped to one tenant.
#[must_use]
pub fn flat_in_enforcer(allowed: Uuid) -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(FlatInResolver { allowed }))
}

/// An authenticated [`SecurityContext`] for `tenant`, with a fresh subject.
#[must_use]
///
/// # Panics
/// Panics if the fixed fixture identity cannot build a security context.
pub fn authed_ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .subject_type(gts_id!("cf.core.security.subject_user.v1~"))
        .token_scopes(vec!["*".to_owned()])
        .build()
        .expect("authed SecurityContext must build")
}

/// The tenant's one door-test author: every [`request`] of a tenant acts as the same
/// principal, so a draft's creator can edit it (D-404 refuses anyone else).
#[must_use]
///
/// # Panics
/// Panics if the fixed fixture identity cannot build a security context.
pub fn tenant_user(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(tenant.as_u128() ^ 0xa11ce))
        .subject_tenant_id(tenant)
        .subject_type(gts_id!("cf.core.security.subject_user.v1~"))
        .token_scopes(vec!["*".to_owned()])
        .build()
        .expect("tenant SecurityContext must build")
}

/// Run `sql` (a `SELECT ... AS v FROM ...`) on its own auxiliary connection
/// into `dsn` and return the single integer column it names `v`.
///
/// Its own connection, deliberately: the door harnesses pin `max_conns: 1` on
/// the production provider, so introspecting through it would contend with the
/// very statement under test.
///
/// # Panics
/// Panics if the fixture connection, query, or required result fails.
pub async fn raw_i64(dsn: &str, sql: &str) -> i64 {
    #[derive(Debug, FromQueryResult)]
    struct Row {
        v: i64,
    }

    let conn = Database::connect(dsn)
        .await
        .expect("open an auxiliary connection for test introspection");
    let row = Row::find_by_statement(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
        .one(&conn)
        .await
        .expect("the introspection query runs")
        .expect("an aggregate SELECT always returns exactly one row");
    conn.close().await.ok();
    row.v
}

/// [`raw_i64`] for a single nullable text column named `v`.
///
/// # Panics
/// Panics if the fixture connection or query fails.
pub async fn raw_string_opt(dsn: &str, sql: &str) -> Option<String> {
    #[derive(Debug, FromQueryResult)]
    struct Row {
        v: Option<String>,
    }

    let conn = Database::connect(dsn)
        .await
        .expect("open an auxiliary connection for test introspection");
    let row = Row::find_by_statement(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
        .one(&conn)
        .await
        .expect("the introspection query runs")
        .expect("the row this test just wrote must exist");
    conn.close().await.ok();
    row.v
}

/// Drop `table` from the database at `dsn`, for the seams that need one gone.
///
/// # Panics
/// Panics if the fixture connection or table deletion fails.
pub async fn drop_table(dsn: &str, table: &str) {
    let conn = Database::connect(dsn)
        .await
        .expect("open an auxiliary connection to drop a table");
    conn.execute_unprepared(&format!("DROP TABLE {table};"))
        .await
        .expect("drop the table this seam needs gone");
    conn.close().await.ok();
}

/// The column names `table` declares, as the executed schema holds them.
///
/// # Panics
/// Panics if the fixture table does not exist or its columns cannot be read.
pub async fn table_columns(dsn: &str, table: &str) -> Vec<String> {
    let joined = raw_string_opt(
        dsn,
        &format!("SELECT group_concat(name, ',') AS v FROM pragma_table_info('{table}')"),
    )
    .await
    .expect("the migration chain created this table, so the pragma answers a non-empty list");
    joined.split(',').map(ToOwned::to_owned).collect()
}

/// How many SDK-envelope outbox rows carry this event type.
///
/// Counted on `_body` rather than `_incoming`: `_incoming` is a staging table
/// the running sequencer drains, so a count taken after the response has raced
/// the pipeline.
pub async fn enqueued_event_count(dsn: &str, payload_type: &str) -> i64 {
    let body_table = format!("{}_body", events::OUTBOX_TABLE_PREFIX);
    raw_i64(
        dsn,
        &format!("SELECT COUNT(*) AS v FROM {body_table} WHERE json_extract(CAST(payload AS TEXT), '$.type') = '{payload_type}'"),
    )
    .await
}

/// The business data of the newest SDK envelope carrying this event type.
///
/// `ORDER BY id DESC LIMIT 1` rather than a bare filter, so a case that
/// enqueued the same token twice reads the one it just wrote. The `payload`
/// column is a `BLOB`; `CAST(.. AS TEXT)` is what lets [`raw_string_opt`]'s
/// single-text-column shape read it.
///
/// # Panics
/// Panics if no matching event exists or its payload is not JSON.
pub async fn enqueued_event_envelope(dsn: &str, payload_type: &str) -> serde_json::Value {
    let body_table = format!("{}_body", events::OUTBOX_TABLE_PREFIX);
    let payload = raw_string_opt(
        dsn,
        &format!(
            "SELECT CAST(payload AS TEXT) AS v FROM {body_table} \
             WHERE json_extract(CAST(payload AS TEXT), '$.type') = '{payload_type}' ORDER BY id DESC LIMIT 1"
        ),
    )
    .await
    .expect("the enqueued row carries a payload");
    serde_json::from_str::<serde_json::Value>(&payload).expect("the door enqueues a JSON envelope")
        ["data"]
        .clone()
}

/// How many idempotency rows carry `client_key`.
pub async fn idempotency_rows_for(dsn: &str, client_key: &str) -> i64 {
    raw_i64(
        dsn,
        &format!(
            "SELECT COUNT(*) AS v FROM products_idempotency WHERE client_key = '{client_key}'"
        ),
    )
    .await
}

/// A predicate matching `column` against `id` under **either** rendering.
///
/// `SQLite` stores a `UUID` as a 16-byte `BLOB`, so a bare `= '<hyphenated>'`
/// misses rows the driver wrote as bytes; `hex()` is the other side of that.
#[must_use]
pub fn id_matches(column: &str, id: Uuid) -> String {
    let hex = id.simple().to_string().to_uppercase();
    format!("({column} = '{id}' OR hex({column}) = '{hex}')")
}

/// One column of **the** audit row, and a proof that there is exactly one.
///
/// Both readers below carried the precondition "where exactly one was written"
/// in their docs and nothing enforced it: an unqualified `SELECT` over the
/// table hands `raw_string_opt`'s `.one()` an arbitrary row, so a case that
/// wrote a second audit row would read whichever sorted first and keep passing.
/// That is the same defect review wave D fixed for the `hex(actor_ref)` read —
/// and the class sweep that wave declared clean did not catch these, because
/// the detector was keyed to `LIMIT 1` without a `WHERE` and these carry no
/// `LIMIT` at all.
async fn the_one_audit_row(dsn: &str, column: &str) -> Option<String> {
    let rows = raw_i64(dsn, "SELECT COUNT(*) AS v FROM products_audit_log").await;
    assert_eq!(
        rows, 1,
        "these readers name **the** audit row; {rows} were written, so the value read would be \
         whichever the engine returned first"
    );
    raw_string_opt(
        dsn,
        &format!("SELECT {column} AS v FROM products_audit_log"),
    )
    .await
}

/// The `action` of the audit row, where exactly one was written.
pub async fn audit_action(dsn: &str) -> Option<String> {
    the_one_audit_row(dsn, "action").await
}

/// The `error_code` of the audit row, where exactly one was written.
pub async fn audit_error_code(dsn: &str) -> Option<String> {
    the_one_audit_row(dsn, "error_code").await
}

// **Owed, and measured rather than guessed**: 24 sites in the door suites still
// spell `SELECT error_code AS v FROM products_audit_log` inline against 6 that
// call the reader above, and 4 against 4 for `action`. Two spellings of one read
// is the drift surface this module exists to remove — but the swap is not
// mechanical, because the reader now asserts the table holds exactly one row and
// some of those sites may legitimately have written more. Each has to be looked
// at, which is why they are recorded here rather than converted blind.

/// A usage-type resolver that answers `Resolved` for every ref — what a test
/// `ApiState` carries unless a probe injects [`StubUsageTypes`] to script the
/// other two answers. Production never sees it: `gear.rs` installs the
/// resolved catalog (P-D-184).
#[must_use]
pub fn resolved_usage_types() -> Arc<dyn bss_products_sdk::usage_types::UsageTypeCatalog> {
    Arc::new(StubUsageTypes::always(
        crate::domain::recognized::UsageTypeAnswer::Resolved(probe_binding()),
    ))
}

/// The binding every `Resolved` stub answers with.
#[must_use]
pub fn probe_binding() -> crate::domain::recognized::UsageTypeBinding {
    crate::domain::recognized::UsageTypeBinding {
        gts_id: "usage:storage".to_owned(),
        kind: "counter".to_owned(),
        metadata_fields: vec!["zone".to_owned(), "region".to_owned()],
    }
}

/// A scripted collector: answers in order, then repeats the last one.
pub struct StubUsageTypes {
    answers:
        std::sync::Mutex<std::collections::VecDeque<crate::domain::recognized::UsageTypeAnswer>>,
    last: crate::domain::recognized::UsageTypeAnswer,
    /// How many times the door asked — the *once per publish* clause's operand.
    pub asked: std::sync::atomic::AtomicUsize,
}

impl StubUsageTypes {
    /// One answer, forever.
    #[must_use]
    pub fn always(answer: crate::domain::recognized::UsageTypeAnswer) -> Self {
        Self::scripted([answer])
    }

    /// `answers` in the order the door will receive them; the last repeats.
    #[must_use]
    ///
    /// # Panics
    /// Panics when the answer sequence is empty.
    pub fn scripted(
        answers: impl IntoIterator<Item = crate::domain::recognized::UsageTypeAnswer>,
    ) -> Self {
        let mut queue: std::collections::VecDeque<_> = answers.into_iter().collect();
        let last = queue.pop_back().expect("a stub needs at least one answer");
        Self {
            answers: std::sync::Mutex::new(queue),
            last,
            asked: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl bss_products_sdk::usage_types::UsageTypeCatalog for StubUsageTypes {
    async fn resolve(
        &self,
        _ctx: &SecurityContext,
        _usage_type_ref: &str,
    ) -> crate::domain::recognized::UsageTypeAnswer {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let next = self.answers.lock().expect("stub lock").pop_front();
        next.unwrap_or_else(|| self.last.clone())
    }

    async fn list(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _q: Option<&str>,
        _kind: Option<&str>,
        _limit: u32,
        _cursor: Option<&str>,
    ) -> Result<
        bss_products_sdk::usage_types::UsageTypePage,
        toolkit_canonical_errors::CanonicalError,
    > {
        // The stub exists for the publish gate; a case that needs the
        // pick-list builds its own catalog and says so, rather than inheriting
        // an answer this one never meant.
        Ok(bss_products_sdk::usage_types::UsageTypePage::default())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct EmptyUsageTypes;

#[async_trait::async_trait]
impl bss_products_sdk::usage_types::UsageTypeCatalog for EmptyUsageTypes {
    async fn resolve(
        &self,
        _ctx: &SecurityContext,
        _usage_type_ref: &str,
    ) -> crate::domain::recognized::UsageTypeAnswer {
        crate::domain::recognized::UsageTypeAnswer::Unresolved
    }

    async fn list(
        &self,
        _ctx: &SecurityContext,
        _q: Option<&str>,
        _kind: Option<&str>,
        _limit: u32,
        _cursor: Option<&str>,
    ) -> Result<
        bss_products_sdk::usage_types::UsageTypePage,
        toolkit_canonical_errors::CanonicalError,
    > {
        Ok(bss_products_sdk::usage_types::UsageTypePage::default())
    }
}

/// A configured catalog that cannot be reached — the 503 leg, which no other
/// stub here can produce because both answer `Ok`.
///
/// Without it the door's `.error_503` and the `USAGE_TYPE_CATALOG_UNAVAILABLE`
/// finding are asserted nowhere, and a regression collapsing either into an
/// empty 200 — the exact failure the surface exists to prevent — would stay
/// green.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnreachableUsageTypes;

#[async_trait::async_trait]
impl bss_products_sdk::usage_types::UsageTypeCatalog for UnreachableUsageTypes {
    async fn resolve(
        &self,
        _ctx: &SecurityContext,
        _usage_type_ref: &str,
    ) -> crate::domain::recognized::UsageTypeAnswer {
        crate::domain::recognized::UsageTypeAnswer::Unavailable
    }

    async fn list(
        &self,
        _ctx: &SecurityContext,
        _q: Option<&str>,
        _kind: Option<&str>,
        _limit: u32,
        _cursor: Option<&str>,
    ) -> Result<
        bss_products_sdk::usage_types::UsageTypePage,
        toolkit_canonical_errors::CanonicalError,
    > {
        Err(
            bss_products_sdk::usage_types::usage_type_catalog_unreachable(
                "the probe's catalog is unreachable by construction",
            ),
        )
    }
}

/// The usage collector refusing every caller with its own `PermissionDenied`, as the platform PDP
/// answers an author without collector read (P-D-207). Every method refuses, so a case that reaches
/// a method it did not mean to is still told no rather than handed a fabricated answer.
pub struct DenyingCollector;

fn collector_denial() -> usage_collector_sdk::UsageCollectorError {
    usage_collector_sdk::UsageCollectorError::permission_denied(
        "the probe's PDP refuses this caller",
    )
}

#[async_trait]
impl usage_collector_sdk::UsageCollectorClientV1 for DenyingCollector {
    async fn create_usage_record(
        &self,
        _: &SecurityContext,
        _: usage_collector_sdk::CreateUsageRecord,
    ) -> Result<usage_collector_sdk::UsageRecord, usage_collector_sdk::UsageCollectorError> {
        Err(collector_denial())
    }
    async fn create_usage_records(
        &self,
        _: &SecurityContext,
        _: Vec<usage_collector_sdk::CreateUsageRecord>,
    ) -> Result<
        Vec<Result<usage_collector_sdk::UsageRecord, usage_collector_sdk::UsageCollectorError>>,
        usage_collector_sdk::UsageCollectorError,
    > {
        Err(collector_denial())
    }
    async fn get_usage_record(
        &self,
        _: &SecurityContext,
        _: Uuid,
    ) -> Result<usage_collector_sdk::UsageRecord, usage_collector_sdk::UsageCollectorError> {
        Err(collector_denial())
    }
    async fn query_aggregated_usage_records(
        &self,
        _: &SecurityContext,
        _: usage_collector_sdk::UsageTypeGtsId,
        _: &toolkit_odata::ODataQuery,
        _: &[usage_collector_sdk::MetadataFilter],
        _: usage_collector_sdk::AggregationSpec,
    ) -> Result<usage_collector_sdk::AggregationResult, usage_collector_sdk::UsageCollectorError>
    {
        Err(collector_denial())
    }
    async fn list_usage_records(
        &self,
        _: &SecurityContext,
        _: usage_collector_sdk::UsageTypeGtsId,
        _: &toolkit_odata::ODataQuery,
        _: &[usage_collector_sdk::MetadataFilter],
    ) -> Result<
        toolkit_odata::Page<usage_collector_sdk::UsageRecord>,
        usage_collector_sdk::UsageCollectorError,
    > {
        Err(collector_denial())
    }
    async fn deactivate_usage_record(
        &self,
        _: &SecurityContext,
        _: Uuid,
    ) -> Result<(), usage_collector_sdk::UsageCollectorError> {
        Err(collector_denial())
    }
    async fn create_usage_type(
        &self,
        _: &SecurityContext,
        _: usage_collector_sdk::UsageType,
    ) -> Result<usage_collector_sdk::UsageType, usage_collector_sdk::UsageCollectorError> {
        Err(collector_denial())
    }
    async fn get_usage_type(
        &self,
        _: &SecurityContext,
        _: usage_collector_sdk::UsageTypeGtsId,
    ) -> Result<usage_collector_sdk::UsageType, usage_collector_sdk::UsageCollectorError> {
        Err(collector_denial())
    }
    async fn list_usage_types(
        &self,
        _: &SecurityContext,
        _: &toolkit_odata::ODataQuery,
    ) -> Result<
        toolkit_odata::Page<usage_collector_sdk::UsageType>,
        usage_collector_sdk::UsageCollectorError,
    > {
        Err(collector_denial())
    }
    async fn delete_usage_type(
        &self,
        _: &SecurityContext,
        _: usage_collector_sdk::UsageTypeGtsId,
    ) -> Result<(), usage_collector_sdk::UsageCollectorError> {
        Err(collector_denial())
    }
}

/// The production collector adapter over [`DenyingCollector`]: the catalog an author without
/// collector read meets.
#[must_use]
pub fn denying_collector_catalog() -> Arc<dyn bss_products_sdk::usage_types::UsageTypeCatalog> {
    Arc::new(crate::infra::usage_types::CollectorUsageTypes::new(
        Arc::new(DenyingCollector),
        std::time::Duration::from_secs(2),
    ))
}

/// The usage collector's usage-type catalog as its real storage plugin serves it
/// (`timescaledb-usage-collector-plugin`, `catalog_store::list` and `query/translate.rs`), for the
/// picker's walk (P-D-207).
///
/// **It refuses what the plugin refuses, with the plugin's words.** The plugin translates the
/// whole `$filter` before it reads a row, and its translator takes comparison operators only:
/// `contains`, `startswith` and `endswith` are `Internal("unsupported operator: …")`, which a
/// deployed picker answered as 503 `internal error: unsupported operator: Contains`. The collector SDK's
/// own field doc says `gts_id` supports them; the plugin does not, and a double written from the
/// SDK doc is how the picker's `q` shipped green. The rest mirrors the plugin too: order by
/// `gts_id` ascending whatever `$orderby` says, a page size floored at 1 and clamped at the
/// plugin's `MAX_PAGE_SIZE` (1000, or a lower `ceiling`), a forward-only keyset cursor over
/// `gts_id` whose filter hash must equal the query's, and a look-ahead row for `next_cursor`.
///
/// Every other method refuses: the picker has no business reaching them.
pub struct PluginLikeCollector {
    /// Sorted by `gts_id`, as the plugin's `ORDER BY gts_id ASC` reads it.
    catalog: Vec<usage_collector_sdk::UsageType>,
    ceiling: u64,
    failing_from: Option<usize>,
    asked: std::sync::Mutex<Vec<toolkit_odata::ODataQuery>>,
}

impl PluginLikeCollector {
    /// The plugin's own page ceiling (`query::MAX_PAGE_SIZE`).
    pub const PLUGIN_MAX_PAGE_SIZE: u64 = 1000;

    /// A catalog of `(gts_id, kind)`; each type declares one metadata field, `region`.
    /// # Panics
    /// Panics if an id is not a valid GTS id.
    #[must_use]
    pub fn new<'a>(
        types: impl IntoIterator<Item = (&'a str, usage_collector_sdk::UsageKind)>,
    ) -> Self {
        let mut catalog: Vec<_> = types
            .into_iter()
            .map(|(id, kind)| usage_collector_sdk::UsageType {
                gts_id: usage_collector_sdk::UsageTypeGtsId::new(id).expect("a valid GTS id"),
                kind,
                metadata_fields: std::iter::once(
                    usage_collector_sdk::MetadataKey::new("region").expect("a valid key"),
                )
                .collect(),
            })
            .collect();
        catalog.sort_by(|a, b| a.gts_id.as_ref().cmp(b.gts_id.as_ref()));
        Self {
            catalog,
            ceiling: Self::PLUGIN_MAX_PAGE_SIZE,
            failing_from: None,
            asked: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// A catalog whose page ceiling is lower than the plugin's, so a walk crosses pages.
    #[must_use]
    pub const fn with_ceiling(mut self, ceiling: u64) -> Self {
        self.ceiling = ceiling;
        self
    }

    /// A catalog whose store goes down at the `call`-th list (0-based) and stays down: the
    /// collector's `ServiceUnavailable`.
    #[must_use]
    pub const fn failing_from(mut self, call: usize) -> Self {
        self.failing_from = Some(call);
        self
    }

    /// Every query the catalog was asked, in order.
    /// # Panics
    /// Panics if the record's lock is poisoned.
    #[must_use]
    pub fn asked(&self) -> Vec<toolkit_odata::ODataQuery> {
        self.asked.lock().unwrap().clone()
    }

    fn column(
        field: usage_collector_sdk::UsageTypeFilterField,
        row: &usage_collector_sdk::UsageType,
    ) -> Result<String, usage_collector_sdk::UsageCollectorError> {
        use toolkit_odata::filter::FilterField as _;
        match field.name() {
            "gts_id" => Ok(row.gts_id.to_string()),
            "kind" => Ok(match row.kind {
                usage_collector_sdk::UsageKind::Counter => "counter",
                usage_collector_sdk::UsageKind::Gauge => "gauge",
            }
            .to_owned()),
            other => Err(usage_collector_sdk::UsageCollectorError::internal(format!(
                "field not allowlisted: {other}"
            ))),
        }
    }

    /// The plugin's translator, as a pass over the whole tree before any row is read.
    fn translate(
        node: &toolkit_odata::filter::FilterNode<usage_collector_sdk::UsageTypeFilterField>,
    ) -> Result<(), usage_collector_sdk::UsageCollectorError> {
        use toolkit_odata::filter::{FilterNode, FilterOp};
        match node {
            FilterNode::Binary { op, .. } => match op {
                FilterOp::Eq
                | FilterOp::Ne
                | FilterOp::Gt
                | FilterOp::Ge
                | FilterOp::Lt
                | FilterOp::Le => Ok(()),
                other => Err(plugin_internal(format!("unsupported operator: {other:?}"))),
            },
            FilterNode::InList { values, .. } if values.is_empty() => {
                Err(plugin_internal("IN list must not be empty"))
            }
            FilterNode::InList { .. } => Ok(()),
            FilterNode::Composite { op, children } => match op {
                FilterOp::And | FilterOp::Or => children.iter().try_for_each(Self::translate),
                other => Err(plugin_internal(format!(
                    "invalid composite operator: {other:?}"
                ))),
            },
            FilterNode::Not(inner) => Self::translate(inner),
        }
    }

    fn holds(
        node: &toolkit_odata::filter::FilterNode<usage_collector_sdk::UsageTypeFilterField>,
        row: &usage_collector_sdk::UsageType,
    ) -> Result<bool, usage_collector_sdk::UsageCollectorError> {
        use toolkit_odata::filter::{FilterNode, FilterOp, ODataValue};
        let text = |value: &ODataValue| match value {
            ODataValue::String(s) => Ok(s.clone()),
            other => Err(usage_collector_sdk::UsageCollectorError::internal(format!(
                "unsupported bind: {other:?}"
            ))),
        };
        Ok(match node {
            FilterNode::Binary { field, op, value } => {
                let order = Self::column(*field, row)?.cmp(&text(value)?);
                match op {
                    FilterOp::Eq => order.is_eq(),
                    FilterOp::Ne => order.is_ne(),
                    FilterOp::Gt => order.is_gt(),
                    FilterOp::Ge => order.is_ge(),
                    FilterOp::Lt => order.is_lt(),
                    _ => order.is_le(),
                }
            }
            FilterNode::InList { field, values } => {
                let column = Self::column(*field, row)?;
                let mut hit = false;
                for value in values {
                    hit |= column == text(value)?;
                }
                hit
            }
            FilterNode::Composite { op, children } => {
                let mut all = true;
                let mut any = false;
                for child in children {
                    let holds = Self::holds(child, row)?;
                    all &= holds;
                    any |= holds;
                }
                if *op == FilterOp::And { all } else { any }
            }
            FilterNode::Not(inner) => !Self::holds(inner, row)?,
        })
    }
}

#[async_trait]
impl usage_collector_sdk::UsageCollectorClientV1 for PluginLikeCollector {
    async fn create_usage_record(
        &self,
        _: &SecurityContext,
        _: usage_collector_sdk::CreateUsageRecord,
    ) -> Result<usage_collector_sdk::UsageRecord, usage_collector_sdk::UsageCollectorError> {
        Err(collector_denial())
    }
    async fn create_usage_records(
        &self,
        _: &SecurityContext,
        _: Vec<usage_collector_sdk::CreateUsageRecord>,
    ) -> Result<
        Vec<Result<usage_collector_sdk::UsageRecord, usage_collector_sdk::UsageCollectorError>>,
        usage_collector_sdk::UsageCollectorError,
    > {
        Err(collector_denial())
    }
    async fn get_usage_record(
        &self,
        _: &SecurityContext,
        _: Uuid,
    ) -> Result<usage_collector_sdk::UsageRecord, usage_collector_sdk::UsageCollectorError> {
        Err(collector_denial())
    }
    async fn query_aggregated_usage_records(
        &self,
        _: &SecurityContext,
        _: usage_collector_sdk::UsageTypeGtsId,
        _: &toolkit_odata::ODataQuery,
        _: &[usage_collector_sdk::MetadataFilter],
        _: usage_collector_sdk::AggregationSpec,
    ) -> Result<usage_collector_sdk::AggregationResult, usage_collector_sdk::UsageCollectorError>
    {
        Err(collector_denial())
    }
    async fn list_usage_records(
        &self,
        _: &SecurityContext,
        _: usage_collector_sdk::UsageTypeGtsId,
        _: &toolkit_odata::ODataQuery,
        _: &[usage_collector_sdk::MetadataFilter],
    ) -> Result<
        toolkit_odata::Page<usage_collector_sdk::UsageRecord>,
        usage_collector_sdk::UsageCollectorError,
    > {
        Err(collector_denial())
    }
    async fn deactivate_usage_record(
        &self,
        _: &SecurityContext,
        _: Uuid,
    ) -> Result<(), usage_collector_sdk::UsageCollectorError> {
        Err(collector_denial())
    }
    async fn create_usage_type(
        &self,
        _: &SecurityContext,
        _: usage_collector_sdk::UsageType,
    ) -> Result<usage_collector_sdk::UsageType, usage_collector_sdk::UsageCollectorError> {
        Err(collector_denial())
    }
    async fn get_usage_type(
        &self,
        _: &SecurityContext,
        gts_id: usage_collector_sdk::UsageTypeGtsId,
    ) -> Result<usage_collector_sdk::UsageType, usage_collector_sdk::UsageCollectorError> {
        self.catalog
            .iter()
            .find(|t| t.gts_id == gts_id)
            .cloned()
            .ok_or_else(|| {
                usage_collector_sdk::UsageCollectorError::internal("not in this double's catalog")
            })
    }
    async fn list_usage_types(
        &self,
        _: &SecurityContext,
        query: &toolkit_odata::ODataQuery,
    ) -> Result<
        toolkit_odata::Page<usage_collector_sdk::UsageType>,
        usage_collector_sdk::UsageCollectorError,
    > {
        let call = {
            let mut asked = self.asked.lock().unwrap();
            asked.push(query.clone());
            asked.len() - 1
        };
        if self.failing_from.is_some_and(|from| call >= from) {
            return Err(
                usage_collector_sdk::UsageCollectorError::service_unavailable(
                    "the probe's catalog store is down",
                    None,
                ),
            );
        }
        let limit = query.limit.unwrap_or(100).clamp(1, self.ceiling);
        let page_rows = usize::try_from(limit).unwrap();
        let node = query
            .filter()
            .map(|expr| {
                toolkit_odata::filter::convert_expr_to_filter_node::<
                    usage_collector_sdk::UsageTypeFilterField,
                >(expr)
                .map_err(|e| plugin_internal(format!("invalid filter: {e}")))
            })
            .transpose()?;
        if let Some(node) = &node {
            Self::translate(node)?;
        }
        let after = match &query.cursor {
            None => None,
            Some(cursor) => {
                if cursor.d != "fwd" {
                    return Err(plugin_internal(format!(
                        "unsupported cursor direction `{}`: only forward paging is supported",
                        cursor.d
                    )));
                }
                if cursor.f.as_deref() != query.filter_hash.as_deref() {
                    return Err(plugin_internal("cursor filter hash mismatch"));
                }
                Some(cursor.k.first().cloned().unwrap_or_default())
            }
        };
        let mut rows = Vec::new();
        for row in &self.catalog {
            if after
                .as_deref()
                .is_some_and(|after| row.gts_id.as_ref() <= after)
            {
                continue;
            }
            if let Some(node) = &node
                && !Self::holds(node, row)?
            {
                continue;
            }
            rows.push(row.clone());
            if rows.len() > page_rows {
                break;
            }
        }
        let has_next = rows.len() > page_rows;
        rows.truncate(page_rows);
        let next_cursor = if has_next {
            let last = rows.last().expect("a page with a next page has a tail");
            Some(
                toolkit_odata::CursorV1 {
                    k: vec![last.gts_id.to_string()],
                    o: toolkit_odata::SortDir::Asc,
                    s: "+gts_id".to_owned(),
                    f: query.filter_hash.clone(),
                    d: "fwd".to_owned(),
                }
                .encode()
                .unwrap(),
            )
        } else {
            None
        };
        Ok(toolkit_odata::Page::new(
            rows,
            toolkit_odata::PageInfo {
                next_cursor,
                prev_cursor: None,
                limit,
            },
        ))
    }
    async fn delete_usage_type(
        &self,
        _: &SecurityContext,
        _: usage_collector_sdk::UsageTypeGtsId,
    ) -> Result<(), usage_collector_sdk::UsageCollectorError> {
        Err(collector_denial())
    }
}

/// The plugin's `UsageCollectorPluginError::internal`, as the host lifts it to the SDK error.
fn plugin_internal(detail: impl Into<String>) -> usage_collector_sdk::UsageCollectorError {
    usage_collector_sdk::UsageCollectorError::internal(detail)
}

/// The production collector adapter over a [`PluginLikeCollector`].
#[must_use]
pub fn plugin_like_catalog(
    collector: Arc<PluginLikeCollector>,
) -> Arc<dyn bss_products_sdk::usage_types::UsageTypeCatalog> {
    Arc::new(crate::infra::usage_types::CollectorUsageTypes::new(
        collector,
        std::time::Duration::from_secs(2),
    ))
}

/// A PDP that refuses every request, for the "authorization is judged first" probes.
struct DenyingResolver;

#[async_trait]
impl AuthZResolverApi for DenyingResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext {
                constraints: Vec::new(),
                deny_reason: None,
            },
        })
    }
}

/// A [`PolicyEnforcer`] over a PDP that refuses everything.
#[must_use]
pub fn denying_enforcer() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(DenyingResolver))
}

/// A test database's DSN and the temporary directory that holds its file, with the file's `-wal`
/// and `-shm`. The directory is removed when the last clone drops, so a test binds this for its
/// whole life (`_dsn`, never `_`, which drops it at once). It reads as the DSN: `&dsn` is a `&str`.
#[derive(Clone, Debug)]
pub struct TestDsn {
    dsn: String,
    _dir: std::sync::Arc<tempfile::TempDir>,
}
impl TestDsn {
    /// A new, empty database file in a new directory of the user's temp dir named `prefix…`.
    ///
    /// # Panics
    /// Panics if the temporary directory cannot be created.
    #[must_use]
    pub fn new(prefix: &str) -> Self {
        let dir = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
        let dsn = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("db.sqlite3").display()
        );
        Self {
            dsn,
            _dir: std::sync::Arc::new(dir),
        }
    }
}
impl std::ops::Deref for TestDsn {
    type Target = str;
    fn deref(&self) -> &str {
        &self.dsn
    }
}
impl std::fmt::Display for TestDsn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.dsn)
    }
}
/// `Database::connect(&dsn)` takes anything that is `Into<String>`.
impl From<&TestDsn> for String {
    fn from(dsn: &TestDsn) -> Self {
        dsn.dsn.clone()
    }
}

/// File-backed database with the production migration chains and a PDP-derived scope; the caller
/// holds its [`TestDsn`] for the test's life.
///
/// # Panics
/// Panics if fixture initialization fails.
pub async fn test_db() -> (
    toolkit_db::DBProvider<toolkit_db::DbError>,
    toolkit_db::secure::AccessScope,
    Uuid,
    TestDsn,
) {
    test_db_with(1).await
}

/// [`test_db`] over a pool of `max_conns` connections: the outbox's workers then read on a
/// connection of their own while a transaction is still open, as they do in a deployment.
///
/// # Panics
/// Panics if fixture initialization fails.
pub async fn test_db_with(
    max_conns: u32,
) -> (
    toolkit_db::DBProvider<toolkit_db::DbError>,
    toolkit_db::secure::AccessScope,
    Uuid,
    TestDsn,
) {
    use sea_orm_migration::MigratorTrait;
    let dsn = TestDsn::new("products-repos-");
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(max_conns),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        toolkit_db::outbox::outbox_migrations_with_prefix(events::OUTBOX_TABLE_PREFIX).unwrap(),
    )
    .await
    .unwrap();
    let tenant = Uuid::new_v4();
    let scope = crate::authz::access_scope(
        &flat_in_enforcer(tenant),
        &authed_ctx(tenant),
        &crate::authz::resource_types::SKU,
        crate::authz::actions::READ,
        Some(tenant),
    )
    .await
    .unwrap();
    (toolkit_db::DBProvider::new(db), scope, tenant, dsn)
}

/// [`test_db`] over a connection whose statements are recorded, for the fixed-statement tests of
/// the set-based reads.
///
/// # Panics
/// Panics if fixture initialization fails.
pub async fn recorded_test_db() -> (
    toolkit_db::DBProvider<toolkit_db::DbError>,
    toolkit_db::secure::AccessScope,
    Uuid,
    TestDsn,
    toolkit_db::test_support::QueryRecorder,
) {
    use sea_orm_migration::MigratorTrait;
    let dsn = TestDsn::new("products-recorded-");
    let (db, recorder) = toolkit_db::test_support::connect_with_recorder(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        toolkit_db::outbox::outbox_migrations_with_prefix(events::OUTBOX_TABLE_PREFIX).unwrap(),
    )
    .await
    .unwrap();
    let tenant = Uuid::new_v4();
    let scope = toolkit_db::secure::AccessScope::for_tenant(tenant);
    recorder.clear();
    (
        toolkit_db::DBProvider::new(db),
        scope,
        tenant,
        dsn,
        recorder,
    )
}

/// The statements `recorder` holds on the gear's own tables (`products_*`), normalized, in order.
#[must_use]
pub fn products_statements(recorder: &toolkit_db::test_support::QueryRecorder) -> Vec<String> {
    recorder
        .events()
        .into_iter()
        .filter(|q| {
            q.table
                .as_deref()
                .is_some_and(|t| t.starts_with("products_"))
        })
        .map(|q| q.sql)
        .collect()
}

/// Running outbox lifetime retained by every clone of a REST test router.
struct RestOutbox {
    _handle: toolkit_db::outbox::OutboxHandle,
}

/// Build a door with the production database/outbox migrations and a resolved catalog.
pub async fn rest_app(
    tenant: Uuid,
    build: fn(Arc<crate::api::rest::ApiState>, &dyn toolkit::api::OpenApiRegistry) -> axum::Router,
) -> (axum::Router, String) {
    rest_app_with_catalog(tenant, build, resolved_usage_types(), "test").await
}

/// The same REST fixture with an explicitly selected catalog answer and provenance.
/// # Panics
/// Panics if fixture setup or the asserted operation fails.
pub async fn rest_app_with_catalog(
    tenant: Uuid,
    build: fn(Arc<crate::api::rest::ApiState>, &dyn toolkit::api::OpenApiRegistry) -> axum::Router,
    catalog: Arc<dyn bss_products_sdk::usage_types::UsageTypeCatalog>,
    source: &'static str,
) -> (axum::Router, String) {
    let (db, _, _, dsn) = test_db().await;
    let (app, _) = rest_app_on_db(tenant, build, catalog, source, db).await;
    // The router holds the database's temporary directory: it goes with the last clone.
    let dsn_text = dsn.to_string();
    (app.layer(axum::Extension(dsn)), dsn_text)
}

/// Build a router on a supplied provider so race tests use independent connections.
/// # Panics
/// Panics if outbox initialization fails.
pub async fn rest_app_on_db(
    tenant: Uuid,
    build: fn(Arc<crate::api::rest::ApiState>, &dyn toolkit::api::OpenApiRegistry) -> axum::Router,
    catalog: Arc<dyn bss_products_sdk::usage_types::UsageTypeCatalog>,
    source: &'static str,
    db: toolkit_db::DBProvider<toolkit_db::DbError>,
) -> (axum::Router, Arc<crate::api::rest::ApiState>) {
    let hub = Arc::new(toolkit::ClientHub::new());
    let handle = toolkit_db::outbox::Outbox::builder(db.db().clone())
        .table_prefix(events::OUTBOX_TABLE_PREFIX)
        .unwrap()
        .queue(
            events::QUEUE_NAME,
            toolkit_db::outbox::Partitions::of(events::PARTITIONS),
        )
        .leased(events::PendingBrokerProducer)
        .start()
        .await
        .unwrap();
    let state = Arc::new(crate::api::rest::ApiState {
        db,
        sink: crate::infra::broker::EventSink::Interim(Arc::clone(handle.outbox())),
        usage_type_catalog: catalog,
        usage_type_catalog_source: source,
        idempotency_retention_hours: 24,
        fence_ttl_minutes: 30,
        reference_principals: std::collections::BTreeMap::from([(
            Uuid::from_u128(42),
            "pricing".into(),
        )]),
        hub: Arc::clone(&hub),
        actor_names: crate::api::rest::ApiState::names_from(&hub),
    });
    let app = build(state.clone(), &toolkit::api::OpenApiRegistryImpl::new())
        .layer(axum::Extension(flat_in_enforcer(tenant)))
        .layer(axum::Extension(Arc::new(RestOutbox { _handle: handle })));
    (app, state)
}

/// Open an auxiliary scoped provider to seed the REST fixture through repositories.
/// # Panics
/// Panics if fixture setup or the asserted operation fails.
pub async fn repo_connection(
    dsn: &str,
    tenant: Uuid,
) -> (
    toolkit_db::DBProvider<toolkit_db::DbError>,
    toolkit_db::secure::AccessScope,
) {
    let db = toolkit_db::connect_db(
        dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let scope = crate::authz::access_scope(
        &flat_in_enforcer(tenant),
        &authed_ctx(tenant),
        &crate::authz::resource_types::SKU,
        crate::authz::actions::AUTHOR,
        Some(tenant),
    )
    .await
    .unwrap();
    (toolkit_db::DBProvider::new(db), scope)
}

/// Seed an unmetered draft for category and SKU door tests.
/// # Panics
/// Panics if fixture setup or the asserted operation fails.
pub async fn seed_rest_sku(
    runner: &impl toolkit_db::secure::DBRunner,
    scope: &toolkit_db::secure::AccessScope,
    tenant: Uuid,
    category_id: Uuid,
    code: &str,
) -> bss_products_sdk::models::Sku {
    crate::infra::storage::repo::insert_sku(
        runner,
        scope,
        tenant,
        crate::domain::sku::NewSku {
            code: code.to_owned(),
            name: code.to_owned(),
            r#type: bss_products_sdk::models::SkuType::Usage,
            category_id: Some(category_id),
            description: String::new(),
            sellable: true,
            gl_code: None,
            tax_category: None,
            invoice_line_template: None,
            billing_timing: None,
            usage_type_ref: None,
            unit: None,
        },
        tenant_user(tenant).subject_id(),
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap()
}

/// Exercise the router with a request-scoped authenticated principal.
/// # Panics
/// Panics if fixture setup or the asserted operation fails.
pub async fn request(
    app: &axum::Router,
    tenant: Uuid,
    method: axum::http::Method,
    uri: &str,
    body: Option<serde_json::Value>,
    etag: Option<&str>,
) -> axum::response::Response {
    request_as(app, &tenant_user(tenant), method, uri, body, etag).await
}
/// Exercise the router as a given principal.
/// # Panics
/// Panics if fixture setup or the asserted operation fails.
pub async fn request_as(
    app: &axum::Router,
    ctx: &SecurityContext,
    method: axum::http::Method,
    uri: &str,
    body: Option<serde_json::Value>,
    etag: Option<&str>,
) -> axum::response::Response {
    use tower::ServiceExt;
    let mut builder = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .extension(ctx.clone());
    if let Some(etag) = etag {
        builder = builder.header("If-Match", etag);
    }
    let body = body.map_or_else(axum::body::Body::empty, |b| {
        axum::body::Body::from(b.to_string())
    });
    app.clone()
        .oneshot(
            builder
                .header("Content-Type", "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap()
}
/// POST a JSON request.
pub async fn post(
    app: &axum::Router,
    tenant: Uuid,
    uri: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    request(app, tenant, axum::http::Method::POST, uri, Some(body), None).await
}
/// PATCH under the supplied revision precondition.
pub async fn patch(
    app: &axum::Router,
    tenant: Uuid,
    uri: &str,
    body: serde_json::Value,
    etag: Option<&str>,
) -> axum::response::Response {
    request(
        app,
        tenant,
        axum::http::Method::PATCH,
        uri,
        Some(body),
        etag,
    )
    .await
}
/// GET with the request principal.
pub async fn get(app: &axum::Router, tenant: Uuid, uri: &str) -> axum::response::Response {
    request(app, tenant, axum::http::Method::GET, uri, None, None).await
}
/// Decode an HTTP response body.
/// # Panics
/// Panics if fixture setup or the asserted operation fails.
pub async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Read a machine code from a canonical reason or precondition violation.
/// # Panics
/// Panics if the response has no machine-readable error code.
#[must_use]
pub fn problem_code(body: &serde_json::Value) -> String {
    find_code(body).expect("problem contains a machine-readable code")
}
fn find_code(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            for key in ["reason", "type", "code"] {
                if let Some(serde_json::Value::String(found)) = map.get(key)
                    && found.chars().all(|c| c.is_ascii_uppercase() || c == '_')
                    && found.len() > 3
                {
                    return Some(found.clone());
                }
            }
            map.values().find_map(find_code)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_code),
        _ => None,
    }
}

/// Read the violation for a wire field.
pub fn violation_for(body: &serde_json::Value, subject: &str) -> Option<String> {
    fn violations(value: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
        match value {
            serde_json::Value::Object(map) => map
                .get("violations")
                .and_then(serde_json::Value::as_array)
                .or_else(|| map.values().find_map(violations)),
            serde_json::Value::Array(items) => items.iter().find_map(violations),
            _ => None,
        }
    }
    violations(body)?
        .iter()
        .find(|violation| violation["subject"] == serde_json::json!(subject))
        .and_then(|violation| violation["description"].as_str())
        .map(ToOwned::to_owned)
}
// Reuse Pricing's explicit contract provider in the real cross-gear authoring test.
#[path = "../../../pricing/pricing/tests/policy_support/mod.rs"]
pub(crate) mod pricing_policy_support;
