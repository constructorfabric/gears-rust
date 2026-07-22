// Test modules using bare `panic!` opt in explicitly
// (clippy.toml allows unwrap/expect in tests, not panic).
#![allow(clippy::panic)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use toolkit_odata::{CursorV1, ODataOrderBy, ODataQuery, OrderKey, SortDir};
use usage_collector_sdk::{UsageCollectorPluginError, UsageTypeGtsId};

use super::{
    ConflictRead, DedupKey, MAX_BATCH_ATTEMPTS, PgRecordStore, batch_retry_backoff,
    batch_retry_backoff_base, canonical_equal, dedup_key, is_retryable_batch_error, plan_batch,
    with_retry,
};
use crate::domain::ports::RecordStore;
use crate::infra::metrics::Metrics;
use crate::infra::storage::entity::UsageRecordRow;

const VCPU_GTS: &str = "gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1";

/// A store over a lazy pool: no connection is opened, so the pre-DB validation
/// paths under test return before any query is issued. The tiny acquire timeout
/// keeps an accidental DB touch from hanging the test.
fn lazy_store() -> PgRecordStore {
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy("postgres://user:pass@localhost/db")
        .expect("a syntactically valid DSN yields a lazy pool without connecting");
    PgRecordStore::new(
        pool.clone(),
        Arc::new(Metrics::new(pool)),
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn list_rejects_cursor_whose_sort_order_differs_from_query() {
    let store = lazy_store();
    let gts_id = UsageTypeGtsId::new(VCPU_GTS).expect("valid gts id");

    // The live query sorts (created_at asc, id asc); the cursor was minted
    // under a different order (id first). The keys are individually valid, so
    // without the guard the request binds old key strings against new columns —
    // silently wrong pagination. The filter hash agrees (both unset), so only
    // the sort-order guard can reject this.
    let query = ODataQuery::new()
        .with_order(ODataOrderBy(vec![
            OrderKey {
                field: "created_at".to_owned(),
                dir: SortDir::Asc,
            },
            OrderKey {
                field: "id".to_owned(),
                dir: SortDir::Asc,
            },
        ]))
        .with_cursor(CursorV1 {
            k: vec![
                "2024-01-01T00:00:00Z".to_owned(),
                "00000000-0000-0000-0000-000000000001".to_owned(),
            ],
            o: SortDir::Asc,
            s: "+id,+created_at".to_owned(),
            f: None,
            d: "fwd".to_owned(),
        });

    let err = store
        .list(gts_id, &query, &[])
        .await
        .expect_err("a cursor minted under a different order must be rejected");

    match err {
        UsageCollectorPluginError::Internal(msg) => {
            assert!(
                msg.contains("sort order"),
                "unexpected error message: {msg}"
            );
        }
        other => panic!("expected an Internal sort-order mismatch, got {other:?}"),
    }
}

#[tokio::test]
async fn list_rejects_backward_cursor() {
    let store = lazy_store();
    let gts_id = UsageTypeGtsId::new(VCPU_GTS).expect("valid gts id");

    // A backward cursor whose filter hash and sort order both agree with the
    // query, so only the direction guard can reject it. Without the guard the
    // request would page FORWARD (the keyset operator is derived from the sort
    // direction, not `d`) and silently return the wrong page.
    let query = ODataQuery::new()
        .with_order(ODataOrderBy(vec![
            OrderKey {
                field: "created_at".to_owned(),
                dir: SortDir::Asc,
            },
            OrderKey {
                field: "id".to_owned(),
                dir: SortDir::Asc,
            },
        ]))
        .with_cursor(CursorV1 {
            k: vec![
                "2024-01-01T00:00:00Z".to_owned(),
                "00000000-0000-0000-0000-000000000001".to_owned(),
            ],
            o: SortDir::Asc,
            s: "+created_at,+id".to_owned(),
            f: None,
            d: "bwd".to_owned(),
        });

    let err = store
        .list(gts_id, &query, &[])
        .await
        .expect_err("a backward cursor must be rejected before any DB access");

    match err {
        UsageCollectorPluginError::Internal(msg) => {
            assert!(msg.contains("direction"), "unexpected error message: {msg}");
        }
        other => panic!("expected an Internal direction error, got {other:?}"),
    }
}

/// Minimal in-memory `UsageRecord` for pure (no-DB) unit tests.
fn unit_record(tenant: uuid::Uuid, idem: &str, seq: u128) -> usage_collector_sdk::UsageRecord {
    usage_collector_sdk::UsageRecord {
        id: uuid::Uuid::from_u128(seq),
        gts_id: usage_collector_sdk::UsageTypeGtsId::new(VCPU_GTS).expect("valid gts id"),
        tenant_id: tenant,
        resource_ref: usage_collector_sdk::ResourceRef::new("res-1", "compute.vm")
            .expect("valid resource_ref"),
        subject_ref: None,
        metadata: std::collections::BTreeMap::new(),
        value: rust_decimal::Decimal::new(1, 0),
        idempotency_key: usage_collector_sdk::IdempotencyKey::new(idem).expect("valid idem key"),
        corrects_id: None,
        status: usage_collector_sdk::UsageRecordStatus::Active,
        created_at: time::OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("valid ts"),
    }
}

#[test]
fn plan_batch_collapses_and_sorts_distinct_keys() {
    let tenant = uuid::Uuid::from_u128(1);
    let mk = |idem: &str, seq: u128| unit_record(tenant, idem, seq);
    let records = vec![
        mk("kb", 10), // idx 0
        mk("ka", 11), // idx 1
        mk("kb", 12), // idx 2 — duplicate of idx 0's key
        mk("kc", 13), // idx 3
    ];

    let plan = plan_batch(&records);

    let idems: Vec<&str> = plan
        .reps
        .iter()
        .map(|r| r.idempotency_key.as_str())
        .collect();
    assert_eq!(idems, vec!["ka", "kb", "kc"], "distinct, sorted by key");

    assert_eq!(
        plan.first_index[&dedup_key(&records[1])],
        1,
        "ka first at idx 1"
    );
    assert_eq!(
        plan.first_index[&dedup_key(&records[0])],
        0,
        "kb first at idx 0"
    );
    assert_eq!(
        plan.first_index[&dedup_key(&records[3])],
        3,
        "kc first at idx 3"
    );

    let kb_rep = plan
        .reps
        .iter()
        .find(|r| r.idempotency_key.as_str() == "kb")
        .expect("kb rep present");
    assert_eq!(
        kb_rep.id,
        uuid::Uuid::from_u128(10),
        "kb rep is the first occurrence"
    );
}

/// A `UsageRecordRow` whose canonical fields equal `record`, carrying the
/// given stored `metadata` jsonb verbatim. Used to exercise `canonical_equal`'s
/// absorb/conflict/decode-failure paths without a database.
fn row_matching(
    record: &usage_collector_sdk::UsageRecord,
    metadata: serde_json::Value,
) -> UsageRecordRow {
    UsageRecordRow {
        id: record.id,
        tenant_id: record.tenant_id,
        gts_id: VCPU_GTS.to_owned(),
        value: record.value,
        created_at: record.created_at,
        resource_id: record.resource_ref.resource_id().to_owned(),
        resource_type: record.resource_ref.resource_type().to_owned(),
        subject_id: None,
        subject_type: None,
        idempotency_key: record.idempotency_key.as_str().to_owned(),
        corrects_id: record.corrects_id,
        status: "active".to_owned(),
        metadata,
        ingested_at: record.created_at,
    }
}

#[test]
fn canonical_equal_surfaces_corrupt_stored_metadata_as_internal() {
    let tenant = uuid::Uuid::from_u128(7);
    let record = unit_record(tenant, "k", 700);
    // Stored metadata that cannot decode back to the typed map (a JSON string,
    // not an object). Every other canonical field matches, so the old `.ok()`
    // swallow turned this stored-data corruption into a silent `IdempotencyConflict`.
    let row = row_matching(&record, serde_json::Value::String("corrupt".to_owned()));

    let err = canonical_equal(&row, &record)
        .expect_err("a corrupt stored metadata blob must surface as an error, not absorb/conflict");

    match err {
        UsageCollectorPluginError::Internal(msg) => {
            assert!(msg.contains("metadata"), "unexpected error message: {msg}");
        }
        other => panic!("expected an Internal stored-metadata-decode error, got {other:?}"),
    }
}

#[test]
fn canonical_equal_absorbs_an_exact_match() {
    let tenant = uuid::Uuid::from_u128(8);
    let record = unit_record(tenant, "k", 800);
    let row = row_matching(&record, serde_json::Value::Object(serde_json::Map::new()));

    assert!(
        canonical_equal(&row, &record).expect("valid metadata decodes"),
        "a row whose canonical fields all match must compare equal"
    );
}

#[test]
fn canonical_equal_reports_a_field_mismatch_as_not_equal() {
    let tenant = uuid::Uuid::from_u128(9);
    let record = unit_record(tenant, "k", 900);
    let mut row = row_matching(&record, serde_json::Value::Object(serde_json::Map::new()));
    row.value = rust_decimal::Decimal::new(999, 0);

    assert!(
        !canonical_equal(&row, &record).expect("valid metadata decodes"),
        "a differing canonical field must compare not-equal (the conflict path)"
    );
}

#[test]
fn canonical_equal_treats_id_as_canonical() {
    // The record `id` is part of the canonical set: a same-key request whose
    // other canonical fields all match but whose stored `id` differs is a
    // fail-closed `IdempotencyConflict`, not a silent absorb. Since the SDK made
    // `id` a deterministic projection of the dedup key, a real dedup hit always
    // carries a matching id — so this is now a defensive guard against a
    // corrupted stored row (a non-deterministic id) rather than a mismatched
    // caller-supplied one; see `canonical_equal`'s doc.
    let tenant = uuid::Uuid::from_u128(10);
    let record = unit_record(tenant, "k", 1000);
    let mut row = row_matching(&record, serde_json::Value::Object(serde_json::Map::new()));
    row.id = uuid::Uuid::from_u128(0xDEAD_BEEF);
    assert_ne!(row.id, record.id, "test setup: the ids must differ");

    assert!(
        !canonical_equal(&row, &record).expect("valid metadata decodes"),
        "a differing id is a canonical-field mismatch; the request must conflict"
    );
}

#[test]
fn canonical_equal_ignores_created_at() {
    // Approach A: `created_at` joins the dedup key (the 4-tuple
    // `(tenant, gts, idem, created_at)` UNIQUE), so `canonical_equal` — which
    // only runs once that key has already matched — no longer compares it. A row
    // whose `created_at` differs but whose every other canonical field matches
    // must still compare equal (absorb), never conflict on the timestamp.
    let tenant = uuid::Uuid::from_u128(11);
    let record = unit_record(tenant, "k", 1100);
    let mut row = row_matching(&record, serde_json::Value::Object(serde_json::Map::new()));
    row.created_at = record.created_at + time::Duration::seconds(5);

    assert!(
        canonical_equal(&row, &record).expect("valid metadata decodes"),
        "created_at is part of the dedup key now, not a compared canonical field"
    );
}

#[test]
fn dedup_key_includes_created_at_but_not_value() {
    // Approach A: the dedup identity is the 4-tuple
    // `(tenant, gts, idem, created_at)` enforced on the hypertable's own UNIQUE,
    // so two records sharing a key but carrying different event times are
    // distinct slots (silent duplicate), not a conflict. `value` remains outside
    // the key (it is a compared canonical field).
    let tenant = uuid::Uuid::from_u128(2);
    let a = unit_record(tenant, "same", 100);

    let mut diff_time = unit_record(tenant, "same", 200);
    diff_time.created_at = a.created_at + time::Duration::seconds(5);
    assert_ne!(
        dedup_key(&a),
        dedup_key(&diff_time),
        "created_at is part of the dedup identity (the 4-tuple)"
    );

    let mut diff_value = unit_record(tenant, "same", 300);
    diff_value.value = rust_decimal::Decimal::new(999, 0);
    assert_eq!(
        dedup_key(&a),
        dedup_key(&diff_value),
        "value is not part of the dedup key"
    );
}

// --- `resolve_batch` invariant-break / defensive arms (DB-free) ---
//
// `resolve_batch` is a pure function of its (`won`, `inserted`, `conflict`)
// maps, so these arms are exercised over a lazy pool that is never touched. The
// branches below fire only on a broken DB invariant (a won slot with no
// inserted record) or a cleanup race (the dedup pointer vanished between claim
// and read) — unreachable from the happy-path integration tests, hence easy to
// break silently. Each is pinned here against a hand-built map.

#[tokio::test]
async fn resolve_batch_winner_with_no_inserted_record_is_internal() {
    let store = lazy_store();
    let tenant = uuid::Uuid::from_u128(0xB1);
    let records = vec![unit_record(tenant, "win", 0x10)];
    let plan = plan_batch(&records);
    let key = dedup_key(&records[0]);

    // We claimed (won) the slot, but the multi-row insert returned no row for it
    // — a concurrent-insert invariant break, not a normal outcome.
    let won = HashSet::from([key]);
    let inserted: HashMap<DedupKey, UsageRecordRow> = HashMap::new();
    let conflict: HashMap<DedupKey, ConflictRead> = HashMap::new();

    let results = store.resolve_batch(&records, &plan, &won, &inserted, &conflict);

    assert_eq!(results.len(), 1, "one result per input row");
    match results.into_iter().next().expect("one result") {
        Err(UsageCollectorPluginError::Internal(msg)) => assert!(
            msg.contains("no inserted record was returned"),
            "unexpected message: {msg}"
        ),
        other => panic!("a won slot with no inserted record must be Internal, got {other:?}"),
    }
}

#[tokio::test]
async fn resolve_batch_intra_batch_dup_of_won_key_with_no_record_is_internal() {
    let store = lazy_store();
    let tenant = uuid::Uuid::from_u128(0xB2);
    // Two rows share one dedup key: idx 0 is the winner, idx 1 the in-batch dup.
    let records = vec![
        unit_record(tenant, "dup", 0x20),
        unit_record(tenant, "dup", 0x21),
    ];
    let plan = plan_batch(&records);
    let key = dedup_key(&records[0]);

    // Won the slot, but no inserted record came back for it.
    let won = HashSet::from([key]);
    let inserted: HashMap<DedupKey, UsageRecordRow> = HashMap::new();
    let conflict: HashMap<DedupKey, ConflictRead> = HashMap::new();

    let results = store.resolve_batch(&records, &plan, &won, &inserted, &conflict);

    assert_eq!(results.len(), 2, "one result per input row");
    // idx 0 hits the winner-missing Internal arm...
    assert!(
        matches!(results[0], Err(UsageCollectorPluginError::Internal(_))),
        "winner with no inserted record is Internal: {:?}",
        results[0]
    );
    // ...idx 1 is the in-batch duplicate of that won key — the distinct second
    // Internal arm, identified by its message.
    match &results[1] {
        Err(UsageCollectorPluginError::Internal(msg)) => assert!(
            msg.contains("intra-batch duplicate"),
            "unexpected message: {msg}"
        ),
        other => {
            panic!("intra-batch dup of a won key with no record must be Internal, got {other:?}")
        }
    }
}

#[tokio::test]
async fn resolve_batch_missing_conflict_entry_falls_through_to_transient() {
    let store = lazy_store();
    let tenant = uuid::Uuid::from_u128(0xB4);
    let records = vec![unit_record(tenant, "absent", 0x40)];
    let plan = plan_batch(&records);

    // Not won, and the conflict map has no entry for the key at all. The
    // defensive `None` fallthrough must still be a retryable Transient — never a
    // silent success and never a panic.
    let won: HashSet<DedupKey> = HashSet::new();
    let inserted: HashMap<DedupKey, UsageRecordRow> = HashMap::new();
    let conflict: HashMap<DedupKey, ConflictRead> = HashMap::new();

    let results = store.resolve_batch(&records, &plan, &won, &inserted, &conflict);

    assert_eq!(results.len(), 1);
    assert!(
        matches!(results[0], Err(UsageCollectorPluginError::Transient { .. })),
        "a key absent from the conflict map must fall through to Transient: {:?}",
        results[0]
    );
}

// --- Bounded-retry combinator (`with_retry`) — DB-free mechanics ---
//
// The combinator wraps the whole `create_batch_inner` call so a rare deadlock
// victim (`40P01` → outer `Transient`) self-heals. These tests pin its
// mechanics without a database; the `should_retry` predicate and the zero
// backoff are injected so each case is deterministic and instant.

/// Backoff fed to the combinator in tests: never actually sleep.
fn no_backoff(_attempt: u32) -> Duration {
    Duration::ZERO
}

#[tokio::test]
async fn with_retry_calls_operation_once_on_immediate_success() {
    let calls = AtomicU32::new(0);
    let retries = AtomicU32::new(0);
    let result: Result<u32, u32> = with_retry(
        3,
        no_backoff,
        |_err| true, // would retry, but the operation succeeds first try
        |_attempt, _err| {
            retries.fetch_add(1, Ordering::SeqCst);
        },
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok::<u32, u32>(7))
        },
    )
    .await;

    assert_eq!(result, Ok(7), "first-try success is returned verbatim");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "success on the first attempt runs the operation exactly once"
    );
    assert_eq!(
        retries.load(Ordering::SeqCst),
        0,
        "on_retry must not fire when the first attempt succeeds (no false retry signal)"
    );
}

