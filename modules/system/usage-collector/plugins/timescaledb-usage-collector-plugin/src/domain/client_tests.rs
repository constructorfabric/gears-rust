// @cpt-dod:cpt-cf-usage-collector-dod-production-storage-plugin-testing-and-observability

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use chrono::Utc;
use modkit_security::AccessScope;
use uuid::Uuid;

use usage_collector_sdk::models::{
    AggregationFn, AggregationQuery, AggregationResult, RawQuery, UsageKind, UsageRecord,
};
use usage_collector_sdk::{Page, PageInfo, UsageCollectorError, UsageCollectorPluginClientV1};

use super::TimescaleDbPluginClient;
use crate::domain::error::StoragePluginError;
use crate::domain::insert_port::InsertPort;
use crate::domain::metrics::PluginMetrics;
use crate::domain::query_port::QueryPort;

// ── Mock: insert port ─────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum InsertBehavior {
    Success(u64),
    PoolTimeout,
    QueryFailed,
    UnexpectedUniqueViolation,
}

struct MockInsertPort {
    behavior: InsertBehavior,
    captured_value: Option<Arc<Mutex<f64>>>,
}

impl MockInsertPort {
    fn success(rows: u64) -> Arc<Self> {
        Arc::new(Self {
            behavior: InsertBehavior::Success(rows),
            captured_value: None,
        })
    }

    fn pool_timeout() -> Arc<Self> {
        Arc::new(Self {
            behavior: InsertBehavior::PoolTimeout,
            captured_value: None,
        })
    }

    fn query_failed() -> Arc<Self> {
        Arc::new(Self {
            behavior: InsertBehavior::QueryFailed,
            captured_value: None,
        })
    }

    fn unexpected_unique_violation() -> Arc<Self> {
        Arc::new(Self {
            behavior: InsertBehavior::UnexpectedUniqueViolation,
            captured_value: None,
        })
    }

    fn capturing(cap: Arc<Mutex<f64>>) -> Arc<Self> {
        Arc::new(Self {
            behavior: InsertBehavior::Success(1),
            captured_value: Some(cap),
        })
    }
}

#[async_trait]
impl InsertPort for MockInsertPort {
    async fn insert_usage_record(&self, record: &UsageRecord) -> Result<u64, StoragePluginError> {
        if let Some(ref cap) = self.captured_value {
            *cap.lock().unwrap() = record.value;
        }
        match self.behavior {
            InsertBehavior::Success(n) => Ok(n),
            InsertBehavior::PoolTimeout => Err(StoragePluginError::Transient(Box::new(
                std::io::Error::other("pool timed out"),
            ))),
            InsertBehavior::QueryFailed => Err(StoragePluginError::QueryFailed(Box::new(
                std::io::Error::other("mock non-transient query failure"),
            ))),
            InsertBehavior::UnexpectedUniqueViolation => {
                Err(StoragePluginError::UnexpectedUniqueViolation(Box::new(
                    std::io::Error::other("mock unique violation after claim"),
                )))
            }
        }
    }
}

// ── Mock: query port ──────────────────────────────────────────────────────────

struct MockQueryPort {
    agg_fail: bool,
    raw_fail: bool,
    captured_agg: Mutex<Option<AggregationQuery>>,
    captured_raw: Mutex<Option<RawQuery>>,
    // `agg_response` and `raw_response` are set once at construction and only
    // read thereafter, so they do not need interior mutability — the previous
    // `Mutex` wrapping locked on every async call and obscured that
    // read-only-after-construction contract.
    agg_response: Vec<AggregationResult>,
    raw_response: Page<UsageRecord>,
}

impl MockQueryPort {
    fn build(
        agg_fail: bool,
        raw_fail: bool,
        agg_response: Vec<AggregationResult>,
        raw_response: Page<UsageRecord>,
    ) -> Self {
        Self {
            agg_fail,
            raw_fail,
            captured_agg: Mutex::new(None),
            captured_raw: Mutex::new(None),
            agg_response,
            raw_response,
        }
    }

    fn new(agg_fail: bool, raw_fail: bool) -> Self {
        Self::build(agg_fail, raw_fail, vec![], Page::empty(10))
    }

