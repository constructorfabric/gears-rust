#![allow(clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use quota_enforcement_sdk::TenantId;
use sea_orm::ActiveValue::Set;
use time::macros::datetime;
use tokio::sync::{Semaphore, oneshot};
use toolkit_db::Db;
use toolkit_db::secure::secure_insert;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::{ConfigCache, ConfigSnapshot, ConfigUnavailable};
use crate::infra::storage::entity::{
    DEFAULT_KEY, contention_timeout_config as timeout, idempotency_retention_config as retention,
    lease_capacity_config as cap,
};
use crate::test_support::test_db;

const M: &str = "metric-a";

impl From<ConfigUnavailable> for &'static str {
    fn from(_: ConfigUnavailable) -> Self {
        "unavailable"
    }
}

/// The configured tenant.
fn t() -> TenantId {
    TenantId::new(Uuid::from_u128(0xa))
}

/// A tenant with no rows of its own.
fn u() -> TenantId {
    TenantId::new(Uuid::from_u128(0xb))
}

/// One row per specificity level; `None` leaves the level unconfigured.
struct Rows {
    cap: [Option<i32>; 4],
    retention: [Option<i64>; 4],
    timeout: [Option<i64>; 2],
}

async fn seed(db: &Db, rows: &Rows) {
    let tenant = t().as_uuid().to_string();
    let pairs = [
        (tenant.as_str(), M),
        (tenant.as_str(), DEFAULT_KEY),
        (DEFAULT_KEY, M),
        (DEFAULT_KEY, DEFAULT_KEY),
    ];
    let conn = db.conn().expect("conn");
    let scope = AccessScope::allow_all();
    let at = datetime!(2026-03-17 10:00:00 UTC);
    for ((tenant, metric), value) in pairs.into_iter().zip(rows.cap) {
        if let Some(value) = value {
            secure_insert::<cap::Entity>(
                cap::ActiveModel {
                    tenant_key: Set(tenant.to_owned()),
                    metric_key: Set(metric.to_owned()),
                    max_active_leases: Set(value),
                    updated_at: Set(at),
                },
                &scope,
                &conn,
            )
            .await
            .expect("cap row");
        }
    }
    for ((tenant, metric), value) in pairs.into_iter().zip(rows.retention) {
        if let Some(value) = value {
            secure_insert::<retention::Entity>(
                retention::ActiveModel {
                    tenant_key: Set(tenant.to_owned()),
                    metric_key: Set(metric.to_owned()),
                    retention_seconds: Set(value),
                    updated_at: Set(at),
                },
                &scope,
                &conn,
            )
            .await
            .expect("retention row");
        }
    }
    for (metric, value) in [M, DEFAULT_KEY].into_iter().zip(rows.timeout) {
        if let Some(value) = value {
            secure_insert::<timeout::Entity>(
                timeout::ActiveModel {
                    metric_key: Set(metric.to_owned()),
                    timeout_ms: Set(value),
                    updated_at: Set(at),
                },
                &scope,
                &conn,
            )
            .await
            .expect("timeout row");
        }
    }
}

/// Name, rows, tenant, metric, and the resolved `(cap, retention s, timeout ms)`.
type Case<'a> = (&'a str, &'a Rows, TenantId, &'a str, (u64, i64, u64));

/// `(lease cap, retention seconds, contention timeout ms)` for one pair.
async fn resolve(db: &Db, tenant: TenantId, metric: &str) -> (u64, i64, u64) {
    let conn = db.conn().expect("conn");
    let snapshot = ConfigSnapshot::load(&conn).await.expect("load");
    (
        snapshot.lease_cap(tenant, metric),
        snapshot
            .idempotency_retention(tenant, metric)
            .whole_seconds(),
        u64::try_from(snapshot.contention_timeout(metric).as_millis()).expect("millis"),
    )
}

#[tokio::test]
async fn the_most_specific_row_wins_and_defaults_fill_the_rest() {
    let layered = Rows {
        cap: [Some(11), Some(12), Some(13), Some(14)],
        retention: [Some(21), Some(22), Some(23), Some(24)],
        timeout: [Some(31), Some(32)],
    };
    let empty = Rows {
        cap: [None; 4],
        retention: [None; 4],
        timeout: [None; 2],
    };
    let negative = Rows {
        cap: [None, None, None, Some(-1)],
        retention: [None, None, None, Some(-5)],
        timeout: [None, Some(-1)],
    };
    let skipping = Rows {
        cap: [None, None, Some(13), Some(14)],
        retention: [None, Some(22), None, Some(24)],
        timeout: [None, Some(32)],
    };
    let cases: [Case<'_>; 10] = [
        ("tenant and metric", &layered, t(), M, (11, 21, 31)),
        ("tenant, any metric", &layered, t(), "other", (12, 22, 32)),
        ("any tenant, metric", &layered, u(), M, (13, 23, 31)),
        (
            "any tenant, any metric",
            &layered,
            u(),
            "other",
            (14, 24, 32),
        ),
        (
            "no rows: shipped defaults",
            &empty,
            t(),
            M,
            (1000, 86_400, 0),
        ),
        ("negative values", &negative, t(), M, (1000, 0, 0)),
        ("cap skips to (*, m)", &skipping, t(), M, (13, 22, 32)),
        (
            "retention skips to (t, *)",
            &skipping,
            t(),
            "other",
            (14, 22, 32),
        ),
        (
            "any tenant falls to (*, *)",
            &skipping,
            u(),
            "other",
            (14, 24, 32),
        ),
        (
            "default key as the metric",
            &layered,
            t(),
            DEFAULT_KEY,
            (12, 22, 32),
        ),
    ];
    for (name, rows, tenant, metric, expected) in cases {
        let db = test_db().await;
        seed(&db, rows).await;
        assert_eq!(resolve(&db, tenant, metric).await, expected, "{name}");
    }
}

