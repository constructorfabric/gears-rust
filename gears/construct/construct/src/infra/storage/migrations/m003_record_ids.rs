use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// Creates the record identity table and retires the foundation's placeholder
/// note table.
///
/// @cpt-dod:cpt-cf-construct-dod-record-intake-storage:p1
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
        conn.execute_unprepared(DROP_PLACEHOLDER).await?;
        Ok(())
    }

    /// Drops the record identity table. The placeholder note table is not
    /// brought back; rolling back past `initial_001` drops it anyway.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("DROP TABLE IF EXISTS construct__record_ids;")
            .await?;
        Ok(())
    }
}

const DROP_PLACEHOLDER: &str = "DROP TABLE IF EXISTS construct__foundation_notes;";

/// DDL for the record identity table per backend. One row per tenant and
/// record identity. The identity (connector, provenance, version) can be
/// longer than a `MySQL` key allows, so the key holds its SHA-256 in hex,
/// `record_key`; the parts are kept beside it. The subject index serves
/// erasure, which removes a subject's rows.
fn ddl_for(backend: sea_orm::DatabaseBackend) -> Option<&'static str> {
    Some(match backend {
        sea_orm::DatabaseBackend::Postgres => {
            r"
CREATE TABLE IF NOT EXISTS construct__record_ids (
    tenant_id UUID NOT NULL,
    record_key CHAR(64) NOT NULL,
    connector TEXT NOT NULL,
    provenance TEXT NOT NULL,
    version TEXT NOT NULL,
    subject_id UUID NULL,
    received_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (tenant_id, record_key)
);
CREATE INDEX IF NOT EXISTS idx_construct__record_ids__subject ON construct__record_ids (tenant_id, subject_id);
                "
        }
        sea_orm::DatabaseBackend::MySql => {
            r"
CREATE TABLE IF NOT EXISTS construct__record_ids (
    tenant_id BINARY(16) NOT NULL,
    record_key CHAR(64) NOT NULL,
    connector TEXT NOT NULL,
    provenance TEXT NOT NULL,
    version TEXT NOT NULL,
    subject_id BINARY(16) NULL,
    received_at TIMESTAMP(6) NOT NULL,
    PRIMARY KEY (tenant_id, record_key),
    INDEX idx_construct__record_ids__subject (tenant_id, subject_id)
);
                "
        }
        sea_orm::DatabaseBackend::Sqlite => {
            r"
CREATE TABLE IF NOT EXISTS construct__record_ids (
    tenant_id TEXT NOT NULL,
    record_key TEXT NOT NULL,
    connector TEXT NOT NULL,
    provenance TEXT NOT NULL,
    version TEXT NOT NULL,
    subject_id TEXT NULL,
    received_at TEXT NOT NULL,
    PRIMARY KEY (tenant_id, record_key)
);
CREATE INDEX IF NOT EXISTS idx_construct__record_ids__subject ON construct__record_ids (tenant_id, subject_id);
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

    /// The refusal of a second identity is shown on `SQLite` by the repository
    /// test `a_second_insert_of_the_same_identity_is_a_repeat`.
    #[test]
    fn every_backend_keys_the_table_by_tenant_then_record_key() {
        for backend in BACKENDS {
            let sql = ddl_for(backend).expect("ddl");
            assert!(
                sql.contains("CREATE TABLE IF NOT EXISTS construct__record_ids ("),
                "{backend:?}"
            );
            assert!(
                sql.contains("PRIMARY KEY (tenant_id, record_key)"),
                "{backend:?}"
            );
            assert!(
                sql.contains("idx_construct__record_ids__subject"),
                "{backend:?}"
            );
        }
    }

    /// The placeholder table exists after `initial_001` and is gone after
    /// the whole chain, on a real `SQLite` database.
    #[tokio::test]
    async fn the_placeholder_note_table_is_dropped() {
        use sea_orm_migration::MigratorTrait;
        use sea_orm_migration::sea_orm::{ConnectionTrait, Database, Statement};

        let db = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite connects");
        let has_notes = |db: &sea_orm_migration::sea_orm::DatabaseConnection| {
            let db = db.clone();
            async move {
                db.query_one_raw(Statement::from_string(
                    db.get_database_backend(),
                    "SELECT name FROM sqlite_master WHERE type = 'table' \
                     AND name = 'construct__foundation_notes'",
                ))
                .await
                .expect("query sqlite_master")
                .is_some()
            }
        };

        super::super::Migrator::up(&db, Some(1))
            .await
            .expect("initial_001 applies");
        assert!(has_notes(&db).await, "initial_001 creates the note table");

        super::super::Migrator::up(&db, None)
            .await
            .expect("the rest applies");
        assert!(!has_notes(&db).await, "m003_record_ids drops it");
    }

    #[test]
    fn identifiers_are_namespaced_and_within_63_bytes() {
        for backend in BACKENDS {
            let sql = ddl_for(backend).expect("ddl");
            assert!(
                !sql.replace("construct__record_ids", "")
                    .contains(" record_ids")
            );
            for ident in sql.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                assert!(ident.len() <= 63, "identifier too long: {ident}");
            }
        }
    }
}
