//! In-memory copy of the three configuration tables, reloaded per refresh interval.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use arc_swap::ArcSwapOption;
use quota_enforcement_sdk::{StorageError, TenantId};
use time::OffsetDateTime;
use tokio::sync::Mutex;
use toolkit_db::Db;
use toolkit_db::secure::{DBRunner, ScopeError};

use crate::infra::storage::entity::DEFAULT_KEY;
use crate::infra::storage::repo::config_repo;

const LOG_TARGET: &str = "qe.storage";

/// How long a snapshot serves before the next read reloads it.
pub const DEFAULT_REFRESH: Duration = Duration::from_secs(30);
/// The active-lease cap when no row applies (I7).
const DEFAULT_LEASE_CAP: u64 = 1000;
/// The idempotency retention when no row applies: 24 hours (PRD 5.8).
const DEFAULT_RETENTION_SECS: i64 = 86_400;

/// A table keyed by tenant, then metric.
type PairTable<T> = HashMap<String, HashMap<String, T>>;

/// The configuration rows as of one read.
#[derive(Debug, Default)]
pub struct ConfigSnapshot {
    contention_timeout: HashMap<String, i64>,
    lease_capacity: PairTable<i32>,
    idempotency_retention: PairTable<i64>,
}

impl ConfigSnapshot {
    /// Read every row of the three tables.
    ///
    /// # Errors
    ///
    /// Returns the database error of the read.
    pub async fn load(runner: &impl DBRunner) -> Result<Self, ScopeError> {
        let mut snapshot = Self::default();
        for row in config_repo::read_all_contention_timeouts(runner).await? {
            snapshot
                .contention_timeout
                .insert(row.metric_key, row.timeout_ms);
        }
        for row in config_repo::read_all_lease_capacities(runner).await? {
            snapshot
                .lease_capacity
                .entry(row.tenant_key)
                .or_default()
                .insert(row.metric_key, row.max_active_leases);
        }
        for row in config_repo::read_all_idempotency_retentions(runner).await? {
            snapshot
                .idempotency_retention
                .entry(row.tenant_key)
                .or_default()
                .insert(row.metric_key, row.retention_seconds);
        }
        Ok(snapshot)
    }

    /// The contention budget of `metric`: its row, else `*`, else fail-fast (I8).
    #[must_use]
    pub fn contention_timeout(&self, metric: &str) -> Duration {
        let millis = [metric, DEFAULT_KEY]
            .into_iter()
            .find_map(|key| self.contention_timeout.get(key).copied())
            .unwrap_or(0);
        Duration::from_millis(u64::try_from(millis).unwrap_or(0))
    }

    /// The strictest contention budget of `metrics`, else that of `*` (I8).
    #[must_use]
    pub fn strictest_contention_timeout<'a>(
        &self,
        metrics: impl IntoIterator<Item = &'a str>,
    ) -> Duration {
        metrics
            .into_iter()
            .map(|metric| self.contention_timeout(metric))
            .min()
            .unwrap_or_else(|| self.contention_timeout(DEFAULT_KEY))
    }

    /// The active-lease cap of `(tenant, metric)` (I7).
    #[must_use]
    pub fn lease_cap(&self, tenant: TenantId, metric: &str) -> u64 {
        most_specific(&self.lease_capacity, tenant, metric).map_or(DEFAULT_LEASE_CAP, |cap| {
            u64::try_from(cap).unwrap_or(DEFAULT_LEASE_CAP)
        })
    }

    /// How long a record of `(tenant, metric)` is kept for replay.
    #[must_use]
    pub fn idempotency_retention(&self, tenant: TenantId, metric: &str) -> time::Duration {
        let seconds = most_specific(&self.idempotency_retention, tenant, metric)
            .unwrap_or(DEFAULT_RETENTION_SECS);
        time::Duration::seconds(seconds.max(0))
    }

    /// The longest idempotency retention of `pairs`, zero when there is none.
    #[must_use]
    pub fn longest_retention<'a>(
        &self,
        pairs: impl IntoIterator<Item = (TenantId, &'a str)>,
    ) -> time::Duration {
        pairs
            .into_iter()
            .map(|(tenant, metric)| self.idempotency_retention(tenant, metric))
            .max()
            .unwrap_or(time::Duration::ZERO)
    }
}

/// The most specific configured row of `(t, m)`, `(t, *)`, `(*, m)`, `(*, *)`.
fn most_specific<T: Copy>(table: &PairTable<T>, tenant: TenantId, metric: &str) -> Option<T> {
    let tenant = tenant.as_uuid().to_string();
    let tenant = tenant.as_str();
    [
        (tenant, metric),
        (tenant, DEFAULT_KEY),
        (DEFAULT_KEY, metric),
        (DEFAULT_KEY, DEFAULT_KEY),
    ]
    .into_iter()
    .find_map(|(tenant, metric)| table.get(tenant)?.get(metric).copied())
}

