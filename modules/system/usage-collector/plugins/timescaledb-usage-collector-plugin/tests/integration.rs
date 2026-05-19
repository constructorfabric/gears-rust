// @cpt-dod:cpt-cf-usage-collector-dod-production-storage-plugin-testing-and-observability:p10
//! Level 2 integration tests for the `TimescaleDB` usage-collector storage plugin.
//!
//! Run with:
//!   cargo test -p cyberware-timescaledb-usage-collector-plugin --features integration

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Timelike, Utc};
use modkit_odata::CursorV1;
use modkit_security::{AccessScope, ScopeConstraint, ScopeFilter, pep_properties};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::{
    ContainerAsync, ContainerRequest, GenericImage, ImageExt, runners::AsyncRunner,
};
use usage_collector_sdk::models::{
    AggregationFn, AggregationQuery, BucketSize, GroupByDimension, RawQuery, Subject, UsageKind,
    UsageRecord,
};
use usage_collector_sdk::{UsageCollectorError, UsageCollectorPluginClientV1};
use uuid::Uuid;

use timescaledb_usage_collector_plugin::__integration_test_api::domain::client::TimescaleDbPluginClient;
use timescaledb_usage_collector_plugin::__integration_test_api::domain::insert_port::InsertPort;
use timescaledb_usage_collector_plugin::__integration_test_api::domain::metrics::{
    NoopMetrics, PluginMetrics,
};
use timescaledb_usage_collector_plugin::__integration_test_api::domain::query_port::QueryPort;
use timescaledb_usage_collector_plugin::__integration_test_api::infra::continuous_aggregate::setup_continuous_aggregate;
use timescaledb_usage_collector_plugin::__integration_test_api::infra::migrations::run_migrations;
use timescaledb_usage_collector_plugin::__integration_test_api::infra::pg_insert_port::PgInsertPort;
use timescaledb_usage_collector_plugin::__integration_test_api::infra::pg_query_port::PgQueryPort;
use timescaledb_usage_collector_plugin::__integration_test_api::module::health_check_for_tests;

// ── Container and pool setup ──────────────────────────────────────────────────

struct TestDb {
    _container: ContainerAsync<GenericImage>,
    pool: PgPool,
}

fn timescaledb_image() -> GenericImage {
    GenericImage::new("timescale/timescaledb", "latest-pg16")
        .with_exposed_port(ContainerPort::Tcp(5432))
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
}

/// Starts a `TimescaleDB` container, waits for it to be ready, runs migrations,
/// sets up the continuous aggregate, and returns the container drop handle and pool.
async fn setup_container_and_pool() -> TestDb {
    let container = ContainerRequest::from(timescaledb_image())
        .with_env_var("POSTGRES_PASSWORD", "testpass")
        .with_env_var("POSTGRES_USER", "testuser")
        .with_env_var("POSTGRES_DB", "testdb")
        .start()
        .await
        .expect("failed to start timescaledb container");

    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("failed to get mapped port for 5432");

    let url = format!("postgres://testuser:testpass@127.0.0.1:{port}/testdb");

    let pool = connect_with_retry(&url, 60).await;

    run_migrations(&pool)
        .await
        .expect("schema migration failed");
    setup_continuous_aggregate(&pool)
        .await
        .expect("continuous aggregate setup failed");

    TestDb {
        _container: container,
        pool,
    }
}

async fn connect_with_retry(url: &str, timeout_secs: u64) -> PgPool {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(5))
            .connect(url)
            .await
        {
            Ok(pool) => return pool,
            Err(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => panic!("timed out waiting for database: {e}"),
        }
    }
}

// ── Client construction helper ────────────────────────────────────────────────

fn make_client(pool: &PgPool) -> TimescaleDbPluginClient {
    let insert_port: Arc<dyn InsertPort> = Arc::new(PgInsertPort::new(pool.clone()));
    let query_port: Arc<dyn QueryPort> = Arc::new(PgQueryPort::new(pool.clone()));
    let metrics: Arc<dyn PluginMetrics> = Arc::new(NoopMetrics);
    TimescaleDbPluginClient::new(insert_port, query_port, metrics)
}

/// Like [`make_client`], but constructs the query port with an injected
/// `max_agg_rows` cap so the cost-control invariant can be exercised without
/// inserting > 10 000 rows. Used by `query_aggregated_result_too_large_small_cap`.
fn make_client_with_max_agg_rows(pool: &PgPool, max_agg_rows: usize) -> TimescaleDbPluginClient {
    let insert_port: Arc<dyn InsertPort> = Arc::new(PgInsertPort::new(pool.clone()));
    let query_port: Arc<dyn QueryPort> = Arc::new(PgQueryPort::new_with_max_agg_rows(
        pool.clone(),
        max_agg_rows,
    ));
    let metrics: Arc<dyn PluginMetrics> = Arc::new(NoopMetrics);
    TimescaleDbPluginClient::new(insert_port, query_port, metrics)
}

// ── CAGG-path test helpers ────────────────────────────────────────────────────

/// Returns a timestamp `hours_back` hours before `now()`, floored to the hour.
///
/// The CAGG path requires hour-aligned endpoints and an end no later than the
/// materialized horizon (`now - 1h`). Tests that exercise the CAGG path build
/// their timestamps from this helper so the routing predicate
/// (`cagg_safe_range`) keeps the query on the CAGG instead of silently
/// downgrading it to the raw hypertable.
fn aligned_past_hour(hours_back: i64) -> DateTime<Utc> {
    let target = Utc::now() - chrono::Duration::hours(hours_back);
    target
        .with_minute(0)
        .unwrap()
        .with_second(0)
        .unwrap()
        .with_nanosecond(0)
        .unwrap()
}

// ── Test-record factory ───────────────────────────────────────────────────────

fn counter_record(tenant_id: Uuid, resource_id: Uuid, key: &str) -> UsageRecord {
    UsageRecord {
        module: "integration-test".to_owned(),
        tenant_id,
        metric: "test.cpu".to_owned(),
        kind: UsageKind::Counter,
        value: 10.0,
        resource_id,
        resource_type: "vm".to_owned(),
        subject: None,
        idempotency_key: key.to_owned(),
        timestamp: Utc::now(),
        metadata: None,
    }
}

