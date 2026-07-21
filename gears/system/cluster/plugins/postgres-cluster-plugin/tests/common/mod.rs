//! Shared testcontainers fixture for the Postgres cluster plugin's Layer 2/3
//! test suites (docs/TESTING.md §4.1). Not a test binary itself — every
//! `tests/*.rs` file that needs it declares `mod common;`.
//!
//! Gated behind `--features integration` end to end (docs/TESTING.md §7): this
//! whole module is compiled out of a default `cargo test`, so it never
//! requires a Docker daemon unless the feature is explicitly enabled.

#![cfg(feature = "integration")]
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test harness: a fixture setup failure IS the test failure, and \
              not every helper below is used by every test binary that `mod \
              common;`s this file"
)]

use std::time::Duration;

use postgres_cluster_plugin::{PostgresClusterConfig, PostgresLockConfig};
use serde_json::json;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers::ContainerAsync;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

/// Builds a [`PostgresClusterConfig`] from `connection_string` plus any
/// `overrides` (a JSON object merged over the minimal required fields),
/// going through `serde_json`/`serde(default = ...)` rather than a `Default`
/// impl — `PostgresClusterConfig` deliberately doesn't derive `Default`
/// (DESIGN.md §7: `connection_string` has no sensible default), so this is
/// the test-only equivalent TESTING.md §4.1's `..PostgresClusterConfig::default()`
/// spread would have needed.
fn cluster_config_json(
    connection_string: &str,
    overrides: serde_json::Value,
) -> PostgresClusterConfig {
    let mut base = json!({
        "connection_string": connection_string,
        "pool_max_size": 5,
    });
    merge(&mut base, overrides);
    serde_json::from_value(base).expect("valid PostgresClusterConfig")
}

fn lock_config_json(connection_string: &str, overrides: serde_json::Value) -> PostgresLockConfig {
    let mut base = json!({
        "connection_string": connection_string,
        "pool_max_size": 5,
    });
    merge(&mut base, overrides);
    serde_json::from_value(base).expect("valid PostgresLockConfig")
}

/// Shallow object merge — sufficient for the flat config shapes here.
fn merge(base: &mut serde_json::Value, overrides: serde_json::Value) {
    let (serde_json::Value::Object(base_map), serde_json::Value::Object(override_map)) =
        (base, overrides)
    else {
        return;
    };
    for (key, value) in override_map {
        base_map.insert(key, value);
    }
}

/// Starts a fresh Postgres container and returns it alongside a
/// `postgres://.../cluster_test` connection string pointed at its mapped host
/// port.
///
/// Retries the whole create -> `.start()` -> `get_host_port_ipv4` sequence up
/// to 5 times with backoff. Under parallel test load the container's own
/// wait-strategy passes but the follow-up Docker port-inspect
/// (`get_host_port_ipv4`) transiently fails; a fresh container per attempt
/// side-steps that flake so every acquisition path here inherits the retry.
async fn spawn_postgres() -> (ContainerAsync<Postgres>, String) {
    // 250ms, 500ms, 1s, 2s between the 5 attempts (last attempt has no sleep).
    let backoffs = [
        Duration::from_millis(250),
        Duration::from_millis(500),
        Duration::from_secs(1),
        Duration::from_secs(2),
    ];
    let attempts = backoffs.len() + 1;
    let mut last_error = String::new();
    for attempt in 1..=attempts {
        let container = match Postgres::default()
            .with_db_name("cluster_test")
            .start()
            .await
        {
            Ok(container) => container,
            Err(error) => {
                last_error = format!("container start: {error}");
                if let Some(backoff) = backoffs.get(attempt - 1) {
                    tokio::time::sleep(*backoff).await;
                }
                continue;
            }
        };
        match container.get_host_port_ipv4(5432).await {
            Ok(port) => {
                let connection_string =
                    format!("postgres://postgres:postgres@127.0.0.1:{port}/cluster_test");
                return (container, connection_string);
            }
            Err(error) => {
                last_error = format!("mapped host port: {error}");
                // Drop the failed container before backing off and retrying.
                drop(container);
                if let Some(backoff) = backoffs.get(attempt - 1) {
                    tokio::time::sleep(*backoff).await;
                }
            }
        }
    }
    panic!(
        "Postgres container acquisition failed after {attempts} attempts; last error: {last_error}"
    );
}