#[tokio::test]
async fn with_retry_retries_a_retryable_error_then_returns_the_eventual_ok() {
    let calls = AtomicU32::new(0);
    // Fail (retryably) twice, then succeed on the third attempt.
    let result: Result<u32, u32> = with_retry(
        5,
        no_backoff,
        |_err| true,
        |_attempt, _err| {},
        || {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            std::future::ready(if n < 3 { Err(n) } else { Ok(n) })
        },
    )
    .await;

    assert_eq!(result, Ok(3), "the eventual Ok is returned");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "two retryable failures then success -> N+1 = 3 calls"
    );
}

#[tokio::test]
async fn with_retry_invokes_on_retry_once_before_each_retry() {
    let retries = std::sync::Mutex::new(Vec::<u32>::new());
    // Fail (retryably) twice, then succeed on the third attempt.
    let calls = AtomicU32::new(0);
    let result: Result<u32, u32> = with_retry(
        5,
        no_backoff,
        |_err| true,
        |attempt, err| retries.lock().unwrap().push(attempt * 10 + *err),
        || {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            std::future::ready(if n < 3 { Err(n) } else { Ok(n) })
        },
    )
    .await;

    assert_eq!(result, Ok(3), "the eventual Ok is returned");
    // on_retry fires once per retry, receiving the failed 1-based attempt number
    // and the error it failed with (encoded here as attempt*10 + err: attempt 1
    // failed with err 1 -> 11; attempt 2 failed with err 2 -> 22).
    assert_eq!(
        *retries.lock().unwrap(),
        vec![11, 22],
        "on_retry fires before each retry with the failed attempt number and error"
    );
}

