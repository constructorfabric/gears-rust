//! Secure database handle and runner types.
//!
//! This gear provides the primary entry point for secure database access:
//!
//! - [`Db`]: The database handle. NOT Clone, NOT storable by services.
//! - [`DbConn`]: Non-transactional runner (borrows from `Db`).
//! - [`DbTx`]: Transactional runner (lives inside transaction closure).
//!
//! # Security Model
//!
//! The transaction bypass vulnerability is prevented by multiple layers:
//!
//! 1. `Db` does NOT implement `Clone`
//! 2. `Db::transaction(self, f)` consumes `self`, making it inaccessible inside the closure
//! 3. Services receive `&impl DBRunner`, not `Db` or any factory
//! 4. **Task-local guard**: `Db::conn()` fails if called inside a transaction
//!
//! The task-local guard provides defense-in-depth: even if code obtains a `Db`
//! reference via another path (e.g., captured `Arc<AppServices>`), calling
//! `conn()` will fail at runtime with `DbError::ConnRequestedInsideTx`.
//!
//! # Example
//!
//! ```ignore
//! // In handler/entrypoint
//! let db: Db = ctx.db()?;
//!
//! // Non-transactional path
//! let conn = db.conn()?;  // Note: returns Result now
//! let user = service.get_user(&conn, &scope, id).await?;
//!
//! // Transactional path
//! let (db, result) = db.transaction(|tx| {
//!     Box::pin(async move {
//!         // Only `tx` is available here - `db` is consumed
//!         // Calling some_db.conn() here would fail with ConnRequestedInsideTx
//!         service.create_user(tx, &scope, data).await?;
//!         Ok(user_id)
//!     })
//! }).await;
//! let user_id = result?;
//! ```

use std::{cell::Cell, future::Future, pin::Pin, sync::Arc};

use sea_orm::{DatabaseConnection, DatabaseTransaction, TransactionTrait};

use super::tx_config::TxConfig;
use super::tx_error::TxError;
use crate::{DbError, DbHandle};

/// Default attempt budget for [`Db::transaction_with_retry`].
///
/// Three attempts is the canonical "small bounded retry" used across
/// CF/Gears that wrap writes in retry-aware transactions
/// (closure-table mutations, hierarchy invariants, write paths under
/// concurrent load). It balances:
///
/// - resilience against transient lock-contention failures detected by
///   [`crate::contention::is_retryable_contention`] (`PostgreSQL`
///   serialization failures / deadlocks, `MySQL`/`InnoDB` deadlocks,
///   `SQLite` `BUSY` / `BUSY_SNAPSHOT`);
/// - bounded latency on the hot path — no exponential backoff, just
///   immediate retry of a guaranteed-stale transaction;
/// - predictable failure semantics — after exhausting attempts the
///   original error is returned, so callers can surface e.g.
///   `503 Service Unavailable`.
pub const DEFAULT_TX_RETRY_ATTEMPTS: u32 = 3;

// Task-local guard to detect transaction bypass attempts.
//
// When set to `true`, any call to `Db::conn()` will fail with
// `DbError::ConnRequestedInsideTx`. This prevents code from creating
// non-transactional runners while inside a transaction closure.
tokio::task_local! {
    static IN_TX: Cell<bool>;
}

/// Check if we're currently inside a transaction context.
///
/// Returns `true` if a transaction is active in the current task.
fn is_in_transaction() -> bool {
    IN_TX.try_with(Cell::get).unwrap_or(false)
}

/// **Test-only**: report whether the current async task is inside a
/// `Db::transaction*` closure (i.e. the transaction-bypass guard above is
/// armed).
///
/// Exists so integration-test query recorders (e.g. the resource-group
/// DB-behavior audit) can tag each captured SQL statement as in-tx /
/// out-of-tx without weakening the guard itself — it reads the exact same
/// task-local the guard enforces. Since a `SeaORM` metric callback fires
/// synchronously from within the same async task that issued the query (no
/// `tokio::spawn` in between), this is precise, not a heuristic.
///
/// Gated behind the `test-support` feature; never used by production code.
#[cfg(feature = "test-support")]
#[must_use]
pub fn in_transaction_for_testing() -> bool {
    is_in_transaction()
}

/// Execute a closure with the transaction guard set.
///
/// This sets `IN_TX = true` for the duration of the closure, ensuring
/// that any calls to `Db::conn()` within will fail.
async fn with_tx_guard<F, T>(f: F) -> T
where
    F: Future<Output = T>,
{
    IN_TX.scope(Cell::new(true), f).await
}

