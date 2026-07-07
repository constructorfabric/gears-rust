//! `LeaseGuard` — the holder-side handle for an acquired lease.
//!
//! Carries the `key` + `locked_by` UUID needed to scope every subsequent
//! operation (renew, release, fence-check) to the exact row the holder inserted
//! or stole. Construction is private to [`super::manager::LeaseManager`] —
//! callers obtain a guard only via [`super::manager::LeaseManager::acquire`].
//!
//! Three properties define the surface (unchanged from the AM original):
//!
//! 1. **Fence-in-tx.** [`LeaseGuard::with_ack_in_tx`] runs the caller's work
//!    and a `coord_leases`-row SELECT inside one `SERIALIZABLE` transaction
//!    (with retry). A peer steal between the work and the commit either aborts
//!    as `40001` and retries, or is caught by the fence SELECT as
//!    [`AckError::LeaseLost`] → rollback. The holder's writes and the lease
//!    validation cannot drift apart.
//! 2. **Renewal heartbeat with explicit lease-loss signal.**
//!    [`LeaseGuard::spawn_renewal`] drives `locked_until` forward every
//!    `period`; an UPDATE returning zero rows surfaces as [`RenewalState::Lost`]
//!    on a `watch::Receiver`, so the holder can pre-empt itself.
//! 3. **Forensic `attempts` counter.** Each steal increments it; `release`
//!    resets it, `release_with_retry` preserves it.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use toolkit_db::Db;
use toolkit_db::secure::{DbTx, ScopeError, SecureUpdateExt, TxConfig};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::entity as coord_leases;
use super::error::{AckError, CoordError};
use super::manager::{Dialect, map_scope_err, ttl_secs_i64};

/// Holder-side handle for an acquired lease. Always release explicitly — there
/// is no `Drop` impl performing async DB I/O (cannot work cleanly under tokio
/// runtime shutdown), so a guard dropped without a `release` /
/// `release_with_retry` relies on the TTL fallback to free the slot.
pub struct LeaseGuard {
    db: Db,
    key: String,
    locked_by: Uuid,
    ttl: Duration,
}

