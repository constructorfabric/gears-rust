//! Advisory locks implementation with namespacing and retry policies.
//!
//! Cross-database advisory locking with proper namespacing and configurable
//! retry/backoff.
//!
//! ## Security policy
//! This crate forbids plain SQL outside migration infrastructure. Therefore, locks
//! are implemented **purely as file-based locks** (no DB-native advisory locks).
//!
//! Notes:
//! - Prefer calling `guard.release().await` for deterministic unlock;
//!   `Drop` provides best-effort cleanup only (may be skipped on runtime shutdown).
//! - File-based locks use `create_new(true)` semantics and keep the file open,
//!   then remove it on release. Consider using `fs2::FileExt::try_lock_exclusive()`
//!   if you want kernel-level advisory locks across processes.

#![cfg_attr(
    not(any(feature = "pg", feature = "mysql", feature = "sqlite")),
    allow(unused_imports, unused_variables, dead_code, unreachable_code)
)]

use std::path::PathBuf;
use std::time::{Duration, Instant};
use thiserror::Error;
use xxhash_rust::xxh3::xxh3_64;

use chrono::SecondsFormat;

use tokio::fs::File;

// --------------------------- Config ------------------------------------------

/// Configuration for lock acquisition attempts.
///
/// The backoff/jitter fields use the workspace-wide retry vocabulary and map
/// straight onto [`tokio_retry::strategy::ExponentialBackoff`]. `max_wait` and
/// `max_retries` are lock-specific termination knobs layered on top.
#[derive(Debug, Clone)]
pub struct LockConfig {
    /// Maximum wall-clock time to wait for acquisition (`None` = unlimited).
    pub max_wait: Option<Duration>,
    /// Maximum retries after the first attempt (`None` = unlimited, bounded
    /// only by `max_wait`).
    pub max_retries: Option<u32>,
    /// [`ExponentialBackoff`](tokio_retry::strategy::ExponentialBackoff) base —
    /// the growth ratio between retry delays.
    pub backoff_base_ms: u64,
    /// Multiplicative factor applied to every retry delay.
    pub backoff_factor: u64,
    /// Upper bound on any single retry delay (cap for exponential backoff).
    pub max_backoff: Duration,
    /// Apply full jitter to each retry delay.
    pub jitter: bool,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            max_wait: Some(Duration::from_secs(30)),
            max_retries: None,
            // base 2 × factor 25 → 50ms, 100ms, 200ms, … (doubling), capped at
            // `max_backoff`.
            backoff_base_ms: 2,
            backoff_factor: 25,
            max_backoff: Duration::from_secs(5),
            jitter: true,
        }
    }
}

/// Outcome of a single `try_acquire_once` used to drive the retry loop:
/// `Pending` is the retryable "held elsewhere, try again" signal.
enum TryLockError {
    Pending,
    Fatal(DbLockError),
}

/* --------------------------- Guard ------------------------------------------- */

#[derive(Debug)]
enum GuardInner {
    /// File-based fallback (keeps descriptor open until release).
    File { path: PathBuf, file: File },
}

/// Database lock guard that can release lock explicitly via `release()`.
/// `Drop` provides best-effort cleanup if you forget to call `release()`.
#[derive(Debug)]
pub struct DbLockGuard {
    namespaced_key: String,
    inner: Option<GuardInner>, // Option to allow moving inner out in Drop
}

impl DbLockGuard {
    /// Lock key with gear namespace ("gear:key").
    pub fn key(&self) -> &str {
        &self.namespaced_key
    }

    /// Deterministically release the lock (preferred).
    pub async fn release(mut self) {
        if let Some(inner) = self.inner.take() {
            unlock_inner(inner).await;
        }
        // drop self
    }
}

impl Drop for DbLockGuard {
    fn drop(&mut self) {
        // Best-effort async unlock if runtime is alive and inner still present.
        if let Some(inner) = self.inner.take()
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            handle.spawn(async move { unlock_inner(inner).await });
        }
        // else: No runtime or no inner; we cannot perform async cleanup here.
        // The lock may remain held until process exit (DB connection)
        // or lock file may remain on disk. Prefer calling `release().await`.
    }
}

async fn unlock_inner(inner: GuardInner) {
    match inner {
        GuardInner::File { path, file } => {
            // Close file first, then try to remove marker. Ignore errors.
            drop(file);
            _ = tokio::fs::remove_file(&path).await;
        }
    }
}

// --------------------------- Lock Manager ------------------------------------

/// Internal lock manager handling different database backends.
pub(crate) struct LockManager {
    dsn: String,
}

