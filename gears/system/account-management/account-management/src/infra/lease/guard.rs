//! `LeaseGuard` — holder-side handle for an acquired lease.
//!
//! Carries the `key` + `locked_by` UUID needed to scope every
//! subsequent operation (renew, release, fence-check) to the exact
//! row the holder inserted or stole. Construction is private to the
//! [`super::manager::LeaseManager`] — callers obtain a guard only
//! via [`super::manager::LeaseManager::acquire`].
//!
//! Renewal heartbeat and `with_ack_in_tx` are added in subsequent
//! phases; this file ships the skeleton + the explicit accessors
//! Phase 2a needs to compile.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use sea_orm::ExprTrait;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use toolkit_db::Db;
use toolkit_db::secure::{DbTx, ScopeError, SecureEntityExt, SecureUpdateExt, TxConfig};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::infra::storage::entity::am_leases;

use super::error::{AckError, CoordError};
use super::manager::{epoch_expr, map_scope_err, now_expr, ttl_expr, ttl_secs_i64};

/// Holder-side handle for an acquired lease. Always release
/// explicitly — there is no `Drop` impl that performs async DB I/O
/// (cannot work cleanly under tokio runtime shutdown), so a guard
/// that goes out of scope without a `release` / `release_with_retry`
/// call relies on the TTL fallback to free the slot.
pub struct LeaseGuard {
    db: Db,
    key: String,
    locked_by: Uuid,
    ttl: Duration,
}

