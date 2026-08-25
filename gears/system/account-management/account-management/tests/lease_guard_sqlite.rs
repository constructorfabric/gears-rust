//! `SQLite` unit tests for [`account_management::infra::lease::LeaseGuard`].
//!
//! Covers renew, release, `release_with_retry`, and the
//! `with_ack_in_tx` fence semantics. The renewal heartbeat task
//! (`spawn_renewal`) is exercised in `coord_lease_integration_pg.rs`
//! — on `SQLite` the single-writer model makes timing-sensitive
//! heartbeat tests flaky (see plan §7.2 `SQLite` test-discipline note).

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

use account_management::infra::lease::{AckError, CoordError, LeaseManager};
use account_management::infra::storage::entity::am_leases;
use account_management::infra::storage::repo_impl::AmDbProvider;

use common::setup_sqlite;

const KEY: &str = "hierarchy_integrity";
const TTL: Duration = Duration::from_mins(10);
const TRACER_KEY: &str = "with-ack-tx-tracer";

// ---------------------------------------------------------------------
// renew
// ---------------------------------------------------------------------

#[tokio::test]
async fn renew_extends_locked_until() {
    let harness = setup_sqlite().await.expect("setup");
    let db = harness.provider.db();
    let mgr = LeaseManager::new(db.clone());

    let guard = mgr.acquire(KEY, TTL).await.expect("acquire");
    let before = read_row(&harness.provider, KEY)
        .await
        .expect("row")
        .locked_until;

    // Sleep one second to make the renew-induced change observable.
    tokio::time::sleep(Duration::from_secs(1)).await;
    guard.renew(Duration::from_mins(15)).await.expect("renew");

    let after = read_row(&harness.provider, KEY)
        .await
        .expect("row")
        .locked_until;
    assert!(
        after > before,
        "renew must push locked_until forward ({after} > {before})"
    );
}

#[tokio::test]
async fn renew_fails_when_stolen() {
    let harness = setup_sqlite().await.expect("setup");
    let db = harness.provider.db();
    let mgr = LeaseManager::new(db.clone());

    let guard = mgr.acquire(KEY, TTL).await.expect("acquire");
    // Peer steals the slot by overwriting locked_by directly.
    steal_to(&harness.provider, KEY, Uuid::new_v4())
        .await
        .expect("manual steal");

    match guard.renew(TTL).await {
        Err(CoordError::LeaseLost) => {}
        other => panic!("expected LeaseLost, got {other:?}"),
    }
}