// ── Test 1: migration_idempotency ─────────────────────────────────────────────

#[cfg(feature = "integration")]
#[tokio::test]
async fn migration_idempotency() {
    let db = setup_container_and_pool().await;

    // Second run must succeed (migrations are idempotent)
    run_migrations(&db.pool)
        .await
        .expect("second migration run must succeed - migrations are not idempotent");

    // Verify hypertable exists after both runs
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM timescaledb_information.hypertables \
         WHERE hypertable_name = 'usage_records'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("failed to query timescaledb_information.hypertables");

    assert_eq!(
        count, 1,
        "usage_records hypertable must exist after idempotent migration"
    );

    // The hypertable check above survives even if every `CREATE INDEX IF NOT EXISTS`
    // or `ADD COLUMN IF NOT EXISTS` step silently regresses on the second pass —
    // those are the steps most likely to break under future TimescaleDB or
    // PostgreSQL version changes. Assert each one explicitly.
    let expected_indexes = [
        ("usage_records", "idx_usage_records_tenant_time"),
        ("usage_records", "idx_usage_records_tenant_metric_time"),
        ("usage_records", "idx_usage_records_tenant_subject_time"),
        ("usage_records", "idx_usage_records_tenant_resource_time"),
        (
            "usage_idempotency_keys",
            "idx_usage_idempotency_keys_created_at",
        ),
    ];
    for (table_name, idx_name) in expected_indexes {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS ( \
                 SELECT 1 FROM pg_indexes \
                 WHERE schemaname = 'public' \
                 AND tablename = $1 \
                 AND indexname = $2 \
             )",
        )
        .bind(table_name)
        .bind(idx_name)
        .fetch_one(&db.pool)
        .await
        .expect("failed to query pg_indexes");
        assert!(
            exists,
            "{idx_name} must still exist on {table_name} after idempotent migration",
        );
    }

    // `usage_idempotency_keys.created_at` is added via `ADD COLUMN IF NOT EXISTS`;
    // verify the column survives the second pass with the expected type so the
    // periodic cleanup `DELETE … WHERE created_at < NOW() - $1::interval` keeps
    // working.
    let created_at_type: Option<String> = sqlx::query_scalar(
        "SELECT data_type FROM information_schema.columns \
         WHERE table_schema = 'public' \
         AND table_name = 'usage_idempotency_keys' \
         AND column_name = 'created_at'",
    )
    .fetch_optional(&db.pool)
    .await
    .expect("failed to query information_schema.columns");
    assert_eq!(
        created_at_type.as_deref(),
        Some("timestamp with time zone"),
        "usage_idempotency_keys.created_at must remain timestamptz after idempotent migration"
    );
}

// ── Test 2: concurrent_upsert_exactly_one_row ─────────────────────────────────

#[cfg(feature = "integration")]
#[tokio::test]
async fn concurrent_upsert_exactly_one_row() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let idempotency_key = format!("idem-{}", Uuid::new_v4());

    let client = Arc::new(make_client(&db.pool));

    // Spawn 5 concurrent tasks each inserting the same record
    let handles: Vec<_> = (0..5)
        .map(|_| {
            let c = client.clone();
            let record = counter_record(tenant_id, resource_id, &idempotency_key);
            tokio::spawn(async move { c.create_usage_record(record).await })
        })
        .collect();

    for handle in handles {
        handle
            .await
            .expect("task panicked")
            .expect("create_usage_record returned error under concurrent upsert");
    }

    // Exactly one row must persist for the idempotency key
    let row_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM usage_records WHERE tenant_id = $1 AND idempotency_key = $2",
    )
    .bind(tenant_id)
    .bind(&idempotency_key)
    .fetch_one(&db.pool)
    .await
    .expect("row count query failed");

    assert_eq!(
        row_count, 1,
        "exactly one row must persist under concurrent inserts with the same idempotency key"
    );
}

// ── Additional helpers ────────────────────────────────────────────────────────

fn counter_record_with_value(
    tenant_id: Uuid,
    resource_id: Uuid,
    key: &str,
    value: f64,
) -> UsageRecord {
    UsageRecord {
        value,
        ..counter_record(tenant_id, resource_id, key)
    }
}

// ── Test 5: health_check_metric ───────────────────────────────────────────────

#[cfg(feature = "integration")]
#[tokio::test]
async fn health_check_metric() {
    // Drives the production `health_check` function directly (rather than
    // re-implementing `SELECT 1` here) and pins the `(value, reason)` tuple
    // emitted to `storage_health_status` for each operational state. A
    // regression that, say, mislabels a probe failure as `healthy` or fails
    // to detect `pool_closed` would slip past the previous test, which only
    // covered the sqlx layer.
    let db = setup_container_and_pool().await;

    // Healthy pool → (1.0, "healthy")
    let (value, reason) = health_check_for_tests(&db.pool).await;
    assert!(
        (value - 1.0).abs() < f64::EPSILON,
        "healthy pool must report storage_health_status=1.0, got {value}"
    );
    assert_eq!(reason, "healthy");

    // Closed pool → (0.0, "pool_closed"); distinct from probe_failed so
    // dashboards can separate operator-initiated shutdown from outage.
    db.pool.close().await;
    let (value, reason) = health_check_for_tests(&db.pool).await;
    assert!(
        value.abs() < f64::EPSILON,
        "closed pool must report storage_health_status=0.0, got {value}"
    );
    assert_eq!(reason, "pool_closed");
}

