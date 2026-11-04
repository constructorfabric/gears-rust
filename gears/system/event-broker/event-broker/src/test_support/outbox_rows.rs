use sea_orm::ConnectionTrait;
use sea_orm::sea_query::{Alias, Expr, ExprTrait, Query};

use crate::domain::outbox::{INGEST_QUEUE_NAME, OUTBOX_TABLE_PREFIX};

/// How many partitions the `default` outbox group's queue is registered with
/// in the database at `dsn`, read over a connection of its own from the
/// outbox's partitions table.
pub async fn outbox_queue_partitions(dsn: &str) -> i64 {
    let conn = sea_orm::Database::connect(dsn).await.expect("connect");
    conn.query_one(
        Query::select()
            .expr_as(Expr::col(Alias::new("queue")).count(), "n")
            .from(Alias::new(format!("{OUTBOX_TABLE_PREFIX}_partitions")))
            .and_where(Expr::col(Alias::new("queue")).eq(INGEST_QUEUE_NAME)),
    )
    .await
    .expect("read the outbox partitions")
    .expect("a count row")
    .try_get::<i64>("", "n")
    .expect("a count")
}
