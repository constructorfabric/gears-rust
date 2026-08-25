//! Real-Postgres integration tests for the AM-local lease
//! primitive ([`account_management::infra::lease`]).
//!
//! Exercises the PG-specific behaviour the `SQLite` unit tests in
//! `lease_manager_sqlite.rs` and `lease_guard_sqlite.rs` cannot:
//!
//! * **Real concurrent writer contention.** PG's per-row locking
//!   lets N tasks attempt `acquire` simultaneously without the
//!   `SQLite` single-writer serialisation; the lease primitive must
//!   produce exactly one winner and `LeaseHeld` for every loser.
//! * **SERIALIZABLE 40001 retry under contention.** The acquire
//!   path's `transaction_with_retry` wraps the SELECT-then-UPDATE
//!   pattern in a SERIALIZABLE tx; peer steals trigger 40001
//!   aborts that retry transparently.
//! * **Mid-tx fence rollback.** The `with_ack_in_tx` body can be
//!   interrupted by a peer steal between the work and the fence
//!   SELECT — under PG, the steal can commit while the holder's
//!   tx is open, the fence catches it, and the whole tx rolls
//!   back atomically.
//! * **Renewal heartbeat under real timing.** The `spawn_renewal`
//!   path uses `tokio::time::interval` against the DB clock; PG
//!   makes the renewal observable end-to-end without the
//!   `SQLite` single-writer interference that would block the
//!   heartbeat behind any in-flight writer-tx.
//!
//! Gated behind `#[cfg(feature = "integration")]` so the default
//! `cargo test` run does not require Docker. Enable explicitly:
//! `cargo test -p cf-gears-account-management --features integration
//!  --test coord_lease_integration_pg`.

#![cfg(feature = "integration")]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![cfg_attr(coverage_nightly, coverage(off))]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use sea_orm::ExprTrait;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::secure::{SecureEntityExt, SecureUpdateExt};
use toolkit_security::AccessScope;
use uuid::Uuid;

use account_management::infra::lease::{AckError, CoordError, LeaseManager, RenewalState};
use account_management::infra::storage::entity::am_leases;
use account_management::infra::storage::repo_impl::AmDbProvider;

use common::pg::bring_up_postgres;

const KEY: &str = "hierarchy_integrity";
const TRACER_KEY: &str = "with-ack-tx-tracer";

// ---------------------------------------------------------------------
// Concurrent acquire — exactly one winner
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_managers_one_wins() {
    const TASK_COUNT: usize = 16;
    let h = bring_up_postgres().await.expect("postgres testcontainer");
    let db = h.provider.db();

    let barrier = Arc::new(tokio::sync::Barrier::new(TASK_COUNT));
    let mut handles = Vec::with_capacity(TASK_COUNT);
    for _ in 0..TASK_COUNT {
        let db = db.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            LeaseManager::new(db)
                .acquire(KEY, Duration::from_mins(1))
                .await
        }));
    }

    let mut ok = 0_usize;
    let mut held = 0_usize;
    let mut other = Vec::new();
    for h in handles {
        match h.await.expect("join") {
            Ok(_guard) => ok += 1,
            Err(CoordError::LeaseHeld) => held += 1,
            Err(other_err) => other.push(format!("{other_err:?}")),
        }
    }
    assert!(
        other.is_empty(),
        "no acquire may fail with a non-LeaseHeld variant under \
         PG SERIALIZABLE retry: {other:?}"
    );
    assert_eq!(
        ok, 1,
        "exactly one task wins acquire (got {ok} of {TASK_COUNT})"
    );
    assert_eq!(
        held,
        TASK_COUNT - 1,
        "every other task surfaces LeaseHeld (got {held} of {})",
        TASK_COUNT - 1
    );
}

// ---------------------------------------------------------------------
// Expired lease can be stolen
// ---------------------------------------------------------------------

#[tokio::test]
async fn expired_lease_can_be_stolen() {
    let h = bring_up_postgres().await.expect("postgres testcontainer");
    let db = h.provider.db();
    let mgr = LeaseManager::new(db.clone());

    let first = mgr
        .acquire(KEY, Duration::from_secs(1))
        .await
        .expect("first acquire");

    // Sleep past the TTL so the row's `locked_until` is in the past
    // (DB-clock anchored). The second acquire should observe the
    // expired row, steal it, and bump `attempts`.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let second = mgr
        .acquire(KEY, Duration::from_mins(1))
        .await
        .expect("steal expired");
    assert_ne!(
        first.locked_by(),
        second.locked_by(),
        "stealer holds a fresh uuid, not the original holder's"
    );

    let row = read_row(&h.provider, KEY).await.expect("row");
    assert_eq!(
        row.attempts, 2,
        "steal increments attempts (1 from initial + 1 from steal)"
    );
    assert_eq!(row.locked_by, Some(second.locked_by()));
}

// ---------------------------------------------------------------------
// Fence-in-tx blocks orphaned writer
// ---------------------------------------------------------------------

