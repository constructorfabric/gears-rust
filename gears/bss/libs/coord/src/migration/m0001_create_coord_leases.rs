//! Migration — the `coord_leases` distributed-lease table (see [`crate::lease`]).
//!
//! Schema rationale:
//! * `key` is the PRIMARY KEY (no synthetic id) — saves an index and makes
//!   adding a second coordination domain a zero-schema change.
//! * `locked_until` defaults to the Unix epoch so the steal filter
//!   `WHERE locked_until < NOW()` works uniformly whether or not the row was
//!   ever held.
//! * `attempts` defaults to `0`; the acquire path bumps it on every steal, so a
//!   flapping cluster surfaces as a high-water value (alert on `attempts >= 3`).
//!
//! **PG schema qualification.** A consuming gear that keeps its domain DDL in a
//! named schema (e.g. the BSS gears' `bss`) constructs this with
//! [`Migration::in_schema`] so the PG `CREATE TABLE` is **qualified**
//! (`bss.coord_leases`) and lands in that schema regardless of the connection's
//! `search_path` order — matching the gear's other qualified domain tables (an
//! unqualified `CREATE TABLE` would otherwise land in whichever schema the
//! `search_path` lists first, e.g. `public`). [`Migration::unqualified`] creates
//! a bare `coord_leases` (`SQLite` — single namespace — or a single-schema PG
//! setup that resolves it via `search_path`).
//!
//! **Self-contained schema.** The toolkit migration runner applies migrations in
//! NAME order, and coord's `m0001_…` name sorts BEFORE a consumer gear's own
//! "create schema" migration — so coord runs first and cannot assume the schema
//! already exists. [`Migration::in_schema`]'s `up` therefore issues
//! `CREATE SCHEMA IF NOT EXISTS {schema}` before the `CREATE TABLE`, making it
//! safe wherever it lands in the run order (idempotent with the consumer's own
//! `IF NOT EXISTS` schema migration that runs afterwards).

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const MYSQL_NOT_SUPPORTED: &str =
    "coord migrations: MySQL is not supported (PostgreSQL/SQLite only)";

/// The `coord_leases` table migration. Carries an optional PG schema to qualify
/// the table into (see the module doc); `SQLite` is always single-namespace.
#[derive(DeriveMigrationName)]
pub struct Migration {
    /// `Some("bss")` ⇒ PG `CREATE TABLE bss.coord_leases`; `None` ⇒ unqualified
    /// (`search_path`-resolved). `SQLite` ignores it (single namespace).
    schema: Option<&'static str>,
}

impl Migration {
    /// Qualify the PG table into `schema` (e.g. `"bss"`): `CREATE TABLE
    /// {schema}.coord_leases` lands there regardless of `search_path` order.
    #[must_use]
    pub fn in_schema(schema: &'static str) -> Self {
        Self {
            schema: Some(schema),
        }
    }

    /// Unqualified `coord_leases` — resolves via the connection's `search_path`
    /// (`SQLite`, or a single-schema PG setup).
    #[must_use]
    pub fn unqualified() -> Self {
        Self { schema: None }
    }

    /// The (optionally schema-qualified) PG table reference.
    fn pg_table(&self) -> String {
        match self.schema {
            Some(s) => format!("{s}.coord_leases"),
            None => "coord_leases".to_owned(),
        }
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let conn = manager.get_connection();

        // For a schema-qualified PG table, ensure the schema exists FIRST. The
        // toolkit migration runner applies migrations in NAME order, and coord's
        // `m0001_…` name sorts BEFORE a consumer gear's own "create schema"
        // migration — so coord runs first and cannot assume the schema already
        // exists. `CREATE SCHEMA IF NOT EXISTS` is idempotent with the consumer's
        // own (also-`IF NOT EXISTS`) schema migration that runs afterwards.
        if backend == sea_orm::DatabaseBackend::Postgres
            && let Some(schema) = self.schema
        {
            conn.execute_unprepared(&format!("CREATE SCHEMA IF NOT EXISTS {schema};"))
                .await?;
        }

        let sql = match backend {
            // Qualified into the gear's schema (e.g. `bss.coord_leases`) so the
            // table lands there regardless of `search_path` order or which
            // migration created the schema.
            sea_orm::DatabaseBackend::Postgres => format!(
                "CREATE TABLE {} ( \
                    key TEXT PRIMARY KEY, \
                    locked_by UUID NULL, \
                    locked_until TIMESTAMPTZ NOT NULL DEFAULT 'epoch', \
                    attempts INTEGER NOT NULL DEFAULT 0 \
                );",
                self.pg_table()
            ),
            // SQLite has no native UUID / TIMESTAMPTZ and a single namespace:
            // sea_orm serialises `Uuid` to canonical TEXT and `DateTime<Utc>` to
            // ISO-8601 TEXT, so both columns are `TEXT`; the epoch default uses
            // the literal form the `DateTime<Utc>` mapper accepts on read.
            sea_orm::DatabaseBackend::Sqlite => "CREATE TABLE coord_leases ( \
                    key TEXT PRIMARY KEY, \
                    locked_by TEXT NULL, \
                    locked_until TEXT NOT NULL DEFAULT '1970-01-01 00:00:00+00:00', \
                    attempts INTEGER NOT NULL DEFAULT 0 \
                );"
            .to_owned(),
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
            }
        };

        conn.execute_unprepared(&sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let table = match backend {
            sea_orm::DatabaseBackend::Postgres => self.pg_table(),
            sea_orm::DatabaseBackend::Sqlite => "coord_leases".to_owned(),
            sea_orm::DatabaseBackend::MySql => {
                return Err(DbErr::Custom(MYSQL_NOT_SUPPORTED.to_owned()));
            }
        };
        manager
            .get_connection()
            .execute_unprepared(&format!("DROP TABLE IF EXISTS {table};"))
            .await?;
        Ok(())
    }
}
