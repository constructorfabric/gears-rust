#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a failed database setup or assertion is a test failure"
)]
//! Registration contention and isolation guarantees on real server backends.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement, TransactionTrait};
#[cfg(feature = "mysql")]
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::{ImageExt, runners::AsyncRunner};
#[cfg(feature = "mysql")]
use tokio::time::Instant;

use super::types::OutboxConfig;
use super::{Outbox, outbox_migrations};
use crate::migration_runner::run_migrations_for_testing;
use crate::{ConnectOpts, Db, connect_db, connect_db_with_metric_callback};

#[cfg(feature = "pg")]
#[tokio::test]
async fn registration_observes_postgres_concurrent_commit_without_changing_default_isolation() {
    let container = test_containers::postgres()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "registration")
        .start()
        .await
        .expect("start pinned PostgreSQL");
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let host = container.get_host().await.unwrap();
    let dsn = format!("postgres://user:pass@{host}:{port}/registration");
    let setup = connect_db(&dsn, options()).await.unwrap();
    run_migrations_for_testing(&setup, outbox_migrations())
        .await
        .unwrap();
    let first = Outbox::new_with_backend(OutboxConfig::default(), DbBackend::Postgres);
    first.register_queue(&setup, "orders", 4).await.unwrap();

    let (db, reads) = connect_with_concurrent_update(&dsn, DbBackend::Postgres).await;
    db.sea_internal()
        .execute_raw(Statement::from_string(
            DbBackend::Postgres,
            "SET default_transaction_isolation = 'repeatable read'",
        ))
        .await
        .unwrap();

    let second = Outbox::new_with_backend(OutboxConfig::default(), DbBackend::Postgres);
    assert!(second.all_partition_ids().is_empty());
    second.register_queue(&db, "orders", 4).await.unwrap();

    assert_registration_preserves_metadata(&db, &first, &second, &reads).await;

    // Pool size one makes the next transaction reuse the registration session.
    // Its default RR view must remain stable across another committed update.
    let next = db.sea_internal().begin().await.unwrap();
    let isolation: String = next
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            "SHOW transaction_isolation",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    assert_eq!(isolation, "repeatable read");
    assert_repeatable_read_snapshot(
        &db,
        &dsn,
        next,
        "SELECT attempts::bigint FROM toolkit_outbox_processor ORDER BY partition_id LIMIT 1",
    )
    .await;
}

#[cfg(feature = "mysql")]
#[tokio::test]
async fn registration_preserves_metadata_after_mariadb_concurrent_commit() {
    let container = test_containers::mariadb()
        .with_wait_for(WaitFor::message_on_stderr("port: 3306"))
        .with_exposed_port(ContainerPort::Tcp(3306))
        .with_env_var("MARIADB_ROOT_PASSWORD", "root")
        .with_env_var("MARIADB_USER", "user")
        .with_env_var("MARIADB_PASSWORD", "pass")
        .with_env_var("MARIADB_DATABASE", "registration")
        .start()
        .await
        .expect("start pinned MariaDB");
    let port = container.get_host_port_ipv4(3306).await.unwrap();
    let host = container.get_host().await.unwrap();
    let dsn = format!("mysql://user:pass@{host}:{port}/registration");
    let setup = authenticated_connection(&dsn).await;
    run_migrations_for_testing(&setup, outbox_migrations())
        .await
        .unwrap();
    let first = Outbox::new_with_backend(OutboxConfig::default(), DbBackend::MySql);
    first.register_queue(&setup, "orders", 4).await.unwrap();

    let (db, reads) = connect_with_concurrent_update(&dsn, DbBackend::MySql).await;
    let settings = db
        .sea_internal()
        .query_one_raw(Statement::from_string(
            DbBackend::MySql,
            "SELECT IF(@@innodb_snapshot_isolation, 'ON', 'OFF') AS snapshot_isolation, \
             @@tx_isolation AS isolation_level, IF(@@log_bin, 'ON', 'OFF') AS logging, @@binlog_format AS binlog_format",
        ))
        .await
        .unwrap()
        .expect("session settings row");
    assert_eq!(settings.try_get_by_index::<String>(0).unwrap(), "ON");
    assert_eq!(
        settings.try_get_by_index::<String>(1).unwrap(),
        "REPEATABLE-READ"
    );

    let logging = settings.try_get_by_index::<String>(2).unwrap();
    let binlog_format = settings.try_get_by_index::<String>(3).unwrap();
    assert!(
        logging == "OFF" || matches!(binlog_format.as_str(), "ROW" | "MIXED"),
        "read-committed writes require row-based binary logging: {logging}/{binlog_format}"
    );

    let second = Outbox::new_with_backend(OutboxConfig::default(), DbBackend::MySql);
    assert!(second.all_partition_ids().is_empty());
    second.register_queue(&db, "orders", 4).await.unwrap();

    let default_isolation = db
        .sea_internal()
        .query_one_raw(Statement::from_string(
            DbBackend::MySql,
            "SELECT @@tx_isolation AS isolation_level",
        ))
        .await
        .unwrap()
        .expect("default isolation row");
    assert_eq!(
        default_isolation.try_get_by_index::<String>(0).unwrap(),
        "REPEATABLE-READ",
        "registration must not change the connection default isolation"
    );

    assert_registration_preserves_metadata(&db, &first, &second, &reads).await;

    // The registration override must not leak to the next transaction on this
    // one-connection pool. A default RR snapshot keeps the pre-update value.
    let next = db.sea_internal().begin().await.unwrap();
    assert_repeatable_read_snapshot(
        &db,
        &dsn,
        next,
        "SELECT CAST(attempts AS SIGNED) FROM toolkit_outbox_processor ORDER BY partition_id LIMIT 1",
    )
    .await;
}

