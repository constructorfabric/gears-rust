use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// @cpt-dod:cpt-cf-construct-dod-subject-settings-storage:p1
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
        conn.execute_unprepared("DROP TABLE IF EXISTS construct__subject_settings;")
            .await?;
        Ok(())
    }
}

/// DDL for the subject settings table per backend. One row per tenant and
/// subject; the primary key leads with the tenant, so it also serves the
/// tenant filter of every scoped read. `MySQL` stores UUIDs as `BINARY(16)`
/// because the driver binds `Uuid` as its 16 raw bytes.
fn ddl_for(backend: sea_orm::DatabaseBackend) -> Option<&'static str> {
    Some(match backend {
        sea_orm::DatabaseBackend::Postgres => {
            r"
CREATE TABLE IF NOT EXISTS construct__subject_settings (
    tenant_id UUID NOT NULL,
    subject_id UUID NOT NULL,
    personalization_enabled BOOLEAN NOT NULL,
    erasure_in_progress BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (tenant_id, subject_id)
);
                "
        }
        sea_orm::DatabaseBackend::MySql => {
            r"
CREATE TABLE IF NOT EXISTS construct__subject_settings (
    tenant_id BINARY(16) NOT NULL,
    subject_id BINARY(16) NOT NULL,
    personalization_enabled BOOLEAN NOT NULL,
    erasure_in_progress BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (tenant_id, subject_id)
);
                "
        }
        sea_orm::DatabaseBackend::Sqlite => {
            r"
CREATE TABLE IF NOT EXISTS construct__subject_settings (
    tenant_id TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    personalization_enabled BOOLEAN NOT NULL,
    erasure_in_progress BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (tenant_id, subject_id)
);
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

    /// The refusal of a second row is shown on `SQLite` by the repository test
    /// `second_row_for_the_same_tenant_and_subject_is_refused`.
    #[test]
    fn every_backend_keys_the_table_by_tenant_then_subject() {
        for backend in BACKENDS {
            let sql = ddl_for(backend).expect("ddl");
            assert!(
                sql.contains("CREATE TABLE IF NOT EXISTS construct__subject_settings ("),
                "{backend:?}"
            );
            assert!(
                sql.contains("PRIMARY KEY (tenant_id, subject_id)"),
                "{backend:?}"
            );
        }
    }

    /// The behavior of the default is shown on `SQLite` by the repository
    /// test `erasure_flag_defaults_to_cleared_when_the_insert_leaves_it_out`;
    /// this only checks that the other backends declare the same default.
    #[test]
    fn every_backend_declares_the_erasure_default() {
        for backend in BACKENDS {
            let sql = ddl_for(backend).expect("ddl");
            let column = sql
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with("erasure_in_progress "))
                .unwrap_or_else(|| panic!("{backend:?}: no erasure column"));
            let words: Vec<&str> = column.trim_end_matches(',').split_whitespace().collect();
            assert_eq!(
                words,
                [
                    "erasure_in_progress",
                    "BOOLEAN",
                    "NOT",
                    "NULL",
                    "DEFAULT",
                    "FALSE"
                ],
                "{backend:?}"
            );
        }
    }

    #[test]
    fn mysql_ddl_uses_binary_uuid_columns() {
        let sql = ddl_for(DatabaseBackend::MySql).expect("mysql ddl");
        assert!(sql.contains("tenant_id BINARY(16) NOT NULL"));
        assert!(sql.contains("subject_id BINARY(16) NOT NULL"));
        assert!(!sql.contains("VARCHAR"));
    }

    #[test]
    fn identifiers_are_namespaced_and_within_63_bytes() {
        for backend in BACKENDS {
            let sql = ddl_for(backend).expect("ddl");
            assert!(
                !sql.replace("construct__subject_settings", "")
                    .contains("subject_settings")
            );
            for ident in sql.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                assert!(ident.len() <= 63, "identifier too long: {ident}");
            }
        }
    }

    /// The `SQLite` DDL runs twice on one database without an error: a row
    /// stored after the first run is still there after the second, and the
    /// table keeps its primary key and column default. `MySQL` and
    /// `PostgreSQL` use the same `IF NOT EXISTS` form, asserted as text above.
    #[tokio::test]
    async fn sqlite_ddl_is_safe_to_run_twice() {
        use sea_orm::{ConnectionTrait, Database, Statement};

        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory database");
        let ddl = ddl_for(DatabaseBackend::Sqlite).expect("sqlite ddl");

        let insert = "INSERT INTO construct__subject_settings \
                      (tenant_id, subject_id, personalization_enabled) VALUES ('t', 's', 0)";
        conn.execute_unprepared(ddl).await.expect("first run");
        conn.execute_unprepared(insert)
            .await
            .expect("row before the second run");
        conn.execute_unprepared(ddl).await.expect("second run");

        let select = |subject: &str| {
            Statement::from_string(
                DatabaseBackend::Sqlite,
                format!(
                    "SELECT personalization_enabled, erasure_in_progress \
                     FROM construct__subject_settings \
                     WHERE tenant_id = 't' AND subject_id = '{subject}'"
                ),
            )
        };
        let kept = conn
            .query_one_raw(select("s"))
            .await
            .expect("select")
            .expect("the row survives the second run");
        assert!(
            !kept
                .try_get::<bool>("", "personalization_enabled")
                .expect("personalization flag"),
            "the row keeps its value"
        );

        // A row stored after the second run, without the erasure flag, takes
        // the column default the table has now.
        conn.execute_unprepared(
            "INSERT INTO construct__subject_settings \
             (tenant_id, subject_id, personalization_enabled) VALUES ('t', 'u', 1)",
        )
        .await
        .expect("row after the second run");
        let fresh = conn
            .query_one_raw(select("u"))
            .await
            .expect("select")
            .expect("the new row");
        assert!(
            !fresh
                .try_get::<bool>("", "erasure_in_progress")
                .expect("erasure flag"),
            "the column default survives the second run"
        );
        assert!(
            conn.execute_unprepared(insert).await.is_err(),
            "the primary key survives the second run"
        );
    }
}