/// Database handle for secure operations.
///
/// # Security
///
/// This type is `Clone` to support ergonomic sharing in runtimes and service containers.
/// Transaction-bypass is still prevented by the task-local guard: any attempt to call
/// `conn()` inside a transaction closure fails with `DbError::ConnRequestedInsideTx`.
///
/// Services and repositories must NOT store this type. They should receive
/// `&impl DBRunner` as a parameter to all methods that need database access.
///
/// # Usage
///
/// ```ignore
/// // At the entrypoint (handler/command)
/// let db: Db = ctx.db()?;
///
/// // Pass runner to service methods
/// let conn = db.conn()?;
/// let result = service.do_something(&conn, &scope).await?;
///
/// // Or use a transaction
/// let (db, result) = db.transaction(|tx| {
///     Box::pin(async move {
///         service.do_something(tx, &scope).await
///     })
/// }).await;
/// ```
#[derive(Clone)]
pub struct Db {
    handle: Arc<DbHandle>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("engine", &self.handle.engine())
            .finish_non_exhaustive()
    }
}

impl Db {
    /// **INTERNAL**: Create a new `Db` from an owned `DbHandle`.
    ///
    /// This is typically called by the runtime/context layer, not by service code.
    #[must_use]
    pub(crate) fn new(handle: DbHandle) -> Self {
        Self {
            handle: Arc::new(handle),
        }
    }

    /// **INTERNAL**: Get a privileged `SeaORM` connection clone.
    ///
    /// This must not be exposed to gear code. It exists for infrastructure
    /// (migrations) inside `toolkit-db`.
    pub(crate) fn sea_internal(&self) -> DatabaseConnection {
        self.handle.sea_internal()
    }

