//! In-crate `SQLite` tests for the lease primitive. They live in-crate (not in
//! `tests/`) so they can read the `pub(crate)` `coord_leases` row directly to
//! assert `locked_by` / `attempts`, which the public API does not surface.
//!
//! Each test opens its own in-memory `SQLite` `Db` (a fresh, isolated namespace),
//! runs the `coord_leases` migration, then drives the real `LeaseManager` /
//! `LeaseGuard`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::{SecureEntityExt, SecureUpdateExt};
use toolkit_db::{ConnectOpts, Db, connect_db};
use toolkit_security::AccessScope;

use super::entity as coord_leases;
use super::error::CoordError;
use super::manager::LeaseManager;

/// Fresh in-memory `SQLite` `Db` with the `coord_leases` table migrated in.
async fn setup() -> Db {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect in-memory sqlite");
    run_migrations_for_testing(
        &db,
        vec![Box::new(crate::migration::Migration::unqualified())],
    )
    .await
    .expect("run coord_leases migration");
    db
}

/// Read the lease row by key (unscoped — `allow_all`, as the primitive does).
async fn read_row(db: &Db, key: &str) -> Option<coord_leases::Model> {
    let conn = db.conn().expect("conn");
    coord_leases::Entity::find()
        .filter(coord_leases::Column::Key.eq(key))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .one(&conn)
        .await
        .expect("read lease row")
}

/// Force a row's `locked_until` into the past (epoch) so the next acquire steals
/// it — deterministic, unlike sleeping past a second-precision TTL.
async fn force_expire(db: &Db, key: &str) {
    let conn = db.conn().expect("conn");
    let epoch: DateTime<Utc> = DateTime::from_timestamp(0, 0).expect("epoch");
    coord_leases::Entity::update_many()
        .col_expr(coord_leases::Column::LockedUntil, Expr::value(epoch))
        .filter(coord_leases::Column::Key.eq(key))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("force-expire");
}

/// Force a row's `locked_until` to a recent past time on the SAME UTC day, written
/// the way the free-slot INSERT writes it (a Rust `DateTime` → RFC-3339, `T`-form).
/// Unlike [`force_expire`]'s epoch — whose `1970-…` date sorts below `datetime('now')`
/// even under a buggy lexicographic TEXT compare — a same-day past time exercises the
/// `T`-vs-space format normalization.
async fn force_expire_same_day(db: &Db, key: &str) {
    let conn = db.conn().expect("conn");
    let past = Utc::now() - chrono::TimeDelta::seconds(30);
    coord_leases::Entity::update_many()
        .col_expr(coord_leases::Column::LockedUntil, Expr::value(past))
        .filter(coord_leases::Column::Key.eq(key))
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .expect("force-expire same-day");
}

/// Regression: a lease whose `locked_until` is stored in
/// RFC-3339 (`T`-separated, as the free-slot INSERT writes it) and is expired but
/// still on the CURRENT UTC day must be reclaimable. A raw lexicographic TEXT compare
/// against `datetime('now')` (space-separated) would mis-order the `T`-form and leave
/// the dead lease un-stolen until the UTC date rolls.
#[tokio::test]
async fn same_day_expired_lease_is_stolen() {
    let db = setup().await;
    let mgr = LeaseManager::new(db.clone());

    let first = mgr
        .acquire("job", Duration::from_mins(1))
        .await
        .expect("acquire");
    let first_holder = first.locked_by();
    force_expire_same_day(&db, "job").await;

    let second = mgr
        .acquire("job", Duration::from_mins(1))
        .await
        .expect("a same-day-expired lease must be stealable");
    assert_ne!(
        second.locked_by(),
        first_holder,
        "the steal installs a fresh holder"
    );
    assert_eq!(
        read_row(&db, "job").await.expect("row").attempts,
        2,
        "the steal bumps the forensic attempts streak"
    );
}

