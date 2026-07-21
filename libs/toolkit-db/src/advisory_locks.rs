//! Advisory locks with namespacing and retry policies.
//!
//! Backends:
//! - **`PostgreSQL`** (`pg`): session-level `pg_try_advisory_lock` on a pinned pool connection
//! - **`MySQL`** (`mysql`): session-level `GET_LOCK(name, 0)` on a pinned pool connection
//! - **`SQLite` / fallback**: file marker locks under the OS cache dir
//!
//! ## Public API semantics
//!
//! - [`LockManager::lock`] — exactly one non-blocking acquisition attempt.
//!   Contention → [`DbLockError::AlreadyHeld`]. On PG/MySQL, SQL/protocol errors map to
//!   `DbLockError::Database`.
//! - [`LockManager::try_lock`] — owns retry/backoff; returns `Ok(None)` when limits are exhausted.
//!   `max_wait` bounds retry scheduling and time between **completed** acquisition attempts. It
//!   does **not** currently impose a cancellation-safe timeout on pool acquisition or an
//!   in-flight database query.
//! - Native backends never use blocking `pg_advisory_lock` or `GET_LOCK` with a positive timeout.
//!
//! ## Native connection ownership
//!
//! A held PG/MySQL lock consumes one pooled connection for its entire lifetime. Guards must only
//! wrap short critical sections — do not retain a guard across remote calls, LLM work, or sleeps.
//!
//! Connections are wrapped in an internal owner with an explicit `Reusable` / `Discard` fate.
//!
//! ### Connection fate invariant
//!
//! ```text
//! new PoolConnection wrapped in ConnOwner → Discard
//!
//! normal acquire contention (PG false / MySQL Some(0))
//!   → Reusable → return to pool
//!
//! successful acquire (PG true / MySQL Some(1))
//!   → remain Discard while guard exists
//!
//! successful unlock / backend-confirmed NotHeld
//!   → Reusable → return to pool
//!
//! SQL error / cancellation / unknown result
//!   → remain Discard → close physical connection
//! ```
//!
//! ### Cancellation and sqlx 0.8.x `close_on_drop`
//!
//! A native connection starts in `Discard` state as soon as it is wrapped in `ConnOwner`.
//!
//! Cancellation of `pg_try_advisory_lock`, `GET_LOCK`, `pg_advisory_unlock` or `RELEASE_LOCK`
//! drops `ConnOwner` in `Discard` state. While a Tokio runtime is still available,
//! `PoolConnection::close_on_drop` schedules a background close instead of returning the session
//! to the pool (sqlx 0.8.x `PoolConnection::Drop` always `spawn`s that work).
//!
//! Cancellation while awaiting `pool.acquire()` is advisory-lock safe because no lock SQL has
//! been issued yet.
//!
//! **Native guards must be released or dropped on a live Tokio runtime.** Dropping a held native
//! guard after the runtime has shut down cannot run unlock SQL. In sqlx 0.8.x,
//! `PoolConnection::Drop` always `rt::spawn`s close/return work; without a current handle that
//! panics (`missing_rt`), and even `detach()` still drops the pool shell (spawn when
//! `min_connections > 0`). This crate's no-runtime fallback therefore `mem::forget`s the
//! `PoolConnection` so it is neither returned to the reusable pool nor dropped through sqlx's
//! spawn path — at the cost of leaking one pool permit and leaving the DB session until process
//! / OS reclaim. This is a last-resort failure mode, not a supported shutdown protocol.
//!
//! Prefer awaiting [`DbLockGuard::release`] on the normal path. Revalidate `close_on_drop`
//! semantics on sqlx upgrades.
//!
//! ### File-marker backend limits
//!
//! The SQLite/fallback backend uses an exclusive create of a marker file (not `fs2` kernel
//! locks). Process termination or cancellation during filesystem acquisition may leave a stale
//! marker — the file backend does not provide kernel-owned lock cleanup.
//!
//! After `open(...).await` returns successfully, ownership is transferred to the guard without
//! another await point. Cancellation while the filesystem open itself is in flight may still
//! leave a marker, depending on Tokio blocking-filesystem cancellation behavior.
//!
//! Implicit [`DbLockGuard`] Drop removes the marker **synchronously** (file cleanup does not
//! depend on a spawned Tokio task). Explicit [`DbLockGuard::release`] for the file backend is
//! likewise synchronous after taking ownership.
//!
//! ## Semver note
//!
//! This module introduces a **breaking** public API change versus prior `0.8.x` releases:
//! [`DbLockGuard::release`] now returns [`Result`], and `DbLockError` (including
//! `Database(sqlx::Error)` when `pg`/`mysql` features are enabled) is publicly re-exported. Bump the crate major/minor appropriately
//! when publishing (intended: `0.9.0`). Local path/patch validation against consumers pinned to
//! `0.8.4` may keep the package version at `0.8.4` until a coordinated Gears release.
//!
//! ## Stable lock namespace
//!
//! ```text
//! canonical_lock_input =
//!   "cf-gears-toolkit-db:v2:{database_scope:016x}:g{gear_utf8_len}:{gear}:k{key_utf8_len}:{key}"
//! ```
//!
//! Length prefixes are UTF-8 byte lengths so `gear`/`key` values that contain `:` cannot collide
//! (e.g. `("a:b","c")` ≠ `("a","b:c")`). The `v2` prefix replaces the ambiguous `v1` colon-joined
//! encoding.
//!
//! `database_scope` is a cross-pod stable fingerprint of host + port + database name (no password,
//! no pod/PID, no implicit `PostgreSQL` `search_path`). File lock paths are derived from the same
//! scope + canonical input (raw DSN does not independently participate in the final path).

#![cfg_attr(
    not(any(feature = "pg", feature = "mysql", feature = "sqlite")),
    allow(unused_imports, unused_variables, dead_code, unreachable_code)
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::fs::File;
use xxhash_rust::xxh3::xxh3_64;

#[cfg(feature = "mysql")]
use sqlx::MySql;
#[cfg(feature = "pg")]
use sqlx::Postgres;
#[cfg(any(feature = "pg", feature = "mysql"))]
use sqlx::pool::PoolConnection;

// --------------------------- Config ------------------------------------------