// ── Group A: all 5 aggregation functions on the raw path ──────────────────────

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_raw_sum() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    for (i, val) in [(0u32, 10.0f64), (1, 20.0), (2, 30.0)] {
        client
            .create_usage_record(counter_record_with_value(
                tenant_id,
                resource_id,
                &format!("sum-raw-{i}"),
                val,
            ))
            .await
            .expect("insert failed");
    }

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: Some(resource_id),
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 60.0).abs() < 1e-6,
        "expected sum=60.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_raw_count() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    for i in 0u32..3 {
        client
            .create_usage_record(counter_record(
                tenant_id,
                resource_id,
                &format!("count-raw-{i}"),
            ))
            .await
            .expect("insert failed");
    }

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Count,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: Some(resource_id),
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 3.0).abs() < 1e-6,
        "expected count=3.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_raw_min() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    for (i, val) in [(0u32, 10.0f64), (1, 20.0), (2, 30.0)] {
        client
            .create_usage_record(counter_record_with_value(
                tenant_id,
                resource_id,
                &format!("min-raw-{i}"),
                val,
            ))
            .await
            .expect("insert failed");
    }

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Min,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: Some(resource_id),
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 10.0).abs() < 1e-6,
        "expected min=10.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_raw_max() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    for (i, val) in [(0u32, 10.0f64), (1, 20.0), (2, 30.0)] {
        client
            .create_usage_record(counter_record_with_value(
                tenant_id,
                resource_id,
                &format!("max-raw-{i}"),
                val,
            ))
            .await
            .expect("insert failed");
    }

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Max,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: Some(resource_id),
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 30.0).abs() < 1e-6,
        "expected max=30.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_raw_avg() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    for (i, val) in [(0u32, 10.0f64), (1, 20.0), (2, 30.0)] {
        client
            .create_usage_record(counter_record_with_value(
                tenant_id,
                resource_id,
                &format!("avg-raw-{i}"),
                val,
            ))
            .await
            .expect("insert failed");
    }

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Avg,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: Some(resource_id),
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 20.0).abs() < 1e-6,
        "expected avg=20.0, got {}",
        results[0].value
    );
}

// ── Group B: all 5 aggregation functions on the cagg path ─────────────────────

async fn cagg_refresh(pool: &sqlx::PgPool) {
    sqlx::query(
        "CALL refresh_continuous_aggregate(\
             'usage_agg_1h', \
             (NOW() - INTERVAL '5 hours')::timestamptz, \
             (NOW() - INTERVAL '1 hour')::timestamptz\
         )",
    )
    .execute(pool)
    .await
    .expect("manual cagg refresh failed");
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_cagg_sum() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let past_ts = aligned_past_hour(3);
    let time_range = (
        past_ts - chrono::Duration::hours(1),
        past_ts + chrono::Duration::hours(2),
    );

    for (i, val) in [(0u32, 10.0f64), (1, 20.0), (2, 30.0)] {
        let mut r =
            counter_record_with_value(tenant_id, resource_id, &format!("sum-cagg-{i}"), val);
        r.timestamp = past_ts;
        client.create_usage_record(r).await.expect("insert failed");
    }
    cagg_refresh(&db.pool).await;

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 60.0).abs() < 1e-6,
        "expected cagg sum=60.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_cagg_count() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let past_ts = aligned_past_hour(3);
    let time_range = (
        past_ts - chrono::Duration::hours(1),
        past_ts + chrono::Duration::hours(2),
    );

    for i in 0u32..3 {
        let mut r = counter_record(tenant_id, resource_id, &format!("count-cagg-{i}"));
        r.timestamp = past_ts;
        client.create_usage_record(r).await.expect("insert failed");
    }
    cagg_refresh(&db.pool).await;

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Count,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 3.0).abs() < 1e-6,
        "expected cagg count=3.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_cagg_min() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let past_ts = aligned_past_hour(3);
    let time_range = (
        past_ts - chrono::Duration::hours(1),
        past_ts + chrono::Duration::hours(2),
    );

    for (i, val) in [(0u32, 10.0f64), (1, 20.0), (2, 30.0)] {
        let mut r =
            counter_record_with_value(tenant_id, resource_id, &format!("min-cagg-{i}"), val);
        r.timestamp = past_ts;
        client.create_usage_record(r).await.expect("insert failed");
    }
    cagg_refresh(&db.pool).await;

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Min,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 10.0).abs() < 1e-6,
        "expected cagg min=10.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_cagg_max() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let past_ts = aligned_past_hour(3);
    let time_range = (
        past_ts - chrono::Duration::hours(1),
        past_ts + chrono::Duration::hours(2),
    );

    for (i, val) in [(0u32, 10.0f64), (1, 20.0), (2, 30.0)] {
        let mut r =
            counter_record_with_value(tenant_id, resource_id, &format!("max-cagg-{i}"), val);
        r.timestamp = past_ts;
        client.create_usage_record(r).await.expect("insert failed");
    }
    cagg_refresh(&db.pool).await;

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Max,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 30.0).abs() < 1e-6,
        "expected cagg max=30.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_cagg_avg() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let past_ts = aligned_past_hour(3);
    let time_range = (
        past_ts - chrono::Duration::hours(1),
        past_ts + chrono::Duration::hours(2),
    );

    for (i, val) in [(0u32, 10.0f64), (1, 20.0), (2, 30.0)] {
        let mut r =
            counter_record_with_value(tenant_id, resource_id, &format!("avg-cagg-{i}"), val);
        r.timestamp = past_ts;
        client.create_usage_record(r).await.expect("insert failed");
    }
    cagg_refresh(&db.pool).await;

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Avg,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 20.0).abs() < 1e-6,
        "expected cagg avg=20.0, got {}",
        results[0].value
    );
}

// ── CAGG end-boundary inclusion ───────────────────────────────────────────────