    fn success() -> Arc<Self> {
        Arc::new(Self::new(false, false))
    }
    fn agg_failing() -> Arc<Self> {
        Arc::new(Self::new(true, false))
    }
    fn raw_failing() -> Arc<Self> {
        Arc::new(Self::new(false, true))
    }

    fn with_agg_response(response: Vec<AggregationResult>) -> Arc<Self> {
        Arc::new(Self::build(false, false, response, Page::empty(10)))
    }

    fn with_raw_response(response: Page<UsageRecord>) -> Arc<Self> {
        Arc::new(Self::build(false, false, vec![], response))
    }
}

#[async_trait]
impl QueryPort for MockQueryPort {
    async fn query_aggregated(
        &self,
        query: AggregationQuery,
    ) -> Result<Vec<AggregationResult>, UsageCollectorError> {
        *self.captured_agg.lock().unwrap() = Some(query);
        if self.agg_fail {
            Err(UsageCollectorError::service_unavailable()
                .with_detail("mock transient")
                .create())
        } else {
            Ok(self.agg_response.clone())
        }
    }

    async fn query_raw(&self, query: RawQuery) -> Result<Page<UsageRecord>, UsageCollectorError> {
        *self.captured_raw.lock().unwrap() = Some(query);
        if self.raw_fail {
            Err(UsageCollectorError::service_unavailable()
                .with_detail("mock transient")
                .create())
        } else {
            Ok(self.raw_response.clone())
        }
    }
}

// ── Mock: metrics ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct MockMetrics {
    ingestion_success: AtomicU32,
    ingestion_error: AtomicU32,
    ingestion_latency_called: AtomicU32,
    query_latency_called: AtomicU32,
    query_success_aggregated: AtomicU32,
    query_success_raw: AtomicU32,
    query_error_aggregated: AtomicU32,
    query_error_raw: AtomicU32,
    dedup: AtomicU32,
    schema_validation_errors: AtomicU32,
}