/// Configuration for lock acquisition attempts.
#[derive(Debug, Clone)]
pub struct LockConfig {
    /// Bounds retry scheduling and time between completed acquisition attempts (`None` = unlimited).
    ///
    /// Does **not** cancel an in-flight `pool.acquire()` or database query; those are limited by
    /// the sqlx/pool/database timeouts instead.
    pub max_wait: Option<Duration>,
    /// Initial delay between retry attempts.
    pub initial_backoff: Duration,
    /// Maximum delay between retry attempts (cap for exponential backoff).
    pub max_backoff: Duration,
    /// Backoff multiplier for exponential backoff.
    pub backoff_multiplier: f64,
    /// Jitter percentage in [0.0, 1.0]; e.g. 0.2 means ±20% jitter.
    pub jitter_pct: f32,
    /// Maximum number of retry attempts (`None` = unlimited).
    pub max_attempts: Option<u32>,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            max_wait: Some(Duration::from_secs(30)),
            initial_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_secs(5),
            backoff_multiplier: 1.5,
            jitter_pct: 0.2,
            max_attempts: None,
        }
    }
}

impl LockConfig {
    /// Validate configuration before the first acquisition attempt.
    ///
    /// # Errors
    /// Returns [`DbLockError::InvalidConfig`] when values would panic or produce nonsense backoff.
    pub fn validate(&self) -> Result<(), DbLockError> {
        if self.initial_backoff.is_zero() {
            return Err(DbLockError::InvalidConfig {
                message: "initial_backoff must be greater than zero".to_owned(),
            });
        }
        if self.max_backoff < self.initial_backoff {
            return Err(DbLockError::InvalidConfig {
                message: "max_backoff must be >= initial_backoff".to_owned(),
            });
        }
        if !self.backoff_multiplier.is_finite() || self.backoff_multiplier < 1.0 {
            return Err(DbLockError::InvalidConfig {
                message: "backoff_multiplier must be finite and >= 1.0".to_owned(),
            });
        }
        if !self.jitter_pct.is_finite() || !(0.0..=1.0).contains(&self.jitter_pct) {
            return Err(DbLockError::InvalidConfig {
                message: "jitter_pct must be finite and in 0.0..=1.0".to_owned(),
            });
        }
        if self.max_attempts == Some(0) {
            return Err(DbLockError::InvalidConfig {
                message: "max_attempts must be None or >= 1".to_owned(),
            });
        }
        Ok(())
    }
}

/// Multiply a [`Duration`] by a non-negative finite `f64` without panicking on overflow.
///
/// Invalid factors (`NaN`, infinite, negative) yield [`Duration::ZERO`]. Overflow saturates to
/// [`Duration::MAX`].
fn saturating_mul_duration(duration: Duration, factor: f64) -> Duration {
    if !factor.is_finite() || factor < 0.0 {
        return Duration::ZERO;
    }
    if factor == 0.0 || duration.is_zero() {
        return Duration::ZERO;
    }
    let secs = duration.as_secs_f64() * factor;
    Duration::try_from_secs_f64(secs).unwrap_or(Duration::MAX)
}

// --------------------------- Scope / key helpers -----------------------------

const CANONICAL_PREFIX: &str = "cf-gears-toolkit-db:v2";

/// Canonical lock input shared by all backends.
///
/// Uses UTF-8 length-prefixed `gear`/`key` fields so values containing `:` cannot collide.
#[must_use]
pub(crate) fn canonical_lock_input(database_scope: u64, gear: &str, key: &str) -> String {
    format!(
        "{CANONICAL_PREFIX}:{database_scope:016x}:g{}:{gear}:k{}:{key}",
        gear.len(),
        key.len(),
    )
}

/// Build a cross-pod-stable database scope fingerprint.
///
/// Identity must be identical for all peers coordinating on the same logical database.
/// Do **not** include pod hostname, PID, `instance_id`, passwords, or `PostgreSQL` `search_path`.
#[must_use]
pub(crate) fn database_scope_from_identity(identity: &str) -> u64 {
    xxh3_64(identity.as_bytes())
}

/// Server-style identity: `scheme://host:port/database` (no credentials).
///
/// TODO(application-namespace): optionally append an explicit application namespace from
/// `DbOptions` when present, so independent apps sharing one database do not collide on
/// advisory locks. Until then, peers on the same host/port/database share one lock space.
#[must_use]
pub(crate) fn server_database_identity(
    scheme: &str,
    host: &str,
    port: u16,
    database: &str,
) -> String {
    format!("{scheme}://{host}:{port}/{database}")
}

/// Parse a DSN into a non-secret scope identity string when possible.
#[must_use]
pub(crate) fn database_scope_from_dsn(dsn: &str) -> u64 {
    let trimmed = dsn.trim_start();
    if let Ok(url) = url::Url::parse(trimmed) {
        let scheme = url.scheme();
        if scheme == "postgres" || scheme == "postgresql" || scheme == "mysql" {
            let host = url.host_str().unwrap_or("");
            let port =
                url.port_or_known_default()
                    .unwrap_or(if scheme == "mysql" { 3306 } else { 5432 });
            let database = url.path().trim_start_matches('/');
            // Normalize postgres/postgresql so both DSNs share one scope.
            let normalized_scheme = if scheme == "postgresql" {
                "postgres"
            } else {
                scheme
            };
            return database_scope_from_identity(&server_database_identity(
                normalized_scheme,
                host,
                port,
                database,
            ));
        }
        #[cfg(feature = "sqlite")]
        if scheme == "sqlite" {
            return database_scope_from_identity(&crate::sqlite::path::sqlite_scope_identity(
                trimmed,
            ));
        }
    }
    #[cfg(feature = "sqlite")]
    if trimmed.starts_with("sqlite:") {
        return database_scope_from_identity(&crate::sqlite::path::sqlite_scope_identity(trimmed));
    }
    // Unrecognized: fingerprint the DSN string (no password stripping possible).
    database_scope_from_identity(&format!("dsn:{trimmed}"))
}

/// `PostgreSQL` advisory key: XXH3-64 bit pattern as signed `i64`.
#[must_use]
#[cfg_attr(not(feature = "pg"), allow(dead_code))]
pub(crate) fn stable_lock_key(canonical: &str) -> i64 {
    xxh3_64(canonical.as_bytes()).cast_signed()
}

/// `MySQL` `GET_LOCK` name: `cf:` + lowercase zero-padded 16-digit hex XXH3-64.
#[must_use]
#[cfg_attr(not(feature = "mysql"), allow(dead_code))]
pub(crate) fn mysql_lock_name(canonical: &str) -> String {
    format!("cf:{:016x}", xxh3_64(canonical.as_bytes()))
}

#[must_use]
#[cfg_attr(not(any(feature = "pg", feature = "mysql")), allow(dead_code))]
fn key_fingerprint(canonical: &str) -> String {
    format!("{:016x}", xxh3_64(canonical.as_bytes()))
}

static NEXT_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

