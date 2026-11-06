//! Test-only fault seam: drops a table so its reads fail, to drive the paths
//! that handle an unreadable store.

use sea_orm::ConnectionTrait;

/// Drops `table` from the database at `dsn`, over a connection of its own, so
/// every later read of it fails while the rest of the database keeps working.
pub async fn drop_table(dsn: &str, table: &str) {
    let conn = sea_orm::Database::connect(dsn).await.expect("connect");
    conn.execute_unprepared(&format!("DROP TABLE {table}"))
        .await
        .unwrap_or_else(|err| panic!("drop {table}: {err}"));
    conn.close().await.expect("close");
}
