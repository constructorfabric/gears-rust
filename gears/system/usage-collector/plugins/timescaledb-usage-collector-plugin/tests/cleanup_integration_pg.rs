#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! TimescaleDB-backed tests for the `usage_records` retention policy
//! registration (idempotent re-apply, concurrent-replica serialization) and
//! end-to-end chunk expiry. Requires Docker.

mod common;

use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use timescaledb_usage_collector_plugin::domain::ports::{CatalogStore, RecordStore};
use timescaledb_usage_collector_plugin::infra::storage::pool::apply_post_migration_setup;

const VCPU_GTS: &str = "gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1";

/// Concurrently-initializing replicas must not corrupt the post-migration
/// setup. The advisory lock in `apply_post_migration_setup` serializes them so
/// every call succeeds and exactly one retention policy remains. Without the
/// lock, concurrent `add_retention_policy` calls (which have no `if_not_exists`)
/// error with "policy already exists".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_concurrent_post_migration_setup_is_serialized() {
    let h = common::bring_up()
        .await
        .expect("timescaledb container (Docker required)");

    // bring_up already applied the setup once; now hammer it concurrently.
    let mut tasks = Vec::new();
    for _ in 0..8u32 {
        let pool = h.pool.clone();
        tasks.push(tokio::spawn(async move {
            apply_post_migration_setup(&pool, 31_536_000).await
        }));
    }
    for t in tasks {
        t.await
            .expect("setup task did not panic")
            .expect("concurrent post-migration setup must succeed under the advisory lock");
    }

    let retention_jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM timescaledb_information.jobs \
         WHERE proc_name = 'policy_retention' AND hypertable_name = 'usage_records'",
    )
    .fetch_one(&h.pool)
    .await
    .expect("retention jobs count");
    assert_eq!(
        retention_jobs, 1,
        "exactly one retention policy must remain after concurrent setup"
    );
}

/// The init advisory lock must not leave a modified `statement_timeout` on any
/// pooled connection. `apply_post_migration_setup` acquires the lock on a pooled
/// connection; the wait is bounded by the connection-level GUC set in
/// `build_pool`, NOT by a per-lock session-level `SET` — a session-level set
/// would leak onto the connection and silently apply to whatever request later
/// reused it. With a distinct configured timeout (17s) and a fixed 2-connection
/// pool (the lock uses one connection, the retention statements a second), every
/// pooled connection must still report the configured value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_init_lock_does_not_leak_statement_timeout() {
    let h = common::bring_up_with(17, 2, 2)
        .await
        .expect("timescaledb container (Docker required)");

    // Hold both pooled connections at once so each distinct one is inspected.
    let mut c1 = h.pool.acquire().await.expect("acquire conn 1");
    let mut c2 = h.pool.acquire().await.expect("acquire conn 2");
    let t1: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&mut *c1)
        .await
        .expect("SHOW statement_timeout on conn 1");
    let t2: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&mut *c2)
        .await
        .expect("SHOW statement_timeout on conn 2");

    assert_eq!(
        t1, "17s",
        "conn 1 carries a leaked statement_timeout; the init path must not set a \
         session-level statement_timeout on a pooled connection"
    );
    assert_eq!(
        t2, "17s",
        "conn 2 carries a leaked statement_timeout; the init path must not set a \
         session-level statement_timeout on a pooled connection"
    );
}

/// End-to-end retention through the REAL registered policy (not a manual
/// `drop_chunks`): backdated data is dropped when the policy job is run, while
/// fresh data survives — proving `apply_retention_policy` wired the right
/// hypertable + window and that the outbound `gts_id` FK does not block it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_registered_retention_policy_drops_aged_data() {
    let h = common::bring_up()
        .await
        .expect("timescaledb container (Docker required)");
    let catalog = common::catalog_store(&h.pool);
    catalog
        .create(common::fixture_usage_type(VCPU_GTS, "counter", &[]))
        .await
        .expect("register usage type (satisfies the gts_id FK)");
    let store = common::record_store(&h.pool);
    let tenant = Uuid::from_u128(0xA6ED);

    // (1) outdated data, 400 days old (> 365d window), via the real ingest path.
    let mut aged =
        common::fixture_usage_record(VCPU_GTS, tenant, "aged", rust_decimal::Decimal::ONE, 0xA01);
    aged.created_at = OffsetDateTime::now_utc() - Duration::days(400);
    let aged_id = aged.id;
    store.create(aged).await.expect("create aged record");

    // fresh row (now) in a different chunk — must survive.
    let mut fresh =
        common::fixture_usage_record(VCPU_GTS, tenant, "fresh", rust_decimal::Decimal::ONE, 0xA02);
    fresh.created_at = OffsetDateTime::now_utc();
    let fresh_id = fresh.id;
    store.create(fresh).await.expect("create fresh record");

    // (2) verify it exists.
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM usage_records WHERE id = $1")
        .bind(aged_id)
        .fetch_one(&h.pool)
        .await
        .expect("count before");
    assert_eq!(before, 1, "aged record must exist before retention runs");

    // (3) trigger the REAL retention policy now.
    let job_id: i32 = sqlx::query_scalar(
        "SELECT job_id FROM timescaledb_information.jobs \
         WHERE proc_name = 'policy_retention' AND hypertable_name = 'usage_records'",
    )
    .fetch_one(&h.pool)
    .await
    .expect("the retention policy must be registered against usage_records");
    sqlx::query("CALL run_job($1)")
        .bind(job_id)
        .execute(&h.pool)
        .await
        .expect("running the retention policy must not error (FK must not block it)");

    // (4) verify it is gone, and the fresh row survived.
    let after_aged: i64 = sqlx::query_scalar("SELECT count(*) FROM usage_records WHERE id = $1")
        .bind(aged_id)
        .fetch_one(&h.pool)
        .await
        .expect("count aged after");
    let after_fresh: i64 = sqlx::query_scalar("SELECT count(*) FROM usage_records WHERE id = $1")
        .bind(fresh_id)
        .fetch_one(&h.pool)
        .await
        .expect("count fresh after");
    assert_eq!(
        after_aged, 0,
        "aged record dropped by the registered retention policy"
    );
    assert_eq!(
        after_fresh, 1,
        "fresh record (inside window) survives retention"
    );
}