#[tokio::test]
async fn acquire_blocks_a_peer_then_release_frees_and_resets_attempts() {
    let db = setup().await;
    let mgr = LeaseManager::new(db.clone());

    let guard = mgr
        .acquire("job", Duration::from_mins(1))
        .await
        .expect("acquire free slot");
    assert_eq!(
        read_row(&db, "job").await.expect("row").locked_by,
        Some(guard.locked_by()),
        "row records the holder"
    );

    // A peer cannot acquire while the slot is live.
    let peer = mgr.acquire("job", Duration::from_mins(1)).await;
    assert!(
        matches!(peer, Err(CoordError::LeaseHeld)),
        "live slot ⇒ LeaseHeld, got {peer:?}"
    );

    // Clean release frees the slot and resets the forensic counter.
    guard.release().await.expect("release");
    let row = read_row(&db, "job").await.expect("row");
    assert_eq!(row.locked_by, None, "freed");
    assert_eq!(row.attempts, 0, "release resets attempts");

    // Freed slot is re-acquirable.
    let again = mgr
        .acquire("job", Duration::from_mins(1))
        .await
        .expect("re-acquire after release");
    assert_eq!(
        read_row(&db, "job").await.expect("row").locked_by,
        Some(again.locked_by())
    );
}

#[tokio::test]
async fn expired_lease_is_stolen_and_bumps_attempts() {
    let db = setup().await;
    let mgr = LeaseManager::new(db.clone());

    let first = mgr
        .acquire("job", Duration::from_mins(1))
        .await
        .expect("acquire");
    assert_eq!(
        read_row(&db, "job").await.unwrap().attempts,
        1,
        "INSERT seeds attempts=1"
    );
    let first_holder = first.locked_by();

    // Expire it, then a fresh acquire (any worker) steals the slot.
    force_expire(&db, "job").await;
    let stealer = mgr
        .acquire("job", Duration::from_mins(1))
        .await
        .expect("expired slot is stealable");

    let row = read_row(&db, "job").await.expect("row");
    assert_eq!(
        row.locked_by,
        Some(stealer.locked_by()),
        "new holder owns the row"
    );
    assert_ne!(stealer.locked_by(), first_holder, "a different worker id");
    assert_eq!(row.attempts, 2, "steal bumps attempts 1 -> 2");
}

#[tokio::test]
async fn release_with_retry_preserves_the_attempts_streak() {
    let db = setup().await;
    let mgr = LeaseManager::new(db.clone());

    // Drive attempts to 2 via a steal.
    let _first = mgr
        .acquire("job", Duration::from_mins(1))
        .await
        .expect("acquire");
    force_expire(&db, "job").await;
    let stealer = mgr
        .acquire("job", Duration::from_mins(1))
        .await
        .expect("steal");
    assert_eq!(read_row(&db, "job").await.unwrap().attempts, 2);

    // The recoverable-failure release frees the slot but keeps the streak.
    stealer
        .release_with_retry()
        .await
        .expect("release_with_retry");
    let row = read_row(&db, "job").await.expect("row");
    assert_eq!(row.locked_by, None, "freed");
    assert_eq!(row.attempts, 2, "release_with_retry preserves attempts");
}

#[tokio::test]
async fn renew_holds_then_reports_lost_after_a_peer_steal() {
    let db = setup().await;
    let mgr = LeaseManager::new(db.clone());

    let guard = mgr
        .acquire("job", Duration::from_mins(1))
        .await
        .expect("acquire");
    // A live lease renews fine.
    guard
        .renew(Duration::from_mins(2))
        .await
        .expect("renew a held lease");

    // A peer steals it out from under us (expire + re-acquire).
    force_expire(&db, "job").await;
    let _peer = LeaseManager::new(db.clone())
        .acquire("job", Duration::from_mins(1))
        .await
        .expect("peer steals expired lease");

    // The original holder's renew now matches zero rows ⇒ LeaseLost.
    let lost = guard.renew(Duration::from_mins(1)).await;
    assert!(
        matches!(lost, Err(CoordError::LeaseLost)),
        "stolen lease ⇒ LeaseLost, got {lost:?}"
    );
}