/// Regression test for the closed-interval contract on the CAGG path.
///
/// The CAGG view buckets records into half-open hour intervals; with no
/// boundary correction, a record at exactly `timestamp = time_range.1`
/// (hour-aligned) is filed in bucket `time_range.1` which the WHERE
/// `bucket < $end` excludes — silently undercounting billing totals. The raw
/// path would include it via `timestamp <= $end`. This test inserts one
/// record squarely inside the period and one at exactly the upper bound,
/// then asserts the CAGG-routed `Sum` matches both records.
#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_cagg_includes_end_boundary_record() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);

    // Pick a fully-materialised window: end is 2h in the past (≥ 1h ago, the
    // CAGG horizon), start is 4h in the past. Both endpoints are hour-aligned
    // so `cagg_safe_range` keeps the query on the CAGG path.
    let end_ts = aligned_past_hour(2);
    let start_ts = aligned_past_hour(4);
    let mid_ts = aligned_past_hour(3);

    // One record inside the period, one at exactly the closed-interval upper
    // bound. Without the boundary correction the boundary record is dropped.
    let mut inside = counter_record_with_value(tenant_id, resource_id, "cagg-boundary-inside", 7.0);
    inside.timestamp = mid_ts;
    client
        .create_usage_record(inside)
        .await
        .expect("insert mid failed");

    let mut boundary = counter_record_with_value(tenant_id, resource_id, "cagg-boundary-end", 11.0);
    boundary.timestamp = end_ts;
    client
        .create_usage_record(boundary)
        .await
        .expect("insert boundary failed");

    cagg_refresh(&db.pool).await;

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range: (start_ts, end_ts),
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 18.0).abs() < 1e-6,
        "expected cagg sum=18.0 (7.0 inside + 11.0 at end boundary), got {}",
        results[0].value
    );
}

// ── Phase 9 helpers ───────────────────────────────────────────────────────────

fn record_with_subject(
    tenant_id: Uuid,
    resource_id: Uuid,
    subject_id: Uuid,
    key: &str,
) -> UsageRecord {
    UsageRecord {
        subject: Some(Subject::with_type(subject_id, "user")),
        ..counter_record(tenant_id, resource_id, key)
    }
}

// ── Test 3: query_aggregated_routing_decision ─────────────────────────────────

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_routing_decision() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);

    // Insert 3 records from 3 hours ago so they land in a past 1-hour cagg bucket
    let past_ts = aligned_past_hour(3);
    for i in 0..3u32 {
        let mut record = counter_record(tenant_id, resource_id, &format!("cagg-key-{i}"));
        record.timestamp = past_ts;
        client
            .create_usage_record(record)
            .await
            .expect("insert failed");
    }

    // Refresh the cagg to materialise the inserted data
    sqlx::query(
        "CALL refresh_continuous_aggregate(\
             'usage_agg_1h', \
             (NOW() - INTERVAL '5 hours')::timestamptz, \
             (NOW() - INTERVAL '1 hour')::timestamptz\
         )",
    )
    .execute(&db.pool)
    .await
    .expect("manual cagg refresh failed");

    // Time range covers the inserted data and the cagg bucket
    let time_range = (
        past_ts - chrono::Duration::hours(1),
        past_ts + chrono::Duration::hours(2),
    );

    // Raw hypertable path: resource_id filter forces routing to usage_records
    let raw_results = client
        .query_aggregated(AggregationQuery {
            scope: scope.clone(),
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: Some(resource_id),
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("raw hypertable path query failed");

    assert_eq!(
        raw_results.len(),
        1,
        "raw hypertable path must return exactly one aggregated row"
    );
    assert!(
        (raw_results[0].value - 30.0).abs() < 1e-6,
        "raw hypertable path must return sum=30.0, got {}",
        raw_results[0].value
    );

    // Continuous aggregate path: no resource_id/subject_id → routed to usage_agg_1h
    let cagg_results = client
        .query_aggregated(AggregationQuery {
            scope: scope.clone(),
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("continuous aggregate path query failed");

    assert_eq!(
        cagg_results.len(),
        1,
        "continuous aggregate path must return exactly one aggregated row"
    );
    assert!(
        (cagg_results[0].value - 30.0).abs() < 1e-6,
        "continuous aggregate path must return sum=30.0 after manual refresh, got {}",
        cagg_results[0].value
    );
}

// ── Test 4: cursor_stability_under_concurrent_inserts ─────────────────────────

#[cfg(feature = "integration")]
#[tokio::test]
async fn cursor_stability_under_concurrent_inserts() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = Arc::new(make_client(&db.pool));
    let scope = AccessScope::for_tenant(tenant_id);

    // Baseline: insert 5 records with distinct timestamps inside the query range
    let base_ts = Utc::now() - chrono::Duration::hours(2);
    let query_range = (
        base_ts - chrono::Duration::minutes(1),
        base_ts + chrono::Duration::hours(1),
    );

    for i in 0..5u32 {
        let mut record = counter_record(tenant_id, resource_id, &format!("stable-{i}"));
        record.timestamp = base_ts + chrono::Duration::minutes(i64::from(i));
        client
            .create_usage_record(record)
            .await
            .expect("baseline insert failed");
    }

    // Get the first page (3 records)
    let first_page = client
        .query_raw(RawQuery {
            scope: scope.clone(),
            time_range: query_range,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_type: None,
            subject_id: None,
            cursor: None,
            page_size: 3,
        })
        .await
        .expect("first page query failed");

    assert_eq!(
        first_page.items.len(),
        3,
        "first page must contain 3 records"
    );
    let cursor_str = first_page
        .page_info
        .next_cursor
        .expect("cursor must be present when page_size equals result count");
    let cursor =
        CursorV1::decode(&cursor_str).expect("cursor string from page 1 must be a valid CursorV1");

    // Concurrently insert 3 records OUTSIDE the query range so they cannot affect pagination
    let outside_ts = base_ts + chrono::Duration::hours(2);
    let c = client.clone();
    let outside_handle = tokio::spawn(async move {
        for i in 0..3u32 {
            let mut r = counter_record(tenant_id, resource_id, &format!("outside-{i}"));
            r.timestamp = outside_ts + chrono::Duration::minutes(i64::from(i));
            c.create_usage_record(r)
                .await
                .expect("outside-range insert failed");
        }
    });

    // Get the second page using the cursor from page 1
    let second_page = client
        .query_raw(RawQuery {
            scope: scope.clone(),
            time_range: query_range,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_type: None,
            subject_id: None,
            cursor: Some(cursor),
            page_size: 3,
        })
        .await
        .expect("second page query failed");

    outside_handle
        .await
        .expect("outside-range insert task panicked");

    assert_eq!(
        second_page.items.len(),
        2,
        "second page must contain the remaining 2 records; concurrent outside-range inserts must not appear"
    );
    assert!(
        second_page.page_info.next_cursor.is_none(),
        "no next cursor expected after the last page is exhausted"
    );
}

// ── Group C: GroupByDimension variants ────────────────────────────────────────

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_group_by_usage_type_raw() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    client
        .create_usage_record(counter_record(tenant_id, resource_id, "gbu-type-raw-1"))
        .await
        .expect("insert failed");

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![GroupByDimension::UsageType],
            bucket_size: None,
            usage_type: None,
            resource_id: Some(resource_id),
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].usage_type, Some("test.cpu".to_owned()));
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_group_by_usage_type_cagg() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let past_ts = aligned_past_hour(3);
    let time_range = (
        past_ts - chrono::Duration::hours(1),
        past_ts + chrono::Duration::hours(2),
    );

    let mut r = counter_record(tenant_id, resource_id, "gbu-type-cagg-1");
    r.timestamp = past_ts;
    client.create_usage_record(r).await.expect("insert failed");
    cagg_refresh(&db.pool).await;

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![GroupByDimension::UsageType],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        results[0].usage_type.is_some(),
        "expected usage_type to be populated on cagg path"
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_group_by_resource() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    client
        .create_usage_record(counter_record(tenant_id, resource_id, "gbr-raw-1"))
        .await
        .expect("insert failed");

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![GroupByDimension::Resource],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].resource_id, Some(resource_id));
    assert_eq!(results[0].resource_type, Some("vm".to_owned()));
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_group_by_subject() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let subject_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    client
        .create_usage_record(record_with_subject(
            tenant_id,
            resource_id,
            subject_id,
            "gbs-raw-1",
        ))
        .await
        .expect("insert failed");

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![GroupByDimension::Subject],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].subject_id, Some(subject_id));
    assert_eq!(results[0].subject_type, Some("user".to_owned()));
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_group_by_source() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    client
        .create_usage_record(counter_record(tenant_id, resource_id, "gbsrc-raw-1"))
        .await
        .expect("insert failed");

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![GroupByDimension::Source],
            bucket_size: None,
            usage_type: None,
            resource_id: Some(resource_id),
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source, Some("integration-test".to_owned()));
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_group_by_time_bucket_raw() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    client
        .create_usage_record(counter_record(tenant_id, resource_id, "gbtb-raw-1"))
        .await
        .expect("insert failed");

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![GroupByDimension::TimeBucket(BucketSize::Hour)],
            bucket_size: Some(BucketSize::Hour),
            usage_type: None,
            resource_id: Some(resource_id),
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert!(!results.is_empty(), "expected at least one result row");
    assert!(
        results[0].bucket_start.is_some(),
        "expected bucket_start to be populated"
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_group_by_time_bucket_cagg() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let past_ts = aligned_past_hour(3);
    let time_range = (
        past_ts - chrono::Duration::hours(1),
        past_ts + chrono::Duration::hours(2),
    );

    let mut r = counter_record(tenant_id, resource_id, "gbtb-cagg-1");
    r.timestamp = past_ts;
    client.create_usage_record(r).await.expect("insert failed");
    cagg_refresh(&db.pool).await;

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![GroupByDimension::TimeBucket(BucketSize::Hour)],
            bucket_size: Some(BucketSize::Hour),
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert!(!results.is_empty(), "expected at least one result row");
    assert!(
        results[0].bucket_start.is_some(),
        "expected bucket_start to be populated on cagg path"
    );
}

