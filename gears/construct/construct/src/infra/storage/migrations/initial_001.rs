use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

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
        conn.execute_unprepared("DROP TABLE IF EXISTS foundation_notes;")
            .await?;
        Ok(())
    }
}

/// DDL for the notes table per backend. `MySQL` stores UUIDs as `BINARY(16)`
/// because the driver binds `Uuid` as its 16 raw bytes.
fn ddl_for(backend: sea_orm::DatabaseBackend) -> Option<&'static str> {
    Some(match backend {
        sea_orm::DatabaseBackend::Postgres => {
            r"
CREATE TABLE IF NOT EXISTS foundation_notes (
    id UUID NOT NULL PRIMARY KEY,
    tenant_id UUID NOT NULL,
    text TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_foundation_notes_tenant ON foundation_notes (tenant_id);
                "
        }
        sea_orm::DatabaseBackend::MySql => {
            r"
CREATE TABLE IF NOT EXISTS foundation_notes (
    id BINARY(16) NOT NULL PRIMARY KEY,
    tenant_id BINARY(16) NOT NULL,
    text TEXT NOT NULL,
    INDEX idx_foundation_notes_tenant (tenant_id)
);
                "
        }
        sea_orm::DatabaseBackend::Sqlite => {
            r"
CREATE TABLE IF NOT EXISTS foundation_notes (
    id TEXT NOT NULL PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    text TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_foundation_notes_tenant ON foundation_notes (tenant_id);
                "
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::ddl_for;
    use sea_orm::DatabaseBackend;

    #[test]
    fn mysql_ddl_uses_binary_uuid_columns() {
        let sql = ddl_for(DatabaseBackend::MySql).expect("mysql ddl");
        assert!(sql.contains("id BINARY(16) NOT NULL PRIMARY KEY"));
        assert!(sql.contains("tenant_id BINARY(16) NOT NULL"));
        assert!(sql.contains("INDEX idx_foundation_notes_tenant (tenant_id)"));
        assert!(!sql.contains("VARCHAR"));
    }

    #[test]
    fn postgres_and_sqlite_ddl_are_unchanged() {
        assert!(
            ddl_for(DatabaseBackend::Postgres)
                .expect("postgres ddl")
                .contains("id UUID NOT NULL PRIMARY KEY")
        );
        assert!(
            ddl_for(DatabaseBackend::Sqlite)
                .expect("sqlite ddl")
                .contains("id TEXT NOT NULL PRIMARY KEY")
        );
    }
}
