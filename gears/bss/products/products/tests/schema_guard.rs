//! P-D-195 on `SQLite`: the chain refuses a legacy or stale products schema before it creates
//! anything, and lets a fresh database and today's database through.
//!
//! Every case drives the gear's real migration list through the toolkit runner (history table,
//! name sort, pending-only), on a file database a second, raw connection can shape first. The
//! Postgres twin is `postgres_schema_guard.rs`; pricing carries the same suite (D-423).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod guard_support;
#[path = "../../../pricing/pricing/tests/lite_support/mod.rs"]
mod lite_support;

use bss_products::gear::BssProductsGear;
use bss_products::infra::storage::migrations::m0000_products_refuse_a_legacy_or_stale_schema as guard;
use guard_support::{
    GUARD, LEGACY_CATEGORY_SQLITE, LEGACY_CHAIN_TABLES, LEGACY_SKU_SQLITE,
    SKU_REFERENCE_BEFORE_RENAME_SQLITE, STALE_REFERENCE, refusal,
};
use lite_support::Lite;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult};

/// A file database for this gear's migration list.
fn lite() -> Lite {
    Lite::new("products-guard-", || {
        BssProductsGear::default().migrations()
    })
}

fn refused(result: Result<MigrationResult, MigrationError>) -> String {
    let error = result.expect_err("the guard refuses this database");
    let text = error.to_string();
    assert!(
        text.contains(&format!("migration '{GUARD}' failed")),
        "the guard itself refused, not a later migration: {text}"
    );
    text
}

/// The legacy chain's tables minus every table a fresh chain creates, measured here and not
/// read from the guard: a name the guard's constant lost is still refused-or-red below.
async fn legacy_set() -> Vec<String> {
    let db = lite();
    db.migrate().await.unwrap();
    let fresh = db.tables().await;
    LEGACY_CHAIN_TABLES
        .iter()
        .filter(|t| !fresh.iter().any(|f| f == *t))
        .map(|t| (*t).to_owned())
        .collect()
}

/// (a) One legacy table, created before the chain ever ran, is refused by name — for every name
/// of the legacy set — and nothing of the gear is created.
#[tokio::test]
async fn every_legacy_table_alone_is_refused_before_the_gear_creates_anything() {
    let legacy_set = legacy_set().await;
    assert_eq!(legacy_set.len(), 35);
    for legacy in &legacy_set {
        let db = lite();
        db.exec(&[&format!("CREATE TABLE {legacy} (id text PRIMARY KEY)")])
            .await;

        let result = db.migrate().await;
        assert!(result.is_err(), "the legacy table {legacy} was not refused");
        let text = refused(result);

        let want = refusal("legacy", &format!("table {legacy}"));
        assert!(text.contains(&want), "{legacy}: {text}\nwanted: {want}");
        assert_eq!(
            db.tables_named("products_").await,
            vec![legacy.clone()],
            "the guard runs before every products migration"
        );
    }
}

/// (a) Several legacy tables are all named, sorted.
#[tokio::test]
async fn several_legacy_tables_are_named_together() {
    let db = lite();
    db.exec(&[
        "CREATE TABLE products_product (id text)",
        "CREATE TABLE products_approval (id text)",
    ])
    .await;

    let text = refused(db.migrate().await);

    assert!(
        text.contains(&refusal(
            "legacy",
            "tables products_approval, products_product"
        )),
        "{text}"
    );
}

/// (b) Today's chain, then `products_sku_reference` rebuilt with the CHECK it had before the
/// phase 2 rename edited `m20260925_000006` in place, and the guard pending again.
#[tokio::test]
async fn a_reference_table_whose_check_refuses_price_book_entry_is_stale() {
    let db = lite();
    db.migrate().await.unwrap();
    db.exec(&["DROP TABLE products_sku_reference"]).await;
    db.exec(SKU_REFERENCE_BEFORE_RENAME_SQLITE).await;
    db.forget(&[GUARD]).await;

    let text = refused(db.migrate().await);

    assert!(text.contains(&refusal("stale", STALE_REFERENCE)), "{text}");
}