#[test]
fn the_strictest_metric_sets_the_budget_and_none_falls_to_the_default() {
    let mut snapshot = ConfigSnapshot::default();
    for (metric, millis) in [(M, 40), ("metric-b", 10), (DEFAULT_KEY, 25)] {
        snapshot
            .contention_timeout
            .insert(metric.to_owned(), millis);
    }
    let ms = Duration::from_millis;
    assert_eq!(
        snapshot.strictest_contention_timeout([M, "metric-b"]),
        ms(10)
    );
    assert_eq!(snapshot.strictest_contention_timeout([M, "unset"]), ms(25));
    assert_eq!(snapshot.strictest_contention_timeout([M]), ms(40));
    assert_eq!(snapshot.strictest_contention_timeout(None), ms(25));
}

const AT: time::OffsetDateTime = datetime!(2026-03-17 10:00:00 UTC);

/// A snapshot whose platform-default lease cap is `cap`.
fn with_cap(cap: i32) -> ConfigSnapshot {
    let mut snapshot = ConfigSnapshot::default();
    snapshot
        .lease_capacity
        .entry(DEFAULT_KEY.to_owned())
        .or_default()
        .insert(DEFAULT_KEY.to_owned(), cap);
    snapshot
}

async fn cap_at(
    cache: &ConfigCache,
    at: i64,
    load: Result<i32, &'static str>,
) -> Result<u64, &'static str> {
    cache
        .get(AT + time::Duration::seconds(at), || async move {
            load.map(with_cap)
        })
        .await
        .map(|snapshot| snapshot.lease_cap(t(), M))
}

#[tokio::test]
async fn a_snapshot_serves_until_the_refresh_interval_passes() {
    let cache = ConfigCache::new(std::time::Duration::from_secs(30));
    assert_eq!(cap_at(&cache, 0, Ok(1)).await, Ok(1));
    assert_eq!(cap_at(&cache, 29, Ok(2)).await, Ok(1), "still fresh");
    assert_eq!(cap_at(&cache, 30, Ok(3)).await, Ok(3), "reloaded");
}

#[tokio::test]
async fn a_failed_reload_keeps_the_last_snapshot_for_another_interval() {
    let cache = ConfigCache::new(std::time::Duration::from_secs(30));
    assert_eq!(cap_at(&cache, 0, Ok(1)).await, Ok(1));
    assert_eq!(cap_at(&cache, 30, Err("down")).await, Ok(1), "kept");
    assert_eq!(
        cap_at(&cache, 59, Ok(2)).await,
        Ok(1),
        "no retry before the interval"
    );
    assert_eq!(cap_at(&cache, 60, Ok(2)).await, Ok(2), "retried");
}

#[tokio::test]
async fn a_failed_first_load_is_the_callers_error() {
    let cache = ConfigCache::new(std::time::Duration::from_secs(30));
    assert_eq!(cap_at(&cache, 0, Err("down")).await, Err("down"));
    assert_eq!(cap_at(&cache, 0, Ok(1)).await, Ok(1), "the next read loads");
}

#[tokio::test]
async fn a_clock_that_moves_back_reloads() {
    let cache = ConfigCache::new(std::time::Duration::from_secs(30));
    assert_eq!(cap_at(&cache, 0, Ok(1)).await, Ok(1));
    assert_eq!(cap_at(&cache, -1, Ok(2)).await, Ok(2));
}

fn at(seconds: i64) -> time::OffsetDateTime {
    AT + time::Duration::seconds(seconds)
}

#[tokio::test]
async fn a_reader_is_not_blocked_while_a_reload_is_in_flight() {
    let cache = Arc::new(ConfigCache::new(Duration::from_secs(30)));
    assert_eq!(cap_at(&cache, 0, Ok(1)).await, Ok(1));
    let (started, has_started) = oneshot::channel();
    let (open, gate) = oneshot::channel::<()>();
    let reload = tokio::spawn({
        let cache = Arc::clone(&cache);
        async move {
            cache
                .get(at(30), move || async move {
                    started.send(()).expect("started");
                    gate.await.expect("gate");
                    Ok::<_, &'static str>(with_cap(2))
                })
                .await
                .map(|snapshot| snapshot.lease_cap(t(), M))
        }
    });
    has_started.await.expect("the reload started");

    let during = tokio::time::timeout(
        Duration::from_millis(500),
        cap_at(&cache, 31, Err("a second load")),
    )
    .await;
    assert_eq!(
        during.expect("a reader does not wait for the reload"),
        Ok(1),
        "the current snapshot serves"
    );

    open.send(()).expect("open");
    assert_eq!(reload.await.expect("join"), Ok(2));
    assert_eq!(cap_at(&cache, 31, Err("no load")).await, Ok(2), "published");
}