// ── Group D: query_aggregated filters on the raw path ─────────────────────────

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_filter_usage_type_raw() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    // Insert "test.cpu" (value 10) and "test.mem" (value 20)
    client
        .create_usage_record(counter_record_with_value(
            tenant_id,
            resource_id,
            "fut-cpu-1",
            10.0,
        ))
        .await
        .expect("insert failed");
    let mut mem_rec = counter_record_with_value(tenant_id, resource_id, "fut-mem-1", 20.0);
    mem_rec.metric = "test.mem".to_owned();
    client
        .create_usage_record(mem_rec)
        .await
        .expect("insert failed");

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: Some("test.cpu".to_owned()),
            resource_id: Some(resource_id),
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 10.0).abs() < 1e-6,
        "expected only test.cpu sum=10.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_filter_resource_type_raw() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    // 2 "vm" records (value 10 each) and 1 "disk" record (value 20)
    client
        .create_usage_record(counter_record_with_value(
            tenant_id,
            resource_id,
            "frt-vm-1",
            10.0,
        ))
        .await
        .expect("insert failed");
    client
        .create_usage_record(counter_record_with_value(
            tenant_id,
            resource_id,
            "frt-vm-2",
            10.0,
        ))
        .await
        .expect("insert failed");
    let mut disk_rec = counter_record_with_value(tenant_id, resource_id, "frt-disk-1", 20.0);
    disk_rec.resource_type = "disk".to_owned();
    client
        .create_usage_record(disk_rec)
        .await
        .expect("insert failed");

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: Some(resource_id),
            resource_type: Some("vm".to_owned()),
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 20.0).abs() < 1e-6,
        "expected vm sum=20.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_filter_subject_type_raw() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let subject_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    // 1 record with subject_type "user" (value 10) and 1 without (value 20)
    client
        .create_usage_record(record_with_subject(
            tenant_id,
            resource_id,
            subject_id,
            "fst-user-1",
        ))
        .await
        .expect("insert failed");
    client
        .create_usage_record(counter_record_with_value(
            tenant_id,
            resource_id,
            "fst-none-1",
            20.0,
        ))
        .await
        .expect("insert failed");

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: Some(resource_id),
            resource_type: None,
            subject_id: None,
            subject_type: Some("user".to_owned()),
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 10.0).abs() < 1e-6,
        "expected user-only sum=10.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_filter_source_raw() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    // 1 record from "mod-a" (value 10) and 1 from "mod-b" (value 20)
    client
        .create_usage_record(counter_record_with_value(
            tenant_id,
            resource_id,
            "fsrc-a-1",
            10.0,
        ))
        .await
        .expect("insert failed");
    let mut mod_b = counter_record_with_value(tenant_id, resource_id, "fsrc-b-1", 20.0);
    mod_b.module = "mod-b".to_owned();
    client
        .create_usage_record(mod_b)
        .await
        .expect("insert failed");

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: Some(resource_id),
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: Some("integration-test".to_owned()),
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 10.0).abs() < 1e-6,
        "expected integration-test-only sum=10.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_filter_multi_raw() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    // (cpu, vm, 10), (cpu, disk, 15), (mem, vm, 20), (mem, disk, 25)
    client
        .create_usage_record(counter_record_with_value(
            tenant_id,
            resource_id,
            "fmulti-cv-1",
            10.0,
        ))
        .await
        .expect("insert failed");
    let mut cpu_disk = counter_record_with_value(tenant_id, resource_id, "fmulti-cd-1", 15.0);
    cpu_disk.resource_type = "disk".to_owned();
    client
        .create_usage_record(cpu_disk)
        .await
        .expect("insert failed");
    let mut mem_vm = counter_record_with_value(tenant_id, resource_id, "fmulti-mv-1", 20.0);
    mem_vm.metric = "test.mem".to_owned();
    client
        .create_usage_record(mem_vm)
        .await
        .expect("insert failed");
    let mut mem_disk = counter_record_with_value(tenant_id, resource_id, "fmulti-md-1", 25.0);
    mem_disk.metric = "test.mem".to_owned();
    mem_disk.resource_type = "disk".to_owned();
    client
        .create_usage_record(mem_disk)
        .await
        .expect("insert failed");

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: Some("test.cpu".to_owned()),
            resource_id: Some(resource_id),
            resource_type: Some("vm".to_owned()),
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 10.0).abs() < 1e-6,
        "expected multi-filter sum=10.0, got {}",
        results[0].value
    );
}

