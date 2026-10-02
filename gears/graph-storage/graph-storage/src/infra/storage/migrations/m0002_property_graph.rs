//! The SQL/PGQ property graph, generated from the one declaration every
//! `MATCH` uses (secure-orm ADR-0002, Policy 3) and applied conditionally on
//! the server major: `PostgreSQL` 19 is a probed backend capability, not a gear
//! requirement (gear ADR-0001 point 2). On an older server the migration is a
//! no-op and the gear serves traversal on the fallback backend, reporting
//! SQL/PGQ unavailable.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};
use toolkit_db::secure::pgq::PropertyGraph as _;

use crate::infra::storage::graph::KnowledgeGraph;

/// First server version that parses `CREATE PROPERTY GRAPH`.
pub const PGQ_MIN_SERVER_VERSION_NUM: i32 = 190_000;

#[derive(DeriveMigrationName)]
pub struct Migration;

async fn server_version_num(manager: &SchemaManager<'_>) -> Result<i32, DbErr> {
    let backend = manager.get_database_backend();
    let row = manager
        .get_connection()
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT current_setting('server_version_num')::int AS v",
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("server_version_num returned no row".into()))?;
    row.try_get::<i32>("", "v")
}

fn declaration_error(error: &toolkit_db::secure::ScopeError) -> DbErr {
    DbErr::Custom(format!(
        "property-graph declaration failed to build: {error}"
    ))
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let version = server_version_num(manager).await?;
        if version < PGQ_MIN_SERVER_VERSION_NUM {
            tracing::warn!(
                server_version_num = version,
                required = PGQ_MIN_SERVER_VERSION_NUM,
                "server does not support SQL/PGQ; skipping property-graph DDL \
                 (traversal will use the fallback backend)"
            );
            return Ok(());
        }

        let declaration =
            KnowledgeGraph::declaration().map_err(|error| declaration_error(&error))?;
        let drop_ddl = declaration
            .drop_statement()
            .map_err(|error| declaration_error(&error))?;
        let create_ddl = declaration
            .create_statement()
            .map_err(|error| declaration_error(&error))?;

        let connection = manager.get_connection();
        connection.execute_unprepared(&drop_ddl).await?;
        connection.execute_unprepared(&create_ddl).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let version = server_version_num(manager).await?;
        if version < PGQ_MIN_SERVER_VERSION_NUM {
            return Ok(());
        }
        let declaration =
            KnowledgeGraph::declaration().map_err(|error| declaration_error(&error))?;
        let drop_ddl = declaration
            .drop_statement()
            .map_err(|error| declaration_error(&error))?;
        manager
            .get_connection()
            .execute_unprepared(&drop_ddl)
            .await?;
        Ok(())
    }
}
