#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(all(feature = "integration", feature = "pg"))]
//! The migration runner over a restricted runtime login.
//!
//! A deployment applies a gear's migrations with an admin login and then runs the gear under a
//! login that may only use the migrated schema. The runner must therefore (1) boot without any
//! DDL when the history table already exists (`PostgreSQL` refuses `CREATE TABLE IF NOT EXISTS`
//! on schema-`CREATE` before it looks for the table) and (2) still fail closed when a migration
//! is pending or the history table is missing, because such a login must never apply DDL.

mod common;
use anyhow::Result;
use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude as mig;

struct CreateTable {
    name: &'static str,
    table: &'static str,
}
impl mig::MigrationName for CreateTable {
    fn name(&self) -> &str {
        self.name
    }
}
#[async_trait::async_trait]
impl mig::MigrationTrait for CreateTable {
    async fn up(&self, manager: &mig::SchemaManager) -> Result<(), mig::DbErr> {
        manager
            .get_connection()
            .execute_unprepared(&format!(
                "CREATE TABLE IF NOT EXISTS \"{}\" (id INTEGER PRIMARY KEY)",
                self.table
            ))
            .await?;
        Ok(())
    }
    async fn down(&self, _: &mig::SchemaManager) -> Result<(), mig::DbErr> {
        Ok(())
    }
}

fn first() -> Vec<Box<dyn mig::MigrationTrait>> {
    vec![Box::new(CreateTable {
        name: "m001_first",
        table: "restricted_first",
    })]
}

#[tokio::test]
async fn a_restricted_login_boots_on_a_migrated_schema_and_fails_closed_otherwise() -> Result<()> {
    const GEAR: &str = "restricted-login-gear";
    let dut = common::bring_up_postgres().await?;
    let admin = toolkit_db::connect_db(&dut.url, toolkit_db::ConnectOpts::default()).await?;
    let applied = toolkit_db::migration_runner::run_migrations_for_gear(&admin, GEAR, first())
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    assert_eq!(applied.applied, 1);

    // A login that may only read the migrated schema: no CREATE on the schema, SELECT on the
    // history table and the gear table.
    let raw = sea_orm::Database::connect(dut.url.clone()).await?;
    raw.execute_unprepared(
        "CREATE ROLE restricted_runtime LOGIN PASSWORD 'restricted' NOSUPERUSER NOCREATEDB NOCREATEROLE; \
         REVOKE CREATE ON SCHEMA public FROM PUBLIC; \
         GRANT USAGE ON SCHEMA public TO restricted_runtime; \
         GRANT SELECT ON restricted_first TO restricted_runtime",
    )
    .await?;
    let history: String = raw
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT table_name AS t FROM information_schema.tables WHERE table_name LIKE 'toolkit_migrations__restricted_login_gear__%'",
        ))
        .await?
        .expect("history table")
        .try_get("", "t")?;
    raw.execute_unprepared(&format!(
        "GRANT SELECT ON \"{history}\" TO restricted_runtime"
    ))
    .await?;
    let restricted_url = dut
        .url
        .replace("user:pass@", "restricted_runtime:restricted@");
    let restricted =
        toolkit_db::connect_db(&restricted_url, toolkit_db::ConnectOpts::default()).await?;

    // (1) Fully migrated: the runtime boots with no DDL, every migration skipped.
    let result = toolkit_db::migration_runner::run_migrations_for_gear(&restricted, GEAR, first())
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    assert_eq!((result.applied, result.skipped), (0, 1));

    // (2) A pending migration cannot be applied by the restricted login: fail closed.
    let mut pending = first();
    pending.push(Box::new(CreateTable {
        name: "m002_second",
        table: "restricted_second",
    }));
    let error =
        toolkit_db::migration_runner::run_migrations_for_gear(&restricted, GEAR, pending).await;
    assert!(error.is_err(), "a pending migration must not be applied");
    let present: Option<_> = raw
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT 1 AS present FROM information_schema.tables WHERE table_name = 'restricted_second'",
        ))
        .await?;
    assert!(present.is_none(), "no table was created");

    // (3) A gear whose history table is missing cannot have it created by the restricted login.
    let error = toolkit_db::migration_runner::run_migrations_for_gear(
        &restricted,
        "restricted-login-other-gear",
        first(),
    )
    .await;
    assert!(
        matches!(
            error,
            Err(toolkit_db::migration_runner::MigrationError::CreateTable { .. })
        ),
        "{error:?}"
    );
    Ok(())
}
