//! A book's description and the delete of an unused book (phase 7, run 7.2) on Postgres, tables in
//! schema `bss`: the forward migration `m20260928_000015_book_description` over a deployed
//! database, books seeded; a lost race of the delete is a 409, never a 500. The `SQLite` twin is
//! `book_writes.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod entry_support;
mod pg_support;
mod schema_dump;

use bss_pricing::infra::storage::{RepoError, repo::book_repo};
use bss_pricing::module::BssPricingGear;
use entry_support::{Script, app_for, request, state_on, user_of};
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::DBProvider;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

const MIGRATION: &str = "m20260928_000015_book_description";
const TENANT: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0015_0002);
const OPEN: Uuid = Uuid::from_u128(0xb00c_0001);
const DATED: Uuid = Uuid::from_u128(0xb00c_0002);

async fn migrate(pg: &Pg, without: Option<&str>) -> Result<MigrationResult, MigrationError> {
    let chain = BssPricingGear::default()
        .migrations()
        .into_iter()
        .filter(|m| Some(m.name()) != without)
        .collect();
    run_migrations_for_testing(&pg.db().await, chain).await
}
async fn strings(pg: &Pg, sql: &str) -> Vec<String> {
    pg.raw()
        .await
        .query_all_raw(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .iter()
        .map(|row| row.try_get::<String>("", "v").unwrap())
        .collect()
}
async fn dump(pg: &Pg) -> Vec<String> {
    schema_dump::postgres_dump(&pg.raw().await)
        .await
        .lines()
        .map(str::to_owned)
        .collect()
}
async fn rows(pg: &Pg) -> Vec<Value> {
    strings(
        pg,
        "SELECT row_to_json(b)::text AS v FROM bss.pricing_price_book b ORDER BY id",
    )
    .await
    .iter()
    .map(|r| serde_json::from_str(r).unwrap())
    .collect()
}
async fn seeded() -> Pg {
    let pg = Pg::empty().await;
    let before = migrate(&pg, Some(MIGRATION)).await.unwrap();
    assert!(
        !before.applied_names.iter().any(|n| n == MIGRATION),
        "the chain before this run"
    );
    for (id, code, from, until) in [
        (OPEN, "open", "NULL", "NULL"),
        (DATED, "dated", "'2027-01-01'", "'2027-01-31'"),
    ] {
        pg.raw()
            .await
            .execute_raw(Statement::from_string(
                DbBackend::Postgres,
                format!(
                    "INSERT INTO bss.pricing_price_book (id, tenant_id, code, name, currency, \
                     valid_from, valid_until, version, created_at, updated_at) VALUES \
                     ('{id}'::uuid, '{TENANT}'::uuid, '{code}', 'Book {code}', 'EUR', {from}, \
                     {until}, 3, '2026-09-01T09:00:00.123456Z', '2026-09-02T10:00:00Z')"
                ),
            ))
            .await
            .unwrap();
    }
    pg
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_the_forward_migration_adds_the_description_and_keeps_every_book() {
    let pg = seeded().await;
    let dump_before = dump(&pg).await;
    let rows_before = rows(&pg).await;
    assert_eq!(rows_before.len(), 2);

    let result = migrate(&pg, None).await.unwrap();
    assert_eq!(result.applied_names, [MIGRATION], "only 000015 was pending");

    let dump_after = dump(&pg).await;
    let removed: Vec<&String> = dump_before
        .iter()
        .filter(|l| !dump_after.contains(l))
        .collect();
    let added: Vec<&String> = dump_after
        .iter()
        .filter(|l| !dump_before.contains(l))
        .collect();
    let show = |lines: &[&String]| {
        lines
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    };
    eprintln!(
        "000015 Postgres dump diff\nremoved:\n  {}\nadded:\n  {}",
        show(&removed),
        show(&added)
    );
    assert!(removed.is_empty(), "{removed:#?}");
    assert_eq!(
        added,
        ["COLUMN bss.pricing_price_book description text NULL DEFAULT -"]
    );
    let rows_after: Vec<Value> = rows(&pg)
        .await
        .into_iter()
        .map(|mut row| {
            assert_eq!(
                row.as_object_mut().unwrap().remove("description"),
                Some(Value::Null)
            );
            row
        })
        .collect();
    assert_eq!(rows_after, rows_before, "every old column as it was");
    let fresh = Pg::applied().await;
    assert_eq!(dump_after, dump(&fresh).await, "upgraded and fresh agree");
    assert!(migrate(&pg, None).await.unwrap().applied_names.is_empty());

    // Through the application: the books read with no description, take one, and an unused one
    // is deleted at its version.
    let state = state_on(DBProvider::new(pg.db().await), Arc::new(Script::default())).await;
    let app = app_for(state, TENANT);
    let ctx = user_of(TENANT);
    let path = |id: Uuid| format!("/price-books/{id}");
    let (s, read, tag) = request(&app, &ctx, "GET", &path(DATED), json!({}), None, None).await;
    assert_eq!(s, 200, "{read}");
    assert_eq!(tag, "\"3\"");
    assert!(read["description"].is_null(), "{read}");
    assert_eq!(read["valid_from"], "2027-01-01");
    let (s, saved, _) = request(
        &app,
        &ctx,
        "PATCH",
        &path(DATED),
        json!({"description":"dated"}),
        Some("\"3\""),
        None,
    )
    .await;
    assert_eq!(s, 200, "{saved}");
    assert_eq!(saved["description"], "dated");
    let (s, b, _) = request(
        &app,
        &ctx,
        "DELETE",
        &path(OPEN),
        json!({}),
        Some("\"3\""),
        None,
    )
    .await;
    assert_eq!(s, 204, "{b}");
    let (s, list, _) = request(&app, &ctx, "GET", "/price-books", json!({}), None, None).await;
    assert_eq!(s, 200, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
}

/// A fresh chain, the production router over it, a user of `TENANT` and a new book.
async fn fresh_book(code: &str) -> (Pg, axum::Router, toolkit_security::SecurityContext, Uuid) {
    let pg = Pg::applied().await;
    let state = state_on(DBProvider::new(pg.db().await), Arc::new(Script::default())).await;
    let app = app_for(state, TENANT);
    let ctx = user_of(TENANT);
    let (s, book, _) = request(
        &app,
        &ctx,
        "POST",
        "/price-books",
        json!({"code":code,"name":code,"currency":"EUR"}),
        None,
        Some(code),
    )
    .await;
    assert_eq!(s, 201, "{book}");
    let id = book["id"].as_str().unwrap().parse().unwrap();
    (pg, app, ctx, id)
}
fn entry_row(book: Uuid) -> String {
    format!(
        "INSERT INTO bss.pricing_price_book_entry (id, tenant_id, book_id, sku_id, charge_kind, \
         period, model, dimension_key, invoice_line_override, reservation_id, reference_state, \
         version, created_at, updated_at) VALUES ('{}'::uuid, '{TENANT}'::uuid, '{book}'::uuid, \
         '{}'::uuid, 'usage', NULL, 'per_unit', NULL, NULL, '{}'::uuid, 'confirmed', 1, now(), \
         now())",
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7()
    )
}

/// The foreign keys decide a race the door's reads did not see: the book's delete that meets a
/// referencing row is the matching 409 (the entry's key `BOOK_HAS_ENTRIES`, a revision's
/// `BOOK_IN_PLAN`), never a 500.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_a_referenced_book_is_refused_by_its_foreign_key_as_a_conflict() {
    let (pg, app, ctx, book) = fresh_book("entry").await;
    pg.raw()
        .await
        .execute_raw(Statement::from_string(DbBackend::Postgres, entry_row(book)))
        .await
        .unwrap();
    let provider = DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let scope = AccessScope::for_tenant(TENANT);
    let refused = book_repo::delete(&provider.conn().unwrap(), &scope, TENANT, book, 1)
        .await
        .unwrap_err();
    assert!(
        matches!(
            refused,
            RepoError::Conflict {
                code: "BOOK_HAS_ENTRIES"
            }
        ),
        "{refused:?}"
    );
    let (s, plan, _) = request(
        &app,
        &ctx,
        "POST",
        "/price-books",
        json!({"code":"plan","name":"plan","currency":"EUR"}),
        None,
        Some("plan-book"),
    )
    .await;
    assert_eq!(s, 201, "{plan}");
    let planned: Uuid = plan["id"].as_str().unwrap().parse().unwrap();
    let (s, b, _) = request(
        &app,
        &ctx,
        "POST",
        "/plans",
        json!({"code":"P","name":"p","book_id":planned}),
        None,
        Some("plan"),
    )
    .await;
    assert_eq!(s, 201, "{b}");
    let refused = book_repo::delete(&provider.conn().unwrap(), &scope, TENANT, planned, 1)
        .await
        .unwrap_err();
    assert!(
        matches!(
            refused,
            RepoError::Conflict {
                code: "BOOK_IN_PLAN"
            }
        ),
        "{refused:?}"
    );
}

/// An entry written in a transaction still open when the delete runs: the delete waits on the
/// book's row, and once the entry commits it fails on the entry's foreign key, which is 409
/// `BOOK_HAS_ENTRIES` (probed in run 7.2: without the key's mapping this answer is a 500), and the
/// book stays.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_a_delete_that_loses_a_race_to_an_entry_is_409() {
    let (pg, app, ctx, book) = fresh_book("race").await;
    let raw = pg.raw().await;
    let open = raw.begin().await.unwrap();
    open.execute_raw(Statement::from_string(DbBackend::Postgres, entry_row(book)))
        .await
        .unwrap();
    let (app2, ctx2) = (app.clone(), ctx.clone());
    let delete = tokio::spawn(async move {
        request(
            &app2,
            &ctx2,
            "DELETE",
            &format!("/price-books/{book}"),
            json!({}),
            Some("\"1\""),
            None,
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    open.commit().await.unwrap();
    let (s, b, _) = delete.await.unwrap();
    assert_eq!(s, 409, "{b}");
    assert!(b.to_string().contains("BOOK_HAS_ENTRIES"), "{b}");
    let (s, read, _) = request(
        &app,
        &ctx,
        "GET",
        &format!("/price-books/{book}"),
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(s, 200, "{read}");
    assert_eq!(read["stats"]["entries"], 1);
}
