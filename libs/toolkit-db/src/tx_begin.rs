//! Opening a [`crate::Db`] transaction, with the `SQLite` lock mode its database asks for.
//!
//! `SQLite` starts a plain `BEGIN` as a *deferred* transaction: it takes no lock
//! until its first statement, and a read first makes it a read transaction.
//! When that transaction later writes while another connection holds the write
//! lock, `SQLite` returns `SQLITE_BUSY` at once without calling the busy
//! handler, so the connection's `busy_timeout` never applies. Under write
//! contention (a request beside an outbox worker, two requests on one row) the
//! writer then fails on every retry for as long as the other one holds the
//! lock, and its caller reports a conflict that never happened.
//!
//! A database configured with `transaction_mode=immediate` (a gear's `params`,
//! or a DSN query parameter given to `connect_db`) avoids that: [`begin`] starts each of its
//! transactions that may write as `BEGIN IMMEDIATE`, which takes the write lock
//! up front and so waits for it under `busy_timeout`. A transaction declared
//! read-only keeps the deferred `BEGIN`: it never writes, and an immediate one
//! would queue it behind writers it does not conflict with. Nested
//! transactions are savepoints, where `SeaORM` ignores the mode.
//!
//! The default stays deferred: with immediate transactions only one that may
//! write is open at a time, so code that holds one open while another
//! connection writes (to interleave two transactions on purpose) waits until
//! `busy_timeout` and fails instead.
//!
//! See [Transactions — deferred, immediate, and exclusive](https://www.sqlite.org/lang_transaction.html#deferred_immediate_and_exclusive_transactions).

use sea_orm::{
    AccessMode, ConnectionTrait, DbBackend, DbErr, IsolationLevel, SqliteTransactionMode,
    TransactionOptions, TransactionTrait,
};

/// Begin a transaction on `conn`: `BEGIN IMMEDIATE` on `SQLite` when `immediate`
/// and the transaction is not read-only, otherwise the backend's plain `BEGIN`.
pub async fn begin<C>(
    conn: &C,
    isolation_level: Option<IsolationLevel>,
    access_mode: Option<AccessMode>,
    immediate: bool,
) -> Result<C::Transaction, DbErr>
where
    C: ConnectionTrait + TransactionTrait,
{
    conn.begin_with_options(TransactionOptions {
        isolation_level,
        access_mode,
        sqlite_transaction_mode: sqlite_mode(conn.get_database_backend(), access_mode, immediate),
    })
    .await
}