impl LockManager {
    #[must_use]
    pub fn new(dsn: String) -> Self {
        Self { dsn }
    }

    /// Acquire an advisory lock for `{gear}:{key}`.
    ///
    /// Returns a guard that releases the lock when dropped (best-effort) or
    /// deterministically when `release().await` is called.
    ///
    /// # Errors
    /// Returns `DbLockError` if the lock cannot be acquired.
    pub async fn lock(&self, gear: &str, key: &str) -> Result<DbLockGuard, DbLockError> {
        let namespaced_key = format!("{gear}:{key}");
        self.lock_file(&namespaced_key).await
    }

    /// Try to acquire an advisory lock with retry/backoff policy.
    ///
    /// Returns:
    /// - `Ok(Some(guard))` if lock acquired
    /// - `Ok(None)` if timed out or attempts exceeded
    /// - `Err(e)` on unrecoverable error
    ///
    /// # Errors
    /// Returns `DbLockError` on unrecoverable lock errors.
    pub async fn try_lock(
        &self,
        gear: &str,
        key: &str,
        config: LockConfig,
    ) -> Result<Option<DbLockGuard>, DbLockError> {
        use tokio_retry::RetryIf;
        use tokio_retry::strategy::{ExponentialBackoff, jitter};

        let namespaced_key = format!("{gear}:{key}");
        let start = Instant::now();

        // Exponential backoff in the shared retry vocabulary, capped at
        // `max_backoff`.
        let jitter_on = config.jitter;
        let max_wait = config.max_wait;
        let strategy = ExponentialBackoff::from_millis(config.backoff_base_ms)
            .factor(config.backoff_factor)
            .max_delay(config.max_backoff)
            // Stop yielding delays once the wall-clock budget is spent — this is
            // what bounds total wait when `max_wait` is set. Pulled between
            // attempts, exactly where the previous loop checked the deadline.
            .take_while(move |_| max_wait.is_none_or(|mw| start.elapsed() < mw))
            // Cap each delay by the remaining budget, then optionally jitter.
            .map(move |d| {
                let capped = max_wait.map_or(d, |mw| d.min(mw.saturating_sub(start.elapsed())));
                if jitter_on { jitter(capped) } else { capped }
            })
            // Retries after the first attempt (unlimited when `None`, bounded
            // only by `max_wait`).
            .take(config.max_retries.map_or(usize::MAX, |r| r as usize));

        // `try_acquire_once` returns `Ok(None)` while the lock is held
        // elsewhere; model that as a retryable sentinel so tokio-retry drives
        // the backoff, and fold the exhausted sentinel back into `Ok(None)`.
        let action = || async {
            match self.try_acquire_once(&namespaced_key).await {
                Ok(Some(guard)) => Ok(guard),
                Ok(None) => Err(TryLockError::Pending),
                Err(e) => Err(TryLockError::Fatal(e)),
            }
        };
        let retryable = |e: &TryLockError| matches!(e, TryLockError::Pending);

        match RetryIf::start(strategy, action, retryable).await {
            Ok(guard) => Ok(Some(guard)),
            Err(TryLockError::Pending) => Ok(None),
            Err(TryLockError::Fatal(e)) => Err(e),
        }
    }

    // ------------------------ File helpers ----------------------

    async fn lock_file(&self, namespaced_key: &str) -> Result<DbLockGuard, DbLockError> {
        let path = self.get_lock_file_path(namespaced_key);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // create_new semantics via tokio
        let file_res = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await;
        let file = match file_res {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(DbLockError::AlreadyHeld {
                    lock_name: namespaced_key.to_owned(),
                });
            }
            Err(e) => return Err(e.into()),
        };

        // Write debug info (best-effort only)
        {
            use tokio::io::AsyncWriteExt;
            let mut f = file.try_clone().await?;
            _ = f
                .write_all(
                    format!(
                        "PID: {}\nKey: {}\nTimestamp: {}\n",
                        std::process::id(),
                        namespaced_key,
                        chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
                    )
                    .as_bytes(),
                )
                .await;
        }