#[tokio::test]
async fn fence_blocks_orphaned_writer() {
    let h = bring_up_postgres().await.expect("postgres testcontainer");
    let db = h.provider.db();
    let mgr = LeaseManager::new(db.clone());

    let guard = mgr
        .acquire(KEY, Duration::from_mins(1))
        .await
        .expect("acquire");

    // Peer steals the slot directly (out-of-band UPDATE that the
    // lease guard's fence SELECT will catch). On PG this commit
    // races the guard's later `with_ack_in_tx`; the fence SELECT
    // (last DB call in the tx) sees the changed `locked_by` and
    // returns LeaseLost → whole tx rolls back.
    let peer = Uuid::new_v4();
    steal_to(&h.provider, KEY, peer)
        .await
        .expect("manual steal");

    let result: Result<(), AckError<DummyErr>> = guard
        .with_ack_in_tx(DummyErr::db_err, |tx| {
            Box::pin(async move {
                insert_tracer(tx).await?;
                Ok(())
            })
        })
        .await;
    match result {
        Err(AckError::LeaseLost) => {}
        other => panic!("expected LeaseLost, got {other:?}"),
    }

    // Tracer row must be absent post-rollback.
    let tracer = read_row(&h.provider, TRACER_KEY).await;
    assert!(
        tracer.is_none(),
        "tracer row must NOT survive a LeaseLost rollback under PG"
    );
}

// ---------------------------------------------------------------------
// Renewal heartbeat keeps the lease alive
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renewal_keeps_lease_alive() {
    let h = bring_up_postgres().await.expect("postgres testcontainer");
    let db = h.provider.db();
    let mgr = LeaseManager::new(db.clone());

    // Short TTL with a 1/3 renewal period — the lease survives any
    // single missed tick (transient DB blip) but expires within
    // ~2x ttl after renewal stops.
    let ttl = Duration::from_secs(2);
    let renew_period = Duration::from_millis(500);

    let guard = mgr.acquire(KEY, ttl).await.expect("acquire");
    let renewal = guard.spawn_renewal(renew_period);

    // Sleep well past the TTL — renewal should keep the lease
    // alive.
    tokio::time::sleep(Duration::from_secs(5)).await;

    // A peer acquire MUST fail (lease still held).
    let peer_mgr = LeaseManager::new(db.clone());
    let peer_attempt = peer_mgr.acquire(KEY, ttl).await;
    match peer_attempt {
        Err(CoordError::LeaseHeld) => {}
        other => panic!("expected LeaseHeld while renewal is active, got {other:?}"),
    }

    // Stop renewal and release.
    renewal.shutdown().await;
    guard.release().await.expect("release");

    // After release + brief settle, peer can acquire.
    let after = peer_mgr
        .acquire(KEY, ttl)
        .await
        .expect("post-release acquire");
    assert_ne!(after.locked_by(), Uuid::nil());
}

// ---------------------------------------------------------------------
// Renewal signals Lost on peer takeover
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renewal_signals_lost_on_takeover() {
    let h = bring_up_postgres().await.expect("postgres testcontainer");
    let db = h.provider.db();
    let mgr = LeaseManager::new(db.clone());

    let ttl = Duration::from_secs(10);
    let renew_period = Duration::from_millis(200);
    let guard = mgr.acquire(KEY, ttl).await.expect("acquire");
    let mut renewal = guard.spawn_renewal(renew_period);

    // Confirm the heartbeat is initially Healthy.
    assert_eq!(*renewal.state.borrow_and_update(), RenewalState::Healthy);

    // Peer steals the slot — the next renewal tick will UPDATE
    // zero rows (locked_by no longer matches) and signal Lost on
    // the watch channel.
    let peer = Uuid::new_v4();
    steal_to(&h.provider, KEY, peer)
        .await
        .expect("manual steal");

    // Wait for the watch to flip to Lost — should happen within
    // one or two heartbeat ticks (~400ms). Cap at 5s to keep the
    // test snappy on slow CI.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if *renewal.state.borrow() == RenewalState::Lost {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "renewal did not signal Lost within 5s after peer steal; state still: {:?}",
            *renewal.state.borrow()
        );
        // Wait for either a state change or the timeout, whichever
        // comes first. The loop head re-checks the state, so the
        // timeout result is intentionally ignored.
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let _elapsed = tokio::time::timeout(remaining, renewal.state.changed()).await;
    }

    // Clean shutdown.
    renewal.shutdown().await;
}

