//! `LeaseManager` — acquire path for the AM-local distributed lease.
//!
//! The acquire algorithm runs inside a `SERIALIZABLE` retry
//! transaction so two concurrent workers cannot both observe a free
//! slot and both insert; the loser surfaces as
//! [`CoordError::LeaseHeld`] (via PK unique-violation on the INSERT
//! path or zero-rows-affected on the steal-UPDATE path). The retry
//! helper handles transient `40001` aborts transparently.
//!
//! Time arithmetic anchors on the DB clock — the worker's
//! `OffsetDateTime::now_utc()` is only used as the *read-side*
//! classification of "is this row expired?". The actual steal
//! UPDATE filter uses `WHERE locked_until < NOW()` (DB-side), so a
//! worker that misreads expiry due to NTP drift simply ends up with
//! `rows_affected == 0` and returns `LeaseHeld` — a false-negative
//! on acquire, never a false-positive on steal.

use std::time::Duration;

use sea_orm::ExprTrait;
use sea_orm::sea_query::{Expr, SimpleExpr};
use sea_orm::{ActiveValue, ColumnTrait, EntityTrait, QueryFilter};
use time::OffsetDateTime;
use toolkit_db::Db;
use toolkit_db::secure::{
    ScopeError, SecureEntityExt, SecureUpdateExt, TxConfig, is_unique_violation, secure_insert,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::infra::storage::entity::am_leases;

use super::error::CoordError;
use super::guard::LeaseGuard;

/// Acquire-side entry point for the AM-local lease primitive. Cheap
/// to construct (clones an `Arc<DbHandle>` internally via `Db`) and
/// safe to keep behind an `Arc` shared across reconciler ticks.
#[derive(Clone)]
pub struct LeaseManager {
    db: Db,
}

impl LeaseManager {
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Try to acquire the lease keyed by `key` with the given `ttl`.
    ///
    /// On success returns a [`LeaseGuard`] bound to a fresh
    /// `locked_by` UUID; on a peer already holding the slot returns
    /// [`CoordError::LeaseHeld`]. Transient `40001` failures are
    /// retried by the underlying [`toolkit_db::Db::transaction_with_retry`]
    /// helper; persistent DB failures surface via
    /// [`CoordError::Db`].
    ///
    /// # Errors
    ///
    /// * [`CoordError::LeaseHeld`] — peer holds the slot (live row,
    ///   non-expired `locked_until`).
    /// * [`CoordError::Db`] — any DB failure not covered by the
    ///   retry helper's contention classification.
    pub async fn acquire(&self, key: &str, ttl: Duration) -> Result<LeaseGuard, CoordError> {
        let my_uuid = Uuid::new_v4();
        let ttl_secs = ttl_secs_i64(ttl);
        let engine = self.db.db_engine();
        let key_owned = key.to_owned();

        self.db
            .transaction_with_retry::<(), CoordError, _, _>(
                TxConfig::serializable(),
                CoordError::db_err,
                move |tx| {
                    // FnMut body — clone the captured key on every
                    // attempt so a retried iteration owns a fresh
                    // String. `my_uuid` is `Copy`.
                    let key = key_owned.clone();
                    Box::pin(async move {
                        let existing = am_leases::Entity::find()
                            .filter(am_leases::Column::Key.eq(key.as_str()))
                            .secure()
                            .scope_with(&AccessScope::allow_all())
                            .one(tx)
                            .await
                            .map_err(map_scope_err)?;

                        match existing {
                            None => {
                                // Free slot — INSERT.
                                //
                                // `locked_until` is written worker-clock here
                                // (see gear docs); the steal-path filter is
                                // DB-clock, so drift can only shorten our own
                                // lease, never extend it past the DB's view.
                                let am = am_leases::ActiveModel {
                                    key: ActiveValue::Set(key.clone()),
                                    locked_by: ActiveValue::Set(Some(my_uuid)),
                                    locked_until: ActiveValue::Set(
                                        OffsetDateTime::now_utc() + ttl,
                                    ),
                                    attempts: ActiveValue::Set(1),
                                };
                                match secure_insert::<am_leases::Entity>(
                                    am,
                                    &AccessScope::allow_all(),
                                    tx,
                                )
                                .await
                                {
                                    Ok(_) => Ok(()),
                                    Err(ScopeError::Db(db)) if is_unique_violation(&db) => {
                                        // A peer raced us between our SELECT and
                                        // our INSERT and committed first. Their
                                        // row is now live; surface as `LeaseHeld`.
                                        Err(CoordError::LeaseHeld)
                                    }
                                    Err(err) => Err(map_scope_err(err)),
                                }
                            }
                            Some(row) if row.locked_until <= OffsetDateTime::now_utc() => {
                                // Read side says expired; the UPDATE re-checks
                                // DB-side via `locked_until < NOW()`. If the row
                                // is in fact still live (drift), the UPDATE
                                // returns zero rows and we return `LeaseHeld`.
                                let result = am_leases::Entity::update_many()
                                    .col_expr(
                                        am_leases::Column::LockedBy,
                                        Expr::value(my_uuid),
                                    )
                                    .col_expr(
                                        am_leases::Column::LockedUntil,
                                        ttl_expr(engine, ttl_secs),
                                    )
                                    .col_expr(
                                        am_leases::Column::Attempts,
                                        Expr::col(am_leases::Column::Attempts).add(1),
                                    )
                                    .filter(
                                        am_leases::Column::Key.eq(key.as_str()).and(
                                            Expr::col(am_leases::Column::LockedUntil)
                                                .lt(now_expr(engine)),
                                        ),
                                    )
                                    .secure()
                                    .scope_with(&AccessScope::allow_all())
                                    .exec(tx)
                                    .await
                                    .map_err(map_scope_err)?;
                                if result.rows_affected == 0 {
                                    // Defensive belt: under PG SERIALIZABLE a
                                    // concurrent steal would normally surface as
                                    // 40001 at commit (and retry), so this branch
                                    // is reached only when the row's
                                    // `locked_until` advanced between our SELECT
                                    // and UPDATE for unrelated reasons. Under
                                    // SQLite BEGIN IMMEDIATE serialises writers
                                    // and this branch is unreachable. Either
                                    // way: surface as `LeaseHeld` so the caller
                                    // backs off instead of looping silently.
                                    tracing::warn!(
                                        target: "am.lease",
                                        key = %key,
                                        "lease steal-UPDATE matched zero rows after read-side classified expired; treating as held"
                                    );
                                    return Err(CoordError::LeaseHeld);
                                }
                                Ok(())
                            }
                            Some(_) => Err(CoordError::LeaseHeld),
                        }
                    })
                },
            )
            .await?;

        Ok(LeaseGuard::new(
            self.db.clone(),
            key.to_owned(),
            my_uuid,
            ttl,
        ))
    }
}

/// SQL expression for "DB-side now" per dialect.
///
/// Used by the steal-path filter (`WHERE locked_until < NOW()`) and
/// by the renewal UPDATE in [`super::guard::LeaseGuard::renew`].
/// Mirrors the existing `integrity::lock::build_db_cutoff_expr`
/// pattern at [`crate::infra::storage::integrity::lock`].
pub(super) fn now_expr(engine: &str) -> SimpleExpr {
    match engine {
        "postgres" => Expr::cust("NOW()"),
        "sqlite" => Expr::cust("datetime('now')"),
        other => panic!("am.lease: unsupported db_engine for now_expr: {other}"),
    }
}

/// SQL expression for "DB-side now + `ttl_secs`" per dialect.
///
/// Used by acquire (steal path), renew, and any future fenced-write
/// that needs to bump `locked_until` while staying anchored on the
/// DB clock.
pub(super) fn ttl_expr(engine: &str, ttl_secs: i64) -> SimpleExpr {
    match engine {
        "postgres" => Expr::cust(format!("NOW() + INTERVAL '{ttl_secs} seconds'")),
        "sqlite" => Expr::cust(format!("datetime('now', '+{ttl_secs} seconds')")),
        other => panic!("am.lease: unsupported db_engine for ttl_expr: {other}"),
    }
}

/// SQL expression for the epoch sentinel per dialect — used by
/// `release` to mark a row as free without deleting it (preserves
/// the `attempts` counter as a forensic streak across the row's
/// lifetime).
pub(super) fn epoch_expr(engine: &str) -> SimpleExpr {
    match engine {
        "postgres" => Expr::cust("TIMESTAMP 'epoch'"),
        "sqlite" => Expr::cust("'1970-01-01 00:00:00+00:00'"),
        other => panic!("am.lease: unsupported db_engine for epoch_expr: {other}"),
    }
}

/// Lift a [`ScopeError`] surfaced from a secure-extension call into
/// [`CoordError::Db`]. `ScopeError::Db` carries the raw `DbErr`
/// straight through; the scope-shape variants are unexpected on the
/// `am_leases` table (no tenant column) and surface as `Db` failures
/// with a synthetic `Custom` `DbErr` so the retry classifier sees
/// `None` and propagates.
pub(super) fn map_scope_err(err: ScopeError) -> CoordError {
    match err {
        ScopeError::Db(db) => CoordError::Db(toolkit_db::DbError::Sea(db)),
        other => CoordError::Db(toolkit_db::DbError::Sea(sea_orm::DbErr::Custom(format!(
            "am.lease: unexpected ScopeError on no-scope am_leases table: {other:?}"
        )))),
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
pub(super) fn ttl_secs_i64(ttl: Duration) -> i64 {
    // `as_secs() -> u64`; coordinator-side TTLs are minutes (15 min
    // by default), well within `i64::MAX`. Saturate as a defensive
    // floor against pathological inputs.
    // `Ord::min` spelled explicitly: `ExprTrait` (in scope for the SQL builders
    // in this module) also has a `min`, which makes the method call ambiguous.
    Ord::min(ttl.as_secs(), i64::MAX as u64) as i64
}