        Ok(DbLockGuard {
            namespaced_key: namespaced_key.to_owned(),
            inner: Some(GuardInner::File { path, file }),
        })
    }

    async fn try_lock_file(
        &self,
        namespaced_key: &str,
    ) -> Result<Option<DbLockGuard>, DbLockError> {
        let path = self.get_lock_file_path(namespaced_key);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => {
                // Write debug info (best-effort only)
                {
                    use tokio::io::AsyncWriteExt;
                    let mut f = file.try_clone().await?;
                    _ = f
                        .write_all(
                            format!(
                                "PID: {}\nKey: {}\nTimestamp: {}\n",
                                std::process::id(),
                                namespaced_key,
                                chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
                            )
                            .as_bytes(),
                        )
                        .await;
                }

                Ok(Some(DbLockGuard {
                    namespaced_key: namespaced_key.to_owned(),
                    inner: Some(GuardInner::File { path, file }),
                }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn try_acquire_once(
        &self,
        namespaced_key: &str,
    ) -> Result<Option<DbLockGuard>, DbLockError> {
        self.try_lock_file(namespaced_key).await
    }

    /// Generate lock file path for `SQLite` (or when using file-based locks).
    fn get_lock_file_path(&self, namespaced_key: &str) -> PathBuf {
        // For ephemeral DSNs (like `memdb`) or tests, use temp dir to avoid global pollution.
        let base_dir = if self.dsn.contains("memdb") || cfg!(test) {
            std::env::temp_dir().join("cf_gears_test_locks")
        } else {
            // Prefer OS cache dir; fallback to temp dir if None (e.g. in minimal containers).
            let cache = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
            cache.join("cf-gears").join("locks")
        };

        let dsn_hash = format!("{:x}", xxh3_64(self.dsn.as_bytes()));
        let key_hash = format!("{:x}", xxh3_64(namespaced_key.as_bytes()));
        base_dir.join(dsn_hash).join(format!("{key_hash}.lock"))
    }
}

// --------------------------- Errors ------------------------------------------

#[derive(Error, Debug)]
pub enum DbLockError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Lock already held: {lock_name}")]
    AlreadyHeld { lock_name: String },

    #[error("Lock not found: {lock_name}")]
    NotFound { lock_name: String },
}

// --------------------------- Tests -------------------------------------------

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_namespaced_locks() -> Result<()> {
        let lock_manager = LockManager::new("test_dsn".to_owned());

        // Unique key suffix (avoid conflicts in parallel)
        let test_id = format!(
            "test_ns_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let guard1 = lock_manager
            .lock("gear1", &format!("{test_id}_key"))
            .await?;
        let guard2 = lock_manager
            .lock("gear2", &format!("{test_id}_key"))
            .await?;

        assert!(!guard1.key().is_empty());
        assert!(!guard2.key().is_empty());

        guard1.release().await;
        guard2.release().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_try_lock_with_timeout() -> Result<()> {
        let lock_manager = Arc::new(LockManager::new("test_dsn".to_owned()));

        let test_id = format!(
            "test_timeout_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let _guard1 = lock_manager
            .lock("test_gear", &format!("{test_id}_key"))
            .await?;

        // Different key should succeed quickly even with retries/timeouts
        let config = LockConfig {
            max_wait: Some(Duration::from_millis(200)),
            max_retries: Some(3),
            ..Default::default()
        };

        let result = lock_manager
            .try_lock("test_gear", &format!("{test_id}_different_key"), config)
            .await?;
        assert!(result.is_some(), "expected successful lock acquisition");
        Ok(())
    }

    #[tokio::test]
    async fn test_try_lock_success() -> Result<()> {
        let lock_manager = LockManager::new("test_dsn".to_owned());

        let test_id = format!(
            "test_success_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let result = lock_manager
            .try_lock(
                "test_gear",
                &format!("{test_id}_key"),
                LockConfig::default(),
            )
            .await?;
        assert!(result.is_some(), "expected lock acquisition");
        Ok(())
    }

    #[tokio::test]
    async fn test_double_lock_same_key_errors() -> Result<()> {
        let lock_manager = LockManager::new("test_dsn".to_owned());

        let test_id = format!(
            "test_double_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let guard = lock_manager.lock("test_gear", &test_id).await?;
        let err = lock_manager.lock("test_gear", &test_id).await.unwrap_err();
        match err {
            DbLockError::AlreadyHeld { lock_name } => {
                assert!(lock_name.contains(&test_id));
            }
            other => panic!("unexpected error: {other:?}"),
        }

        guard.release().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_try_lock_conflict_returns_none() -> Result<()> {
        let lock_manager = LockManager::new("test_dsn".to_owned());

        let key = format!(
            "test_conflict_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let _guard = lock_manager.lock("gear", &key).await?;
        let config = LockConfig {
            max_wait: Some(Duration::from_millis(100)),
            max_retries: Some(2),
            ..Default::default()
        };
        let res = lock_manager.try_lock("gear", &key, config).await?;
        assert!(res.is_none());
        Ok(())
    }
}