/// (b, phase 4 review F2) The legacy chain's `products_category` — a name today's chain creates
/// too — left behind by a clean-up that dropped only what the refusal named, with its migration
/// pending: `m20260925_000001`'s `CREATE TABLE IF NOT EXISTS` would keep it and its index on `code`
/// fail with a raw SQL error. The guard refuses it first.
#[tokio::test]
async fn a_legacy_category_without_code_is_stale() {
    let db = lite();
    db.migrate().await.unwrap();
    db.exec(&["DROP TABLE products_category"]).await;
    db.exec(LEGACY_CATEGORY_SQLITE).await;
    db.forget(&[GUARD, CATEGORY_MIGRATION]).await;

    let text = refused(db.migrate().await);

    assert!(
        text.contains(&refusal("stale", "products_category without column code")),
        "{text}"
    );
}

/// (b, phase 4 review F2) The legacy chain's `products_sku` (`sku_code`, no `code`), with its
/// migration pending: the guard's refusal, not `m20260925_000002`'s.
#[tokio::test]
async fn a_legacy_sku_without_code_is_stale() {
    let db = lite();
    db.migrate().await.unwrap();
    db.exec(&["DROP TABLE products_sku_version", "DROP TABLE products_sku"])
        .await;
    db.exec(LEGACY_SKU_SQLITE).await;
    db.forget(&[GUARD, SKU_MIGRATION]).await;

    let text = refused(db.migrate().await);

    assert!(
        text.contains(&refusal("stale", "products_sku without column code")),
        "{text}"
    );
}

/// The migrations that create the two tables both chains name.
const CATEGORY_MIGRATION: &str = "m20260925_000001_create_products_category";
const SKU_MIGRATION: &str = "m20260925_000002_create_products_sku";

/// (c) A fresh database passes, the guard first.
#[tokio::test]
async fn a_fresh_database_passes_with_the_guard_first() {
    let db = lite();

    let result = db.migrate().await.unwrap();

    assert_eq!(result.applied_names[0], GUARD);
    assert_eq!(
        result.applied,
        BssProductsGear::default().migrations().len()
    );
}

/// (d) Today's chain applied, the guard pending again: it finds nothing and only it runs.
#[tokio::test]
async fn todays_database_with_the_guard_pending_passes() {
    let db = lite();
    db.migrate().await.unwrap();
    db.forget(&[GUARD]).await;

    let result = db.migrate().await.unwrap();

    assert_eq!(result.applied_names, vec![GUARD.to_owned()]);
}

/// (e, H1) No table a fresh chain creates can count as legacy: the guard's constant is exactly
/// the legacy chain's tables minus a fresh chain's, so it is disjoint from the fresh schema.
#[tokio::test]
async fn the_legacy_set_is_the_legacy_chain_minus_a_fresh_chain() {
    let db = lite();
    db.migrate().await.unwrap();
    let fresh = db.tables().await;

    assert_eq!(
        db.tables_named("products_").await.len(),
        12,
        "the fresh chain's products tables"
    );
    let in_both: Vec<&str> = LEGACY_CHAIN_TABLES
        .iter()
        .copied()
        .filter(|t| fresh.iter().any(|f| f == t))
        .collect();
    assert_eq!(
        in_both,
        [
            "products_approval_decision",
            "products_audit_log",
            "products_category",
            "products_idempotency",
            "products_sku",
        ],
        "not evidence (H1)"
    );
    assert_eq!(
        guard::LEGACY_TABLES,
        legacy_set().await,
        "legacy = chain minus fresh"
    );
    for legacy in guard::LEGACY_TABLES {
        assert!(
            !fresh.iter().any(|t| t == legacy),
            "{legacy} is legacy and also created by today's chain"
        );
    }
}