#[tokio::test]
async fn with_retry_stops_at_max_attempts_and_returns_the_last_error() {
    let calls = AtomicU32::new(0);
    let result: Result<u32, u32> = with_retry(
        3,
        no_backoff,
        |_err| true, // always retryable, but the cap bounds it
        |_attempt, _err| {},
        || {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            std::future::ready(Err::<u32, u32>(n))
        },
    )
    .await;

    assert_eq!(result, Err(3), "the last error is returned unchanged");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "a forever-retryable error runs exactly max_attempts times"
    );
}

#[tokio::test]
async fn with_retry_does_not_retry_a_non_retryable_error() {
    let calls = AtomicU32::new(0);
    let retries = AtomicU32::new(0);
    let result: Result<u32, u32> = with_retry(
        3,
        no_backoff,
        |_err| false, // predicate rejects every error → no retry
        |_attempt, _err| {
            retries.fetch_add(1, Ordering::SeqCst);
        },
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err::<u32, u32>(42))
        },
    )
    .await;

    assert_eq!(result, Err(42), "the non-retryable error is returned as-is");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a non-retryable error returns after a single attempt"
    );
    assert_eq!(
        retries.load(Ordering::SeqCst),
        0,
        "on_retry must not fire when the error is not retried"
    );
}

// --- The `create_batch` retry predicate ---