#[tokio::test]
async fn renew_fails_when_lease_expired() {
    let harness = setup_sqlite().await.expect("setup");
    let db = harness.provider.db();
    let mgr = LeaseManager::new(db.clone());

    let guard = mgr.acquire(KEY, TTL).await.expect("acquire");
    // TTL lapses on the DB clock with NO peer steal (`locked_by`
    // unchanged): the renewal filter's `locked_until > NOW()` clause
    // must still treat an expired lease as lost.
    expire_lease(&harness.provider, KEY).await.expect("expire");

    match guard.renew(TTL).await {
        Err(CoordError::LeaseLost) => {}
        other => panic!("expected LeaseLost on an expired lease, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// release / release_with_retry
// ---------------------------------------------------------------------

#[tokio::test]
async fn release_resets_attempts() {
    let harness = setup_sqlite().await.expect("setup");
    let db = harness.provider.db();
    let mgr = LeaseManager::new(db.clone());

    let guard = mgr.acquire(KEY, TTL).await.expect("acquire");
    guard.release().await.expect("release");

    let row = read_row(&harness.provider, KEY).await.expect("row");
    assert_eq!(row.locked_by, None, "released row's locked_by is NULL");
    assert_eq!(row.attempts, 0, "release zeros attempts");
    assert_eq!(
        row.locked_until,
        OffsetDateTime::UNIX_EPOCH,
        "release stamps locked_until to epoch"
    );
}

#[tokio::test]
async fn release_with_retry_preserves_attempts() {
    let harness = setup_sqlite().await.expect("setup");
    let db = harness.provider.db();
    let mgr = LeaseManager::new(db.clone());

    // Seed an expired row to force the steal path → attempts=2.
    let peer = Uuid::new_v4();
    insert_lease_row(
        &harness.provider,
        KEY,
        Some(peer),
        OffsetDateTime::UNIX_EPOCH,
        1,
    )
    .await
    .expect("seed expired");

    let guard = mgr.acquire(KEY, TTL).await.expect("steal");
    assert_eq!(
        read_row(&harness.provider, KEY)
            .await
            .expect("row")
            .attempts,
        2,
        "steal bumped attempts"
    );

    guard
        .release_with_retry()
        .await
        .expect("release_with_retry");

    let row = read_row(&harness.provider, KEY).await.expect("row");
    assert_eq!(row.locked_by, None);
    assert_eq!(
        row.attempts, 2,
        "release_with_retry preserves the forensic streak"
    );
}

#[tokio::test]
async fn release_when_already_stolen_returns_ok() {
    let harness = setup_sqlite().await.expect("setup");
    let db = harness.provider.db();
    let mgr = LeaseManager::new(db.clone());

    let guard = mgr.acquire(KEY, TTL).await.expect("acquire");
    let peer = Uuid::new_v4();
    steal_to(&harness.provider, KEY, peer)
        .await
        .expect("manual steal");

    // Release should be Ok(()) even though it matched zero rows.
    // The WARN log is the observability signal; the test verifies
    // the contract (no error returned to the caller).
    guard.release().await.expect("release returns Ok");

    let row = read_row(&harness.provider, KEY).await.expect("row");
    assert_eq!(
        row.locked_by,
        Some(peer),
        "peer's takeover survives our release attempt"
    );
}

// ---------------------------------------------------------------------
// with_ack_in_tx
// ---------------------------------------------------------------------

#[tokio::test]
async fn with_ack_in_tx_commits_on_success() {
    let harness = setup_sqlite().await.expect("setup");
    let db = harness.provider.db();
    let mgr = LeaseManager::new(db.clone());

    let guard = mgr.acquire(KEY, TTL).await.expect("acquire");

    let result: Result<i32, AckError<DummyErr>> = guard
        .with_ack_in_tx(DummyErr::db_err, |tx| {
            Box::pin(async move { insert_tracer(tx).await.map(|()| 42) })
        })
        .await;
    assert_eq!(result.expect("ok"), 42);

    // Tracer row must exist post-commit.
    let tracer = read_row(&harness.provider, TRACER_KEY).await;
    assert!(tracer.is_some(), "tracer row committed");
}

#[tokio::test]
async fn with_ack_in_tx_rolls_back_on_lease_lost() {
    let harness = setup_sqlite().await.expect("setup");
    let db = harness.provider.db();
    let mgr = LeaseManager::new(db.clone());

    let guard = mgr.acquire(KEY, TTL).await.expect("acquire");

    // Peer steals BEFORE we open the fenced tx. The fence SELECT
    // inside the tx will then find a different locked_by and trip
    // LeaseLost → rollback. (Mid-tx steal on SQLite would block on
    // the single-writer lock and cannot be tested here — covered
    // by the PG integration suite.)
    steal_to(&harness.provider, KEY, Uuid::new_v4())
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
    let tracer = read_row(&harness.provider, TRACER_KEY).await;
    assert!(
        tracer.is_none(),
        "tracer must NOT survive a LeaseLost rollback"
    );
}

#[tokio::test]
async fn with_ack_in_tx_fails_when_lease_expired() {
    let harness = setup_sqlite().await.expect("setup");
    let db = harness.provider.db();
    let mgr = LeaseManager::new(db.clone());

    let guard = mgr.acquire(KEY, TTL).await.expect("acquire");
    // Lease TTL lapses without a peer steal; the fence SELECT's
    // `locked_until > NOW()` clause must fail the ack and roll back.
    expire_lease(&harness.provider, KEY).await.expect("expire");

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
        other => panic!("expected LeaseLost on an expired lease, got {other:?}"),
    }
    assert!(
        read_row(&harness.provider, TRACER_KEY).await.is_none(),
        "tracer must NOT survive an expired-lease rollback"
    );
}

#[tokio::test]
async fn with_ack_in_tx_propagates_work_err() {
    let harness = setup_sqlite().await.expect("setup");
    let db = harness.provider.db();
    let mgr = LeaseManager::new(db.clone());

    let guard = mgr.acquire(KEY, TTL).await.expect("acquire");

    let result: Result<(), AckError<DummyErr>> = guard
        .with_ack_in_tx(DummyErr::db_err, |tx| {
            Box::pin(async move {
                // Write a tracer that should NOT survive — even
                // when the failure is on the work-side, the whole
                // tx rolls back.
                insert_tracer(tx).await?;
                Err(DummyErr::Custom("synthetic work failure".to_owned()))
            })
        })
        .await;
    match result {
        Err(AckError::Work(DummyErr::Custom(msg))) => {
            assert_eq!(msg, "synthetic work failure");
        }
        other => panic!("expected Work(Custom), got {other:?}"),
    }

    let tracer = read_row(&harness.provider, TRACER_KEY).await;
    assert!(
        tracer.is_none(),
        "tracer must NOT survive a Work-error rollback"
    );
}

// ---------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------

/// Minimal work-error type used by the `with_ack_in_tx` tests. Real
/// callers (the coordinator) plug in `TxError` / `DomainError` here;
/// these tests only need an opaque enum that can carry a synthetic
/// failure and surface a `DbErr` accessor.
#[derive(Debug)]
enum DummyErr {
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

async fn insert_lease_row(
    provider: &Arc<AmDbProvider>,
    key: &str,
    locked_by: Option<Uuid>,
    locked_until: OffsetDateTime,
    attempts: i32,
) -> anyhow::Result<()> {
    let conn = provider.conn()?;
    let am = am_leases::ActiveModel {
        key: ActiveValue::Set(key.to_owned()),
        locked_by: ActiveValue::Set(locked_by),
        locked_until: ActiveValue::Set(locked_until),
        attempts: ActiveValue::Set(attempts),
    };
    toolkit_db::secure::secure_insert::<am_leases::Entity>(am, &AccessScope::allow_all(), &conn)
        .await
        .map_err(|e| anyhow::anyhow!("insert am_leases failed: {e:?}"))?;
    Ok(())
}

/// Push `locked_until` into the past WITHOUT touching `locked_by`: the
/// holder still owns the row but its TTL has lapsed on the DB clock.
///
/// Writes via the `datetime(...)` SQL function so the stored text
/// matches the canonical format the production write path (`ttl_expr`)
/// uses — the lease TTL comparison is a same-format string compare on
/// `SQLite`, so seeding via `SeaORM`'s `OffsetDateTime` serialization
/// (offset suffix / fractional seconds) would not compare correctly
/// against `datetime('now')`.
async fn expire_lease(provider: &Arc<AmDbProvider>, key: &str) -> anyhow::Result<()> {
    let conn = provider.conn()?;
    let n = am_leases::Entity::update_many()
        .col_expr(
            am_leases::Column::LockedUntil,
            Expr::cust("datetime('now', '-3600 seconds')"),
        )
        .filter(am_leases::Column::Key.eq(key))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .map_err(|e| anyhow::anyhow!("expire failed: {e:?}"))?;
    if n.rows_affected != 1 {
        anyhow::bail!("expire_lease: expected 1 row, got {}", n.rows_affected);
    }
    Ok(())
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