fn new_instance_id() -> u64 {
    let counter = NEXT_INSTANCE_ID.fetch_add(1, Ordering::Relaxed);
    let seed = format!(
        "{}:{}:{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        counter,
    );
    xxh3_64(seed.as_bytes())
}

// --------------------------- Connection owner --------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(any(feature = "pg", feature = "mysql")), allow(dead_code))]
enum ConnFate {
    /// Known-safe: connection may return to the reusable pool.
    Reusable,
    /// Ambiguous / unsafe / may still hold a session lock: close, do not reuse.
    Discard,
}

impl ConnFate {
    /// Fresh owner: session has not been proven lock-free yet.
    #[cfg_attr(not(any(feature = "pg", feature = "mysql")), allow(dead_code))]
    const fn for_new_owner() -> Self {
        Self::Discard
    }

    /// Known-safe outcome: contention, successful unlock, or confirmed `NotHeld`.
    #[cfg_attr(not(any(feature = "pg", feature = "mysql")), allow(dead_code))]
    const fn after_safe_outcome() -> Self {
        Self::Reusable
    }

    /// Unsafe / ambiguous outcome: SQL error, cancellation, unknown result.
    #[cfg_attr(not(any(feature = "pg", feature = "mysql")), allow(dead_code))]
    const fn after_unsafe_outcome() -> Self {
        Self::Discard
    }
}

/// Owns a pinned pool connection with an explicit reusable/discard fate.
///
/// # Fate invariant
///
/// Owners start as [`ConnFate::Discard`]. While the session may hold an advisory lock (including
/// after a successful acquire and during unlock SQL), fate stays `Discard`. Transition to
/// [`ConnFate::Reusable`] only after a known-safe outcome: acquire contention, successful unlock,
/// or backend-confirmed [`DbLockError::NotHeld`]. Cancellation of `release()` then closes the
/// session via [`Drop`] instead of returning a possibly still-locked connection to the pool.
///
/// # sqlx close semantics (0.8.x)
///
/// - [`PoolConnection::close`] takes the live connection out of the pool permit and closes the
///   physical session. This is the preferred async discard path (`finish` when fate is Discard).
/// - [`PoolConnection::close_on_drop`] redirects `Drop` to close instead of `return_to_pool`
///   by spawning a background close (sqlx 0.8.x). It is the Drop/cancellation safety net whenever
///   async [`ConnOwner::finish`] does not take and close the connection explicitly **and** a
///   Tokio runtime handle is still available. Do not call both before an awaited `close()` —
///   `close()` alone is sufficient on the async path.
/// - Without a current runtime, do **not** arm `close_on_drop` or drop the `PoolConnection`
///   normally: sqlx `rt::spawn` panics. Use [`ConnOwner::abandon_without_runtime`]
///   (`mem::forget`) instead.
#[cfg(any(feature = "pg", feature = "mysql"))]
#[derive(Debug)]
struct ConnOwner<DB: sqlx::Database> {
    connection: Option<PoolConnection<DB>>,
    fate: ConnFate,
}

#[cfg(any(feature = "pg", feature = "mysql"))]
impl<DB: sqlx::Database> ConnOwner<DB> {
    fn new(connection: PoolConnection<DB>) -> Self {
        Self {
            connection: Some(connection),
            fate: ConnFate::for_new_owner(),
        }
    }

    fn conn_mut(&mut self) -> Option<&mut PoolConnection<DB>> {
        self.connection.as_mut()
    }

    /// Allow return to the pool only after a known lock-free outcome.
    fn mark_reusable(&mut self) {
        self.fate = ConnFate::after_safe_outcome();
    }

    /// Mark for discard. Does **not** arm `close_on_drop` — that happens in [`Drop`] if the
    /// connection is still present (sync safety net when a runtime exists). Async path uses
    /// [`Self::finish`].
    fn mark_discard(&mut self) {
        self.fate = ConnFate::after_unsafe_outcome();
    }

    /// Leave the pooled connection without returning it to the pool and without calling
    /// sqlx `close_on_drop`.
    ///
    /// sqlx 0.8.x `PoolConnection::Drop` always `rt::spawn`s (return-to-pool or close). With no
    /// current Tokio handle that panics via `missing_rt`. Even [`PoolConnection::detach`] ends by
    /// dropping the `PoolConnection` shell, which still spawns when `min_connections > 0`.
    ///
    /// Therefore the only panic-free fallback is to `mem::forget` the `PoolConnection`: the pool
    /// permanently loses one permit and the DB session is not gracefully closed or unlocked.
    /// Native guards must be released/dropped on a live runtime; this path is last-resort only.
    fn abandon_without_runtime(mut self) {
        if let Some(conn) = self.connection.take() {
            std::mem::forget(conn);
        }
    }

    /// Consume the owner: return to pool on Reusable; close physical connection on Discard.
    ///
    /// Discard always yields `Ok(())` after attempting close — the connection is already out of
    /// the reusable pool once `close()` takes ownership, so surfacing close I/O as `Database`
    /// would make `release` fail after the system is already in a safe state.
    async fn finish(mut self) -> Result<(), DbLockError> {
        let Some(conn) = self.connection.take() else {
            return Ok(());
        };
        match self.fate {
            ConnFate::Reusable => {
                drop(conn);
                Ok(())
            }
            ConnFate::Discard => {
                if let Err(error) = conn.close().await {
                    tracing::warn!(
                        %error,
                        "failed to close discarded advisory-lock connection (already removed from pool)"
                    );
                }
                Ok(())
            }
        }
    }
}

#[cfg(any(feature = "pg", feature = "mysql"))]
impl<DB: sqlx::Database> Drop for ConnOwner<DB> {
    fn drop(&mut self) {
        // Sync safety net only: async `finish()` already took `connection`.
        if self.fate == ConnFate::Discard
            && let Some(conn) = self.connection.as_mut()
        {
            conn.close_on_drop();
        }
    }
}

// --------------------------- Guard -------------------------------------------

#[derive(Debug)]
enum GuardInner {
    File {
        path: PathBuf,
        file: File,
    },
    #[cfg(feature = "pg")]
    Postgres {
        owner: ConnOwner<Postgres>,
        lock_key: i64,
        key_fingerprint: String,
    },
    #[cfg(feature = "mysql")]
    MySql {
        owner: ConnOwner<MySql>,
        lock_name: String,
        key_fingerprint: String,
    },
}

/// Database lock guard. Prefer [`DbLockGuard::release`]; `Drop` is best-effort only.
#[derive(Debug)]
pub struct DbLockGuard {
    /// Human display key (`"{gear}:{key}"`) — unchanged from prior API.
    namespaced_key: String,
    inner: Option<GuardInner>,
}