#[test]
fn batch_retry_predicate_retries_only_transient() {
    // Transient (the deadlock victim, serialization failure, connection blip
    // all collapse to this) → retry.
    assert!(
        is_retryable_batch_error(&UsageCollectorPluginError::transient(
            "deadlock victim; whole txn rolled back"
        )),
        "an outer Transient must be retried"
    );

    // Non-retryable buckets → no retry.
    assert!(
        !is_retryable_batch_error(&UsageCollectorPluginError::internal("invariant break")),
        "Internal must not be retried"
    );
    assert!(
        !is_retryable_batch_error(&UsageCollectorPluginError::IdempotencyConflict {
            idempotency_key: "k".to_owned(),
            existing_id: uuid::Uuid::from_u128(1),
        }),
        "IdempotencyConflict must not be retried"
    );
}

// --- The `create_batch` backoff schedule ---

#[test]
fn batch_retry_backoff_base_is_short_and_non_decreasing() {
    // The deterministic pre-jitter schedule: a deadlock victim can retry almost
    // immediately, so it stays small and never shrinks between attempts.
    let mut prev = Duration::ZERO;
    for attempt in 1..MAX_BATCH_ATTEMPTS {
        let d = batch_retry_backoff_base(attempt);
        assert!(
            d >= prev,
            "base backoff must not decrease (attempt {attempt})"
        );
        assert!(
            d <= Duration::from_millis(100),
            "base backoff stays small for a deadlock victim (attempt {attempt}): {d:?}"
        );
        prev = d;
    }
}

