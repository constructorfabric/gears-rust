//! D-522 on Postgres: 000023 adds the book's archive mark and widens the entry's
//! `reference_state` with `released` and the reference op's `kind` with `release`, and reverses;
//! and a book is archived, listed and unarchived through its doors on the native engine. The
//! `SQLite` twins are the migration's `_tests.rs` and `book_archive.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod pg_support;
mod plan_support;

use bss_pricing::module::BssPricingGear;
use bss_products_sdk::models::{ReferenceState, SkuType};
use pg_support::Pg;
use plan_support::Catalog;
use plan_support::entry_support::{app_for, request, state_on, user_of};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use sea_orm_migration::SchemaManager;
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::DBProvider;
use uuid::Uuid;

const MIGRATION: &str = "m20261003_000023_book_archive";
const TENANT: Uuid = Uuid::from_u128(0x23);
const BOOK: Uuid = Uuid::from_u128(0x2301);
const ENTRY: Uuid = Uuid::from_u128(0x2302);
const OP: Uuid = Uuid::from_u128(0x2330);
const AUTHOR: Uuid = Uuid::from_u128(0x2340);

fn q(id: Uuid) -> String {
    format!("'{id}'")
}

async fn exec(pg: &Pg, sql: &str) {
    pg.raw()
        .await
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            sql.to_owned(),
        ))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn try_exec(pg: &Pg, sql: &str) -> Result<(), sea_orm::DbErr> {
    pg.raw()
        .await
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            sql.to_owned(),
        ))
        .await
        .map(|_| ())
}

async fn strings(pg: &Pg, sql: &str) -> Vec<String> {
    pg.raw()
        .await
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            sql.to_owned(),
        ))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .iter()
        .map(|row| row.try_get::<String>("", "v").unwrap())
        .collect()
}

async fn step(pg: &Pg, down: bool) -> Result<(), sea_orm::DbErr> {
    let migration = BssPricingGear::default()
        .migrations()
        .into_iter()
        .find(|m| m.name() == MIGRATION)
        .expect("000023 is in the chain");
    let conn = pg.raw().await;
    let manager = SchemaManager::new(&conn);
    if down {
        migration.down(&manager).await
    } else {
        migration.up(&manager).await
    }
}

async fn columns(pg: &Pg) -> Vec<String> {
    strings(
        pg,
        "SELECT column_name::text AS v FROM information_schema.columns WHERE table_schema = 'bss' \
         AND table_name = 'pricing_price_book' ORDER BY column_name",
    )
    .await
}