impl std::fmt::Debug for LeaseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Db` is not `Debug`; render only the identity fields.
        f.debug_struct("LeaseGuard")
            .field("key", &self.key)
            .field("locked_by", &self.locked_by)
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl LeaseGuard {
    pub(super) fn new(db: Db, key: String, locked_by: Uuid, ttl: Duration) -> Self {
        Self {
            db,
            key,
            locked_by,
            ttl,
        }
    }

    /// Lease key — the coordination domain this guard owns.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Holder UUID. Stable for the guard's lifetime; reused across `renew`,
    /// `release`, and `with_ack_in_tx` fence SELECTs so all target this row.
    #[must_use]
    pub fn locked_by(&self) -> Uuid {
        self.locked_by
    }

    /// TTL the lease was acquired with. The renewal heartbeat uses this;
    /// explicit `renew(ttl)` calls can override per invocation.
    #[must_use]
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Push `locked_until` forward by `ttl` against the DB clock.
    ///
    /// The `key + locked_by` filter scopes the UPDATE to this guard's row; zero
    /// rows affected → a peer took over (or the TTL already lapsed) and
    /// surfaces as [`CoordError::LeaseLost`]. One atomic statement; no tx.
    ///
    /// # Errors
    /// * [`CoordError::LeaseLost`] — peer stole the lease, or it had expired.
    /// * [`CoordError::Db`] — DB transport / serialisation failure.
    pub async fn renew(&self, ttl: Duration) -> Result<(), CoordError> {
        renew_once(&self.db, &self.key, self.locked_by, ttl).await
    }

    /// Release on the success path: free the slot and reset the forensic
    /// `attempts` counter to `0`. Consumes the guard.
    ///
    /// A zero-rows-affected UPDATE (lease already stolen) is logged at WARN and
    /// returned `Ok(())` — the contract is "the lease is released or was already
    /// released", not "we executed the release ourselves".
    ///
    /// # Errors
    /// * [`CoordError::Db`] — DB transport / serialisation failure.
    pub async fn release(self) -> Result<(), CoordError> {
        self.release_impl(/* reset_attempts */ true).await
    }

    /// Release on a recoverable-failure path: free the slot but **preserve** the
    /// `attempts` counter so a flapping holder stays visible as a high-water
    /// value. Consumes the guard; otherwise identical to [`Self::release`].
    ///
    /// # Errors
    /// * [`CoordError::Db`] — DB transport / serialisation failure.
    pub async fn release_with_retry(self) -> Result<(), CoordError> {
        self.release_impl(/* reset_attempts */ false).await
    }

    async fn release_impl(self, reset_attempts: bool) -> Result<(), CoordError> {
        let dialect = Dialect::from_engine(self.db.db_engine())?;
        let conn = self.db.conn().map_err(CoordError::Db)?;

        let mut update = coord_leases::Entity::update_many()
            .col_expr(coord_leases::Column::LockedBy, Expr::value(None::<Uuid>))
            .col_expr(coord_leases::Column::LockedUntil, dialect.epoch_expr());
        if reset_attempts {
            update = update.col_expr(coord_leases::Column::Attempts, Expr::value(0_i32));
        }
        let result = update
            .filter(
                coord_leases::Column::Key
                    .eq(self.key.as_str())
                    .and(coord_leases::Column::LockedBy.eq(self.locked_by)),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&conn)
            .await
            .map_err(map_scope_err)?;

        if result.rows_affected == 0 {
            tracing::warn!(
                target: "coord.lease",
                key = %self.key,
                locked_by = %self.locked_by,
                reset_attempts,
                "lease release matched zero rows; row was likely stolen before release",
            );
        }
        Ok(())
    }

    /// Run `f` inside a `SERIALIZABLE` transaction (with retry on transient
    /// contention) and append a fence UPDATE against `coord_leases` as the last
    /// DB call of the tx. Zero matched rows → [`AckError::LeaseLost`] (rollback,
    /// **not** retried).
    ///
    /// The fence is an UPDATE (a no-op self-touch of the holder's own row), not a
    /// SELECT, on purpose. A plain SELECT reads the transaction's frozen
    /// `SERIALIZABLE` snapshot: a peer steal that committed *after* the ack tx
    /// took its snapshot stays invisible, the pre-steal row still tests
    /// "mine + live", and the commit is NOT guaranteed to abort (a lone
    /// read-only rw-antidependency does not trip Postgres SSI). Writing the row
    /// instead makes the fence a ww-conflict: Postgres first-updater-wins raises
    /// `40001` at the fence when a peer already committed a steal, and the
    /// [`Dialect::live_filter_clock`] wall-clock predicate catches a
    /// lapsed-but-unstolen lease — closing the window a SELECT fence left open.
    ///
    /// `extract_work_db_err` lets the retry helper classify `Work(E)` failures:
    /// `Some(&DbErr)` for retryable contention causes a retry; anything else (or
    /// `None`) terminates the loop.
    ///
    /// Critical contract: `f` MUST be idempotent across retries (each attempt
    /// opens a fresh tx; in-memory state mutated by an earlier attempt must be
    /// reset by `f` itself before re-running).
    ///
    /// # Errors
    /// * [`AckError::LeaseLost`] — fence UPDATE found the row stolen / expired.
    /// * [`AckError::Work`] — `f` returned `Err(E)`.
    /// * [`AckError::Db`] — DB transport / serialisation / fence-SELECT failure
    ///   the retry helper did not classify as retryable, or an unsupported
    ///   backend.
    pub async fn with_ack_in_tx<F, T, E, X>(
        &self,
        extract_work_db_err: X,
        mut f: F,
    ) -> Result<T, AckError<E>>
    where
        E: Send + 'static,
        T: Send + 'static,
        X: Fn(&E) -> Option<&sea_orm::DbErr> + Send + Sync,
        F: for<'a> FnMut(&'a DbTx<'a>) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>
            + Send,
    {
        let key_owned = self.key.clone();
        let locked_by = self.locked_by;
        let dialect = match Dialect::from_engine(self.db.db_engine()) {
            Ok(d) => d,
            // `from_engine` only yields `Db`; lift it into the ack taxonomy.
            Err(CoordError::Db(db)) => return Err(AckError::Db(db)),
            Err(_) => {
                return Err(AckError::Db(toolkit_db::DbError::Sea(
                    sea_orm::DbErr::Custom(
                        "coord.lease: unexpected acquire error during fence setup".to_owned(),
                    ),
                )));
            }
        };

        self.db
            .transaction_with_retry::<T, AckError<E>, _, _>(
                TxConfig::serializable(),
                |e: &AckError<E>| e.db_err(&extract_work_db_err),
                move |tx| {
                    // FnMut body — clone `key` per attempt; call `f` OUTSIDE the
                    // async block so its returned future already holds the `tx`
                    // borrow (avoids moving `f` into the async block, which would
                    // consume it across attempts). `dialect` is `Copy`.
                    let key = key_owned.clone();
                    let user_future = f(tx);
                    Box::pin(async move {
                        let work_result = user_future.await.map_err(AckError::Work)?;
                        // Fence UPDATE — last DB call inside the tx. A no-op
                        // self-touch of the holder's row (`locked_until =
                        // locked_until`) so it is a WRITE, not a snapshot read:
                        // a peer steal committed mid-flight then forces a `40001`
                        // here via Postgres first-updater-wins (the retry helper
                        // re-runs and observes the steal → zero rows → lost),
                        // while a lone SELECT could commit against a stale
                        // snapshot. The `locked_by` clause catches a takeover and
                        // the wall-clock `live_filter_clock` catches a
                        // lapsed-but-unstolen lease. Zero rows → `LeaseLost`.
                        let fenced = coord_leases::Entity::update_many()
                            .col_expr(
                                coord_leases::Column::LockedUntil,
                                Expr::col(coord_leases::Column::LockedUntil).into(),
                            )
                            .filter(
                                coord_leases::Column::Key
                                    .eq(key.as_str())
                                    .and(coord_leases::Column::LockedBy.eq(locked_by))
                                    .and(dialect.live_filter_clock()),
                            )
                            .secure()
                            .scope_with(&AccessScope::allow_all())
                            .exec(tx)
                            .await
                            .map_err(map_fence_scope_err)?;
                        if fenced.rows_affected == 0 {
                            return Err(AckError::LeaseLost);
                        }
                        Ok(work_result)
                    })
                },
            )
            .await
    }

    /// Spawn a heartbeat task that renews the lease every `period`. Returns a
    /// [`RenewalHandle`] carrying the cancel token, a
    /// `watch::Receiver<RenewalState>` for the in-band lease-loss signal, and
    /// the join handle.
    ///
    /// Convention: `period` should be `~ttl / 3` so the lease survives one
    /// missed tick (transient DB blip) before TTL expiry. The task exits on
    /// cancellation ([`RenewalState::ShuttingDown`]) or lease loss
    /// ([`RenewalState::Lost`]). Transient renewal failures log at ERROR and
    /// continue — TTL has margin and the next tick retries.
    #[must_use]
    pub fn spawn_renewal(&self, period: Duration) -> RenewalHandle {
        let cancel = tokio_util::sync::CancellationToken::new();
        let (state_tx, state_rx) = tokio::sync::watch::channel(RenewalState::Healthy);
        let db = self.db.clone();
        let key = self.key.clone();
        let locked_by = self.locked_by;
        let ttl = self.ttl;
        let cancel_task = cancel.clone();

        let join = tokio::spawn(async move {
            let mut interval = tokio::time::interval(period);
            // If a `renew_once` runs long (slow DB), skip the missed ticks rather
            // than firing catch-up ticks back-to-back — one renewal per period is
            // enough (TTL has margin), and a burst would issue redundant UPDATEs.
            // Matches every ticker in `module.rs`.
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first tick fires immediately; consume it so the first renewal
            // happens one `period` after spawn, leaving room for the acquire
            // INSERT/UPDATE to commit before we renew the row we just touched.
            interval.tick().await;
            loop {
                tokio::select! {
                    biased;
                    () = cancel_task.cancelled() => {
                        _ = state_tx.send(RenewalState::ShuttingDown);
                        return;
                    }
                    _ = interval.tick() => {
                        match renew_once(&db, &key, locked_by, ttl).await {
                            Ok(()) => {}
                            Err(CoordError::LeaseLost) => {
                                _ = state_tx.send(RenewalState::Lost);
                                return;
                            }
                            Err(other) => {
                                tracing::error!(
                                    target: "coord.lease",
                                    key = %key,
                                    error = ?other,
                                    "lease renewal failed; retrying next tick",
                                );
                            }
                        }
                    }
                }
            }
        });

        RenewalHandle {
            cancel,
            state: state_rx,
            join: Some(join),
        }
    }
}