fn sqlite_mode(
    backend: DbBackend,
    access_mode: Option<AccessMode>,
    immediate: bool,
) -> Option<SqliteTransactionMode> {
    (immediate && backend == DbBackend::Sqlite && access_mode != Some(AccessMode::ReadOnly))
        .then_some(SqliteTransactionMode::Immediate)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCESS_MODES: [Option<AccessMode>; 3] = [
        None,
        Some(AccessMode::ReadWrite),
        Some(AccessMode::ReadOnly),
    ];

    #[test]
    fn immediate_sqlite_writers_begin_immediate() {
        for access_mode in [None, Some(AccessMode::ReadWrite)] {
            assert_eq!(
                sqlite_mode(DbBackend::Sqlite, access_mode, true),
                Some(SqliteTransactionMode::Immediate)
            );
        }
    }

    #[test]
    fn readers_deferred_databases_and_other_backends_keep_the_default() {
        assert_eq!(
            sqlite_mode(DbBackend::Sqlite, Some(AccessMode::ReadOnly), true),
            None
        );
        for access_mode in ACCESS_MODES {
            assert_eq!(sqlite_mode(DbBackend::Sqlite, access_mode, false), None);
            for backend in [DbBackend::Postgres, DbBackend::MySql] {
                for immediate in [false, true] {
                    assert_eq!(sqlite_mode(backend, access_mode, immediate), None);
                }
            }
        }
    }

    /// A writer that reads and then writes while `holder` keeps the write lock: a `Db`
    /// transaction with no retries.
    #[cfg(feature = "sqlite")]
    async fn read_then_write(db: crate::Db) -> Result<(), crate::DbError> {
        fn no_retry(_: &crate::DbError) -> Option<&DbErr> {
            None
        }
        db.transaction_with_retry_max(crate::secure::TxConfig::default(), 1, no_retry, |tx| {
            Box::pin(async move {
                tx.tx
                    .query_one_raw(sea_orm::Statement::from_string(
                        DbBackend::Sqlite,
                        "SELECT count(*) FROM t",
                    ))
                    .await?;
                tx.tx.execute_unprepared("INSERT INTO t VALUES (2)").await?;
                Ok(())
            })
        })
        .await
    }

    /// A database at `path` opened the two ways a gear can set `transaction_mode`: a DSN query
    /// parameter (`connect_db`) and its config `params` (`build_db`).
    #[cfg(feature = "sqlite")]
    async fn open_both_ways(path: &std::path::Path, mode: &str) -> [crate::Db; 2] {
        let dsn = format!(
            "sqlite://{}?mode=rwc&transaction_mode={mode}",
            path.display()
        );
        let by_dsn = crate::connect_db(&dsn, crate::ConnectOpts::default())
            .await
            .unwrap();
        let by_params = crate::build_db(
            crate::DbConnConfig {
                engine: Some(crate::config::DbEngineCfg::Sqlite),
                path: Some(path.to_owned()),
                params: Some(std::collections::HashMap::from([(
                    "transaction_mode".to_owned(),
                    mode.to_owned(),
                )])),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
        [by_dsn, by_params]
    }

    /// While another connection holds the write lock, a transaction that reads and then writes
    /// fails at once on a deferred database (`SQLITE_BUSY`, which the retry classifier calls
    /// contention), and on an immediate one waits in its `BEGIN` until the lock is released,
    /// then commits. Both ways of setting the mode.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn an_immediate_database_waits_for_the_write_lock() {
        for mode in ["deferred", "immediate"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("db.sqlite3");
            for (way, db) in ["dsn", "params"]
                .into_iter()
                .zip(open_both_ways(&path, mode).await)
            {
                let holder = crate::connect_db(
                    &format!("sqlite://{}?mode=rwc", path.display()),
                    crate::ConnectOpts::default(),
                )
                .await
                .unwrap()
                .sea_internal();
                holder
                    .execute_unprepared("CREATE TABLE IF NOT EXISTS t (x INTEGER)")
                    .await
                    .unwrap();
                let held = holder.begin().await.unwrap();
                held.execute_unprepared("INSERT INTO t VALUES (1)")
                    .await
                    .unwrap();

                let writer = tokio::spawn(read_then_write(db));
                if mode == "deferred" {
                    // Fails while the lock is still held: no timing involved.
                    let written = writer.await.unwrap();
                    assert!(
                        matches!(&written, Err(crate::DbError::Sea(e))
                            if crate::contention::is_retryable_contention(DbBackend::Sqlite, e)),
                        "{mode} via {way}: {written:?}"
                    );
                    held.commit().await.unwrap();
                } else {
                    // Still waiting while the lock is held, done once it is released.
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    assert!(!writer.is_finished(), "{mode} via {way}: did not wait");
                    held.commit().await.unwrap();
                    let written = writer.await.unwrap();
                    assert!(written.is_ok(), "{mode} via {way}: {written:?}");
                }
            }
        }
    }

    /// A DSN's `transaction_mode` is strict, as a gear's `params` is: a typo fails the connect
    /// instead of leaving the database's writers deferred.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn an_invalid_transaction_mode_in_a_dsn_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let dsn = format!(
            "sqlite://{}?mode=rwc&transaction_mode=immedate",
            dir.path().join("db.sqlite3").display()
        );
        let error = crate::connect_db(&dsn, crate::ConnectOpts::default())
            .await
            .unwrap_err();
        assert!(
            matches!(&error, crate::DbError::InvalidSqlitePragma { key, .. } if key == "transaction_mode"),
            "{error}"
        );
    }
}