// ── Group E: query_aggregated filters on the cagg path ────────────────────────

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_filter_usage_type_cagg() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let past_ts = aligned_past_hour(3);
    let time_range = (
        past_ts - chrono::Duration::hours(1),
        past_ts + chrono::Duration::hours(2),
    );

    let mut cpu_rec = counter_record_with_value(tenant_id, resource_id, "fute-cpu-1", 10.0);
    cpu_rec.timestamp = past_ts;
    client
        .create_usage_record(cpu_rec)
        .await
        .expect("insert failed");
    let mut mem_rec = counter_record_with_value(tenant_id, resource_id, "fute-mem-1", 20.0);
    mem_rec.metric = "test.mem".to_owned();
    mem_rec.timestamp = past_ts;
    client
        .create_usage_record(mem_rec)
        .await
        .expect("insert failed");
    cagg_refresh(&db.pool).await;

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: Some("test.cpu".to_owned()),
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 10.0).abs() < 1e-6,
        "expected cagg usage_type filter sum=10.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_filter_resource_type_cagg() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let past_ts = aligned_past_hour(3);
    let time_range = (
        past_ts - chrono::Duration::hours(1),
        past_ts + chrono::Duration::hours(2),
    );

    let mut vm_rec = counter_record_with_value(tenant_id, resource_id, "frte-vm-1", 10.0);
    vm_rec.timestamp = past_ts;
    client
        .create_usage_record(vm_rec)
        .await
        .expect("insert failed");
    let mut disk_rec = counter_record_with_value(tenant_id, resource_id, "frte-disk-1", 20.0);
    disk_rec.resource_type = "disk".to_owned();
    disk_rec.timestamp = past_ts;
    client
        .create_usage_record(disk_rec)
        .await
        .expect("insert failed");
    cagg_refresh(&db.pool).await;

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: Some("vm".to_owned()),
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 10.0).abs() < 1e-6,
        "expected cagg resource_type filter sum=10.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_filter_subject_type_cagg() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let subject_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let past_ts = aligned_past_hour(3);
    let time_range = (
        past_ts - chrono::Duration::hours(1),
        past_ts + chrono::Duration::hours(2),
    );

    let mut user_rec = record_with_subject(tenant_id, resource_id, subject_id, "fste-user-1");
    user_rec.timestamp = past_ts;
    client
        .create_usage_record(user_rec)
        .await
        .expect("insert failed");
    let mut none_rec = counter_record_with_value(tenant_id, resource_id, "fste-none-1", 20.0);
    none_rec.timestamp = past_ts;
    client
        .create_usage_record(none_rec)
        .await
        .expect("insert failed");
    cagg_refresh(&db.pool).await;

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: Some("user".to_owned()),
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 10.0).abs() < 1e-6,
        "expected cagg subject_type filter sum=10.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_filter_source_cagg() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let past_ts = aligned_past_hour(3);
    let time_range = (
        past_ts - chrono::Duration::hours(1),
        past_ts + chrono::Duration::hours(2),
    );

    let mut a_rec = counter_record_with_value(tenant_id, resource_id, "fsrce-a-1", 10.0);
    a_rec.timestamp = past_ts;
    client
        .create_usage_record(a_rec)
        .await
        .expect("insert failed");
    let mut b_rec = counter_record_with_value(tenant_id, resource_id, "fsrce-b-1", 20.0);
    b_rec.module = "mod-b".to_owned();
    b_rec.timestamp = past_ts;
    client
        .create_usage_record(b_rec)
        .await
        .expect("insert failed");
    cagg_refresh(&db.pool).await;

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: Some("integration-test".to_owned()),
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 10.0).abs() < 1e-6,
        "expected cagg source filter sum=10.0, got {}",
        results[0].value
    );
}

// ── Group F: query_aggregated scope isolation ──────────────────────────────────

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_scope_isolation() {
    let db = setup_container_and_pool().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope_a = AccessScope::for_tenant(tenant_a);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    // Same resource_id, different tenants
    client
        .create_usage_record(counter_record_with_value(
            tenant_a,
            resource_id,
            "scope-a-1",
            10.0,
        ))
        .await
        .expect("insert tenant_a failed");
    client
        .create_usage_record(counter_record_with_value(
            tenant_b,
            resource_id,
            "scope-b-1",
            20.0,
        ))
        .await
        .expect("insert tenant_b failed");

    let results = client
        .query_aggregated(AggregationQuery {
            scope: scope_a,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: Some(resource_id),
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 10.0).abs() < 1e-6,
        "expected only tenant_a sum=10.0, got {}",
        results[0].value
    );
}