/// A published snapshot and when it was last loaded and last checked.
struct Published {
    snapshot: Arc<ConfigSnapshot>,
    /// The last successful load.
    loaded_at: OffsetDateTime,
    /// The last load attempt, successful or not; the next one waits an interval from here.
    checked_at: OffsetDateTime,
}

impl Published {
    fn fresh(&self, now: OffsetDateTime, refresh: Duration) -> bool {
        now >= self.checked_at && now - self.checked_at < refresh
    }
}

/// The error of a caller that queued behind a failed first load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigUnavailable;

impl std::fmt::Display for ConfigUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the first configuration load failed")
    }
}

impl From<ConfigUnavailable> for StorageError {
    fn from(error: ConfigUnavailable) -> Self {
        unavailable(&error)
    }
}

/// A configuration read that found no snapshot and could not load one.
fn unavailable(error: &dyn std::fmt::Display) -> StorageError {
    tracing::warn!(
        target: LOG_TARGET,
        error = %error,
        "configuration read failed"
    );
    StorageError::Unavailable("read configuration failed: config unavailable".to_owned())
}

/// Whether two reads of the published slot saw the same entry.
fn same(a: Option<&Arc<Published>>, b: Option<&Arc<Published>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// The current [`ConfigSnapshot`], reloaded by the first read past the interval.
pub struct ConfigCache {
    refresh: Duration,
    current: ArcSwapOption<Published>,
    reload: Mutex<()>,
    failed_first_loads: AtomicU64,
}

impl ConfigCache {
    /// An empty cache; the first read loads.
    #[must_use]
    pub fn new(refresh: Duration) -> Self {
        Self {
            refresh,
            current: ArcSwapOption::empty(),
            reload: Mutex::new(()),
            failed_first_loads: AtomicU64::new(0),
        }
    }

    /// The snapshot as of `now`; one caller reloads a stale one while the rest keep reading it.
    ///
    /// # Errors
    ///
    /// Returns the error of `load` when there is no earlier snapshot to keep, and
    /// [`ConfigUnavailable`] to a caller that queued behind a failed first load.
    pub async fn get<F, Fut, E>(
        &self,
        now: OffsetDateTime,
        load: F,
    ) -> Result<Arc<ConfigSnapshot>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<ConfigSnapshot, E>>,
        E: std::fmt::Display + From<ConfigUnavailable>,
    {
        let current = self.current.load_full();
        if let Some(published) = current.as_deref()
            && published.fresh(now, self.refresh)
        {
            return Ok(Arc::clone(&published.snapshot));
        }
        let failures = self.failed_first_loads.load(Ordering::Acquire);
        let _reloading = match (self.reload.try_lock(), current.as_deref()) {
            (Ok(guard), _) => guard,
            (Err(_), Some(published)) => return Ok(Arc::clone(&published.snapshot)),
            (Err(_), None) => self.reload.lock().await,
        };
        // A caller that queued behind another's attempt takes its outcome.
        let latest = self.current.load_full();
        if !same(current.as_ref(), latest.as_ref()) {
            if let Some(published) = latest {
                return Ok(Arc::clone(&published.snapshot));
            }
        } else if latest.is_none() && self.failed_first_loads.load(Ordering::Acquire) != failures {
            return Err(E::from(ConfigUnavailable));
        }
        match load().await {
            Ok(snapshot) => {
                let snapshot = Arc::new(snapshot);
                self.current.store(Some(Arc::new(Published {
                    snapshot: Arc::clone(&snapshot),
                    loaded_at: now,
                    checked_at: now,
                })));
                Ok(snapshot)
            }
            Err(error) => {
                let Some(published) = current else {
                    self.failed_first_loads.fetch_add(1, Ordering::Release);
                    return Err(error);
                };
                tracing::warn!(
                    target: LOG_TARGET,
                    error = %error,
                    snapshot_age_secs = (now - published.loaded_at).whole_seconds(),
                    "configuration reload failed, keeping the last snapshot"
                );
                self.current.store(Some(Arc::new(Published {
                    snapshot: Arc::clone(&published.snapshot),
                    loaded_at: published.loaded_at,
                    checked_at: now,
                })));
                Ok(Arc::clone(&published.snapshot))
            }
        }
    }

    /// The snapshot as of `now`, reloaded from `db` when stale.
    ///
    /// # Errors
    ///
    /// [`StorageError::Unavailable`] when there is no snapshot and `db` cannot be read.
    pub(crate) async fn read(
        &self,
        db: &Db,
        now: OffsetDateTime,
    ) -> Result<Arc<ConfigSnapshot>, StorageError> {
        self.get(now, || async {
            let conn = db.conn().map_err(|error| unavailable(&error))?;
            ConfigSnapshot::load(&conn)
                .await
                .map_err(|error| unavailable(&error))
        })
        .await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_cache_tests.rs"]
mod tests;