    /// Get a reference to the underlying `DbHandle`.
    ///
    /// # Security
    ///
    /// This is `pub(crate)` to allow internal infrastructure access (migrations, etc.)
    /// but prevents service code from extracting the handle.
    /// Create a non-transactional database runner.
    ///
    /// The returned `DbConn` borrows from `self`, ensuring that while a `DbConn`
    /// exists, the `Db` cannot be used for other purposes (like starting a transaction).
    ///
    /// # Errors
    ///
    /// Returns `DbError::ConnRequestedInsideTx` if called from within a transaction
    /// closure. This prevents the transaction bypass vulnerability where code could
    /// create a non-transactional runner that persists writes outside the transaction.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let db: Db = ctx.db()?;
    /// let conn = db.conn()?;
    ///
    /// // Use conn for queries
    /// let users = Entity::find()
    ///     .secure()
    ///     .scope_with(&scope)
    ///     .all(&conn)
    ///     .await?;
    /// ```
    ///
    /// The `Result` itself is `#[must_use]`; this method does not add an extra must-use
    /// marker to avoid clippy `double_must_use`.
    pub fn conn(&self) -> Result<DbConn<'_>, DbError> {
        if is_in_transaction() {
            return Err(DbError::ConnRequestedInsideTx);
        }
        Ok(DbConn {
            conn: self.handle.sea_internal_ref(),
        })
    }

    /// Database backend in use (`Postgres` / `MySql` / `Sqlite`).
    ///
    /// Required by [`crate::contention::is_retryable_contention`] to scope
    /// retryable-error detection to the correct engine.
    #[must_use]
    pub fn backend(&self) -> sea_orm::DbBackend {
        use sea_orm::ConnectionTrait;
        self.handle.sea_internal_ref().get_database_backend()
    }

    // --- Advisory locks (forwarded, no `DbHandle` exposure) ---

    /// Acquire an advisory lock with the given key and gear namespace.
    ///
    /// # Errors
    /// Returns an error if the lock cannot be acquired.
    pub async fn lock(&self, gear: &str, key: &str) -> crate::Result<crate::DbLockGuard> {
        self.handle.lock(gear, key).await
    }

    /// Try to acquire an advisory lock with configurable retry/backoff policy.
    ///
    /// # Errors
    /// Returns an error if an unrecoverable lock error occurs.
    pub async fn try_lock(
        &self,
        gear: &str,
        key: &str,
        config: crate::LockConfig,
    ) -> crate::Result<Option<crate::DbLockGuard>> {
        self.handle.try_lock(gear, key, config).await
    }

    /// Execute a closure inside a database transaction (borrowed form).
    ///
    /// This variant keeps the call site ergonomic for service containers that store a
    /// reusable DB entrypoint (e.g. `DBProvider`) without exposing `DbHandle`.
    ///
    /// # Security
    ///
    /// The task-local guard is still enforced: any call to `Db::conn()` within the closure
    /// will fail with `DbError::ConnRequestedInsideTx`.
    ///
    /// # Errors
    ///
    /// Returns `DbError` if:
    /// - starting the transaction fails
    /// - the closure returns an error
    /// - commit fails (rollback is attempted on closure error)
    pub async fn transaction_ref<F, T>(&self, f: F) -> Result<T, DbError>
    where
        F: for<'a> FnOnce(
                &'a DbTx<'a>,
            )
                -> Pin<Box<dyn Future<Output = Result<T, DbError>> + Send + 'a>>
            + Send,
        T: Send + 'static,
    {
        let txn = self.handle.sea_internal_ref().begin().await?;
        let tx = DbTx { tx: &txn };

        // Run the closure with the transaction guard set
        let res = with_tx_guard(f(&tx)).await;

        match res {
            Ok(v) => {
                txn.commit().await?;
                Ok(v)
            }
            Err(e) => {
                _ = txn.rollback().await;
                Err(e)
            }
        }
    }

    /// Execute a closure inside a database transaction, mapping infrastructure errors into `E`.
    ///
    /// This is the preferred building block for service-facing entrypoints (like `DBProvider`)
    /// that must return **domain** errors while still acquiring connections internally.
    ///
    /// - The transaction closure returns `Result<T, E>` (domain error).
    /// - Begin/commit failures are `DbError` and are mapped via `E: From<DbError>`.
    ///
    /// # Security
    ///
    /// The task-local guard is enforced for the duration of the closure.
    ///
    /// # Errors
    ///
    /// Returns `E` if:
    /// - starting the transaction fails (mapped from `DbError`)
    /// - the closure returns an error
    /// - commit fails (mapped from `DbError`)
    pub async fn transaction_ref_mapped<F, T, E>(&self, f: F) -> Result<T, E>
    where
        E: From<DbError> + Send + 'static,
        F: for<'a> FnOnce(&'a DbTx<'a>) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>
            + Send,
        T: Send + 'static,
    {
        let txn = self
            .handle
            .sea_internal_ref()
            .begin()
            .await
            .map_err(DbError::from)
            .map_err(E::from)?;
        let tx = DbTx { tx: &txn };

        // Run the closure with the transaction guard set
        let res = with_tx_guard(f(&tx)).await;

        match res {
            Ok(v) => {
                txn.commit().await.map_err(DbError::from).map_err(E::from)?;
                Ok(v)
            }
            Err(e) => {
                _ = txn.rollback().await;
                Err(e)
            }
        }
    }

    /// Execute a closure inside a database transaction with custom configuration
    /// (isolation level, access mode), mapping infrastructure errors into `E`.
    ///
    /// This is the preferred building block for service-facing entrypoints (like `DBProvider`)
    /// that must return **domain** errors and need non-default transaction settings
    /// (e.g., `SERIALIZABLE` isolation).
    ///
    /// # Security
    ///
    /// The task-local guard is enforced for the duration of the closure.
    ///
    /// # Errors
    ///
    /// Returns `E` if:
    /// - starting the transaction fails (mapped from `DbError`)
    /// - the closure returns an error
    /// - commit fails (mapped from `DbError`)
    pub async fn transaction_ref_mapped_with_config<F, T, E>(
        &self,
        tx_config: TxConfig,
        f: F,
    ) -> Result<T, E>
    where
        E: From<DbError> + Send + 'static,
        F: for<'a> FnOnce(&'a DbTx<'a>) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>
            + Send,
        T: Send + 'static,
    {
        use sea_orm::{AccessMode, IsolationLevel};

        let isolation: Option<IsolationLevel> = tx_config.isolation.map(Into::into);
        let access_mode: Option<AccessMode> = tx_config.access_mode.map(Into::into);

        let txn = self
            .handle
            .sea_internal_ref()
            .begin_with_config(isolation, access_mode)
            .await
            .map_err(DbError::from)
            .map_err(E::from)?;
        let tx = DbTx { tx: &txn };

        // Run the closure with the transaction guard set
        let res = with_tx_guard(f(&tx)).await;

        match res {
            Ok(v) => {
                txn.commit().await.map_err(DbError::from).map_err(E::from)?;
                Ok(v)
            }
            Err(e) => {
                _ = txn.rollback().await;
                Err(e)
            }
        }
    }

    /// Execute a closure inside a transaction with bounded retries on transient
    /// lock-contention failures.
    ///
    /// Retry detection is delegated to [`crate::contention::is_retryable_contention`],
    /// which is backend-aware (`PostgreSQL` serialization failure / deadlock,
    /// `MySQL`/`InnoDB` deadlock, `SQLite` `BUSY` / `BUSY_SNAPSHOT`). The caller
    /// supplies a small `extract_db_err` accessor that reaches into the domain
    /// error `E` and returns the underlying [`sea_orm::DbErr`], if any — that is
    /// the only piece of glue the helper needs from the caller.
    ///
    /// Uses [`DEFAULT_TX_RETRY_ATTEMPTS`] as the attempt budget. Use
    /// [`Self::transaction_with_retry_max`] if you need to override the budget
    /// (typically only in tests).
    ///
    /// # Parameters
    ///
    /// - `tx_config`: Transaction configuration (isolation level + access
    ///   mode). The helper itself is isolation-agnostic — pick whichever
    ///   level the operation needs:
    ///   - [`TxConfig::default()`] — engine default. Right for retry on
    ///     `SQLite` (BUSY) or `MySQL`/`InnoDB` (deadlocks happen at any
    ///     isolation level), or for `PostgreSQL` work that doesn't need
    ///     stronger guarantees than `READ COMMITTED`.
    ///   - [`TxConfig::serializable()`] — full `SERIALIZABLE`. Right when
    ///     the body relies on predicate-level invariants (closure-table
    ///     mutations, hierarchy or uniqueness checks across rows that
    ///     concurrent writers could insert), so that conflicting reads
    ///     surface as `40001` and get retried by this helper.
    ///   - Custom `TxConfig` (e.g. `RepeatableRead` + `ReadOnly`) for
    ///     reporting paths that want repeatable snapshots without paying
    ///     the cost of `SERIALIZABLE`.
    /// - `extract_db_err`: Accessor returning `Some(&DbErr)` if the domain
    ///   error wraps a database error, `None` otherwise. Returning `None`
    ///   always short-circuits the retry loop (the failure is non-DB and
    ///   is propagated immediately).
    /// - `body`: The transactional work. Called with a fresh `&DbTx` per
    ///   attempt. Each retry runs in a brand-new transaction, so the
    ///   closure must be idempotent across attempts (any in-memory state
    ///   mutated by an earlier attempt must be reset by the closure
    ///   itself before re-running).
    ///
    /// # Behaviour
    ///
    /// 1. Begin a transaction with the given `tx_config` and invoke `body`.
    /// 2. On `Ok`, commit and return.
    /// 3. On `Err(e)`:
    ///    - if `extract_db_err(&e)` yields a `DbErr` that
    ///      [`crate::contention::is_retryable_contention`] flags as
    ///      retryable for the active backend, and attempts remain, log at
    ///      `WARN` and retry;
    ///    - otherwise, return the error.
    ///
    /// On exhausting all attempts the **last** error is returned.
    ///
    /// # Errors
    ///
    /// Returns `E` if the transaction fails (after retries) or if any
    /// infrastructure error mapped from `DbError` occurs.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let result: Result<MyType, MyError> = db
    ///     .transaction_with_retry(
    ///         TxConfig::serializable(),
    ///         MyError::db_err, // fn(&MyError) -> Option<&sea_orm::DbErr>
    ///         |tx| Box::pin(async move {
    ///             repo.do_atomic_work(tx).await?;
    ///             Ok(MyType::default())
    ///         }),
    ///     )
    ///     .await;
    /// ```
    pub async fn transaction_with_retry<T, E, X, F>(
        &self,
        tx_config: TxConfig,
        extract_db_err: X,
        body: F,
    ) -> Result<T, E>
    where
        E: From<DbError> + Send + 'static,
        T: Send + 'static,
        X: Fn(&E) -> Option<&sea_orm::DbErr> + Send,
        F: for<'a> FnMut(&'a DbTx<'a>) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>
            + Send,
    {
        self.transaction_with_retry_max(tx_config, DEFAULT_TX_RETRY_ATTEMPTS, extract_db_err, body)
            .await
    }

    /// Like [`Self::transaction_with_retry`] but with an explicit attempt
    /// budget. See that method for behaviour and parameter semantics.
    ///
    /// `max_attempts` includes the first try (so `1` disables retries). Values
    /// below `1` are clamped to `1`. Production code should call the default
    /// variant instead of hard-coding a number here; this method exists mainly
    /// for tests and for the rare case where a service has a justified reason
    /// to deviate from the workspace-wide default.
    ///
    /// # Errors
    ///
    /// Returns `E` if the transaction fails (after retries) or if any
    /// infrastructure error mapped from `DbError` occurs.
    pub async fn transaction_with_retry_max<T, E, X, F>(
        &self,
        tx_config: TxConfig,
        max_attempts: u32,
        extract_db_err: X,
        mut body: F,
    ) -> Result<T, E>
    where
        E: From<DbError> + Send + 'static,
        T: Send + 'static,
        X: Fn(&E) -> Option<&sea_orm::DbErr> + Send,
        F: for<'a> FnMut(&'a DbTx<'a>) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>
            + Send,
    {
        let max = max_attempts.max(1);
        let backend = self.backend();
        let mut attempt: u32 = 1;

        loop {
            let result = self
                .transaction_ref_mapped_with_config(tx_config.clone(), |tx| body(tx))
                .await;

            match result {
                Ok(value) => return Ok(value),
                Err(e) => {
                    let retryable = extract_db_err(&e).is_some_and(|db_err| {
                        crate::contention::is_retryable_contention(backend, db_err)
                    });
                    if retryable && attempt < max {
                        tracing::warn!(
                            attempt,
                            max_attempts = max,
                            "retrying transaction after retryable failure"
                        );
                        attempt += 1;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }

    /// Execute a closure inside a database transaction.
    ///
    /// # Security
    ///
    /// This method **consumes** `self` and returns it after the transaction completes.
    /// This is critical for security: inside the closure, the original `Db` is not
    /// accessible, so code cannot call `db.conn()` to create a non-transactional runner.
    ///
    /// Additionally, a task-local guard is set during the transaction, so any call
    /// to `conn()` on *any* `Db` instance will fail with `DbError::ConnRequestedInsideTx`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let db: Db = ctx.db()?;
    ///
    /// let (db, result) = db.transaction(|tx| {
    ///     Box::pin(async move {
    ///         // Only `tx` is available here
    ///         service.create_user(tx, &scope, data).await?;
    ///         Ok(user_id)
    ///     })
    /// }).await;
    ///
    /// let user_id = result?;
    /// ```
    ///
    /// # Returns
    ///
    /// Returns `(Self, Result<T>)` where:
    /// - `Self` is always returned (even on error) so the connection can be reused
    /// - `Result<T>` contains the transaction result or error
    pub async fn transaction<F, T>(self, f: F) -> (Self, anyhow::Result<T>)
    where
        F: for<'a> FnOnce(
                &'a DbTx<'a>,
            )
                -> Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send + 'a>>
            + Send,
        T: Send + 'static,
    {
        let txn = match self.handle.sea_internal_ref().begin().await {
            Ok(t) => t,
            Err(e) => return (self, Err(e.into())),
        };
        let tx = DbTx { tx: &txn };

        // Run the closure with the transaction guard set
        let res = with_tx_guard(f(&tx)).await;

        match res {
            Ok(v) => match txn.commit().await {
                Ok(()) => (self, Ok(v)),
                Err(e) => (self, Err(e.into())),
            },
            Err(e) => {
                _ = txn.rollback().await;
                (self, Err(e))
            }
        }
    }

    /// Execute a transaction with typed domain errors.
    ///
    /// This variant separates infrastructure errors (connection issues, commit failures)
    /// from domain errors returned by the closure.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let (db, result) = db.in_transaction(|tx| {
    ///     Box::pin(async move {
    ///         service.create_user(tx, &scope, data).await
    ///     })
    /// }).await;
    ///
    /// match result {
    ///     Ok(user) => println!("Created: {:?}", user),
    ///     Err(TxError::Domain(e)) => println!("Business error: {}", e),
    ///     Err(TxError::Infra(e)) => println!("DB error: {}", e),
    /// }
    /// ```
    pub async fn in_transaction<T, E, F>(self, f: F) -> (Self, Result<T, TxError<E>>)
    where
        T: Send + 'static,
        E: std::fmt::Debug + std::fmt::Display + Send + 'static,
        F: for<'a> FnOnce(&'a DbTx<'a>) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>
            + Send,
    {
        use super::tx_error::InfraError;

        let txn = match self.handle.sea_internal_ref().begin().await {
            Ok(txn) => txn,
            Err(e) => return (self, Err(TxError::Infra(InfraError::new(e.to_string())))),
        };

        let tx = DbTx { tx: &txn };

        // Run the closure with the transaction guard set
        let res = with_tx_guard(f(&tx)).await;

        match res {
            Ok(v) => match txn.commit().await {
                Ok(()) => (self, Ok(v)),
                Err(e) => (self, Err(TxError::Infra(InfraError::new(e.to_string())))),
            },
            Err(e) => {
                _ = txn.rollback().await;
                (self, Err(TxError::Domain(e)))
            }
        }
    }

    /// Execute a transaction with custom configuration (isolation level, access mode).
    ///
    /// # Example
    ///
    /// ```ignore
    /// use toolkit_db::secure::{TxConfig, TxIsolationLevel};
    ///
    /// let config = TxConfig {
    ///     isolation: Some(TxIsolationLevel::Serializable),
    ///     access_mode: None,
    /// };
    ///
    /// let (db, result) = db.transaction_with_config(config, |tx| {
    ///     Box::pin(async move {
    ///         // Serializable isolation
    ///         service.reconcile(tx, &scope).await
    ///     })
    /// }).await;
    /// ```
    pub async fn transaction_with_config<T, F>(
        self,
        config: TxConfig,
        f: F,
    ) -> (Self, anyhow::Result<T>)
    where
        T: Send + 'static,
        F: for<'a> FnOnce(
                &'a DbTx<'a>,
            )
                -> Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send + 'a>>
            + Send,
    {
        use sea_orm::{AccessMode, IsolationLevel};

        let isolation: Option<IsolationLevel> = config.isolation.map(Into::into);
        let access_mode: Option<AccessMode> = config.access_mode.map(Into::into);

        let txn = match self
            .handle
            .sea_internal_ref()
            .begin_with_config(isolation, access_mode)
            .await
        {
            Ok(t) => t,
            Err(e) => return (self, Err(e.into())),
        };
        let tx = DbTx { tx: &txn };

        // Run the closure with the transaction guard set
        let res = with_tx_guard(f(&tx)).await;

        match res {
            Ok(v) => match txn.commit().await {
                Ok(()) => (self, Ok(v)),
                Err(e) => (self, Err(e.into())),
            },
            Err(e) => {
                _ = txn.rollback().await;
                (self, Err(e))
            }
        }
    }

    /// Return database engine identifier for logging/tracing.
    #[must_use]
    pub fn db_engine(&self) -> &'static str {
        use sea_orm::{ConnectionTrait, DbBackend};

        match self.handle.sea_internal_ref().get_database_backend() {
            DbBackend::Postgres => "postgres",
            DbBackend::MySql => "mysql",
            DbBackend::Sqlite => "sqlite",
        }
    }
}