// ── Group G: QueryResultTooLarge ──────────────────────────────────────────────
//
// SDK drift: fe5e58dc passed `max_rows: 2` on `AggregationQuery` to force the
// plugin to exceed the row cap. HEAD's `AggregationQuery` no longer carries
// `max_rows`; the plugin enforces a hard internal cap of `MAX_AGG_ROWS = 10_000`
// (see `src/infra/pg_query_port.rs`). Exercising the cap therefore requires
// emitting > 10_000 distinct grouping rows, which is impractical inside a
// per-test container and would dwarf the rest of the suite's runtime. The test
// is preserved at parity with fe5e58dc but `#[ignore]`-gated so it compiles and
// the `#[tokio::test]` count matches; the assertion path is reworked to use
// `MAX_AGG_ROWS + 1` distinct resource ids if a developer opts into running it.
#[cfg(feature = "integration")]
#[ignore = "exercising MAX_AGG_ROWS=10_000 cap requires > 10k distinct rows; \
            kept for fe5e58dc parity but skipped in default runs"]
#[tokio::test]
async fn query_aggregated_result_too_large() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    // Insert MAX_AGG_ROWS + 1 records with distinct resource_ids → group by
    // Resource yields > MAX_AGG_ROWS rows, tripping the plugin-side cap.
    for i in 0u32..10_001 {
        let rid = Uuid::new_v4();
        client
            .create_usage_record(counter_record(tenant_id, rid, &format!("too-large-{i}")))
            .await
            .expect("insert failed");
    }

    let result = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![GroupByDimension::Resource],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await;

    assert!(
        matches!(result, Err(ref e) if matches!(e, UsageCollectorError::ResourceExhausted { .. })),
        "expected QueryResultTooLarge, got {result:?}"
    );
}

// Same cost-control invariant as `query_aggregated_result_too_large`, but with
// the row cap injected via `PgQueryPort::new_with_max_agg_rows` so we can trip
// the cap with a handful of rows. Runs in default integration CI (no
// `#[ignore]`) so a regression that silently drops the cap fails the build.
#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_result_too_large_small_cap() {
    const SMALL_CAP: usize = 3;

    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let client = make_client_with_max_agg_rows(&db.pool, SMALL_CAP);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    for i in 0..=SMALL_CAP {
        let rid = Uuid::new_v4();
        client
            .create_usage_record(counter_record(tenant_id, rid, &format!("small-cap-{i}")))
            .await
            .expect("insert failed");
    }

    let result = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![GroupByDimension::Resource],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await;

    assert!(
        matches!(result, Err(ref e) if matches!(e, UsageCollectorError::ResourceExhausted { .. })),
        "expected ResourceExhausted with cap={SMALL_CAP}, got {result:?}"
    );
}

// ── Group H: query_raw filters ────────────────────────────────────────────────

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_raw_filter_usage_type() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    client
        .create_usage_record(counter_record(tenant_id, resource_id, "rut-cpu-1"))
        .await
        .expect("insert failed");
    let mut mem_rec = counter_record(tenant_id, resource_id, "rut-mem-1");
    mem_rec.metric = "test.mem".to_owned();
    client
        .create_usage_record(mem_rec)
        .await
        .expect("insert failed");

    let page = client
        .query_raw(RawQuery {
            scope,
            time_range,
            usage_type: Some("test.cpu".to_owned()),
            resource_id: None,
            resource_type: None,
            subject_type: None,
            subject_id: None,
            cursor: None,
            page_size: 100,
        })
        .await
        .expect("query_raw failed");

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].metric, "test.cpu");
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_raw_filter_resource_id() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id_a = Uuid::new_v4();
    let resource_id_b = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    client
        .create_usage_record(counter_record(tenant_id, resource_id_a, "rrid-a-1"))
        .await
        .expect("insert failed");
    client
        .create_usage_record(counter_record(tenant_id, resource_id_b, "rrid-b-1"))
        .await
        .expect("insert failed");

    let page = client
        .query_raw(RawQuery {
            scope,
            time_range,
            usage_type: None,
            resource_id: Some(resource_id_a),
            resource_type: None,
            subject_type: None,
            subject_id: None,
            cursor: None,
            page_size: 100,
        })
        .await
        .expect("query_raw failed");

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].resource_id, resource_id_a);
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_raw_filter_resource_type() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    client
        .create_usage_record(counter_record(tenant_id, resource_id, "rrtype-vm-1"))
        .await
        .expect("insert failed");
    let mut disk_rec = counter_record(tenant_id, resource_id, "rrtype-disk-1");
    disk_rec.resource_type = "disk".to_owned();
    client
        .create_usage_record(disk_rec)
        .await
        .expect("insert failed");

    let page = client
        .query_raw(RawQuery {
            scope,
            time_range,
            usage_type: None,
            resource_id: None,
            resource_type: Some("vm".to_owned()),
            subject_type: None,
            subject_id: None,
            cursor: None,
            page_size: 100,
        })
        .await
        .expect("query_raw failed");

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].resource_type, "vm");
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_raw_filter_subject_id() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let subject_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    client
        .create_usage_record(record_with_subject(
            tenant_id,
            resource_id,
            subject_id,
            "rsid-with-1",
        ))
        .await
        .expect("insert failed");
    client
        .create_usage_record(counter_record(tenant_id, resource_id, "rsid-without-1"))
        .await
        .expect("insert failed");

    let page = client
        .query_raw(RawQuery {
            scope,
            time_range,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_type: None,
            subject_id: Some(subject_id),
            cursor: None,
            page_size: 100,
        })
        .await
        .expect("query_raw failed");

    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.items[0].subject.as_ref().map(|s| s.id),
        Some(subject_id)
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_raw_filter_subject_type() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let subject_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    client
        .create_usage_record(record_with_subject(
            tenant_id,
            resource_id,
            subject_id,
            "rst-user-1",
        ))
        .await
        .expect("insert failed");
    client
        .create_usage_record(counter_record(tenant_id, resource_id, "rst-none-1"))
        .await
        .expect("insert failed");

    let page = client
        .query_raw(RawQuery {
            scope,
            time_range,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_type: Some("user".to_owned()),
            subject_id: None,
            cursor: None,
            page_size: 100,
        })
        .await
        .expect("query_raw failed");

    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.items[0]
            .subject
            .as_ref()
            .and_then(|s| s.r#type.clone()),
        Some("user".to_owned())
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_raw_scope_isolation() {
    let db = setup_container_and_pool().await;
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope_a = AccessScope::for_tenant(tenant_a);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    client
        .create_usage_record(counter_record(tenant_a, resource_id, "rscope-a-1"))
        .await
        .expect("insert tenant_a failed");
    client
        .create_usage_record(counter_record(tenant_b, resource_id, "rscope-b-1"))
        .await
        .expect("insert tenant_b failed");

    let page = client
        .query_raw(RawQuery {
            scope: scope_a,
            time_range,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_type: None,
            subject_id: None,
            cursor: None,
            page_size: 100,
        })
        .await
        .expect("query_raw failed");

    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.items[0].tenant_id, tenant_a,
        "must only return tenant_a records"
    );
}