#[tokio::test]
async fn concurrent_readers_trigger_exactly_one_load() {
    for warm in [false, true] {
        let cache = Arc::new(ConfigCache::new(Duration::from_secs(30)));
        if warm {
            assert_eq!(cap_at(&cache, 0, Ok(1)).await, Ok(1));
        }
        let loads = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Semaphore::new(0));
        let readers: Vec<_> = (0..8)
            .map(|_| {
                let (cache, loads, gate) =
                    (Arc::clone(&cache), Arc::clone(&loads), Arc::clone(&gate));
                tokio::spawn(async move {
                    cache
                        .get(at(30), move || async move {
                            loads.fetch_add(1, Ordering::SeqCst);
                            let _permit = gate.acquire().await.expect("gate");
                            Ok::<_, &'static str>(with_cap(2))
                        })
                        .await
                        .map(|snapshot| snapshot.lease_cap(t(), M))
                })
            })
            .collect();
        while loads.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
        gate.add_permits(8);
        for reader in readers {
            assert!(reader.await.expect("join").is_ok(), "warm: {warm}");
        }
        assert_eq!(loads.load(Ordering::SeqCst), 1, "warm: {warm}");
    }
}

#[tokio::test]
async fn a_failed_reload_keeps_the_last_success_time_apart_from_the_retry_time() {
    let cache = ConfigCache::new(Duration::from_secs(30));
    assert_eq!(cap_at(&cache, 0, Ok(1)).await, Ok(1));
    assert_eq!(cap_at(&cache, 30, Err("down")).await, Ok(1), "kept");
    let published = cache.current.load_full().expect("published");
    assert_eq!(published.loaded_at, at(0), "the last successful load");
    assert_eq!(
        published.checked_at,
        at(30),
        "the next retry counts from here"
    );
}

type Read = tokio::task::JoinHandle<Result<u64, &'static str>>;

/// A cold read at `now` whose load signals its start and waits for the returned sender.
fn gated_cold_read(
    cache: &Arc<ConfigCache>,
    now: time::OffsetDateTime,
    load: Result<i32, &'static str>,
) -> (oneshot::Receiver<()>, oneshot::Sender<()>, Read) {
    let (started, has_started) = oneshot::channel();
    let (open, gate) = oneshot::channel::<()>();
    let cache = Arc::clone(cache);
    let read = tokio::spawn(async move {
        cache
            .get(now, move || async move {
                started.send(()).expect("started");
                gate.await.expect("gate");
                load.map(with_cap)
            })
            .await
            .map(|snapshot| snapshot.lease_cap(t(), M))
    });
    (has_started, open, read)
}

/// A read at `now` that counts its loads.
fn counted_read(
    cache: &Arc<ConfigCache>,
    now: time::OffsetDateTime,
    loads: &Arc<AtomicUsize>,
) -> Read {
    let (cache, loads) = (Arc::clone(cache), Arc::clone(loads));
    tokio::spawn(async move {
        cache
            .get(now, move || async move {
                loads.fetch_add(1, Ordering::SeqCst);
                Ok::<_, &'static str>(with_cap(2))
            })
            .await
            .map(|snapshot| snapshot.lease_cap(t(), M))
    })
}

#[tokio::test]
async fn a_caller_queued_behind_a_newer_load_takes_it() {
    let cache = Arc::new(ConfigCache::new(Duration::from_secs(30)));
    let (has_started, open, first) = gated_cold_read(&cache, at(31), Ok(1));
    has_started.await.expect("the first load started");
    let loads = Arc::new(AtomicUsize::new(0));
    let queued = counted_read(&cache, at(30), &loads);
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    open.send(()).expect("open");
    assert_eq!(first.await.expect("join"), Ok(1));
    assert_eq!(queued.await.expect("join"), Ok(1), "takes the newer load");
    assert_eq!(loads.load(Ordering::SeqCst), 0, "no second load");
}

#[tokio::test]
async fn callers_queued_behind_a_failed_first_load_do_not_retry_it() {
    let cache = Arc::new(ConfigCache::new(Duration::from_secs(30)));
    let (has_started, open, first) = gated_cold_read(&cache, at(0), Err("down"));
    has_started.await.expect("the first load started");
    let loads = Arc::new(AtomicUsize::new(0));
    let queued = counted_read(&cache, at(0), &loads);
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    open.send(()).expect("open");
    assert_eq!(first.await.expect("join"), Err("down"));
    assert_eq!(
        queued.await.expect("join"),
        Err("unavailable"),
        "shares the failure"
    );
    assert_eq!(loads.load(Ordering::SeqCst), 0, "no serial retry");
    assert_eq!(cap_at(&cache, 0, Ok(3)).await, Ok(3), "a later read loads");
}
