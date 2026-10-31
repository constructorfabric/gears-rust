// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-mutation-lock:p1
//! The key's mutation lock over `toolkit-db`'s advisory locks.
//!
//! Every value change, restriction change, registration and retire of a key
//! takes this lock: a named advisory lock keyed on the gear and the setting key
//! (`pg_try_advisory_lock` on `PostgreSQL`, a file marker on `SQLite`), so one
//! key has one lock across every replica and two keys never share one. The
//! waiter polls through `try_lock` on a schedule of its own — a first wait of
//! tens of milliseconds, doubling, capped at one second, jittered — within a
//! budget, and a wait that runs out is `Unavailable` with nothing done.
//!
//! The guarantee rests on the lock, not on the isolation level, and holds only
//! while the settings database is reached through session pooling or a direct
//! connection: the lock is session-level, and a transaction-pooling proxy
//! voids it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use toolkit_db::{DBProvider, DbError, LockConfig};

use crate::domain::activation::{HeldKey, KeyLocks, LockMetrics, ScanLock};
use crate::domain::error::DomainError;
use crate::log_text::LogSafe;

/// The gear name the locks are namespaced under.
pub const LOCK_GEAR: &str = "settings-service";

/// The name of the delivery loop's scan lock, which the SQLite start-up
/// removes the stale marker of along with the keys'.
pub const SCAN_LOCK_KEY: &str = "activation-delivery-scan";

/// The retry schedule of one acquisition: `delay(n) = factor * base^n`
/// milliseconds up to `cap`, jittered. The defaults are 2 and 25 — 50 ms,
/// 100 ms, 200 ms, and so on, doubling — and a cap of one second. Injectable,
/// so a test makes the lock's polls short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockSchedule {
    /// The growth ratio between delays.
    pub base_ms: u64,
    /// The factor on every delay.
    pub factor: u64,
    /// The longest single delay.
    pub cap: Duration,
    /// Whether each delay is jittered.
    pub jitter: bool,
}

impl Default for LockSchedule {
    fn default() -> Self {
        Self {
            base_ms: 2,
            factor: 25,
            cap: Duration::from_secs(1),
            jitter: true,
        }
    }
}

/// The lock over a database provider.
pub struct MutationLock {
    db: Arc<DBProvider<DbError>>,
    budget: Duration,
    schedule: LockSchedule,
    metrics: Arc<dyn LockMetrics>,
}

impl MutationLock {
    /// A lock whose waits give up after `budget`
    /// (`mutation_lock_wait_seconds`).
    #[must_use]
    pub fn new(
        db: Arc<DBProvider<DbError>>,
        budget: Duration,
        metrics: Arc<dyn LockMetrics>,
    ) -> Self {
        Self {
            db,
            budget,
            schedule: LockSchedule::default(),
            metrics,
        }
    }

    /// The same lock on another retry schedule.
    #[must_use]
    pub fn with_schedule(mut self, schedule: LockSchedule) -> Self {
        self.schedule = schedule;
        self
    }

    /// The wait one acquisition is allowed: the budget, further capped by the
    /// remainder of the request deadline in a batch.
    #[must_use]
    pub fn wait_for(&self, cap: Option<Duration>) -> Duration {
        cap.map_or(self.budget, |cap| cap.min(self.budget))
    }

    /// The `toolkit-db` configuration of one acquisition.
    #[must_use]
    pub fn config(&self, cap: Option<Duration>) -> LockConfig {
        LockConfig {
            max_wait: Some(self.wait_for(cap)),
            max_retries: None,
            backoff_base_ms: self.schedule.base_ms,
            backoff_factor: self.schedule.factor,
            max_backoff: self.schedule.cap,
            jitter: self.schedule.jitter,
        }
    }
}

/// What releases a held lock; the production binding is `toolkit-db`'s guard.
#[async_trait]
trait Releaser: Send {
    async fn release(self: Box<Self>) -> Result<(), String>;
}

#[async_trait]
impl Releaser for toolkit_db::DbLockGuard {
    async fn release(self: Box<Self>) -> Result<(), String> {
        (*self).release().await.map_err(|e| e.to_string())
    }
}

/// A key's lock, held.
struct Held {
    key: String,
    guard: Box<dyn Releaser>,
    metrics: Arc<dyn LockMetrics>,
}

#[async_trait]
impl HeldKey for Held {
    async fn release(self: Box<Self>) {
        let Self {
            key,
            guard,
            metrics,
        } = *self;
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-8
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-9
        // An unlock that fails (`NotHeld`, a database error) is logged and
        // counted, never raised: the mutation it surrounded has its outcome.
        // A lock session lost while the lock was held is not observable here:
        // `toolkit-db` treats it as already released and returns `Ok`.
        if let Err(err) = guard.release().await {
            tracing::warn!(
                %key,
                err = %LogSafe(&err),
                "the mutation lock of a setting was not released cleanly"
            );
            metrics.error();
        }
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-9
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-8
    }
}