/// Standalone renewal — used by [`LeaseGuard::renew`] and by the heartbeat task
/// (which cannot hold a `&LeaseGuard` across `tokio::spawn`).
///
/// The UPDATE filter scopes to `(key, locked_by, locked_until > NOW())` so
/// either a peer takeover (changed `locked_by`) OR an already-expired lease
/// gets zero rows affected → [`CoordError::LeaseLost`].
async fn renew_once(db: &Db, key: &str, locked_by: Uuid, ttl: Duration) -> Result<(), CoordError> {
    let dialect = Dialect::from_engine(db.db_engine())?;
    let ttl_secs = ttl_secs_i64(ttl);
    let conn = db.conn().map_err(CoordError::Db)?;

    let result = coord_leases::Entity::update_many()
        .col_expr(
            coord_leases::Column::LockedUntil,
            dialect.ttl_expr(ttl_secs),
        )
        .filter(
            coord_leases::Column::Key
                .eq(key)
                .and(coord_leases::Column::LockedBy.eq(locked_by))
                // Require the lease still live on the DB clock: an expired row
                // is logically lost (a peer may steal it), so renewal MUST NOT
                // resurrect it. Zero rows → `LeaseLost`.
                .and(dialect.live_filter()),
        )
        .secure()
        .scope_with(&AccessScope::allow_all())
        .exec(&conn)
        .await
        .map_err(map_scope_err)?;

    if result.rows_affected == 0 {
        return Err(CoordError::LeaseLost);
    }
    Ok(())
}