impl DbLockGuard {
    /// Lock key with gear namespace (`"gear:key"`).
    #[must_use]
    pub fn key(&self) -> &str {
        &self.namespaced_key
    }

    /// Deterministically release the lock (preferred path).
    ///
    /// # Errors
    /// Returns [`DbLockError`] if unlock fails or the lock was not held.
    pub async fn release(mut self) -> Result<(), DbLockError> {
        if let Some(inner) = self.inner.take() {
            unlock_or_discard(inner).await?;
        }
        tracing::debug!(key = %self.namespaced_key, "advisory lock released");
        Ok(())
    }
}

impl Drop for DbLockGuard {
    fn drop(&mut self) {
        let Some(inner) = self.inner.take() else {
            return;
        };

        match inner {
            // File markers do not need async cleanup: remove synchronously so runtime shutdown /
            // never-polled / aborted tasks cannot leave an orphan marker.
            GuardInner::File { path, file } => {
                drop(file);
                remove_file_lock_marker(&path);
            }
            #[cfg(any(feature = "pg", feature = "mysql"))]
            native => drop_native_guard(native),
        }
    }
}

/// Native Drop path: arm Discard, then best-effort async unlock or sync close.
#[cfg(any(feature = "pg", feature = "mysql"))]
fn drop_native_guard(mut inner: GuardInner) {
    // Arm close-on-drop *before* handing ownership to an async task.
    // If the task is never polled or is cancelled mid-unlock, ConnOwner::Drop
    // closes the session rather than returning a still-locked connection to the pool.
    arm_drop_discard(&mut inner);

    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            if let Err(error) = best_effort_unlock_and_close(inner).await {
                tracing::warn!(%error, "best-effort advisory unlock/close failed");
            }
        });
    } else {
        // No runtime: cannot issue unlock SQL. Connection already armed for Discard.
        close_native_without_unlock(inner);
    }
}

/// Synchronous removal of a file-backend lock marker.
///
/// `NotFound` is treated as success (already released / cleaned up).
fn remove_file_lock_marker_result(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Best-effort synchronous removal used from [`DbLockGuard`] Drop.
fn remove_file_lock_marker(path: &std::path::Path) {
    if let Err(error) = remove_file_lock_marker_result(path) {
        tracing::warn!(
            %error,
            path = %path.display(),
            "failed to remove file lock marker on Drop"
        );
    }
}

/// Mark native owners as Discard before async Drop cleanup may be cancelled.
#[cfg(any(feature = "pg", feature = "mysql"))]
fn arm_drop_discard(inner: &mut GuardInner) {
    match inner {
        GuardInner::File { .. } => {}
        #[cfg(feature = "pg")]
        GuardInner::Postgres { owner, .. } => owner.mark_discard(),
        #[cfg(feature = "mysql")]
        GuardInner::MySql { owner, .. } => owner.mark_discard(),
    }
}

/// Sync Drop fallback when no Tokio runtime is available (native backends only).
#[cfg(any(feature = "pg", feature = "mysql"))]
fn close_native_without_unlock(inner: GuardInner) {
    match inner {
        GuardInner::File { path, file } => {
            // Defensive: file Drop normally returns earlier. Keep sync cleanup if reached.
            drop(file);
            remove_file_lock_marker(&path);
        }
        #[cfg(feature = "pg")]
        GuardInner::Postgres {
            owner,
            key_fingerprint,
            ..
        } => {
            tracing::warn!(
                backend = "postgres",
                key_fingerprint = %key_fingerprint,
                "dropping advisory lock guard without runtime; leaking pool connection to avoid sqlx spawn panic (no unlock)"
            );
            owner.abandon_without_runtime();
        }
        #[cfg(feature = "mysql")]
        GuardInner::MySql {
            owner,
            key_fingerprint,
            ..
        } => {
            tracing::warn!(
                backend = "mysql",
                key_fingerprint = %key_fingerprint,
                "dropping advisory lock guard without runtime; leaking pool connection to avoid sqlx spawn panic (no unlock)"
            );
            owner.abandon_without_runtime();
        }
    }
}

/// Drop-path cleanup: best-effort unlock, then always close native connections.
#[cfg(any(feature = "pg", feature = "mysql"))]
async fn best_effort_unlock_and_close(inner: GuardInner) -> Result<(), DbLockError> {
    match inner {
        GuardInner::File { path, file } => unlock_or_discard(GuardInner::File { path, file }).await,
        #[cfg(feature = "pg")]
        GuardInner::Postgres {
            mut owner,
            lock_key,
            key_fingerprint,
        } => {
            if let Some(conn) = owner.conn_mut() {
                match sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1)")
                    .bind(lock_key)
                    .fetch_one(&mut **conn)
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::warn!(
                            backend = "postgres",
                            key_fingerprint = %key_fingerprint,
                            "pg_advisory_unlock returned false on Drop"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            backend = "postgres",
                            key_fingerprint = %key_fingerprint,
                            %error,
                            "pg_advisory_unlock failed on Drop"
                        );
                    }
                }
            }
            owner.mark_discard();
            owner.finish().await
        }
        #[cfg(feature = "mysql")]
        GuardInner::MySql {
            mut owner,
            lock_name,
            key_fingerprint,
        } => {
            if let Some(conn) = owner.conn_mut() {
                match sqlx::query_scalar::<_, Option<i64>>("SELECT RELEASE_LOCK(?)")
                    .bind(&lock_name)
                    .fetch_one(&mut **conn)
                    .await
                {
                    Ok(Some(1)) => {}
                    Ok(Some(0)) => {
                        tracing::debug!(
                            backend = "mysql",
                            key_fingerprint = %key_fingerprint,
                            reason = "not_owned_by_session",
                            "RELEASE_LOCK returned 0 on Drop"
                        );
                    }
                    Ok(None) => {
                        tracing::debug!(
                            backend = "mysql",
                            key_fingerprint = %key_fingerprint,
                            reason = "not_found",
                            "RELEASE_LOCK returned NULL on Drop"
                        );
                    }
                    Ok(Some(_)) => {
                        tracing::warn!(
                            backend = "mysql",
                            key_fingerprint = %key_fingerprint,
                            "RELEASE_LOCK returned unexpected value on Drop"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            backend = "mysql",
                            key_fingerprint = %key_fingerprint,
                            %error,
                            "RELEASE_LOCK failed on Drop"
                        );
                    }
                }
            }
            owner.mark_discard();
            owner.finish().await
        }
    }
}