#[async_trait]
impl KeyLocks for MutationLock {
    async fn acquire(
        &self,
        key: &str,
        cap: Option<Duration>,
    ) -> Result<Box<dyn HeldKey>, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-1
        // `LOCK_GEAR` and the key: one key has one lock across every replica,
        // and two keys never share one.
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-1
        let started = Instant::now();
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-3
        let attempt = self
            .db
            .db()
            .try_lock(LOCK_GEAR, key, self.config(cap))
            .await;
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-3
        match attempt {
            Ok(Some(guard)) => {
                self.metrics.acquired(started.elapsed());
                Ok(Box::new(Held {
                    key: key.to_owned(),
                    guard: Box::new(guard),
                    metrics: Arc::clone(&self.metrics),
                }))
            }
            Ok(None) => {
                self.metrics.exhausted(started.elapsed());
                // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-11
                Err(DomainError::Unavailable {
                    detail: "the setting is being changed by another request; retry".to_owned(),
                })
                // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-11
            }
            Err(err) => {
                self.metrics.error();
                Err(DomainError::dependency_unavailable(
                    "lock service",
                    "take the setting's mutation lock",
                    err,
                ))
            }
        }
    }
}

/// The delivery loop's scan lock: the same facility as the key's lock, named
/// [`SCAN_LOCK_KEY`], attempted exactly once and never waited for — a replica
/// that does not get it skips the tick, so exactly one replica scans per tick.
pub struct DeliveryScanLock {
    db: Arc<DBProvider<DbError>>,
    key: String,
}

impl DeliveryScanLock {
    /// Over this database, under the name every replica uses.
    #[must_use]
    pub fn new(db: Arc<DBProvider<DbError>>) -> Self {
        Self::named(db, SCAN_LOCK_KEY)
    }

    /// Under another name: what a test uses so that its lock is its own on the
    /// file-marker backend, whose markers every test process shares.
    #[must_use]
    pub fn named(db: Arc<DBProvider<DbError>>, key: impl Into<String>) -> Self {
        Self {
            db,
            key: key.into(),
        }
    }

    /// The `toolkit-db` configuration of the one attempt: no retry, no wait.
    #[must_use]
    pub fn config() -> LockConfig {
        LockConfig {
            max_wait: None,
            max_retries: Some(0),
            ..LockConfig::default()
        }
    }
}

#[async_trait]
impl ScanLock for DeliveryScanLock {
    async fn try_acquire(&self) -> Result<Option<Box<dyn HeldKey>>, DomainError> {
        let attempt = self
            .db
            .db()
            .try_lock(LOCK_GEAR, &self.key, Self::config())
            .await
            .map_err(|err| {
                DomainError::dependency_unavailable("lock service", "attempt the scan lock", err)
            })?;
        Ok(attempt.map(|guard| {
            Box::new(Held {
                key: self.key.clone(),
                guard: Box::new(guard) as Box<dyn Releaser>,
                metrics: Arc::new(crate::domain::activation::NoLockMetrics) as Arc<dyn LockMetrics>,
            }) as Box<dyn HeldKey>
        }))
    }
}

/// At gear init on the single-process `SQLite` profile, remove the lock markers
/// a crash may have left, for the scan lock and for the mutation lock of every
/// declared key; in every other profile remove none — `PostgreSQL` drops a dead
/// session's locks itself, and a marker removed there would be a live
/// holder's. Returns how many markers were removed.
///
/// # Errors
/// [`DomainError`] when a marker exists but cannot be removed.
pub async fn clear_startup_markers(
    db: &DBProvider<DbError>,
    keys: impl IntoIterator<Item = String>,
) -> Result<usize, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-4
    if db.db().backend() != sea_orm::DbBackend::Sqlite {
        return Ok(0);
    }
    let mut removed = 0;
    let scan = std::iter::once(SCAN_LOCK_KEY.to_owned());
    for key in scan.chain(keys) {
        let gone = db
            .db()
            .remove_lock_marker_at_startup(LOCK_GEAR, &key)
            .await
            .map_err(|e| {
                DomainError::dependency_unavailable("lock service", "remove a stale lock marker", e)
            })?;
        if gone {
            removed += 1;
        }
    }
    Ok(removed)
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-4
}

#[cfg(test)]
#[path = "mutation_lock_tests.rs"]
mod mutation_lock_tests;