impl PluginMetrics for MockMetrics {
    fn record_ingestion_success(&self) {
        self.ingestion_success.fetch_add(1, Ordering::SeqCst);
    }
    fn record_ingestion_error(&self) {
        self.ingestion_error.fetch_add(1, Ordering::SeqCst);
    }
    fn record_ingestion_latency_ms(&self, _elapsed_ms: f64) {
        self.ingestion_latency_called.fetch_add(1, Ordering::SeqCst);
    }
    fn record_dedup(&self) {
        self.dedup.fetch_add(1, Ordering::SeqCst);
    }
    fn record_schema_validation_error(&self) {
        self.schema_validation_errors.fetch_add(1, Ordering::SeqCst);
    }
    fn record_query_latency_ms(&self, _query_type: &str, _elapsed_ms: f64) {
        self.query_latency_called.fetch_add(1, Ordering::SeqCst);
    }
    fn record_query_success(&self, query_type: &str) {
        match query_type {
            "aggregated" => {
                self.query_success_aggregated.fetch_add(1, Ordering::SeqCst);
            }
            "raw" => {
                self.query_success_raw.fetch_add(1, Ordering::SeqCst);
            }
            other => panic!("unexpected query_type for success: {other}"),
        }
    }
    fn record_query_error(&self, query_type: &str) {
        match query_type {
            "aggregated" => {
                self.query_error_aggregated.fetch_add(1, Ordering::SeqCst);
            }
            "raw" => {
                self.query_error_raw.fetch_add(1, Ordering::SeqCst);
            }
            other => panic!("unexpected query_type for error: {other}"),
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_client(
    insert_port: Arc<dyn InsertPort>,
    metrics: Arc<MockMetrics>,
) -> TimescaleDbPluginClient {
    TimescaleDbPluginClient::new(insert_port, MockQueryPort::success(), metrics)
}

fn make_client_q(
    query_port: Arc<dyn QueryPort>,
    metrics: Arc<MockMetrics>,
) -> TimescaleDbPluginClient {
    TimescaleDbPluginClient::new(MockInsertPort::success(0), query_port, metrics)
}

fn base_counter_record() -> UsageRecord {
    UsageRecord {
        module: "test-module".to_owned(),
        tenant_id: Uuid::new_v4(),
        metric: "test.cpu".to_owned(),
        kind: UsageKind::Counter,
        value: 1.0,
        resource_id: Uuid::new_v4(),
        resource_type: "vm".to_owned(),
        subject: None,
        idempotency_key: "idem-key-1".to_owned(),
        timestamp: Utc::now(),
        metadata: None,
    }
}

fn base_gauge_record() -> UsageRecord {
    UsageRecord {
        kind: UsageKind::Gauge,
        idempotency_key: String::new(),
        ..base_counter_record()
    }
}

// ── create_usage_record tests ─────────────────────────────────────────────────

#[tokio::test]
async fn test_create_usage_record_valid_counter() {
    // Scenario: valid counter insert — DB mock returns 1 row affected
    let metrics = Arc::new(MockMetrics::default());
    let client = make_client(MockInsertPort::success(1), metrics.clone());

    let result = client.create_usage_record(base_counter_record()).await;

    assert!(result.is_ok());
    assert_eq!(
        metrics.ingestion_success.load(Ordering::SeqCst),
        1,
        "ingestion_success counter"
    );
    assert_eq!(
        metrics.ingestion_latency_called.load(Ordering::SeqCst),
        1,
        "latency histogram"
    );
}

#[tokio::test]
async fn test_create_usage_record_valid_gauge() {
    // Scenario: valid gauge insert — DB mock returns 1 row affected
    let metrics = Arc::new(MockMetrics::default());
    let client = make_client(MockInsertPort::success(1), metrics.clone());

    let result = client.create_usage_record(base_gauge_record()).await;

    assert!(result.is_ok());
    assert_eq!(
        metrics.ingestion_success.load(Ordering::SeqCst),
        1,
        "ingestion_success counter"
    );
}

#[tokio::test]
async fn test_create_usage_record_negative_counter_value_rejected() {
    // Scenario: counter with negative value rejected before any DB call
    let metrics = Arc::new(MockMetrics::default());
    let client = make_client(MockInsertPort::success(0), metrics.clone());
    let record = UsageRecord {
        value: -1.0,
        ..base_counter_record()
    };

    let result = client.create_usage_record(record).await;

    assert!(
        matches!(result, Err(UsageCollectorError::InvalidArgument { .. })),
        "expected InvalidArgument error for negative counter value"
    );
    assert_eq!(
        metrics.schema_validation_errors.load(Ordering::SeqCst),
        1,
        "validation error counter"
    );
    assert_eq!(
        metrics.ingestion_success.load(Ordering::SeqCst),
        0,
        "no success on validation failure"
    );
}

#[tokio::test]
async fn test_create_usage_record_missing_idempotency_key_for_counter_rejected() {
    // Scenario: counter without idempotency_key rejected before any DB call
    let metrics = Arc::new(MockMetrics::default());
    let client = make_client(MockInsertPort::success(0), metrics.clone());
    let record = UsageRecord {
        idempotency_key: String::new(),
        ..base_counter_record()
    };

    let result = client.create_usage_record(record).await;

    assert!(
        matches!(result, Err(UsageCollectorError::InvalidArgument { .. })),
        "expected InvalidArgument error for missing idempotency_key"
    );
    assert_eq!(
        metrics.schema_validation_errors.load(Ordering::SeqCst),
        1,
        "validation error counter"
    );
    assert_eq!(
        metrics.ingestion_success.load(Ordering::SeqCst),
        0,
        "no success on validation failure"
    );
}

#[tokio::test]
async fn test_create_usage_record_query_failed_maps_to_internal() {
    // Pins the classification: a non-transient `QueryFailed` from the insert
    // port must surface as `UsageCollectorError::Internal`, not as
    // `ServiceUnavailable`. The integration suite exercises real-DB unique
    // violations but does not assert the mapped variant; this mock-based
    // test is where the contract lives so a regression that swapped the
    // mapping (e.g. routed `23505` through the transient path) is caught
    // here before it ever reaches the production storage backend.
    let metrics = Arc::new(MockMetrics::default());
    let client = make_client(MockInsertPort::query_failed(), metrics.clone());

    let result = client.create_usage_record(base_counter_record()).await;

    assert!(
        matches!(result, Err(UsageCollectorError::Internal { .. })),
        "non-transient QueryFailed must map to Internal, got: {result:?}"
    );
    assert_eq!(
        metrics.ingestion_error.load(Ordering::SeqCst),
        1,
        "ingestion_error counter must record the failure"
    );
    assert_eq!(
        metrics.ingestion_success.load(Ordering::SeqCst),
        0,
        "no success on non-transient error"
    );
}

#[tokio::test]
async fn test_create_usage_record_unexpected_unique_violation_maps_to_internal() {
    // Pins the classification of the post-claim unique violation path: it
    // signals an internal invariant break (the idempotency-key claim said
    // the slot was free, then the records insert collided anyway), so it
    // must surface as `Internal`. A regression that routed this through the
    // transient path would mask a genuine bug behind a retry loop.
    let metrics = Arc::new(MockMetrics::default());
    let client = make_client(
        MockInsertPort::unexpected_unique_violation(),
        metrics.clone(),
    );

    let result = client.create_usage_record(base_counter_record()).await;

    assert!(
        matches!(result, Err(UsageCollectorError::Internal { .. })),
        "UnexpectedUniqueViolation must map to Internal, got: {result:?}"
    );
    assert_eq!(
        metrics.ingestion_error.load(Ordering::SeqCst),
        1,
        "ingestion_error counter must record the failure"
    );
    assert_eq!(
        metrics.ingestion_success.load(Ordering::SeqCst),
        0,
        "no success on internal error"
    );
}

#[tokio::test]
async fn test_create_usage_record_transient_db_error() {
    // Scenario: DB mock returns pool-timeout (transient); mapped to Unavailable
    let metrics = Arc::new(MockMetrics::default());
    let client = make_client(MockInsertPort::pool_timeout(), metrics.clone());

    let result = client.create_usage_record(base_counter_record()).await;

    assert!(
        matches!(result, Err(UsageCollectorError::ServiceUnavailable { .. })),
        "transient error must map to Unavailable"
    );
    assert_eq!(
        metrics.ingestion_error.load(Ordering::SeqCst),
        1,
        "ingestion_error counter"
    );
    assert_eq!(
        metrics.ingestion_success.load(Ordering::SeqCst),
        0,
        "no success on transient error"
    );
}

#[tokio::test]
async fn test_create_usage_record_idempotent_insert() {
    // Scenario: DB mock returns 0 rows affected (ON CONFLICT DO NOTHING); dedup recorded
    let metrics = Arc::new(MockMetrics::default());
    let client = make_client(MockInsertPort::success(0), metrics.clone());

    let result = client.create_usage_record(base_counter_record()).await;

    assert!(result.is_ok());
    assert_eq!(
        metrics.dedup.load(Ordering::SeqCst),
        1,
        "dedup counter must be incremented"
    );
    assert_eq!(
        metrics.ingestion_success.load(Ordering::SeqCst),
        0,
        "success not reported for dedup"
    );
}

#[tokio::test]
async fn test_create_usage_record_gauge_no_accumulation() {
    // Scenario: gauge value passed to insert equals submitted value — no accumulation applied
    let captured = Arc::new(Mutex::new(0.0_f64));
    let port = MockInsertPort::capturing(captured.clone());
    let metrics = Arc::new(MockMetrics::default());
    let client = make_client(port, metrics);

    let submitted_value = 42.75_f64;
    let record = UsageRecord {
        value: submitted_value,
        ..base_gauge_record()
    };
    client.create_usage_record(record).await.unwrap();

    let stored = *captured.lock().unwrap();
    assert!(
        (stored - submitted_value).abs() < f64::EPSILON,
        "gauge value must not be accumulated or transformed before insert",
    );
}

// `scope_to_sql` is covered by its own tests in `domain/scope.rs`; this file
// exercises the client wiring around it.

// ── query path tests ──────────────────────────────────────────────────────────

fn base_agg_query() -> AggregationQuery {
    AggregationQuery {
        scope: AccessScope::for_tenant(Uuid::new_v4()),
        time_range: (Utc::now() - chrono::Duration::hours(1), Utc::now()),
        function: AggregationFn::Sum,
        group_by: vec![],
        bucket_size: None,
        usage_type: None,
        resource_id: None,
        resource_type: None,
        subject_id: None,
        subject_type: None,
        source: None,
    }
}

fn base_raw_query() -> RawQuery {
    RawQuery {
        scope: AccessScope::for_tenant(Uuid::new_v4()),
        time_range: (Utc::now() - chrono::Duration::hours(1), Utc::now()),
        usage_type: None,
        resource_id: None,
        resource_type: None,
        subject_type: None,
        subject_id: None,
        cursor: None,
        page_size: 10,
    }
}

#[tokio::test]
async fn test_query_aggregated_forwards_query_and_returns_response() {
    // Pins both the latency-recording side effect and the wiring: the query the
    // client receives must reach the port unchanged, and the port's response must
    // travel back to the caller unchanged. A regression that swapped the query or
    // dropped the result would slip past a "just check latency" assertion.
    let scope_tid = Uuid::new_v4();
    let expected_query = AggregationQuery {
        scope: AccessScope::for_tenant(scope_tid),
        time_range: (Utc::now() - chrono::Duration::hours(2), Utc::now()),
        function: AggregationFn::Sum,
        group_by: vec![],
        bucket_size: None,
        usage_type: Some("test.metric".to_owned()),
        resource_id: None,
        resource_type: None,
        subject_id: None,
        subject_type: None,
        source: Some("billing".to_owned()),
    };
    let expected_response = vec![AggregationResult {
        function: AggregationFn::Sum,
        value: 42.0,
        bucket_start: None,
        usage_type: Some("test.metric".to_owned()),
        subject_id: None,
        subject_type: None,
        resource_id: None,
        resource_type: None,
        source: Some("billing".to_owned()),
    }];
    let metrics = Arc::new(MockMetrics::default());
    let port = MockQueryPort::with_agg_response(expected_response.clone());
    let client = make_client_q(port.clone(), metrics.clone());

    let result = client.query_aggregated(expected_query.clone()).await;

    let returned = result.expect("aggregated query must succeed");
    // `AggregationResult` only derives `PartialEq` inside the SDK's test cfg;
    // compare every field explicitly so a regression that zeroed any of them
    // during plugin-side mapping fails this test.
    assert_eq!(returned.len(), expected_response.len());
    let r = &returned[0];
    let e = &expected_response[0];
    assert_eq!(r.function, e.function);
    assert!((r.value - e.value).abs() < f64::EPSILON);
    assert_eq!(r.bucket_start, e.bucket_start);
    assert_eq!(r.usage_type, e.usage_type);
    assert_eq!(r.subject_id, e.subject_id);
    assert_eq!(r.subject_type, e.subject_type);
    assert_eq!(r.resource_id, e.resource_id);
    assert_eq!(r.resource_type, e.resource_type);
    assert_eq!(r.source, e.source);

    let captured = port
        .captured_agg
        .lock()
        .unwrap()
        .clone()
        .expect("port should have captured query");
    assert_eq!(captured.usage_type, expected_query.usage_type);
    assert_eq!(captured.source, expected_query.source);
    assert_eq!(captured.function, expected_query.function);
    assert_eq!(captured.time_range, expected_query.time_range);

    assert_eq!(
        metrics.query_latency_called.load(Ordering::SeqCst),
        1,
        "query latency must be recorded on success"
    );
    assert_eq!(
        metrics.query_success_aggregated.load(Ordering::SeqCst),
        1,
        "aggregated query success counter must increment on Ok"
    );
    assert_eq!(
        metrics.query_error_aggregated.load(Ordering::SeqCst),
        0,
        "aggregated query error counter must not increment on Ok"
    );
}

#[tokio::test]
async fn test_query_aggregated_error_propagates_and_records_latency() {
    let metrics = Arc::new(MockMetrics::default());
    let port = MockQueryPort::agg_failing();
    let client = make_client_q(port.clone(), metrics.clone());

    let query = base_agg_query();
    let result = client.query_aggregated(query.clone()).await;

    assert!(
        matches!(result, Err(UsageCollectorError::ServiceUnavailable { .. })),
        "agg_failing mock must propagate the ServiceUnavailable error variant"
    );
    let captured = port
        .captured_agg
        .lock()
        .unwrap()
        .clone()
        .expect("port should observe the forwarded query even on failure");
    assert_eq!(captured.time_range, query.time_range);
    assert_eq!(
        metrics.query_latency_called.load(Ordering::SeqCst),
        1,
        "query latency must be recorded even on error"
    );
    assert_eq!(
        metrics.query_error_aggregated.load(Ordering::SeqCst),
        1,
        "aggregated query error counter must increment on Err"
    );
    assert_eq!(
        metrics.query_success_aggregated.load(Ordering::SeqCst),
        0,
        "aggregated query success counter must not increment on Err"
    );
}

#[tokio::test]
async fn test_query_raw_forwards_query_and_returns_response() {
    let expected_query = base_raw_query();
    let expected_record = UsageRecord {
        value: 17.0,
        ..base_counter_record()
    };
    let expected_page = Page::new(
        vec![expected_record.clone()],
        PageInfo {
            next_cursor: Some("next-cursor".to_owned()),
            prev_cursor: None,
            limit: 10,
        },
    );

    let metrics = Arc::new(MockMetrics::default());
    let port = MockQueryPort::with_raw_response(expected_page.clone());
    let client = make_client_q(port.clone(), metrics.clone());

    let result = client.query_raw(expected_query.clone()).await;

    let returned = result.expect("raw query must succeed");
    // `UsageRecord` derives `PartialEq`; compare every field explicitly so a
    // regression that zeroed `subject`, `metric`, `tenant_id`, etc. during
    // plugin-side mapping fails this test instead of slipping past a "just
    // check value" assertion. `value` uses an explicit epsilon because
    // `PartialEq` on `f64` is `==` (NaN-bites — finite here, but pinning the
    // pattern keeps future record fixtures safe).
    assert_eq!(returned.items.len(), 1);
    let r = &returned.items[0];
    let e = &expected_record;
    assert_eq!(r.module, e.module);
    assert_eq!(r.tenant_id, e.tenant_id);
    assert_eq!(r.metric, e.metric);
    assert_eq!(r.kind, e.kind);
    assert!((r.value - e.value).abs() < f64::EPSILON);
    assert_eq!(r.resource_id, e.resource_id);
    assert_eq!(r.resource_type, e.resource_type);
    assert_eq!(r.subject, e.subject);
    assert_eq!(r.idempotency_key, e.idempotency_key);
    assert_eq!(r.timestamp, e.timestamp);
    assert_eq!(r.metadata, e.metadata);
    assert_eq!(
        returned.page_info.next_cursor.as_deref(),
        Some("next-cursor")
    );
    assert_eq!(
        returned.page_info.prev_cursor,
        expected_page.page_info.prev_cursor
    );
    assert_eq!(returned.page_info.limit, expected_page.page_info.limit);

    let captured = port
        .captured_raw
        .lock()
        .unwrap()
        .clone()
        .expect("port should have captured query");
    assert_eq!(captured.page_size, expected_query.page_size);
    assert_eq!(captured.time_range, expected_query.time_range);

    assert_eq!(
        metrics.query_latency_called.load(Ordering::SeqCst),
        1,
        "query latency must be recorded on success"
    );
    assert_eq!(
        metrics.query_success_raw.load(Ordering::SeqCst),
        1,
        "raw query success counter must increment on Ok"
    );
    assert_eq!(
        metrics.query_error_raw.load(Ordering::SeqCst),
        0,
        "raw query error counter must not increment on Ok"
    );
}

#[tokio::test]
async fn test_query_raw_error_propagates_and_records_latency() {
    let metrics = Arc::new(MockMetrics::default());
    let port = MockQueryPort::raw_failing();
    let client = make_client_q(port.clone(), metrics.clone());

    let query = base_raw_query();
    let result = client.query_raw(query.clone()).await;

    assert!(
        matches!(result, Err(UsageCollectorError::ServiceUnavailable { .. })),
        "raw_failing mock must propagate the ServiceUnavailable error variant"
    );
    let captured = port
        .captured_raw
        .lock()
        .unwrap()
        .clone()
        .expect("port should observe the forwarded query even on failure");
    assert_eq!(captured.page_size, query.page_size);
    assert_eq!(
        metrics.query_latency_called.load(Ordering::SeqCst),
        1,
        "query latency must be recorded even on error"
    );
    assert_eq!(
        metrics.query_error_raw.load(Ordering::SeqCst),
        1,
        "raw query error counter must increment on Err"
    );
    assert_eq!(
        metrics.query_success_raw.load(Ordering::SeqCst),
        0,
        "raw query success counter must not increment on Err"
    );
}

#[tokio::test]
async fn test_create_usage_record_nan_rejected() {
    // Production guard at client.rs: `record.value.is_finite()` rejects NaN
    // before any DB call. Covers the schema_validation_errors path for NaN.
    let metrics = Arc::new(MockMetrics::default());
    let client = make_client(MockInsertPort::success(0), metrics.clone());
    let record = UsageRecord {
        value: f64::NAN,
        ..base_gauge_record()
    };

    let result = client.create_usage_record(record).await;

    assert!(
        matches!(result, Err(UsageCollectorError::InvalidArgument { .. })),
        "expected InvalidArgument error for NaN value"
    );
    assert_eq!(
        metrics.schema_validation_errors.load(Ordering::SeqCst),
        1,
        "validation error counter must increment for NaN"
    );
    assert_eq!(
        metrics.ingestion_success.load(Ordering::SeqCst),
        0,
        "no success on validation failure"
    );
}

#[tokio::test]
async fn test_create_usage_record_positive_infinity_rejected() {
    // Production guard at client.rs: `record.value.is_finite()` rejects +Inf
    // before any DB call. Split from the -Inf case so a regression that
    // accepts one but rejects the other surfaces with a precise failure site.
    let metrics = Arc::new(MockMetrics::default());
    let client = make_client(MockInsertPort::success(0), metrics.clone());
    let record = UsageRecord {
        value: f64::INFINITY,
        ..base_gauge_record()
    };

    let result = client.create_usage_record(record).await;

    assert!(
        matches!(result, Err(UsageCollectorError::InvalidArgument { .. })),
        "expected InvalidArgument error for +Inf value"
    );
    assert_eq!(
        metrics.schema_validation_errors.load(Ordering::SeqCst),
        1,
        "validation error counter must increment for +Inf"
    );
    assert_eq!(
        metrics.ingestion_success.load(Ordering::SeqCst),
        0,
        "no success on validation failure"
    );
}

#[tokio::test]
async fn test_create_usage_record_negative_infinity_rejected() {
    // Production guard at client.rs: `record.value.is_finite()` rejects -Inf
    // before any DB call. Split from the +Inf case so a regression that
    // accepts one but rejects the other surfaces with a precise failure site.
    let metrics = Arc::new(MockMetrics::default());
    let client = make_client(MockInsertPort::success(0), metrics.clone());
    let record = UsageRecord {
        value: f64::NEG_INFINITY,
        ..base_gauge_record()
    };

    let result = client.create_usage_record(record).await;

    assert!(
        matches!(result, Err(UsageCollectorError::InvalidArgument { .. })),
        "expected InvalidArgument error for -Inf value"
    );
    assert_eq!(
        metrics.schema_validation_errors.load(Ordering::SeqCst),
        1,
        "validation error counter must increment for -Inf"
    );
    assert_eq!(
        metrics.ingestion_success.load(Ordering::SeqCst),
        0,
        "no success on validation failure"
    );
}

#[tokio::test]
async fn test_create_usage_record_negative_gauge_allowed() {
    // Gauges are allowed to carry negative values (e.g. delta-style metrics).
    // Only counters enforce non-negativity at the validation layer; this test
    // pins that contract so a future change cannot tighten it silently.
    let metrics = Arc::new(MockMetrics::default());
    let client = make_client(MockInsertPort::success(1), metrics.clone());
    let record = UsageRecord {
        value: -3.5,
        ..base_gauge_record()
    };

    let result = client.create_usage_record(record).await;

    assert!(result.is_ok(), "negative gauge values must be accepted");
    assert_eq!(
        metrics.schema_validation_errors.load(Ordering::SeqCst),
        0,
        "no validation error for a negative gauge"
    );
    assert_eq!(
        metrics.ingestion_success.load(Ordering::SeqCst),
        1,
        "success counter must increment for a valid gauge insert"
    );
}