/// Non-transactional database runner.
///
/// This type borrows from a [`Db`] and can be used to execute queries outside
/// of a transaction context.
///
/// # Security
///
/// - NOT `Clone`: Cannot be duplicated
/// - Borrows from `Db`: While `DbConn` exists, the `Db` cannot start a transaction
/// - Cannot be constructed by user code: Only `Db::conn()` creates it
///
/// # Example
///
/// ```ignore
/// let conn = db.conn()?;
///
/// let users = Entity::find()
///     .secure()
///     .scope_with(&scope)
///     .all(&conn)
///     .await?;
/// ```
pub struct DbConn<'a> {
    pub(crate) conn: &'a DatabaseConnection,
}

impl std::fmt::Debug for DbConn<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbConn").finish_non_exhaustive()
    }
}

/// Transactional database runner.
///
/// This type is only available inside a transaction closure and represents
/// an active database transaction.
///
/// # Security
///
/// - NOT `Clone`: Cannot be duplicated
/// - Lifetime-bound: Cannot escape the transaction closure
/// - Cannot be constructed by user code: Only `Db::transaction()` creates it
///
/// # Example
///
/// ```ignore
/// let (db, result) = db.transaction(|tx| {
///     Box::pin(async move {
///         Entity::insert(model)
///             .secure()
///             .scope_with_model(&scope, &model)?
///             .exec(tx)
///             .await?;
///         Ok(())
///     })
/// }).await;
/// ```
pub struct DbTx<'a> {
    pub(crate) tx: &'a DatabaseTransaction,
}

impl std::fmt::Debug for DbTx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbTx").finish_non_exhaustive()
    }
}

// NOTE: tests for `Db` live under `libs/toolkit-db/tests/` so they can be gated per-backend
// without creating feature-specific unused-import warnings in this gear.