// ---------------------------------------------------------------------
// Heartbeat fires concurrently with a long-running writer transaction.
//
// The lease is acquired with a TTL that is **shorter** than the
// `with_ack_in_tx` body's sleep, so if the renewal task were
// blocked behind the writer-tx the lease would expire and the
// fence SELECT inside the tx would return `LeaseLost`. A successful
// commit therefore proves the heartbeat ran in parallel with the
// in-flight repair tx — directly validates the heartbeat × repair
// concurrency contract on PG's per-row writer model.
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn renewal_runs_concurrently_with_long_writer_tx() {
    let h = bring_up_postgres().await.expect("postgres testcontainer");
    let db = h.provider.db();
    let mgr = LeaseManager::new(db.clone());

    // Tight TTL so the writer-tx alone would exceed it; renewal is
    // the only thing that keeps the lease alive.
    let ttl = Duration::from_secs(2);
    let renew_period = Duration::from_millis(500);
    let work_sleep = Duration::from_secs(5);

    let guard = mgr.acquire(KEY, ttl).await.expect("acquire");
    let renewal = guard.spawn_renewal(renew_period);

    // Snapshot `locked_until` before the writer-tx — the renewal
    // task should push this forward repeatedly while the tx is
    // held open.
    let before = read_row(&h.provider, KEY)
        .await
        .expect("row present")
        .locked_until;

    let result: Result<(), AckError<DummyErr>> = guard
        .with_ack_in_tx(DummyErr::db_err, move |tx| {
            Box::pin(async move {
                // Hold the writer-tx open for `work_sleep` — much
                // longer than TTL. On PG this does NOT block the
                // renewal task's UPDATE on the same `am_leases` row
                // because PG uses per-row locking; the renewal
                // commits while we sleep, pushing `locked_until`
                // forward. The fence SELECT at the end then sees
                // the still-valid lease and the tx commits.
                tokio::time::sleep(work_sleep).await;
                insert_tracer(tx).await
            })
        })
        .await;

    match result {
        Ok(()) => {}
        other => {
            panic!("expected Ok (renewal kept lease alive during long writer-tx), got {other:?}")
        }
    }

    // After the writer-tx commits, the row's `locked_until` must
    // be in the future (renewal touched it during the sleep).
    let after = read_row(&h.provider, KEY)
        .await
        .expect("row present")
        .locked_until;
    assert!(
        after > before,
        "renewal must have pushed locked_until forward during the writer-tx \
         (before={before}, after={after})"
    );
    // Tracer row committed alongside the lease — proves the work
    // closure ran and the whole tx (work + fence) committed.
    assert!(
        read_row(&h.provider, TRACER_KEY).await.is_some(),
        "tracer row must be present post-commit"
    );

    renewal.shutdown().await;
    guard.release().await.expect("release");
}

// ---------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------

#[derive(Debug)]
#[allow(dead_code)]
enum DummyErr {
    /// Carries the formatted message of any non-Sea `DbError` so
    /// the from-impl below is total. The string itself is never
    /// read by these tests — we only branch on the variant.
    Custom(String),
    Db(sea_orm::DbErr),
}

impl DummyErr {
    fn db_err(&self) -> Option<&sea_orm::DbErr> {
        match self {
            Self::Db(e) => Some(e),
            Self::Custom(_) => None,
        }
    }
}

impl From<toolkit_db::DbError> for DummyErr {
    fn from(err: toolkit_db::DbError) -> Self {
        match err {
            toolkit_db::DbError::Sea(e) => Self::Db(e),
            other => Self::Custom(format!("{other:?}")),
        }
    }
}

async fn insert_tracer(tx: &toolkit_db::secure::DbTx<'_>) -> Result<(), DummyErr> {
    let am = am_leases::ActiveModel {
        key: ActiveValue::Set(TRACER_KEY.to_owned()),
        locked_by: ActiveValue::Set(None),
        locked_until: ActiveValue::Set(OffsetDateTime::UNIX_EPOCH),
        attempts: ActiveValue::Set(0),
    };
    toolkit_db::secure::secure_insert::<am_leases::Entity>(am, &AccessScope::allow_all(), tx)
        .await
        .map_err(|sce| match sce {
            toolkit_db::secure::ScopeError::Db(e) => DummyErr::Db(e),
            other => DummyErr::Custom(format!("scope error: {other:?}")),
        })?;
    Ok(())
}

async fn read_row(provider: &Arc<AmDbProvider>, key: &str) -> Option<am_leases::Model> {
    let conn = provider.conn().expect("conn");
    am_leases::Entity::find()
        .filter(am_leases::Column::Key.eq(key))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(&conn)
        .await
        .expect("read am_leases row")
}

async fn steal_to(provider: &Arc<AmDbProvider>, key: &str, new_holder: Uuid) -> anyhow::Result<()> {
    let conn = provider.conn()?;
    let n = am_leases::Entity::update_many()
        .col_expr(am_leases::Column::LockedBy, Expr::value(new_holder))
        .col_expr(
            am_leases::Column::LockedUntil,
            Expr::value(OffsetDateTime::now_utc() + Duration::from_hours(1)),
        )
        .col_expr(
            am_leases::Column::Attempts,
            Expr::col(am_leases::Column::Attempts).add(1),
        )
        .filter(am_leases::Column::Key.eq(key))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .map_err(|e| anyhow::anyhow!("steal failed: {e:?}"))?;
    if n.rows_affected != 1 {
        anyhow::bail!("steal_to: expected 1 row, got {}", n.rows_affected);
    }
    Ok(())
}