// Async for native unlock SQL; file path is intentionally synchronous (no await) so
// cancelling `release()` cannot leave a stale marker. `unused_async` only applies when
// neither `pg` nor `mysql` is enabled.
#[cfg_attr(
    not(any(feature = "pg", feature = "mysql")),
    allow(clippy::unused_async)
)]
async fn unlock_or_discard(inner: GuardInner) -> Result<(), DbLockError> {
    match inner {
        GuardInner::File { path, file } => {
            drop(file);
            // Synchronous: no await after taking ownership, so cancelling `release()` cannot
            // leave a stale marker the way async `remove_file` could.
            Ok(remove_file_lock_marker_result(&path)?)
        }
        #[cfg(feature = "pg")]
        GuardInner::Postgres {
            mut owner,
            lock_key,
            key_fingerprint,
        } => {
            let Some(conn) = owner.conn_mut() else {
                return Ok(());
            };
            let result: Result<bool, sqlx::Error> =
                sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
                    .bind(lock_key)
                    .fetch_one(&mut **conn)
                    .await;

            match result {
                Ok(true) => {
                    owner.mark_reusable();
                    owner.finish().await
                }
                Ok(false) => {
                    owner.mark_reusable();
                    owner.finish().await?;
                    Err(DbLockError::NotHeld)
                }
                Err(error) => {
                    tracing::warn!(
                        backend = "postgres",
                        key_fingerprint = %key_fingerprint,
                        %error,
                        "ambiguous advisory unlock; discarding connection"
                    );
                    owner.mark_discard();
                    owner.finish().await?;
                    Err(DbLockError::Database(error))
                }
            }
        }
        #[cfg(feature = "mysql")]
        GuardInner::MySql {
            mut owner,
            lock_name,
            key_fingerprint,
        } => {
            let Some(conn) = owner.conn_mut() else {
                return Ok(());
            };
            let result: Result<Option<i64>, sqlx::Error> =
                sqlx::query_scalar("SELECT RELEASE_LOCK(?)")
                    .bind(&lock_name)
                    .fetch_one(&mut **conn)
                    .await;

            match result {
                Ok(Some(1)) => {
                    owner.mark_reusable();
                    owner.finish().await
                }
                Ok(Some(0)) => {
                    tracing::debug!(
                        backend = "mysql",
                        key_fingerprint = %key_fingerprint,
                        reason = "not_owned_by_session",
                        "RELEASE_LOCK returned 0"
                    );
                    owner.mark_reusable();
                    owner.finish().await?;
                    Err(DbLockError::NotHeld)
                }
                Ok(None) => {
                    tracing::debug!(
                        backend = "mysql",
                        key_fingerprint = %key_fingerprint,
                        reason = "not_found",
                        "RELEASE_LOCK returned NULL"
                    );
                    owner.mark_reusable();
                    owner.finish().await?;
                    Err(DbLockError::NotHeld)
                }
                Ok(Some(other)) => {
                    owner.mark_discard();
                    owner.finish().await?;
                    Err(DbLockError::UnexpectedDatabaseResult {
                        message: format!("RELEASE_LOCK returned {other}"),
                    })
                }
                Err(error) => {
                    tracing::warn!(
                        backend = "mysql",
                        key_fingerprint = %key_fingerprint,
                        %error,
                        "ambiguous RELEASE_LOCK; discarding connection"
                    );
                    owner.mark_discard();
                    owner.finish().await?;
                    Err(DbLockError::Database(error))
                }
            }
        }
    }
}

// --------------------------- Lock Manager ------------------------------------

#[derive(Debug, Clone)]
enum LockBackend {
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))]
    File,
    #[cfg(feature = "pg")]
    Postgres { pool: sqlx::PgPool },
    #[cfg(feature = "mysql")]
    MySql { pool: sqlx::MySqlPool },
}

/// Internal lock manager handling different database backends.
#[derive(Debug, Clone)]
pub(crate) struct LockManager {
    backend: LockBackend,
    instance_id: u64,
    database_scope: u64,
}

impl LockManager {
    #[must_use]
    #[cfg_attr(not(any(feature = "sqlite", test)), allow(dead_code))]
    pub fn file(database_scope: u64) -> Self {
        Self {
            backend: LockBackend::File,
            instance_id: new_instance_id(),
            database_scope,
        }
    }

    #[cfg(feature = "pg")]
    #[must_use]
    pub fn postgres(pool: sqlx::PgPool, database_scope: u64) -> Self {
        Self {
            backend: LockBackend::Postgres { pool },
            instance_id: new_instance_id(),
            database_scope,
        }
    }

    #[cfg(feature = "mysql")]
    #[must_use]
    pub fn mysql(pool: sqlx::MySqlPool, database_scope: u64) -> Self {
        Self {
            backend: LockBackend::MySql { pool },
            instance_id: new_instance_id(),
            database_scope,
        }
    }

    #[must_use]
    #[allow(dead_code)] // diagnostics / tests
    pub fn database_scope(&self) -> u64 {
        self.database_scope
    }

    #[must_use]
    #[allow(dead_code)] // diagnostics / tests
    pub fn instance_id(&self) -> u64 {
        self.instance_id
    }

    /// Acquire an advisory lock for `{gear}:{key}` with a single non-blocking attempt.
    ///
    /// # Errors
    /// Returns [`DbLockError::AlreadyHeld`] on contention. On PG/MySQL, SQL errors map to
    /// `DbLockError::Database`.
    pub async fn lock(&self, gear: &str, key: &str) -> Result<DbLockGuard, DbLockError> {
        let display_key = format!("{gear}:{key}");
        let canonical = canonical_lock_input(self.database_scope, gear, key);
        match self.try_acquire_once(&display_key, &canonical).await? {
            Some(guard) => {
                tracing::debug!(key = %display_key, "advisory lock acquired");
                Ok(guard)
            }
            None => Err(DbLockError::AlreadyHeld {
                lock_name: display_key,
            }),
        }
    }

