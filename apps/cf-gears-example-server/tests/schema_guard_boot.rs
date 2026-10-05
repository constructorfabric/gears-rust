//! The schema guard at boot (pricing D-423, products P-D-195): a data root holding one legacy
//! table refuses to migrate, and the log names the gear and the table.
//!
//! `migrate` runs the same database phase `run` does before any gear starts, so the refusal is
//! the one a boot meets; it exits instead of serving, which is what lets a test observe it.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(feature = "bss-pricing")]

use std::process::Command;

use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

fn cf_gears_binary() -> String {
    std::env::var("CARGO_BIN_EXE_cf-gears-example-server")
        .or_else(|_| std::env::var("CARGO_BIN_EXE_CF_GEARS_EXAMPLE_SERVER"))
        .expect("CARGO_BIN_EXE_cf-gears-example-server must be set for tests")
}

/// Create `<home>/<gear>/<file>` holding one table, as a database left by the legacy chain.
async fn seed(home: &std::path::Path, gear: &str, file: &str, table: &str) {
    let dir = home.join(gear);
    std::fs::create_dir_all(&dir).unwrap();
    let dsn = format!("sqlite://{}?mode=rwc", dir.join(file).display());
    let conn = Database::connect(dsn).await.unwrap();
    conn.execute_raw(Statement::from_string(
        DbBackend::Sqlite,
        format!("CREATE TABLE {table} (id text PRIMARY KEY)"),
    ))
    .await
    .unwrap();
    conn.close().await.unwrap();
}

fn migrate(home: &std::path::Path) -> (bool, String) {
    let output = Command::new(cf_gears_binary())
        .env("APP__SERVER__HOME_DIR", home)
        .arg("--config")
        .arg("../../config/e2e-local.yaml")
        .arg("migrate")
        .output()
        .expect("failed to execute cf-gears-server");
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), log)
}

#[tokio::test]
async fn a_legacy_pricing_table_stops_the_boot_naming_the_gear_and_the_table() {
    let home = tempfile::tempdir().expect("temporary data root");
    seed(
        home.path(),
        "bss-pricing",
        "bss_pricing.db",
        "pricing_bundle",
    )
    .await;

    let (ok, log) = migrate(home.path());

    assert!(!ok, "a legacy pricing table must stop the boot:\n{log}");
    assert!(
        log.contains(
            "bss-pricing: this database holds a legacy bss-pricing schema (table pricing_bundle)"
        ),
        "the log names the gear and the table:\n{log}"
    );
}

#[tokio::test]
async fn a_legacy_products_table_stops_the_boot_naming_the_gear_and_the_table() {
    let home = tempfile::tempdir().expect("temporary data root");
    seed(
        home.path(),
        "bss-products",
        "bss_products.db",
        "products_product",
    )
    .await;

    let (ok, log) = migrate(home.path());

    assert!(!ok, "a legacy products table must stop the boot:\n{log}");
    assert!(
        log.contains(
            "bss-products: this database holds a legacy bss-products schema (table products_product)"
        ),
        "the log names the gear and the table:\n{log}"
    );
}