/// Starts a fresh Postgres container and returns a [`PostgresClusterConfig`]
/// pointed at it (docs/TESTING.md §4.1).
pub async fn start_postgres() -> (ContainerAsync<Postgres>, PostgresClusterConfig) {
    start_postgres_with(json!({})).await
}

/// Like [`start_postgres`], but lets the caller override any
/// `PostgresClusterConfig` field (e.g. `lock_reaper_interval_ms`,
/// `lock_name_cardinality_warn_threshold`) for scenarios that need a
/// non-default value.
pub async fn start_postgres_with(
    overrides: serde_json::Value,
) -> (ContainerAsync<Postgres>, PostgresClusterConfig) {
    let (container, connection_string) = spawn_postgres().await;
    let config = cluster_config_json(&connection_string, overrides);
    (container, config)
}

/// Same container, but returns a [`PostgresLockConfig`] (DESIGN.md §3.5) for
/// tests exercising the standalone lock-only provider path.
pub async fn start_postgres_lock_only() -> (ContainerAsync<Postgres>, PostgresLockConfig) {
    start_postgres_lock_only_with(json!({})).await
}

/// Like [`start_postgres_lock_only`], with field overrides.
pub async fn start_postgres_lock_only_with(
    overrides: serde_json::Value,
) -> (ContainerAsync<Postgres>, PostgresLockConfig) {
    let (container, connection_string) = spawn_postgres().await;
    let config = lock_config_json(&connection_string, overrides);
    (container, config)
}

/// Creates `schema` (if missing) on the container behind
/// `base_connection_string` and returns a connection string whose default
/// `search_path` is that schema — via the standard libpq `options` startup
/// parameter (`?options[search_path]=<schema>`, which `sqlx::PgConnectOptions`
/// parses into `-c search_path=<schema>`; confirmed empirically against a
/// real server).
///
/// This is how the Layer 2 conformance tests (`tests/conformance.rs`) get N
/// genuinely independent backends — each its own migrated
/// `cluster_cache`/`cluster_lock` pair, with its own `_sqlx_migrations`
/// tracking row — on a **single shared container**, rather than needing N
/// separate containers or a fragile async-reset-inside-a-sync-closure
/// bridge (the latter was tried and reverted: see this crate's `tests/
/// conformance.rs` module doc for why it deadlocked).
pub async fn isolated_schema_connection_string(
    base_connection_string: &str,
    schema: &str,
) -> String {
    let pool = raw_pool(base_connection_string).await;
    sqlx::query(&format!("CREATE SCHEMA IF NOT EXISTS {schema}"))
        .execute(&pool)
        .await
        .expect("create schema succeeds");
    pool.close().await;
    format!("{base_connection_string}?options[search_path]={schema}")
}

/// Builds a [`PostgresClusterConfig`] pointed at `connection_string` with
/// `schema` as both the migration target (via `connection_string`'s
/// `search_path`, see [`isolated_schema_connection_string`]) and the
/// query-qualification schema (`PostgresClusterConfig::schema`) — the two
/// must agree, or migrations land in one schema while queries look in
/// another.
///
/// `pool_max_size` defaults to 1 here: callers that pre-build many
/// independent instances on one shared container (`tests/conformance.rs`)
/// need to stay well under the server's global `max_connections` (the stock
/// image's default is 100). Measured empirically (`pg_stat_activity`), a
/// combined-plugin instance's *real* steady-state connection cost is `2
/// (dedicated LISTEN connections — cache watch + lock release-wake) +
/// pool_max_size` — **not** `pool_max_size + 1` as a first pass assumed,
/// because the cache TTL reaper and the lock TTL reaper each grab their own
/// pool connection on their first sweep and then sit idle-but-open in the
/// pool afterward (normal pool reuse — sqlx doesn't close an idle pooled
/// connection just because nothing is using it right now), so with
/// `pool_max_size: 2` both reapers' connections alone consumed the *entire*
/// per-instance pool budget: `4 * N` measured for `N` cache-conformance
/// instances, not the originally-assumed `3 * N`. Cache/leader instances
/// (this function) never hold a connection indefinitely the way a lock does
/// (`PostgresLock`'s `held` map only pins connections for the standalone
/// lock plugin's own `try_lock`/`lock`, which nothing calls when the
/// combined plugin is only being used for its cache half here), so `1` is
/// safe — see [`lock_config_for_schema`] for why the standalone lock plugin
/// needs `2`.
pub fn cluster_config_for_schema(connection_string: &str, schema: &str) -> PostgresClusterConfig {
    cluster_config_json(
        connection_string,
        json!({ "schema": schema, "pool_max_size": 1 }),
    )
}