#[test]
fn batch_retry_backoff_applies_full_jitter_within_base() {
    // Full jitter: every sampled backoff lands in `[0, base]`, so batches that
    // deadlocked together spread across the window instead of retrying in
    // lockstep (thundering herd). Sampled repeatedly since the value is random
    // per call; the run must also observe at least one sub-base value, proving
    // the jitter is actually applied and not a no-op.
    for attempt in 1..MAX_BATCH_ATTEMPTS {
        let base = batch_retry_backoff_base(attempt);
        let mut seen_below_base = false;
        for _ in 0..1_000 {
            let d = batch_retry_backoff(attempt);
            assert!(
                d <= base,
                "jittered backoff must not exceed its base (attempt {attempt}): {d:?} > {base:?}"
            );
            if d < base {
                seen_below_base = true;
            }
        }
        assert!(
            seen_below_base,
            "full jitter must sometimes back off less than the base (attempt {attempt})"
        );
    }
}

#[tokio::test]
async fn acquire_failure_clears_ready_gauge() {
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

    // Local in-memory meter provider so the gauge read is parallel-safe (never
    // touches opentelemetry::global), mirroring metrics_tests.
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();

    // A lazy pool pointed at a dead port: the first acquire is refused fast.
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(200))
        .connect_lazy("postgres://user:pass@127.0.0.1:1/db")
        .expect("a syntactically valid DSN yields a lazy pool without connecting");
    let metrics = Arc::new(Metrics::with_meter(
        &provider.meter("uc.timescaledb"),
        pool.clone(),
    ));
    let store = PgRecordStore::new(pool, metrics, CancellationToken::new());

    // Every operation routes through timed_acquire; call it directly.
    let result = store.timed_acquire().await;
    assert!(result.is_err(), "acquire against a dead port must fail");

    provider.force_flush().expect("flush in-memory metrics");

    // Read the last value of the `uc_timescaledb_ready` gauge.
    let ready = {
        let metrics = exporter.get_finished_metrics().expect("collected metrics");
        let mut found = None;
        for rm in &metrics {
            for sm in rm.scope_metrics() {
                for m in sm.metrics() {
                    if m.name() == "uc_timescaledb_ready"
                        && let AggregatedMetrics::U64(MetricData::Gauge(g)) = m.data()
                    {
                        found = g
                            .data_points()
                            .next()
                            .map(opentelemetry_sdk::metrics::data::GaugeDataPoint::value);
                    }
                }
            }
        }
        found
    };
    assert_eq!(
        ready,
        Some(0),
        "a pool-acquire failure must clear the readiness gauge to 0"
    );
}
