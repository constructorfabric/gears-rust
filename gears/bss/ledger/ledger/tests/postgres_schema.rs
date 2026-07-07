//! Postgres-only: running the migrator creates the `bss` schema.
//! Ignored by default; run with `cargo test -p bss-ledger -- --ignored`.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use sea_orm::{ConnectionTrait, Database, Statement};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

use bss_ledger::infra::storage::migrations::Migrator;

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn migrator_creates_bss_schema() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let db = Database::connect(&url).await.unwrap();

    Migrator::up(&db, None).await.unwrap();

    let row = db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            "SELECT schema_name FROM information_schema.schemata WHERE schema_name = 'bss'"
                .to_owned(),
        ))
        .await
        .unwrap();
    assert!(row.is_some(), "bss schema must exist after migration");
}