/// The CHECK definitions of the two widened columns.
async fn checks(pg: &Pg) -> Vec<String> {
    strings(
        pg,
        "SELECT conname::text || ': ' || pg_get_constraintdef(oid) AS v FROM pg_constraint \
         WHERE conname IN ('pricing_price_book_entry_reference_state_check', \
         'pricing_reference_op_kind_check') ORDER BY conname",
    )
    .await
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_000023_widens_the_checks_adds_the_mark_and_reverses() {
    let pg = Pg::empty().await;
    let chain = BssPricingGear::default()
        .migrations()
        .into_iter()
        .filter(|m| m.name() != MIGRATION)
        .collect();
    toolkit_db::migration_runner::run_migrations_for_testing(&pg.db().await, chain)
        .await
        .unwrap();
    let now = "'2026-10-03T00:00:00Z'";
    for sql in [
        format!(
            "INSERT INTO bss.pricing_price_book (id,tenant_id,code,name,currency,version,created_at,updated_at) VALUES ({},{},'eur','eur','EUR',1,{now},{now})",
            q(BOOK),
            q(TENANT)
        ),
        format!(
            "INSERT INTO bss.pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,period,reservation_id,reference_state,version,created_at,updated_at,model) VALUES ({},{},{},{},'recurring','month',{},'confirmed',1,{now},{now},'flat')",
            q(ENTRY),
            q(TENANT),
            q(BOOK),
            q(Uuid::from_u128(0x2341)),
            q(Uuid::from_u128(0x2342))
        ),
        format!(
            "INSERT INTO bss.pricing_reference_op (op_id,tenant_id,kind,ref_kind,ref_id,sku_id,state,attempts,next_attempt_at,created_by,created_at,updated_at) VALUES ({},{},'delete','price_book_entry',{},{},'done',0,{now},{},{now},{now})",
            q(OP),
            q(TENANT),
            q(ENTRY),
            q(Uuid::from_u128(0x2341)),
            q(AUTHOR)
        ),
    ] {
        exec(&pg, &sql).await;
    }
    let before_columns = columns(&pg).await;
    let before_checks = checks(&pg).await;
    assert_eq!(before_checks.len(), 2, "{before_checks:?}");
    let applied = toolkit_db::migration_runner::run_migrations_for_testing(
        &pg.db().await,
        BssPricingGear::default().migrations(),
    )
    .await
    .unwrap();
    assert_eq!(applied.applied_names, [MIGRATION]);
    let after = columns(&pg).await;
    for name in ["archived_at", "archived_by"] {
        assert!(after.iter().any(|c| c == name), "{after:?}");
    }
    let types = strings(
        &pg,
        "SELECT data_type::text AS v FROM information_schema.columns WHERE table_schema = 'bss' \
         AND table_name = 'pricing_price_book' AND column_name IN ('archived_at','archived_by') \
         ORDER BY column_name",
    )
    .await;
    assert_eq!(types, ["timestamp with time zone", "uuid"]);
    let widened = checks(&pg).await;
    assert!(widened[0].contains("'released'"), "{widened:?}");
    assert!(widened[1].contains("'release'"), "{widened:?}");
    // The archive mark is set and cleared as a pair.
    for (sql, what) in [
        (
            format!(
                "UPDATE bss.pricing_price_book SET archived_at = now() WHERE id = {}",
                q(BOOK)
            ),
            "an archive time with no archiver",
        ),
        (
            format!(
                "UPDATE bss.pricing_price_book SET archived_by = {} WHERE id = {}",
                q(AUTHOR),
                q(BOOK)
            ),
            "an archiver with no archive time",
        ),
    ] {
        assert!(try_exec(&pg, &sql).await.is_err(), "{what}: {sql}");
    }
    exec(
        &pg,
        &format!(
            "UPDATE bss.pricing_price_book SET archived_at = now(), archived_by = {} WHERE id = {}",
            q(AUTHOR),
            q(BOOK)
        ),
    )
    .await;
    exec(
        &pg,
        &format!(
            "UPDATE bss.pricing_price_book_entry SET reference_state = 'released' WHERE id = {}",
            q(ENTRY)
        ),
    )
    .await;
    exec(
        &pg,
        &format!(
            "UPDATE bss.pricing_reference_op SET kind = 'release' WHERE op_id = {}",
            q(OP)
        ),
    )
    .await;
    assert!(
        try_exec(
            &pg,
            &format!(
                "UPDATE bss.pricing_price_book_entry SET reference_state = 'gone' WHERE id = {}",
                q(ENTRY)
            )
        )
        .await
        .is_err()
    );
    {
        // As the runner runs it: in a transaction, which the refusal rolls back.
        use sea_orm::TransactionTrait;
        let migration = BssPricingGear::default()
            .migrations()
            .into_iter()
            .find(|m| m.name() == MIGRATION)
            .unwrap();
        let conn = pg.raw().await;
        let txn = conn.begin().await.unwrap();
        let refused = migration.down(&SchemaManager::new(&txn)).await;
        txn.rollback().await.unwrap();
        assert!(
            refused.is_err(),
            "down refuses while a released entry needs the wider check"
        );
    }
    assert_eq!(checks(&pg).await, widened);
    exec(
        &pg,
        &format!(
            "UPDATE bss.pricing_price_book_entry SET reference_state = 'confirmed' WHERE id = {}",
            q(ENTRY)
        ),
    )
    .await;
    exec(
        &pg,
        &format!(
            "UPDATE bss.pricing_reference_op SET kind = 'delete' WHERE op_id = {}",
            q(OP)
        ),
    )
    .await;
    step(&pg, true).await.unwrap();
    assert_eq!(columns(&pg).await, before_columns);
    assert_eq!(checks(&pg).await, before_checks);
    step(&pg, false).await.unwrap();
    assert!(columns(&pg).await.iter().any(|c| c == "archived_by"));
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_a_book_archives_releases_lists_and_unarchives() {
    let pg = Pg::applied().await;
    let catalog = Arc::new(Catalog::default());
    let state = state_on(DBProvider::new(pg.db().await), catalog.clone()).await;
    let tenant = Uuid::new_v4();
    let app = app_for(state, tenant);
    let ctx = user_of(tenant);
    let call = |method: &'static str, path: String, body: Value, tag: Option<String>| {
        let (app, ctx) = (app.clone(), ctx.clone());
        async move {
            request(
                &app,
                &ctx,
                method,
                &path,
                body,
                tag.as_deref(),
                Some(&Uuid::new_v4().to_string()),
            )
            .await
        }
    };
    let (s, book, _) = call(
        "POST",
        "/price-books".into(),
        json!({"code":"pg","name":"pg","currency":"EUR"}),
        None,
    )
    .await;
    assert_eq!(s, 201, "{book}");
    let book = book["id"].as_str().unwrap().to_owned();
    let sku = catalog.sku(SkuType::Recurring);
    let (s, entry, _) = call(
        "POST",
        format!("/price-books/{book}/entries"),
        json!({"sku_id": sku, "period": "month", "model": "per_unit"}),
        None,
    )
    .await;
    assert_eq!(s, 201, "{entry}");
    let entry: Uuid = entry["id"].as_str().unwrap().parse().unwrap();

    let (s, archived, tag) = call(
        "POST",
        format!("/price-books/{book}/archive"),
        json!({}),
        Some("\"1\"".into()),
    )
    .await;
    assert_eq!(s, 200, "{archived}");
    assert_eq!(tag, "\"2\"");
    let (_, read, _) = call(
        "GET",
        format!("/price-book-entries/{entry}"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(read["reference_state"], "released", "{read}");
    // The guard is dropped before the assertion runs (review RF-P item 9).
    let held = { catalog.refs.lock().unwrap()[&entry].1 };
    assert_eq!(held, ReferenceState::Released);
    let (_, list, _) = call("GET", "/price-books".into(), json!({}), None).await;
    assert_eq!(list["items"], json!([]), "{list}");
    let (_, list, _) = call(
        "GET",
        "/price-books?$filter=archived%20eq%20true".into(),
        json!({}),
        None,
    )
    .await;
    assert_eq!(list["items"][0]["id"], json!(book), "{list}");

    let (s, back, tag) = call(
        "POST",
        format!("/price-books/{book}/unarchive"),
        json!({}),
        Some("\"2\"".into()),
    )
    .await;
    assert_eq!(s, 200, "{back}");
    assert_eq!(tag, "\"3\"");
    assert_eq!(back["released_entries"], json!([]), "{back}");
    let (_, read, _) = call(
        "GET",
        format!("/price-book-entries/{entry}"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(read["reference_state"], "confirmed", "{read}");
}