impl std::fmt::Debug for LeaseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Db` is not `Debug`; omit it from the rendering and show
        // only the identity fields tests need to assert on.
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

    /// Lease key — the coordination domain. Today only
    /// `"hierarchy_integrity"` is used.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Holder UUID. Stable for the lifetime of this guard; reused
    /// across `renew`, `release`, and `with_ack_in_tx` fence
    /// SELECTs so all of them target the row this guard owns.
    #[must_use]
    pub fn locked_by(&self) -> Uuid {
        self.locked_by
    }

    /// Configured TTL the lease was acquired with. The renewal
    /// heartbeat uses this; explicit `renew(ttl)` calls can override
    /// per invocation.
    #[must_use]
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Push `locked_until` forward by `ttl` against the DB clock.
    ///
    /// `key + locked_by` filter scopes the UPDATE to this guard's
    /// row; zero rows affected → a peer took over the lease and
    /// surfaces as [`CoordError::LeaseLost`]. Renewal is one atomic
    /// statement; no transaction needed.
    ///
    /// # Errors
    ///
    /// * [`CoordError::LeaseLost`] — peer stole the lease.
    /// * [`CoordError::Db`] — DB transport / serialisation failure.
    pub async fn renew(&self, ttl: Duration) -> Result<(), CoordError> {
        renew_once(&self.db, &self.key, self.locked_by, ttl).await
    }

    /// Release the lease on the success path: free the slot and
    /// reset the forensic `attempts` counter to `0`.
    ///
    /// Consumes the guard so it cannot be reused after release. A
    /// zero-rows-affected UPDATE (lease was already stolen) is
    /// logged at WARN and returned as `Ok(())` — the holder's
    /// contract is "the lease is released or was already released",
    /// not "we executed the release ourselves".
    ///
    /// # Errors
    ///
    /// * [`CoordError::Db`] — DB transport / serialisation failure.
    ///   `LeaseLost` is **not** returned here; see the WARN-log path.
    pub async fn release(self) -> Result<(), CoordError> {
        self.release_impl(/* reset_attempts */ true).await
    }

    /// Release the lease on a recoverable-failure path: free the
    /// slot but **preserve** the `attempts` counter so a flapping
    /// holder is visible as a high-water value to operators.
    ///
    /// Consumes the guard; otherwise identical to [`Self::release`].
    ///
    /// # Errors
    ///
    /// * [`CoordError::Db`] — DB transport / serialisation failure.
    pub async fn release_with_retry(self) -> Result<(), CoordError> {
        self.release_impl(/* reset_attempts */ false).await
    }

    async fn release_impl(self, reset_attempts: bool) -> Result<(), CoordError> {
        let engine = self.db.db_engine();
        let conn = self.db.conn().map_err(CoordError::Db)?;

        let mut update = am_leases::Entity::update_many()
            .col_expr(am_leases::Column::LockedBy, Expr::value(None::<Uuid>))
            .col_expr(am_leases::Column::LockedUntil, epoch_expr(engine));
        if reset_attempts {
            update = update.col_expr(am_leases::Column::Attempts, Expr::value(0_i32));
        }
        let result = update
            .filter(
                am_leases::Column::Key
                    .eq(self.key.as_str())
                    .and(am_leases::Column::LockedBy.eq(self.locked_by)),
            )
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&conn)
            .await
            .map_err(map_scope_err)?;

        if result.rows_affected == 0 {
            // Lease was already stolen before this release ran. The
            // peer's acquire incremented attempts and reset
            // locked_until; nothing we can do here is correct
            // beyond observing the anomaly. Matches lock.rs:194-199.
            tracing::warn!(
                target: "am.lease",
                key = %self.key,
                locked_by = %self.locked_by,
                reset_attempts,
                "lease release matched zero rows; row was likely stolen before release",
            );
        }
        Ok(())
    }
}

/// Standalone renewal — used both by [`LeaseGuard::renew`] and by
/// the heartbeat task spawned by [`LeaseGuard::spawn_renewal`]
/// (which cannot hold a `&LeaseGuard` across `tokio::spawn` because
/// the guard is owned by the reconciler on the caller side).
///
/// The UPDATE filter scopes to `(key, locked_by, locked_until > NOW())`
/// so either a peer that took over (changed `locked_by`) OR a lease
/// that already expired on the DB clock gets zero rows affected →
/// [`CoordError::LeaseLost`].
async fn renew_once(db: &Db, key: &str, locked_by: Uuid, ttl: Duration) -> Result<(), CoordError> {
    let engine = db.db_engine();
    let ttl_secs = ttl_secs_i64(ttl);
    let conn = db.conn().map_err(CoordError::Db)?;

    let result = am_leases::Entity::update_many()
        .col_expr(am_leases::Column::LockedUntil, ttl_expr(engine, ttl_secs))
        .filter(
            am_leases::Column::Key
                .eq(key)
                .and(am_leases::Column::LockedBy.eq(locked_by))
                // Require the lease still live on the DB clock: an
                // already-expired row is logically lost (a peer is
                // entitled to steal it), so renewal MUST NOT
                // resurrect it. Zero rows → `LeaseLost` below.
                .and(Expr::col(am_leases::Column::LockedUntil).gt(now_expr(engine))),
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

impl LeaseGuard {
    /// Run `f` inside a `SERIALIZABLE` transaction (with retry on
    /// transient contention) and append a fence SELECT against
    /// `am_leases` as the last DB call of the tx. Zero matched rows
    /// → [`AckError::LeaseLost`] (rollback, **not** retried).
    ///
    /// `extract_work_db_err` lets the underlying retry helper
    /// classify `Work(E)` failures: returning `Some(&DbErr)` for
    /// retryable contention causes a retry, anything else (or
    /// `None`) terminates the loop. This preserves the
    /// SERIALIZABLE-retry coverage today provided by
    /// `with_serializable_retry`, which the reconciler's repair
    /// path relies on against saga contention.
    ///
    /// Critical contract: `f` MUST be idempotent across retries
    /// (each attempt opens a fresh tx; in-memory state mutated by
    /// an earlier attempt must be reset by `f` itself before
    /// re-running). The reconciler's repair body satisfies this by
    /// re-loading its snapshot and re-planning inside each attempt
    /// — the same shape already used by `with_serializable_retry`
    /// callers.
    ///
    /// # Errors
    ///
    /// * [`AckError::LeaseLost`] — fence SELECT found the row
    ///   stolen.
    /// * [`AckError::Work`] — `f` returned `Err(E)`.
    /// * [`AckError::Db`] — DB transport / serialisation /
    ///   fence-SELECT failure that the retry helper did not
    ///   classify as retryable.
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
        let engine = self.db.db_engine();

        self.db
            .transaction_with_retry::<T, AckError<E>, _, _>(
                TxConfig::serializable(),
                |e: &AckError<E>| e.db_err(&extract_work_db_err),
                move |tx| {
                    // FnMut body — clone `key` per attempt; call `f`
                    // outside the async block so its returned future
                    // already holds the `tx` borrow (avoids moving
                    // `f` into the async block, which would consume
                    // it across attempts).
                    let key = key_owned.clone();
                    let user_future = f(tx);
                    Box::pin(async move {
                        let work_result = user_future.await.map_err(AckError::Work)?;
                        // Fence SELECT — last DB call inside the tx.
                        // A peer steal that committed between work
                        // and commit normally surfaces as 40001 here
                        // (read-set conflict on the lease row) and
                        // retries; the explicit zero-rows check
                        // covers the case where the steal already
                        // committed BEFORE this tx began. The
                        // `locked_until > NOW()` clause additionally
                        // fences an expired-but-unstolen lease: a
                        // lapsed TTL is logically lost even if no peer
                        // has taken over yet.
                        let still_mine = am_leases::Entity::find()
                            .filter(
                                am_leases::Column::Key
                                    .eq(key.as_str())
                                    .and(am_leases::Column::LockedBy.eq(locked_by))
                                    .and(
                                        Expr::col(am_leases::Column::LockedUntil)
                                            .gt(now_expr(engine)),
                                    ),
                            )
                            .secure()
                            .scope_with(&AccessScope::allow_all())
                            .one(tx)
                            .await
                            .map_err(map_fence_scope_err)?;
                        if still_mine.is_none() {
                            return Err(AckError::LeaseLost);
                        }
                        Ok(work_result)
                    })
                },
            )
            .await
    }

    /// Spawn a heartbeat task that renews the lease every `period`.
    /// Returns a [`RenewalHandle`] carrying the cancel token, a
    /// `watch::Receiver<RenewalState>` for in-band lease-loss
    /// signal, and the join handle.
    ///
    /// Convention: `period` should be `~ttl / 3` so the lease
    /// survives one missed tick (transient DB blip) before TTL
    /// expiry. Caller picks the numbers.
    ///
    /// The task exits on:
    /// * cancellation → emits [`RenewalState::ShuttingDown`].
    /// * lease loss (peer stole it) → emits [`RenewalState::Lost`].
    ///
    /// Transient renewal failures (DB transport blip) log at ERROR
    /// and continue — TTL has built-in margin and the next tick
    /// retries; surfacing each blip as `Lost` would cause spurious
    /// reconciler aborts.
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
            // The first tick of `tokio::time::interval` fires
            // immediately; consume it so the first renewal happens
            // one `period` after spawn, leaving room for the
            // acquire-side INSERT/UPDATE to fully commit before we
            // try to renew the row we just touched.
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
                                    target: "am.lease",
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

/// Lift the fence-SELECT's `ScopeError` into `AckError::Db`. The
/// `am_leases` table is `no_tenant, no_resource, no_owner, no_type`
/// so the only realistic variant is `Db(_)`; the others surface as
/// a `Custom` `DbErr` carrying the variant text so the retry
/// classifier sees `None` and propagates immediately.
fn map_fence_scope_err<E>(err: ScopeError) -> AckError<E> {
    match err {
        ScopeError::Db(db) => AckError::Db(toolkit_db::DbError::Sea(db)),
        other => AckError::Db(toolkit_db::DbError::Sea(sea_orm::DbErr::Custom(format!(
            "am.lease: fence SELECT ScopeError: {other:?}"
        )))),
    }
}

/// Handle returned by [`LeaseGuard::spawn_renewal`].
///
/// Two shutdown paths:
///
/// * [`Self::shutdown`] — cooperative: cancels the task and awaits
///   its exit. Use this when you want to observe the final
///   [`RenewalState::ShuttingDown`] before continuing.
/// * Dropping the handle — safety net for early-return paths. The
///   [`Drop`] impl cancels the cancellation token; the runtime
///   detaches the held `JoinHandle` and the task drives itself to
///   exit. No `await` happens in `Drop`.
pub struct RenewalHandle {
    pub cancel: tokio_util::sync::CancellationToken,
    pub state: tokio::sync::watch::Receiver<RenewalState>,
    /// `Option` so [`Self::shutdown`] can `.take()` the handle and
    /// move it out for awaiting. With a plain `JoinHandle` field,
    /// the [`Drop`] impl on this struct would prevent partial-move
    /// of the field — a compile-time block on any call site that
    /// awaits the join.
    join: Option<tokio::task::JoinHandle<()>>,
}

impl RenewalHandle {
    /// Cancel the heartbeat task and await its exit. Preferred
    /// shutdown path when the caller wants the task to observably
    /// reach [`RenewalState::ShuttingDown`] before proceeding.
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(join) = self.join.take() {
            _ = join.await;
        }
    }
}

impl Drop for RenewalHandle {
    fn drop(&mut self) {
        // Safety-net cancel for early-return paths that drop the
        // handle without going through `shutdown`. The task sees
        // the cancellation on its next `select!` poll and exits
        // via the `RenewalState::ShuttingDown` arm. Awaiting is
        // not possible inside `Drop` — the runtime detaches the
        // held `JoinHandle` and the task completes asynchronously.
        // If the caller already went through `shutdown`,
        // `self.join` is `None` and `self.cancel` is already
        // cancelled — this drop is a no-op.
        self.cancel.cancel();
    }
}

/// State transitions emitted by the renewal heartbeat task.
///
/// `Healthy` is the steady state; `Lost` signals that a UPDATE
/// returned zero rows (peer stole the lease) and the holder should
/// pre-empt itself; `ShuttingDown` is the cooperative-cancel exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenewalState {
    Healthy,
    Lost,
    ShuttingDown,
}
