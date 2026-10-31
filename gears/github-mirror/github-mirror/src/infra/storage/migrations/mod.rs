use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseBackend};

pub mod m0001_initial;
pub mod m0002_artifacts;

async fn run_schema(
    manager: &SchemaManager<'_>,
    postgres: &str,
    mysql: &str,
    sqlite: &str,
) -> Result<(), DbErr> {
    let schema = match manager.get_database_backend() {
        DatabaseBackend::Postgres => postgres,
        DatabaseBackend::MySql => mysql,
        DatabaseBackend::Sqlite => sqlite,
        other => {
            return Err(DbErr::Custom(format!(
                "migration has no DDL for database backend {other:?}"
            )));
        }
    };
    let conn = manager.get_connection();
    for statement in schema
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        conn.execute_unprepared(statement).await?;
    }
    Ok(())
}

async fn drop_tables(manager: &SchemaManager<'_>, tables: &[&str]) -> Result<(), DbErr> {
    let conn = manager.get_connection();
    for table in tables {
        conn.execute_unprepared(&format!("DROP TABLE IF EXISTS {table}"))
            .await?;
    }
    Ok(())
}

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m0001_initial::Migration),
            Box::new(m0002_artifacts::Migration),
        ]
    }
}
