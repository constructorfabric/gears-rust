//! Internal helpers for executing SQL through `DBRunner`.

use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, ExecResult, QueryResult, Statement};

use crate::secure::{DBRunner, SeaOrmRunner};

/// Get the database backend from a runner.
pub(super) fn backend(runner: &impl DBRunner) -> DatabaseBackend {
    match runner.as_seaorm() {
        SeaOrmRunner::Conn(c) => c.get_database_backend(),
        SeaOrmRunner::Tx(t) => t.get_database_backend(),
    }
}

/// Execute a statement through a `DBRunner`.
pub(super) async fn exec(runner: &impl DBRunner, stmt: Statement) -> Result<ExecResult, DbErr> {
    match runner.as_seaorm() {
        SeaOrmRunner::Conn(c) => c.execute(stmt).await,
        SeaOrmRunner::Tx(t) => t.execute(stmt).await,
    }
}

/// Query a single row through a `DBRunner`.
pub(super) async fn query_one(
    runner: &impl DBRunner,
    stmt: Statement,
) -> Result<Option<QueryResult>, DbErr> {
    match runner.as_seaorm() {
        SeaOrmRunner::Conn(c) => c.query_one(stmt).await,
        SeaOrmRunner::Tx(t) => t.query_one(stmt).await,
    }
}

/// Query all rows through a `DBRunner`.
pub(super) async fn query_all(
    runner: &impl DBRunner,
    stmt: Statement,
) -> Result<Vec<QueryResult>, DbErr> {
    match runner.as_seaorm() {
        SeaOrmRunner::Conn(c) => c.query_all(stmt).await,
        SeaOrmRunner::Tx(t) => t.query_all(stmt).await,
    }
}