    /// Try to acquire an advisory lock with retry/backoff policy.
    ///
    /// Returns:
    /// - `Ok(Some(guard))` if lock acquired
    /// - `Ok(None)` if timed out or attempts exceeded
    /// - `Err(e)` on unrecoverable error (including invalid config)
    ///
    /// `config.max_wait` bounds retry scheduling between completed attempts; it does not cancel
    /// an in-flight pool acquire or advisory-lock SQL query.
    ///
    /// # Cancellation safety
    ///
    /// This future is cancellation-safe. Dropping it mid-retry will not leak a lock: file
    /// markers are cleaned synchronously on [`DbLockGuard`] Drop, and native connections are
    /// armed with `close_on_drop` before any lock SQL executes. Callers that need cooperative
    /// shutdown may wrap the call in `tokio::select!`:
    ///
    /// ```ignore
    /// tokio::select! {
    ///     result = manager.try_lock(gear, key, config) => { /* handle */ }
    ///     _ = cancellation_token.cancelled() => { /* shutdown */ }
    /// }
    /// ```
    pub async fn try_lock(
        &self,
        gear: &str,
        key: &str,
        config: LockConfig,
    ) -> Result<Option<DbLockGuard>, DbLockError> {
        config.validate()?;

        let display_key = format!("{gear}:{key}");
        let canonical = canonical_lock_input(self.database_scope, gear, key);
        let start = Instant::now();
        let mut attempt = 0u32;
        let mut backoff = config.initial_backoff;

        loop {
            attempt += 1;

            if let Some(max_attempts) = config.max_attempts
                && attempt > max_attempts
            {
                return Ok(None);
            }
            if let Some(max_wait) = config.max_wait
                && start.elapsed() >= max_wait
            {
                return Ok(None);
            }

            if let Some(guard) = self.try_acquire_once(&display_key, &canonical).await? {
                tracing::debug!(
                    key = %display_key,
                    attempt,
                    elapsed = ?start.elapsed(),
                    "advisory lock acquired via try_lock"
                );
                return Ok(Some(guard));
            }

            // Do not sleep after the last permitted attempt.
            if config
                .max_attempts
                .is_some_and(|max_attempts| attempt >= max_attempts)
            {
                return Ok(None);
            }

            let remaining = config
                .max_wait
                .map_or(backoff, |mw| mw.saturating_sub(start.elapsed()));

            if remaining.is_zero() {
                return Ok(None);
            }

            let jitter_factor = {
                let pct = f64::from(config.jitter_pct.clamp(0.0, 1.0));
                let lo = 1.0 - pct;
                let hi = 1.0 + pct;
                let seed = format!("{canonical}:{attempt}:{}", self.instance_id);
                #[allow(clippy::cast_precision_loss)]
                let h = xxh3_64(seed.as_bytes()) as f64;
                #[allow(clippy::cast_precision_loss)]
                let frac = h / (u64::MAX as f64);
                lo + frac * (hi - lo)
            };

            // Cap AFTER jitter so we never knowingly sleep past max_wait.
            let jittered = saturating_mul_duration(backoff, jitter_factor);
            let sleep_for = std::cmp::min(jittered, remaining);
            tokio::time::sleep(sleep_for).await;

            let next = saturating_mul_duration(backoff, config.backoff_multiplier);
            backoff = std::cmp::min(next, config.max_backoff);
        }
    }

    async fn try_acquire_once(
        &self,
        display_key: &str,
        canonical: &str,
    ) -> Result<Option<DbLockGuard>, DbLockError> {
        match &self.backend {
            LockBackend::File => self.try_lock_file(display_key, canonical).await,
            #[cfg(feature = "pg")]
            LockBackend::Postgres { pool } => {
                self.try_lock_postgres(pool, display_key, canonical).await
            }
            #[cfg(feature = "mysql")]
            LockBackend::MySql { pool } => self.try_lock_mysql(pool, display_key, canonical).await,
        }
    }

    async fn try_lock_file(
        &self,
        display_key: &str,
        canonical: &str,
    ) -> Result<Option<DbLockGuard>, DbLockError> {
        let path = self.get_lock_file_path(canonical);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            // Marker creation defines acquisition. Transfer ownership to the guard immediately
            // after a successful open result — no further await in this path. (Cancellation
            // while the open itself is in flight may still leave a marker; see module docs.)
            Ok(file) => Ok(Some(DbLockGuard {
                namespaced_key: display_key.to_owned(),
                inner: Some(GuardInner::File { path, file }),
            })),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// File path derived from `database_scope` + hash(canonical). Raw DSN does not participate.
    fn get_lock_file_path(&self, canonical: &str) -> PathBuf {
        let base_dir = if cfg!(test) {
            std::env::temp_dir().join("cf_gears_test_locks")
        } else {
            let cache = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
            cache.join("cf-gears").join("locks")
        };

        let scope_dir = format!("{:016x}", self.database_scope);
        let key_hash = format!("{:016x}", xxh3_64(canonical.as_bytes()));
        base_dir.join(scope_dir).join(format!("{key_hash}.lock"))
    }

    #[cfg(feature = "pg")]
    async fn try_lock_postgres(
        &self,
        pool: &sqlx::PgPool,
        display_key: &str,
        canonical: &str,
    ) -> Result<Option<DbLockGuard>, DbLockError> {
        let connection = pool.acquire().await?;
        let mut owner = ConnOwner::new(connection);
        let lock_key = stable_lock_key(canonical);
        let fingerprint = key_fingerprint(canonical);

        let Some(conn) = owner.conn_mut() else {
            return Ok(None);
        };

        let result: Result<bool, sqlx::Error> =
            sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
                .bind(lock_key)
                .fetch_one(&mut **conn)
                .await;

        match result {
            Ok(true) => {
                // Keep Discard while this session owns the advisory lock.
                Ok(Some(DbLockGuard {
                    namespaced_key: display_key.to_owned(),
                    inner: Some(GuardInner::Postgres {
                        owner,
                        lock_key,
                        key_fingerprint: fingerprint,
                    }),
                }))
            }
            Ok(false) => {
                owner.mark_reusable();
                owner.finish().await?;
                Ok(None)
            }
            Err(error) => {
                tracing::warn!(
                    backend = "postgres",
                    key_fingerprint = %fingerprint,
                    %error,
                    "ambiguous pg_try_advisory_lock; discarding connection"
                );
                owner.mark_discard();
                owner.finish().await?;
                Err(DbLockError::Database(error))
            }
        }
    }

    #[cfg(feature = "mysql")]
    async fn try_lock_mysql(
        &self,
        pool: &sqlx::MySqlPool,
        display_key: &str,
        canonical: &str,
    ) -> Result<Option<DbLockGuard>, DbLockError> {
        let connection = pool.acquire().await?;
        let mut owner = ConnOwner::new(connection);
        let lock_name = mysql_lock_name(canonical);
        let fingerprint = key_fingerprint(canonical);

        let Some(conn) = owner.conn_mut() else {
            return Ok(None);
        };

        // Timeout 0 — non-blocking only.
        let result: Result<Option<i64>, sqlx::Error> = sqlx::query_scalar("SELECT GET_LOCK(?, 0)")
            .bind(&lock_name)
            .fetch_one(&mut **conn)
            .await;

        match result {
            Ok(Some(1)) => {
                // Keep Discard while this session owns the advisory lock.
                Ok(Some(DbLockGuard {
                    namespaced_key: display_key.to_owned(),
                    inner: Some(GuardInner::MySql {
                        owner,
                        lock_name,
                        key_fingerprint: fingerprint,
                    }),
                }))
            }
            Ok(Some(0)) => {
                owner.mark_reusable();
                owner.finish().await?;
                Ok(None)
            }
            Ok(None) => {
                tracing::warn!(
                    backend = "mysql",
                    key_fingerprint = %fingerprint,
                    "GET_LOCK returned NULL; discarding connection"
                );
                owner.mark_discard();
                owner.finish().await?;
                Err(DbLockError::UnexpectedDatabaseResult {
                    message: "GET_LOCK returned NULL".to_owned(),
                })
            }
            Ok(Some(other)) => {
                owner.mark_discard();
                owner.finish().await?;
                Err(DbLockError::UnexpectedDatabaseResult {
                    message: format!("GET_LOCK returned {other}"),
                })
            }
            Err(error) => {
                tracing::warn!(
                    backend = "mysql",
                    key_fingerprint = %fingerprint,
                    %error,
                    "ambiguous GET_LOCK; discarding connection"
                );
                owner.mark_discard();
                owner.finish().await?;
                Err(DbLockError::Database(error))
            }
        }
    }
}

// --------------------------- Errors ------------------------------------------

#[derive(Error, Debug)]
pub enum DbLockError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[cfg(any(feature = "pg", feature = "mysql"))]
    #[error("Database advisory-lock operation failed: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Lock already held: {lock_name}")]
    AlreadyHeld { lock_name: String },