/// Lift the fence-SELECT's `ScopeError` into `AckError::Db`. The `coord_leases`
/// table is unscoped, so the only realistic variant is `Db(_)`; the others
/// surface as a `Custom` `DbErr` so the retry classifier sees `None`.
fn map_fence_scope_err<E>(err: ScopeError) -> AckError<E> {
    match err {
        ScopeError::Db(db) => AckError::Db(toolkit_db::DbError::Sea(db)),
        other => AckError::Db(toolkit_db::DbError::Sea(sea_orm::DbErr::Custom(format!(
            "coord.lease: fence SELECT ScopeError: {other:?}"
        )))),
    }
}

/// Handle returned by [`LeaseGuard::spawn_renewal`].
///
/// Two shutdown paths: [`Self::shutdown`] (cooperative — cancels and awaits the
/// task's exit, so the caller observes [`RenewalState::ShuttingDown`]); or
/// dropping the handle (safety net — [`Drop`] cancels the token and the runtime
/// detaches the held `JoinHandle`; no `await` happens in `Drop`).
pub struct RenewalHandle {
    pub cancel: tokio_util::sync::CancellationToken,
    pub state: tokio::sync::watch::Receiver<RenewalState>,
    /// `Option` so [`Self::shutdown`] can `.take()` and move the handle out for
    /// awaiting — a plain `JoinHandle` field would let [`Drop`] block the
    /// partial move at any call site that awaits the join.
    join: Option<tokio::task::JoinHandle<()>>,
}

impl RenewalHandle {
    /// Cancel the heartbeat task and await its exit. Preferred when the caller
    /// wants the task to observably reach [`RenewalState::ShuttingDown`].
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(join) = self.join.take() {
            _ = join.await;
        }
    }
}

impl Drop for RenewalHandle {
    fn drop(&mut self) {
        // Safety-net cancel for early-return paths that drop the handle without
        // `shutdown`. The task sees cancellation on its next `select!` poll and
        // exits. Awaiting is not possible in `Drop`; the runtime detaches the
        // held `JoinHandle`. If the caller already ran `shutdown`, `self.join`
        // is `None` and the token is already cancelled — this is a no-op.
        self.cancel.cancel();
    }
}

/// State transitions emitted by the renewal heartbeat task.
///
/// `Healthy` is steady state; `Lost` signals an UPDATE returned zero rows (peer
/// stole the lease or the TTL lapsed) and the holder should pre-empt itself;
/// `ShuttingDown` is the cooperative-cancel exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenewalState {
    Healthy,
    Lost,
    ShuttingDown,
}
