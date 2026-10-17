//! Drop the SQL/PGQ property graph `kb` where an earlier release created it.
//!
//! Only a database created on `PostgreSQL` 19 beta 1–3 has it: `m0002` used to
//! create it there, and no other server could. The gear no longer queries it
//! (gears-rust #5267), and leaving it in place costs on the way out of the
//! beta: `pg_dump` writes `CREATE PROPERTY GRAPH`, which `PostgreSQL` 18 and 19
//! GA refuse, and the graph holds a dependency on the columns it maps.
//!
//! Whether it exists is read from the catalog rather than by attempting the
//! drop. `DROP PROPERTY GRAPH` is a syntax error on every released server,
//! and the migration runner holds one transaction, which a failed statement
//! would abort. A property graph is a `pg_class` row of kind `g`; on a server
//! without SQL/PGQ the query finds nothing and the migration does nothing. The
//! schema is matched by name against `pg_namespace`, not cast with
//! `::regnamespace`, which would case-fold a quoted mixed-case schema name.
//!
//! `down` does not recreate it: the declaration it was built from is gone,
//! and nothing would read it.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        let backend = manager.get_database_backend();
        let present = connection
            .query_one_raw(Statement::from_string(
                backend,
                "SELECT 1 AS present FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.relname = 'kb' AND c.relkind = 'g' \
                   AND n.nspname = current_schema()",
            ))
            .await?
            .is_some();
        if present {
            connection
                .execute_unprepared("DROP PROPERTY GRAPH kb")
                .await?;
            tracing::info!("dropped the retired SQL/PGQ property graph `kb`");
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