// ── Group I: create_usage_record validation errors ────────────────────────────

#[cfg(feature = "integration")]
#[tokio::test]
async fn create_record_negative_counter_value() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);

    let mut record = counter_record(tenant_id, resource_id, "neg-val-key");
    record.value = -1.0;

    let err = client.create_usage_record(record).await.unwrap_err();
    assert!(
        matches!(err, UsageCollectorError::InvalidArgument { .. }),
        "expected InvalidArgument error for negative counter value, got {err:?}"
    );
}

// ── Regression: scope-level resource_id / subject_id forces raw path ──────────
//
// The continuous aggregate (`usage_agg_1h`) groups out `resource_id` and
// `subject_id`. A previous version of `query_aggregated` decided whether to
// hit the CAGG using only query-level `resource_id` / `subject_id` filters,
// so a caller whose PDP-compiled scope constrained those columns (with no
// explicit resource/subject filter or group-by in the query) produced SQL
// that referenced columns absent on the view, failing at runtime. These
// tests pin that the routing now inspects scope contents too.

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_scope_constrained_resource_id_succeeds() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);

    let scope = AccessScope::from_constraints(vec![ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant_id]),
        ScopeFilter::in_uuids("resource_id", vec![resource_id]),
    ])]);

    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    for (i, val) in [(0u32, 10.0f64), (1, 20.0), (2, 30.0)] {
        client
            .create_usage_record(counter_record_with_value(
                tenant_id,
                resource_id,
                &format!("scope-rid-{i}"),
                val,
            ))
            .await
            .expect("insert failed");
    }

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated must succeed when scope filters by resource_id");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 60.0).abs() < 1e-6,
        "expected sum=60.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_scope_constrained_subject_id_succeeds() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let subject_id = Uuid::new_v4();
    let client = make_client(&db.pool);

    let scope = AccessScope::from_constraints(vec![ScopeConstraint::new(vec![
        ScopeFilter::in_uuids(pep_properties::OWNER_TENANT_ID, vec![tenant_id]),
        ScopeFilter::in_uuids("subject_id", vec![subject_id]),
    ])]);

    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::hours(1),
        now + chrono::Duration::hours(1),
    );

    for (i, val) in [(0u32, 10.0f64), (1, 20.0), (2, 30.0)] {
        let mut record =
            counter_record_with_value(tenant_id, resource_id, &format!("scope-sid-{i}"), val);
        record.subject = Some(Subject {
            id: subject_id,
            r#type: Some("user".to_owned()),
        });
        client
            .create_usage_record(record)
            .await
            .expect("insert failed");
    }

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated must succeed when scope filters by subject_id");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 60.0).abs() < 1e-6,
        "expected sum=60.0, got {}",
        results[0].value
    );
}

/// `query_aggregated` with a low-cardinality dimension shape but a time range
/// that overlaps the unmaterialized window (current hour) must fall back to
/// the raw hypertable rather than silently return an empty / stale CAGG
/// result. Without the fall-back, the CAGG view (which under
/// `materialized_only = true` exposes only refreshed buckets up to
/// `now - 1h`) would miss the just-inserted records and the caller would see
/// a zero-row result.
#[cfg(feature = "integration")]
#[tokio::test]
async fn query_aggregated_current_hour_routes_to_raw() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);
    let scope = AccessScope::for_tenant(tenant_id);
    let now = Utc::now();
    let time_range = (
        now - chrono::Duration::minutes(30),
        now + chrono::Duration::minutes(30),
    );

    for (i, val) in [(0u32, 10.0f64), (1, 20.0), (2, 30.0)] {
        let mut r =
            counter_record_with_value(tenant_id, resource_id, &format!("current-hour-{i}"), val);
        r.timestamp = now;
        client.create_usage_record(r).await.expect("insert failed");
    }
    // Deliberately do NOT call cagg_refresh — the CAGG cannot materialize the
    // current hour anyway (`end_offset = 1h` in the refresh policy).

    let results = client
        .query_aggregated(AggregationQuery {
            scope,
            time_range,
            function: AggregationFn::Sum,
            group_by: vec![],
            bucket_size: None,
            usage_type: None,
            resource_id: None,
            resource_type: None,
            subject_id: None,
            subject_type: None,
            source: None,
        })
        .await
        .expect("query_aggregated failed");

    assert_eq!(results.len(), 1);
    assert!(
        (results[0].value - 60.0).abs() < 1e-6,
        "current-hour low-cardinality query must fall back to raw and return sum=60.0, got {}",
        results[0].value
    );
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn create_record_empty_idempotency_key() {
    let db = setup_container_and_pool().await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let client = make_client(&db.pool);

    let record = counter_record(tenant_id, resource_id, "");

    let err = client.create_usage_record(record).await.unwrap_err();
    assert!(
        matches!(err, UsageCollectorError::InvalidArgument { .. }),
        "expected InvalidArgument error for empty idempotency_key, got {err:?}"
    );
}