    #[error("Advisory lock was not held during release")]
    NotHeld,

    #[error("Unexpected database advisory-lock result: {message}")]
    UnexpectedDatabaseResult { message: String },

    #[error("Lock configuration is invalid: {message}")]
    InvalidConfig { message: String },
}

// --------------------------- Tests -------------------------------------------

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::sync::Arc;

    #[test]
    fn connection_owner_fate_state_machine() {
        assert_eq!(ConnFate::for_new_owner(), ConnFate::Discard);
        assert_eq!(ConnFate::after_safe_outcome(), ConnFate::Reusable);
        assert_eq!(ConnFate::after_unsafe_outcome(), ConnFate::Discard);

        // Successful acquire must not transition to Reusable.
        assert_ne!(
            ConnFate::for_new_owner(),
            ConnFate::after_safe_outcome(),
            "successful acquire stays Discard"
        );
    }

    #[test]
    fn stable_lock_key_is_stable() {
        // database_scope = 1 → "0000000000000001"
        // input: UTF-8 of full canonical; XXH3-64; PG key = bit pattern as i64
        let canonical = canonical_lock_input(1, "zoveon", "phone-case");
        assert_eq!(
            canonical,
            "cf-gears-toolkit-db:v2:0000000000000001:g6:zoveon:k10:phone-case"
        );
        assert_eq!(stable_lock_key(&canonical), 7_193_862_067_539_650_702_i64);
        assert_eq!(mysql_lock_name(&canonical), "cf:63d5bb6f8adba88e");
    }

    #[test]
    fn canonical_input_has_no_component_boundary_collisions() {
        let a = canonical_lock_input(1, "a:b", "c");
        let b = canonical_lock_input(1, "a", "b:c");

        assert_ne!(a, b);
        assert_eq!(a, "cf-gears-toolkit-db:v2:0000000000000001:g3:a:b:k1:c");
        assert_eq!(b, "cf-gears-toolkit-db:v2:0000000000000001:g1:a:k3:b:c");
        assert_ne!(stable_lock_key(&a), stable_lock_key(&b));
        assert_ne!(mysql_lock_name(&a), mysql_lock_name(&b));
    }

    #[test]
    fn canonical_has_no_case_normalization() {
        let a = canonical_lock_input(1, "Gear", "Key");
        let b = canonical_lock_input(1, "gear", "key");
        assert_ne!(a, b);
        assert_ne!(stable_lock_key(&a), stable_lock_key(&b));
    }

    #[test]
    fn database_scope_ignores_credentials_in_dsn() {
        let a = database_scope_from_dsn("postgres://alice:secret@db.example:5432/zoveon");
        let b = database_scope_from_dsn("postgres://bob:other@db.example:5432/zoveon");
        assert_eq!(a, b);
    }

    #[test]
    fn database_scope_normalizes_postgres_scheme() {
        let a = database_scope_from_dsn("postgres://db.example:5432/app");
        let b = database_scope_from_dsn("postgresql://db.example:5432/app");
        assert_eq!(a, b);
    }

    #[test]
    #[cfg(feature = "sqlite")]
    fn sqlite_relative_dot_paths_share_scope() {
        let a = database_scope_from_dsn("sqlite:./test.db");
        let b = database_scope_from_dsn("sqlite:test.db");
        assert_eq!(a, b);
    }

    #[test]
    #[cfg(feature = "sqlite")]
    fn sqlite_dotdot_paths_share_scope_lexically() {
        let a = database_scope_from_dsn("sqlite:./data/../data/app.db");
        let b = database_scope_from_dsn("sqlite:data/app.db");
        assert_eq!(a, b);
    }

    #[test]
    fn database_scope_differs_by_database_name() {
        let a = database_scope_from_dsn("postgres://db.example:5432/zoveon");
        let b = database_scope_from_dsn("postgres://db.example:5432/other");
        assert_ne!(a, b);
    }

    #[test]
    fn lock_config_rejects_invalid_values() {
        assert!(matches!(
            LockConfig {
                initial_backoff: Duration::ZERO,
                ..Default::default()
            }
            .validate(),
            Err(DbLockError::InvalidConfig { .. })
        ));

        assert!(matches!(
            LockConfig {
                backoff_multiplier: 0.5,
                ..Default::default()
            }
            .validate(),
            Err(DbLockError::InvalidConfig { .. })
        ));

        assert!(matches!(
            LockConfig {
                jitter_pct: 1.5,
                ..Default::default()
            }
            .validate(),
            Err(DbLockError::InvalidConfig { .. })
        ));

        assert!(matches!(
            LockConfig {
                max_attempts: Some(0),
                ..Default::default()
            }
            .validate(),
            Err(DbLockError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn lock_config_allows_unlimited_wait() {
        let cfg = LockConfig {
            max_wait: None,
            max_attempts: None,
            ..Default::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn clone_preserves_instance_id_and_scope() {
        let a = LockManager::file(0xdead_beef);
        let b = a.clone();
        assert_eq!(a.instance_id(), b.instance_id());
        assert_eq!(a.database_scope(), b.database_scope());

        let c = LockManager::file(0xdead_beef);
        assert_ne!(a.instance_id(), c.instance_id());
        assert_eq!(a.database_scope(), c.database_scope());
    }

    #[test]
    fn saturating_mul_duration_normal_multiplier() {
        let base = Duration::from_millis(100);
        assert_eq!(
            saturating_mul_duration(base, 1.5),
            Duration::from_millis(150)
        );
    }

    #[test]
    fn saturating_mul_duration_huge_finite_multiplier_does_not_panic() {
        let base = Duration::from_secs(1);
        let result = saturating_mul_duration(base, f64::MAX);
        assert_eq!(result, Duration::MAX);
    }

    #[test]
    fn saturating_mul_duration_overflow_saturates() {
        let base = Duration::from_secs(1 << 62);
        let result = saturating_mul_duration(base, 4.0);
        assert_eq!(result, Duration::MAX);
    }

    #[test]
    fn jitter_delay_capped_by_remaining() {
        let backoff = Duration::from_millis(100);
        let remaining = Duration::from_millis(30);
        let jitter_factor = 1.2_f64;
        let jittered = saturating_mul_duration(backoff, jitter_factor);
        let sleep_for = std::cmp::min(jittered, remaining);
        assert_eq!(sleep_for, remaining);
    }

    #[tokio::test]
    async fn test_namespaced_locks() -> Result<()> {
        let lock_manager = LockManager::file(0x11);
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

        guard1.release().await?;
        guard2.release().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_try_lock_different_key_succeeds() -> Result<()> {
        let lock_manager = Arc::new(LockManager::file(0x22));
        let test_id = format!(
            "test_diff_key_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let _guard1 = lock_manager
            .lock("test_gear", &format!("{test_id}_key"))
            .await?;

        let config = LockConfig {
            max_wait: Some(Duration::from_millis(200)),
            initial_backoff: Duration::from_millis(50),
            max_attempts: Some(3),
            ..Default::default()
        };

        let result = lock_manager
            .try_lock("test_gear", &format!("{test_id}_different_key"), config)
            .await?;
        assert!(result.is_some(), "expected successful lock acquisition");
        Ok(())
    }

    #[tokio::test]
    async fn test_try_lock_exhausted_attempts_returns_none_without_extra_sleep() -> Result<()> {
        let lock_manager = LockManager::file(0x66);
        let key = format!(
            "test_no_extra_sleep_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let _guard = lock_manager.lock("gear", &key).await?;
        let config = LockConfig {
            max_wait: Some(Duration::from_secs(30)),
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(100),
            max_attempts: Some(2),
            ..Default::default()
        };

        let start = Instant::now();
        let res = lock_manager.try_lock("gear", &key, config).await?;
        assert!(res.is_none());
        // Two failed attempts + one inter-attempt sleep (~100ms), not a second post-final sleep.
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "unexpected long wait: {:?}",
            start.elapsed()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_try_lock_success() -> Result<()> {
        let lock_manager = LockManager::file(0x33);
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
        if let Some(g) = result {
            g.release().await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_double_lock_same_key_errors() -> Result<()> {
        let lock_manager = LockManager::file(0x44);
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

        guard.release().await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_try_lock_conflict_returns_none() -> Result<()> {
        let lock_manager = LockManager::file(0x55);
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
            max_attempts: Some(2),
            ..Default::default()
        };
        let res = lock_manager.try_lock("gear", &key, config).await?;
        assert!(res.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn file_explicit_release_allows_immediate_reacquire() -> Result<()> {
        let manager = LockManager::file(0x71);
        let key = format!(
            "reacquire_release_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let guard = manager.lock("gear", &key).await?;
        guard.release().await?;

        let guard = manager.lock("gear", &key).await?;
        guard.release().await?;
        Ok(())
    }

    #[tokio::test]
    async fn file_drop_allows_immediate_reacquire() -> Result<()> {
        let manager = LockManager::file(0x72);
        let key = format!(
            "reacquire_drop_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        {
            let _guard = manager.lock("gear", &key).await?;
        }

        let guard = manager.lock("gear", &key).await?;
        guard.release().await?;
        Ok(())
    }

    #[test]
    fn file_drop_after_runtime_shutdown_does_not_panic() {
        // File cleanup is synchronous and must not depend on a live runtime / spawned task.
        let manager = LockManager::file(0x73);
        let key = format!(
            "drop_after_shutdown_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let guard = runtime
            .block_on(async { manager.lock("gear", &key).await })
            .expect("lock");
        drop(runtime);

        // No current Tokio handle: file Drop must still remove the marker without panicking.
        drop(guard);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let guard = manager.lock("gear", &key).await.expect("reacquire");
            guard.release().await.expect("release");
        });
    }

    #[tokio::test]
    async fn file_path_uses_scope_not_independent_dsn() -> Result<()> {
        let a = LockManager::file(0xabc);
        let b = LockManager::file(0xabc);
        let path_a = a.get_lock_file_path(&canonical_lock_input(0xabc, "g", "k"));
        let path_b = b.get_lock_file_path(&canonical_lock_input(0xabc, "g", "k"));
        assert_eq!(path_a, path_b);

        let c = LockManager::file(0xdef);
        let path_c = c.get_lock_file_path(&canonical_lock_input(0xdef, "g", "k"));
        assert_ne!(path_a, path_c);
        Ok(())
    }

    #[test]
    fn lock_config_rejects_max_backoff_below_initial() {
        assert!(matches!(
            LockConfig {
                initial_backoff: Duration::from_millis(200),
                max_backoff: Duration::from_millis(50),
                ..Default::default()
            }
            .validate(),
            Err(DbLockError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn lock_config_rejects_nan_and_infinity() {
        assert!(matches!(
            LockConfig {
                backoff_multiplier: f64::NAN,
                ..Default::default()
            }
            .validate(),
            Err(DbLockError::InvalidConfig { .. })
        ));
        assert!(matches!(
            LockConfig {
                backoff_multiplier: f64::INFINITY,
                ..Default::default()
            }
            .validate(),
            Err(DbLockError::InvalidConfig { .. })
        ));
        assert!(matches!(
            LockConfig {
                jitter_pct: f32::NAN,
                ..Default::default()
            }
            .validate(),
            Err(DbLockError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn database_scope_fallback_for_unrecognized_dsn() {
        let scope = database_scope_from_dsn("custom://foo/bar");
        assert_ne!(scope, 0);
        assert_eq!(scope, database_scope_from_dsn("custom://foo/bar"));
        assert_ne!(scope, database_scope_from_dsn("postgres://foo:5432/bar"));
    }
}