/// Like [`cluster_config_for_schema`], with extra field `overrides` merged in
/// (e.g. a short `cache_reaper_interval_ms` for real-time TTL-expiry
/// conformance scenarios that need the sweeper to actually fire within a real
/// wait). `pool_max_size` defaults to `2` here rather than `1`, since a fast
/// reaper competes with the scenario's own `get`/`watch` for a pool connection.
pub fn cluster_config_for_schema_with(
    connection_string: &str,
    schema: &str,
    overrides: serde_json::Value,
) -> PostgresClusterConfig {
    let mut base = json!({ "schema": schema, "pool_max_size": 2 });
    merge(&mut base, overrides);
    cluster_config_json(connection_string, base)
}

/// Like [`cluster_config_for_schema`], for the standalone lock-only plugin.
/// Kept at `2`, not `1`: some `SC-LOCK-*` scenarios (`scenario_lock_004`)
/// spawn a concurrent waiter that needs its own connection *while* the
/// holder's connection is still pinned, so a given lock instance can
/// legitimately need two connections at once (this doesn't apply to
/// [`cluster_config_for_schema`]'s cache/leader use, which never pins a
/// connection).
pub fn lock_config_for_schema(connection_string: &str, schema: &str) -> PostgresLockConfig {
    lock_config_json(
        connection_string,
        json!({ "schema": schema, "pool_max_size": 2 }),
    )
}

/// Like [`lock_config_for_schema`], with extra field `overrides` merged in
/// (e.g. a short `lock_reaper_interval_ms` for real-time TTL-reclaim scenarios).
pub fn lock_config_for_schema_with(
    connection_string: &str,
    schema: &str,
    overrides: serde_json::Value,
) -> PostgresLockConfig {
    let mut base = json!({ "schema": schema, "pool_max_size": 2 });
    merge(&mut base, overrides);
    lock_config_json(connection_string, base)
}

/// A bare `sqlx::PgPool` against the same container, for tests that need to
/// assert Postgres-level state directly (catalog views, `pg_terminate_backend`,
/// raw `SET`/`SHOW`) rather than only through the plugin's own API.
pub async fn raw_pool(connection_string: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(5))
        .connect(connection_string)
        .await
        .expect("raw pool connects")
}

/// Whether `table` (schema-qualified, e.g. `"public.cluster_cache"`) exists.
pub async fn table_exists(pool: &PgPool, schema: &str, table: &str) -> bool {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
         WHERE table_schema = $1 AND table_name = $2)",
    )
    .bind(schema)
    .bind(table)
    .fetch_one(pool)
    .await
    .expect("information_schema query succeeds");
    exists
}

/// Polls `condition` until it returns `true` or `timeout` elapses, sleeping
/// `interval` between attempts. Used in place of a single fixed `sleep` for
/// assertions on state that changes asynchronously in the background (a
/// reaper sweep, a LISTEN-driven watch delivery) without hard-coding a
/// latency bound tighter than the scenario is actually about.
pub async fn wait_until<F>(timeout: Duration, interval: Duration, mut condition: F) -> bool
where
    F: AsyncFnMut() -> bool,
{
    // Bound the *whole* wait — including each in-flight `condition().await` — by
    // `timeout` (PGR-E5). Checking the deadline only after `condition` returns
    // let a stalled SQL/network call inside it overrun `timeout` indefinitely.
    tokio::time::timeout(timeout, async {
        loop {
            if condition().await {
                return true;
            }
            tokio::time::sleep(interval).await;
        }
    })
    .await
    .unwrap_or(false)
}
