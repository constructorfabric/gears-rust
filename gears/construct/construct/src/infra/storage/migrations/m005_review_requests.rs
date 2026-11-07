use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// Creates the review request table.
///
/// @cpt-dod:cpt-cf-construct-dod-review-queue-storage:p1
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        let sql = ddl_for(backend).ok_or_else(|| {
            DbErr::Custom(format!(
                "migration has no DDL for database backend {backend:?}"
            ))
        })?;

        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("DROP TABLE IF EXISTS construct__review_requests;")
            .await?;
        Ok(())
    }
}

/// DDL for the review request table per backend. The unique key on the tenant
/// and `open_fact_id` allows one open request per fact: a closed request has
/// no `open_fact_id`, and every backend lets any number of rows hold none.
/// A partial index would say the same, but `MySQL` has none. The subject
/// index serves the subject's list, the hide and skip checks and erasure; the
/// state index serves the reviewers' list. A fact id is at most 200
/// characters, so it fits a `MySQL` key in `utf8mb4`.
fn ddl_for(backend: sea_orm::DatabaseBackend) -> Option<&'static str> {
    Some(match backend {
        sea_orm::DatabaseBackend::Postgres => {
            r"
CREATE TABLE IF NOT EXISTS construct__review_requests (
    id UUID NOT NULL PRIMARY KEY,
    tenant_id UUID NOT NULL,
    subject_id UUID NOT NULL,
    fact_id VARCHAR(200) NOT NULL,
    entity_kind TEXT NOT NULL,
    property TEXT NOT NULL,
    value TEXT NULL,
    comment TEXT NULL,
    state TEXT NOT NULL,
    open_fact_id VARCHAR(200) NULL,
    outcome TEXT NULL,
    reason TEXT NULL,
    claim_id UUID NULL,
    claimed_at TIMESTAMPTZ NULL,
    pending TEXT NULL,
    resolved_by UUID NULL,
    corrected_fact_id VARCHAR(200) NULL,
    created_at TIMESTAMPTZ NOT NULL,
    resolved_at TIMESTAMPTZ NULL,
    CONSTRAINT uq_construct__review_requests__open_fact UNIQUE (tenant_id, open_fact_id)
);
CREATE INDEX IF NOT EXISTS idx_construct__review_requests__subject ON construct__review_requests (tenant_id, subject_id, created_at);
CREATE INDEX IF NOT EXISTS idx_construct__review_requests__state ON construct__review_requests (tenant_id, state, created_at);
                "
        }
        sea_orm::DatabaseBackend::MySql => {
            r"
CREATE TABLE IF NOT EXISTS construct__review_requests (
    id BINARY(16) NOT NULL PRIMARY KEY,
    tenant_id BINARY(16) NOT NULL,
    subject_id BINARY(16) NOT NULL,
    fact_id VARCHAR(200) NOT NULL,
    entity_kind VARCHAR(64) NOT NULL,
    property TEXT NOT NULL,
    value TEXT NULL,
    comment TEXT NULL,
    state VARCHAR(16) NOT NULL,
    open_fact_id VARCHAR(200) NULL,
    outcome VARCHAR(16) NULL,
    reason TEXT NULL,
    claim_id BINARY(16) NULL,
    claimed_at TIMESTAMP(6) NULL,
    pending TEXT NULL,
    resolved_by BINARY(16) NULL,
    corrected_fact_id VARCHAR(200) NULL,
    created_at TIMESTAMP(6) NOT NULL,
    resolved_at TIMESTAMP(6) NULL,
    UNIQUE KEY uq_construct__review_requests__open_fact (tenant_id, open_fact_id),
    INDEX idx_construct__review_requests__subject (tenant_id, subject_id, created_at),
    INDEX idx_construct__review_requests__state (tenant_id, state, created_at)
) DEFAULT CHARACTER SET utf8mb4;
                "
        }
        sea_orm::DatabaseBackend::Sqlite => {
            r"
CREATE TABLE IF NOT EXISTS construct__review_requests (
    id TEXT NOT NULL PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    fact_id TEXT NOT NULL,
    entity_kind TEXT NOT NULL,
    property TEXT NOT NULL,
    value TEXT NULL,
    comment TEXT NULL,
    state TEXT NOT NULL,
    open_fact_id TEXT NULL,
    outcome TEXT NULL,
    reason TEXT NULL,
    claim_id TEXT NULL,
    claimed_at TEXT NULL,
    pending TEXT NULL,
    resolved_by TEXT NULL,
    corrected_fact_id TEXT NULL,
    created_at TEXT NOT NULL,
    resolved_at TEXT NULL,
    CONSTRAINT uq_construct__review_requests__open_fact UNIQUE (tenant_id, open_fact_id)
);
CREATE INDEX IF NOT EXISTS idx_construct__review_requests__subject ON construct__review_requests (tenant_id, subject_id, created_at);
CREATE INDEX IF NOT EXISTS idx_construct__review_requests__state ON construct__review_requests (tenant_id, state, created_at);
                "
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::ddl_for;
    use sea_orm::DatabaseBackend;

    const BACKENDS: [DatabaseBackend; 3] = [
        DatabaseBackend::Postgres,
        DatabaseBackend::MySql,
        DatabaseBackend::Sqlite,
    ];

    /// The refusal of a second open request is shown on `SQLite` by the store
    /// test `a_second_open_request_on_a_fact_is_refused_and_a_closed_one_is_not`.
    #[test]
    fn every_backend_keys_one_open_request_per_fact_and_indexes_the_lists() {
        for backend in BACKENDS {
            let sql = ddl_for(backend).expect("ddl");
            assert!(
                sql.contains("CREATE TABLE IF NOT EXISTS construct__review_requests ("),
                "{backend:?}"
            );
            let unique = if backend == DatabaseBackend::MySql {
                "UNIQUE KEY uq_construct__review_requests__open_fact (tenant_id, open_fact_id)"
            } else {
                "CONSTRAINT uq_construct__review_requests__open_fact UNIQUE (tenant_id, open_fact_id)"
            };
            assert!(sql.contains(unique), "{backend:?}");
            assert!(
                sql.contains("idx_construct__review_requests__subject"),
                "{backend:?}"
            );
            assert!(
                sql.contains("idx_construct__review_requests__state"),
                "{backend:?}"
            );
        }
    }

    #[test]
    fn mysql_ddl_pins_utf8mb4() {
        let sql = ddl_for(DatabaseBackend::MySql).expect("mysql ddl");
        assert!(sql.contains(") DEFAULT CHARACTER SET utf8mb4;"));
    }

    #[test]
    fn identifiers_are_within_63_bytes() {
        for backend in BACKENDS {
            let sql = ddl_for(backend).expect("ddl");
            for ident in sql.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                assert!(ident.len() <= 63, "identifier too long: {ident}");
            }
        }
    }
}