/// A TCP listener can precede completion of the image's authentication setup.
#[cfg(feature = "mysql")]
async fn authenticated_connection(dsn: &str) -> Db {
    let deadline = Instant::now() + Duration::from_mins(2);
    loop {
        match connect_db(dsn, options()).await {
            Ok(db) => return db,
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "MariaDB authentication did not become ready: {error}"
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

fn options() -> ConnectOpts {
    ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    }
}

async fn update_processors(dsn: &str, backend: DbBackend) {
    tokio::time::timeout(Duration::from_secs(10), async {
        // Create the connection on the writer's own reactor: the metric callback
        // blocks the registration reactor until this update commits.
        let writer = connect_db(dsn, options()).await.unwrap();
        writer
            .sea_internal()
            .execute_raw(Statement::from_string(
                backend,
                "UPDATE toolkit_outbox_processor SET attempts = attempts + 1",
            ))
            .await
            .unwrap();
    })
    .await
    .expect("the independent writer committed");
}

async fn count(db: &Db, table: &str, predicate: &str) -> i64 {
    db.sea_internal()
        .query_one_raw(Statement::from_string(
            db.backend(),
            format!("SELECT COUNT(*) FROM {table} {predicate}"),
        ))
        .await
        .unwrap()
        .expect("count row")
        .try_get_by_index(0)
        .unwrap()
}

async fn connect_with_concurrent_update(dsn: &str, backend: DbBackend) -> (Db, Arc<AtomicUsize>) {
    let reads = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&reads);
    let writer_dsn = dsn.to_owned();
    let db = connect_db_with_metric_callback(dsn, options(), move |info| {
        if info
            .statement
            .sql
            .starts_with("SELECT id FROM toolkit_outbox_partitions")
            && observed.fetch_add(1, Ordering::SeqCst) == 0
        {
            // The first SELECT established the registration snapshot. Commit
            // on a separate reactor before registration attempts its inserts.
            let writer_dsn = writer_dsn.clone();
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(update_processors(&writer_dsn, backend));
            })
            .join()
            .expect("the concurrent writer finished");
        }
    })
    .await
    .unwrap();
    (db, reads)
}

async fn assert_registration_preserves_metadata(
    db: &Db,
    first: &Outbox,
    second: &Outbox,
    reads: &AtomicUsize,
) {
    assert!(
        reads.load(Ordering::SeqCst) >= 1,
        "the concurrent commit was injected after the metadata read"
    );
    assert_eq!(second.all_partition_ids(), first.all_partition_ids());
    for table in [
        "toolkit_outbox_partitions",
        "toolkit_outbox_processor",
        "toolkit_outbox_vacuum_counter",
    ] {
        assert_eq!(count(db, table, "").await, 4, "complete {table}");
    }
    assert_eq!(
        count(
            db,
            "toolkit_outbox_processor",
            "WHERE attempts = 1 AND processed_seq = 0",
        )
        .await,
        4,
        "registration preserves every concurrent processor update"
    );
    for id in second.all_partition_ids() {
        assert_eq!(second.partition_to_queue(id).as_deref(), Some("orders"));
    }
}

/// The one-connection pool must reuse the registration session. A default RR
/// transaction must keep its snapshot across another committed processor update.
async fn assert_repeatable_read_snapshot(
    db: &Db,
    dsn: &str,
    next: DatabaseTransaction,
    read_sql: &str,
) {
    let read = Statement::from_string(db.backend(), read_sql);
    let before: i64 = next
        .query_one_raw(read.clone())
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    update_processors(dsn, db.backend()).await;
    let after: i64 = next
        .query_one_raw(read)
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    assert_eq!(before, 1);
    assert_eq!(
        after, before,
        "the next transaction must retain its default repeatable-read snapshot"
    );
    next.rollback().await.unwrap();
    assert_eq!(
        count(db, "toolkit_outbox_processor", "WHERE attempts = 2").await,
        4,
        "the snapshot test's writer committed every update"
    );
}
