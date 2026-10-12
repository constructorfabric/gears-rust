//! D-522 on `SQLite`: a finished price book is archived, and archiving it releases its entries'
//! SKU references (ask 58).
//!
//! - `POST /price-books/{id}/archive` under If-Match: refused while a plan revision that is not
//!   superseded names the book (`BOOK_IN_PLAN`) or a price of it is pending (`BOOK_HAS_PENDING`).
//!   Each entry's reference is released through a `release` op, driven after the commit; the
//!   ticker finishes a release the door could not.
//! - The archived book's entries and prices are read-only: `BOOK_ARCHIVED`.
//! - `POST /price-books/{id}/unarchive` re-reserves each released entry whose SKU still admits a
//!   reference and lists the entries it could not. It is refused (`ENTRY_RELEASE_PENDING`) while a
//!   release or a re-reservation of an entry is still open.
//! - `GET /price-books` hides an archived book unless asked `archived eq true`.
//!
//! The Postgres twin is `postgres_book_archive.rs`. Products' side of ask 58 (the SKU then
//! retires and archives) runs in products' `tests/book_archive_e2e.rs`, where both gears run.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod book_support;
mod plan_support;
use book_support::bare_revision;
use bss_pricing::infra::{
    reference_ticker::Ticker,
    reference_work::Clock,
    storage::repo::{price_book_entry_repo, price_repo},
};
use bss_products_sdk::models::{Lifecycle, ReferenceState, SkuType};
use plan_support::{
    Catalog, Fixture, entry_support, id_of, item, ops_for, plan, publish, raw, scope,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use uuid::Uuid;

struct Later;
impl Clock for Later {
    fn now(&self) -> time::OffsetDateTime {
        time::OffsetDateTime::now_utc() + time::Duration::days(2)
    }
}

fn future(days: i64) -> String {
    (time::OffsetDateTime::now_utc().date() + time::Duration::days(days)).to_string()
}

async fn quorum(f: &Fixture, quorum: u32) {
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let (s, b, _) = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"quorum": quorum}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
}

async fn new_book(f: &Fixture, code: &str) -> Uuid {
    plan_support::book(f, code).await
}

/// A recurring entry of `book` for `sku`, through its door: reserved, written and confirmed.
async fn door_entry(f: &Fixture, book: Uuid, sku: Uuid) -> Uuid {
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/price-books/{book}/entries"),
            json!({"sku_id": sku, "period": "month", "model": "per_unit"}),
            None,
            Some(&Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(b["reference_state"], "confirmed", "{b}");
    id_of(&b["id"])
}

/// An approved price of `entry` that starts `days` from today, written through the repository.
async fn approved_from(f: &Fixture, entry: Uuid, days: i64) -> Uuid {
    let tenant = f.ctx.subject_tenant_id();
    let conn = f.db.conn().unwrap();
    let e = price_book_entry_repo::find(&conn, &scope(f), tenant, entry)
        .await
        .unwrap()
        .unwrap();
    let mut price = entry_support::price(&e);
    price.state = "approved".into();
    price.effective_from = time::OffsetDateTime::now_utc().date() + time::Duration::days(days);
    price_repo::insert(&conn, &scope(f), price)
        .await
        .unwrap()
        .id
}

/// An approved price of `entry`, written through the repository.
async fn approved(f: &Fixture, entry: Uuid) -> Uuid {
    let tenant = f.ctx.subject_tenant_id();
    let conn = f.db.conn().unwrap();
    let e = price_book_entry_repo::find(&conn, &scope(f), tenant, entry)
        .await
        .unwrap()
        .unwrap();
    let mut price = entry_support::price(&e);
    price.state = "approved".into();
    price.effective_from = time::OffsetDateTime::now_utc().date() - time::Duration::days(10);
    price_repo::insert(&conn, &scope(f), price)
        .await
        .unwrap()
        .id
}

/// A draft price of `entry`, through its door.
async fn draft(f: &Fixture, entry: Uuid) -> Value {
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/price-book-entries/{entry}/prices"),
            json!({"price": {"rate": "0.10"}, "eligibility": "all", "effective_from": future(40)}),
            None,
            Some(&Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    b["items"][0].clone()
}

/// One price of `entry` as the entry's price list reads it.
async fn price_of(f: &Fixture, entry: Uuid, price: &str) -> Value {
    let (s, b, _) = f
        .call(
            "GET",
            &format!("/price-book-entries/{entry}/prices"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    b["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == price)
        .unwrap_or_else(|| panic!("{price} is listed: {b}"))
        .clone()
}

async fn book_tag(f: &Fixture, book: Uuid) -> String {
    let (s, b, tag) = f
        .call(
            "GET",
            &format!("/price-books/{book}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    tag
}

async fn mark(f: &Fixture, book: Uuid, what: &str, tag: Option<&str>) -> (u16, Value, String) {
    f.call(
        "POST",
        &format!("/price-books/{book}/{what}"),
        json!({}),
        tag,
        None,
    )
    .await
}

/// Archive at the book's current tag; it must answer 200.
async fn archive(f: &Fixture, book: Uuid) -> Value {
    let tag = book_tag(f, book).await;
    let (s, b, _) = mark(f, book, "archive", Some(&tag)).await;
    assert_eq!(s, 200, "{b}");
    b
}

async fn reference_state(f: &Fixture, entry: Uuid) -> String {
    let (s, b, _) = f
        .call(
            "GET",
            &format!("/price-book-entries/{entry}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    b["reference_state"].as_str().unwrap().to_owned()
}

/// The catalog's state of `entry`'s reference.
fn held(catalog: &Catalog, entry: Uuid) -> ReferenceState {
    catalog.refs.lock().unwrap()[&entry].1
}

fn codes(page: &Value) -> Vec<String> {
    let mut codes: Vec<String> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap().to_owned())
        .collect();
    codes.sort_unstable();
    codes
}

async fn listed(f: &Fixture, query: &str) -> Vec<String> {
    let (s, b, _) = f
        .call(
            "GET",
            &format!("/price-books{query}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    codes(&b)
}

/// The answer's status and its code, exactly: a conflict's reason, or a 400's first violation.
fn refused(answer: &(u16, Value, String), status: u16, code: &str) {
    assert_eq!(answer.0, status, "{answer:?}");
    let context = &answer.1["context"];
    let found = context["reason"]
        .as_str()
        .or_else(|| context["field_violations"][0]["reason"].as_str());
    assert_eq!(found, Some(code), "{answer:?}");
}

/// Ask 58: a book with an entry, an approved price and only a superseded plan revision is
/// archived. Each entry's reference is released in Products, the entries read `released`, and
/// the book leaves the list, reading by id as archived.
#[tokio::test]
async fn ask_58_a_finished_book_archives_and_releases_its_sku_references() {
    let (f, catalog) = plan_support::setup().await;
    let sold = catalog.sku(SkuType::Recurring);
    let planned = catalog.sku(SkuType::Recurring);
    let book = new_book(&f, "finished").await;
    let other = new_book(&f, "other").await;
    let e1 = door_entry(&f, book, sold).await;
    let e2 = door_entry(&f, book, planned).await;
    approved(&f, e1).await;
    // A plan whose revision on the book is superseded by one on another book.
    let (p, first) = plan(&f, "moved", book).await;
    let p = id_of(&p["id"]);
    item(&f, first, planned, Some(e2), "paid").await;
    publish(&f, p, first).await;
    let second = bare_revision(&f, p, 2, other).await;
    publish(&f, p, second).await;
    assert_eq!(listed(&f, "").await, ["finished", "other"]);

    let tag = book_tag(&f, book).await;
    assert_eq!(tag, "\"1\"");
    let (s, b, new_tag) = mark(&f, book, "archive", Some(&tag)).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(new_tag, "\"2\"", "the mark is a write of the book");
    assert!(b["archived_at"].is_string(), "{b}");
    assert_eq!(b["archived_by"], json!(f.ctx.subject_id()), "{b}");

    for entry in [e1, e2] {
        assert_eq!(reference_state(&f, entry).await, "released");
        assert_eq!(held(&catalog, entry), ReferenceState::Released, "{entry}");
        let ops = ops_for(&f, entry).await;
        let release = ops.iter().find(|op| op.kind == "release").unwrap();
        assert_eq!(release.state, "done", "{release:?}");
    }
    // The op journal says why.
    let (s, journal, _) = f.call("GET", "/reference-ops", json!({}), None, None).await;
    assert_eq!(s, 200, "{journal}");
    let released: Vec<&Value> = journal["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|op| op["kind"] == "release")
        .collect();
    assert_eq!(released.len(), 2, "{journal}");
    assert!(
        released.iter().all(|op| op["reason"] == "book_archived"),
        "{journal}"
    );

    assert_eq!(listed(&f, "").await, ["other"]);
    assert_eq!(
        listed(&f, "?$filter=archived%20eq%20false").await,
        ["other"]
    );
    assert_eq!(
        listed(&f, "?$filter=archived%20eq%20true").await,
        ["finished"]
    );
    let (s, read, _) = f
        .call(
            "GET",
            &format!("/price-books/{book}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "a read by id ignores the mark: {read}");
    assert!(read["archived_at"].is_string(), "{read}");
    // Archiving an archived book answers it unchanged.
    let (s, again, tag) = mark(&f, book, "archive", Some("\"2\"")).await;
    assert_eq!(s, 200, "{again}");
    assert_eq!(tag, "\"2\"");
}

/// A release op's reason is read through its work record, as the drive reads it (D-522, review
/// RF-P item 5): a reason outside the closed set, or a work record that does not decode, is a
/// corrupt row, a 500 that does not echo it, never a free string or a silent null. The row
/// restored reads `book_archived` again.
#[tokio::test]
async fn a_release_op_of_a_poisoned_work_record_is_a_corrupt_row() {
    use sea_orm::{ConnectionTrait, Database};
    let (f, catalog) = plan_support::setup().await;
    let book = new_book(&f, "poisoned").await;
    let entry = door_entry(&f, book, catalog.sku(SkuType::Recurring)).await;
    archive(&f, book).await;
    let release = ops_for(&f, entry)
        .await
        .into_iter()
        .find(|op| op.kind == "release")
        .unwrap();
    let stored = release.outcome.clone().unwrap();
    assert!(stored.contains("\"book_archived\""), "{stored}");
    let hex = release.op_id.simple().to_string().to_uppercase();
    let row = format!("WHERE op_id = '{}' OR hex(op_id) = '{hex}'", release.op_id);
    let raw = Database::connect(&f.dsn).await.unwrap();
    for poison in [
        stored.replace("\"book_archived\"", "\"shelved\""),
        "{".to_owned(),
    ] {
        let written = raw
            .execute_unprepared(&format!(
                "UPDATE pricing_reference_op SET outcome = '{poison}' {row}"
            ))
            .await
            .unwrap();
        assert_eq!(written.rows_affected(), 1);
        let (s, b, _) = f.call("GET", "/reference-ops", json!({}), None, None).await;
        assert_eq!(s, 500, "{poison}: {b}");
        assert!(!b.to_string().contains("shelved"), "{b}");
    }
    raw.execute_unprepared(&format!(
        "UPDATE pricing_reference_op SET outcome = '{stored}' {row}"
    ))
    .await
    .unwrap();
    raw.close().await.unwrap();
    let (s, journal, _) = f.call("GET", "/reference-ops", json!({}), None, None).await;
    assert_eq!(s, 200, "{journal}");
    let read = journal["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|op| op["kind"] == "release")
        .unwrap();
    assert_eq!(read["reason"], "book_archived", "{journal}");
}

/// Set an entry's stored reference state, as a confirmation in flight or a lost reference leaves it.
async fn stored_reference(f: &Fixture, entry: Uuid, state: &str) {
    let hex = entry.simple().to_string().to_uppercase();
    raw(
        f,
        &format!(
            "UPDATE pricing_price_book_entry SET reference_state = '{state}' \
             WHERE id = '{entry}' OR hex(id) = '{hex}'"
        ),
    )
    .await;
}

/// D-522 (review RF-P item 9): an entry whose reference is being confirmed refuses the archive,
/// 409 `ENTRY_CONFIRMATION_PENDING`, and nothing is written; a `lost` entry is released as a
/// confirmed one is, through a `release` op.
#[tokio::test]
async fn a_confirmation_in_flight_refuses_the_archive_and_a_lost_entry_is_released() {
    let (f, catalog) = plan_support::setup().await;
    let book = new_book(&f, "lossy").await;
    let pending = door_entry(&f, book, catalog.sku(SkuType::Recurring)).await;
    let lost = door_entry(&f, book, catalog.sku(SkuType::Recurring)).await;
    stored_reference(&f, pending, "confirmation_pending").await;
    stored_reference(&f, lost, "lost").await;
    let tag = book_tag(&f, book).await;
    refused(
        &mark(&f, book, "archive", Some(&tag)).await,
        409,
        "ENTRY_CONFIRMATION_PENDING",
    );
    assert_eq!(
        book_tag(&f, book).await,
        tag,
        "the refused archive wrote nothing"
    );
    assert_eq!(reference_state(&f, lost).await, "lost");
    assert!(
        ops_for(&f, lost)
            .await
            .iter()
            .all(|op| op.kind != "release"),
        "no release op"
    );

    stored_reference(&f, pending, "confirmed").await;
    archive(&f, book).await;
    assert_eq!(reference_state(&f, lost).await, "released");
    let ops = ops_for(&f, lost).await;
    let release = ops.iter().find(|op| op.kind == "release").unwrap();
    assert_eq!(release.state, "done", "{release:?}");
    assert_eq!(held(&catalog, lost), ReferenceState::Released);
}

/// The book's archive mark, as a read by id serves it.
async fn archived_at(f: &Fixture, book: Uuid) -> Value {
    let (s, b, _) = f
        .call(
            "GET",
            &format!("/price-books/{book}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    b["archived_at"].clone()
}

/// D-522 (amended 2026-10-04): an unarchive is refused 409 `ENTRY_RELEASE_PENDING` while an entry
/// of the book has an open `release` or `rereserve` op, and nothing is written: the book stays
/// archived at the tag the caller read, and no `rereserve` op is made.
#[tokio::test]
async fn an_unarchive_is_refused_while_a_release_or_a_rereserve_is_open() {
    let (f, catalog) = plan_support::setup().await;
    let book = new_book(&f, "early").await;
    let entry = door_entry(&f, book, catalog.sku(SkuType::Recurring)).await;
    catalog.down.store(true, Ordering::SeqCst);
    archive(&f, book).await;
    let release = ops_for(&f, entry)
        .await
        .into_iter()
        .find(|op| op.kind == "release")
        .unwrap();
    assert_eq!(release.state, "releasing", "{release:?}");
    let tag = book_tag(&f, book).await;
    refused(
        &mark(&f, book, "unarchive", Some(&tag)).await,
        409,
        "ENTRY_RELEASE_PENDING",
    );
    assert_eq!(book_tag(&f, book).await, tag, "the refusal wrote nothing");
    assert!(!archived_at(&f, book).await.is_null(), "still archived");
    assert_eq!(reference_state(&f, entry).await, "released");
    let ops = ops_for(&f, entry).await;
    assert!(
        ops.iter().all(|op| op.kind != "rereserve"),
        "no re-reservation while the release is open: {ops:?}"
    );

    // A re-reservation an unarchive could not finish, in a book archived again meanwhile.
    catalog.down.store(false, Ordering::SeqCst);
    let book = new_book(&f, "again").await;
    let entry = door_entry(&f, book, catalog.sku(SkuType::Recurring)).await;
    archive(&f, book).await;
    catalog.down.store(true, Ordering::SeqCst);
    let (s, b, _) = mark(&f, book, "unarchive", Some(&book_tag(&f, book).await)).await;
    assert_eq!(s, 200, "{b}");
    archive(&f, book).await;
    let rereserve = ops_for(&f, entry)
        .await
        .into_iter()
        .find(|op| op.kind == "rereserve")
        .unwrap();
    assert_ne!(rereserve.state, "done", "{rereserve:?}");
    let tag = book_tag(&f, book).await;
    refused(
        &mark(&f, book, "unarchive", Some(&tag)).await,
        409,
        "ENTRY_RELEASE_PENDING",
    );
    assert_eq!(book_tag(&f, book).await, tag, "the refusal wrote nothing");
    assert!(!archived_at(&f, book).await.is_null(), "still archived");
    let rereserves = ops_for(&f, entry)
        .await
        .into_iter()
        .filter(|op| op.kind == "rereserve")
        .count();
    assert_eq!(rereserves, 1, "no second re-reservation");
}

/// D-522 (amended 2026-10-04): once the ticker has finished the release that refused an unarchive,
/// the same tag unarchives the book, and the entry is re-reserved: `confirmed`, a new reservation
/// in Products, and writable again.
#[tokio::test]
async fn an_unarchive_after_the_ticker_finished_the_release_rereserves_the_entry() {
    let (f, catalog) = plan_support::setup().await;
    let book = new_book(&f, "later").await;
    let entry = door_entry(&f, book, catalog.sku(SkuType::Recurring)).await;
    let first_receipt = catalog.refs.lock().unwrap()[&entry].0;
    catalog.down.store(true, Ordering::SeqCst);
    archive(&f, book).await;
    let tag = book_tag(&f, book).await;
    refused(
        &mark(&f, book, "unarchive", Some(&tag)).await,
        409,
        "ENTRY_RELEASE_PENDING",
    );

    catalog.down.store(false, Ordering::SeqCst);
    Ticker::new(f.state.clone(), Arc::new(Later), 10, 100)
        .tick()
        .await
        .unwrap();
    assert_eq!(held(&catalog, entry), ReferenceState::Released);
    let (s, b, new_tag) = mark(&f, book, "unarchive", Some(&tag)).await;
    assert_eq!(s, 200, "{b}");
    assert!(b["archived_at"].is_null(), "{b}");
    assert_eq!(b["released_entries"], json!([]), "{b}");
    assert_ne!(new_tag, tag);
    assert_eq!(reference_state(&f, entry).await, "confirmed");
    let (receipt, state) = catalog.refs.lock().unwrap()[&entry];
    assert_ne!(receipt, first_receipt, "a new reservation");
    assert_eq!(state, ReferenceState::Confirmed);
    let ops = ops_for(&f, entry).await;
    let rereserve = ops.iter().find(|op| op.kind == "rereserve").unwrap();
    assert_eq!(rereserve.state, "done", "{rereserve:?}");
    draft(&f, entry).await;
}

/// D-522 (review RF-P item 9): the `archived` term joins the rest of the book list's `$filter`,
/// which still applies; two terms that disagree keep no book; and an `archived` term under `or`
/// is 400 before any read.
#[tokio::test]
async fn the_archived_term_joins_the_rest_of_the_book_filter() {
    let (f, _) = plan_support::setup().await;
    let finished = new_book(&f, "finished").await;
    new_book(&f, "other").await;
    archive(&f, finished).await;
    let filtered = |expr: &str| format!("?$filter={}", expr.replace(' ', "%20"));
    for (expr, kept) in [
        ("archived eq true and code eq 'finished'", vec!["finished"]),
        ("archived eq true and code eq 'other'", vec![]),
        ("code eq 'finished' and archived eq false", vec![]),
        ("archived ne true and code eq 'other'", vec!["other"]),
        ("archived eq true and archived eq false", vec![]),
    ] {
        assert_eq!(listed(&f, &filtered(expr)).await, kept, "{expr}");
    }
    let (s, b, _) = f
        .call(
            "GET",
            &format!(
                "/price-books{}",
                filtered("archived eq true or code eq 'x'")
            ),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 400, "{b}");
    assert_eq!(b["status"], 400, "a problem body: {b}");
    assert!(
        b.to_string().contains("joined only by top-level `and`"),
        "it says which shapes `archived` takes: {b}"
    );
}

/// The refusals: a missing If-Match is 400, a stale one 409 `STALE_REVISION`, an unknown book 404;
/// a plan revision that is not superseded is `BOOK_IN_PLAN`, a pending price `BOOK_HAS_PENDING`.
/// A refused archive writes nothing.
#[tokio::test]
async fn a_book_in_use_is_refused_and_kept() {
    let (f, catalog) = plan_support::setup().await;
    quorum(&f, 1).await;
    let free = new_book(&f, "free").await;
    assert_eq!(mark(&f, free, "archive", None).await.0, 400);
    refused(
        &mark(&f, free, "archive", Some("\"9\"")).await,
        409,
        "STALE_REVISION",
    );
    assert_eq!(
        mark(&f, Uuid::now_v7(), "archive", Some("\"1\"")).await.0,
        404
    );

    let drafted = new_book(&f, "drafted").await;
    plan(&f, "draft", drafted).await;
    refused(
        &mark(&f, drafted, "archive", Some("\"1\"")).await,
        409,
        "BOOK_IN_PLAN",
    );
    let live = new_book(&f, "live").await;
    let (p, first) = plan(&f, "live", live).await;
    publish(&f, id_of(&p["id"]), first).await;
    refused(
        &mark(&f, live, "archive", Some("\"1\"")).await,
        409,
        "BOOK_IN_PLAN",
    );

    let pending = new_book(&f, "pending").await;
    let entry = door_entry(&f, pending, catalog.sku(SkuType::Recurring)).await;
    let price = draft(&f, entry).await;
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/prices/{}/submit", price["id"].as_str().unwrap()),
            json!({}),
            None,
            Some("submit"),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(b["unit"]["state"], "pending", "{b}");
    refused(
        &mark(&f, pending, "archive", Some("\"1\"")).await,
        409,
        "BOOK_HAS_PENDING",
    );
    // A cancel in review keeps the book too: it is a prices unit of the book.
    let cancelling = new_book(&f, "cancelling").await;
    let scheduled = door_entry(&f, cancelling, catalog.sku(SkuType::Recurring)).await;
    let target = approved_from(&f, scheduled, 30).await;
    let (s, change, _) = f
        .call(
            "POST",
            &format!("/prices/{target}/cancel"),
            json!({}),
            None,
            Some("cancel"),
        )
        .await;
    assert_eq!(s, 201, "{change}");
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/prices/{}/submit", change["id"].as_str().unwrap()),
            json!({}),
            None,
            Some("submit-cancel"),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    refused(
        &mark(&f, cancelling, "archive", Some("\"1\"")).await,
        409,
        "BOOK_HAS_PENDING",
    );
    for book in [drafted, live, pending, cancelling] {
        let (_, read, tag) = f
            .call(
                "GET",
                &format!("/price-books/{book}"),
                json!({}),
                None,
                None,
            )
            .await;
        assert!(read["archived_at"].is_null(), "{read}");
        assert_eq!(tag, "\"1\"");
    }
    assert_eq!(reference_state(&f, entry).await, "confirmed");
}

/// An archived book's entries and prices are read-only: an entry create or PATCH, a price
/// create, a cancel, an end, a submit, and a plan item naming one of its entries are 409
/// `BOOK_ARCHIVED`. A delete still runs: it adds no money.
#[tokio::test]
async fn an_archived_books_entries_and_prices_are_read_only() {
    let (f, catalog) = plan_support::setup().await;
    quorum(&f, 0).await;
    let sku = catalog.sku(SkuType::Recurring);
    let book = new_book(&f, "shelved").await;
    let entry = door_entry(&f, book, sku).await;
    let spare = door_entry(&f, book, catalog.sku(SkuType::Recurring)).await;
    let live = approved(&f, entry).await;
    let pending_draft = draft(&f, entry).await;
    archive(&f, book).await;

    let other = catalog.sku(SkuType::Recurring);
    refused(
        &f.call(
            "POST",
            &format!("/price-books/{book}/entries"),
            json!({"sku_id": other, "period": "month", "model": "per_unit"}),
            None,
            Some("late-entry"),
        )
        .await,
        409,
        "BOOK_ARCHIVED",
    );
    refused(
        &f.call(
            "POST",
            &format!("/price-book-entries/{entry}/prices"),
            json!({"price": {"rate": "0.20"}, "eligibility": "all", "effective_from": future(50)}),
            None,
            Some("late-price"),
        )
        .await,
        409,
        "BOOK_ARCHIVED",
    );
    refused(
        &f.call(
            "POST",
            &format!("/prices/{live}/cancel"),
            json!({}),
            None,
            Some("late-cancel"),
        )
        .await,
        409,
        "BOOK_ARCHIVED",
    );
    refused(
        &f.call(
            "POST",
            &format!("/prices/{live}/end"),
            json!({"effective_to": future(60)}),
            None,
            Some("late-end"),
        )
        .await,
        409,
        "BOOK_ARCHIVED",
    );
    let pending_id = pending_draft["id"].as_str().unwrap();
    refused(
        &f.call(
            "POST",
            &format!("/prices/{pending_id}/submit"),
            json!({}),
            None,
            Some("late-submit"),
        )
        .await,
        409,
        "BOOK_ARCHIVED",
    );
    let kept = price_of(&f, entry, pending_id).await;
    assert_eq!(
        kept["state"], "draft",
        "the refused submit wrote nothing: {kept}"
    );
    assert!(kept["pending_unit_id"].is_null(), "{kept}");
    let (_, read, tag) = f
        .call(
            "GET",
            &format!("/price-book-entries/{entry}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(read["reference_state"], "released");
    refused(
        &f.call(
            "PATCH",
            &format!("/price-book-entries/{entry}"),
            json!({"invoice_line_override": "{name}"}),
            Some(&tag),
            None,
        )
        .await,
        409,
        "BOOK_ARCHIVED",
    );
    // A plan item naming the archived book's entry, created or patched onto it.
    let (_, revision) = plan(&f, "after", book).await;
    refused(
        &f.call(
            "POST",
            &format!("/plan-revisions/{revision}/items"),
            json!({"sku_id": sku, "price_book_entry_id": entry}),
            None,
            Some("late-item"),
        )
        .await,
        409,
        "BOOK_ARCHIVED",
    );
    let written = item(&f, revision, sku, Some(entry), "paid").await;
    refused(
        &f.call(
            "PATCH",
            &format!("/plan-items/{}", written.id),
            json!({"price_book_entry_id": entry}),
            Some("\"1\""),
            None,
        )
        .await,
        409,
        "BOOK_ARCHIVED",
    );
    // A delete adds no money: the spare entry goes.
    let (s, b, _) = f
        .call(
            "DELETE",
            &format!("/price-book-entries/{spare}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 204, "{b}");
}

/// Unarchive re-reserves each released entry whose SKU still admits a reference, and lists the
/// entries it could not: a retired SKU leaves its entry `released`, and that entry stays
/// read-only (`ENTRY_REFERENCE_RELEASED`) in the book, which is listed again.
#[tokio::test]
async fn unarchive_rereserves_the_live_skus_and_lists_the_others() {
    let (f, catalog) = plan_support::setup().await;
    let live = catalog.sku(SkuType::Recurring);
    let gone = catalog.sku(SkuType::Recurring);
    let book = new_book(&f, "back").await;
    let e1 = door_entry(&f, book, live).await;
    let e2 = door_entry(&f, book, gone).await;
    let stranded = draft(&f, e2).await;
    let first_receipt = catalog.refs.lock().unwrap()[&e1].0;
    archive(&f, book).await;
    catalog.age(gone, Lifecycle::Retired);

    let tag = book_tag(&f, book).await;
    let (s, b, new_tag) = mark(&f, book, "unarchive", Some(&tag)).await;
    assert_eq!(s, 200, "{b}");
    assert!(b["archived_at"].is_null(), "{b}");
    assert_eq!(b["released_entries"], json!([e2]), "{b}");
    assert_eq!(new_tag, "\"3\"");
    assert_eq!(reference_state(&f, e1).await, "confirmed");
    assert_eq!(reference_state(&f, e2).await, "released");
    let (receipt, state) = catalog.refs.lock().unwrap()[&e1];
    assert_ne!(receipt, first_receipt, "a new reservation");
    assert_eq!(state, ReferenceState::Confirmed);
    assert_eq!(held(&catalog, e2), ReferenceState::Released);
    assert_eq!(listed(&f, "").await, ["back"]);
    refused(
        &f.call(
            "POST",
            &format!("/price-book-entries/{e2}/prices"),
            json!({"price": {"rate": "0.20"}, "eligibility": "all", "effective_from": future(50)}),
            None,
            Some("released-price"),
        )
        .await,
        409,
        "ENTRY_REFERENCE_RELEASED",
    );
    // A draft written before the archive is not submitted either: the prices unit answers the
    // released entry as the door does.
    let stranded_id = stranded["id"].as_str().unwrap();
    refused(
        &f.call(
            "POST",
            &format!("/prices/{stranded_id}/submit"),
            json!({}),
            None,
            Some("released-submit"),
        )
        .await,
        409,
        "ENTRY_REFERENCE_RELEASED",
    );
    let kept = price_of(&f, e2, stranded_id).await;
    assert_eq!(kept["state"], "draft", "{kept}");
    assert!(kept["pending_unit_id"].is_null(), "{kept}");
    draft(&f, e1).await;
}

/// An unarchive that committed is answered as committed (D-522, review RF-P item 4): when the read
/// of the entries still released fails after the commit, the door answers the unarchived book with
/// `released_entries` null, and logs the failure; it neither fails nor invents a list. A retry
/// under the old tag then meets the committed version.
#[tokio::test]
async fn an_unarchive_answers_its_book_when_the_released_entries_cannot_be_read() {
    use sea_orm::{ConnectionTrait, Database};
    let (f, catalog) = plan_support::setup().await;
    let book = new_book(&f, "unread").await;
    let entry = door_entry(&f, book, catalog.sku(SkuType::Recurring)).await;
    archive(&f, book).await;
    // The unarchive's own write makes the entry's row stop decoding, after its transaction read
    // the entries: the read of the released entries after the commit fails. A trigger rather than
    // a hook in the drive, which the door cuts at its deadline before the hook's write lands.
    let hex = entry.simple().to_string().to_uppercase();
    let raw = Database::connect(&f.dsn).await.unwrap();
    raw.execute_unprepared(&format!(
        "CREATE TRIGGER unread_entry AFTER UPDATE OF archived_at ON pricing_price_book \
         WHEN NEW.archived_at IS NULL BEGIN \
         UPDATE pricing_price_book_entry SET created_at = 'not a time' \
         WHERE id = '{entry}' OR hex(id) = '{hex}'; END"
    ))
    .await
    .unwrap();
    raw.close().await.unwrap();
    let tag = book_tag(&f, book).await;
    let (s, b, new_tag) = mark(&f, book, "unarchive", Some(&tag)).await;
    assert_eq!(s, 200, "the unarchive committed: {b}");
    assert!(b["archived_at"].is_null(), "{b}");
    assert!(
        b["released_entries"].is_null(),
        "not read, not invented: {b}"
    );
    assert_eq!(new_tag, "\"3\"");
    refused(
        &mark(&f, book, "unarchive", Some(&tag)).await,
        409,
        "STALE_REVISION",
    );
    let (s, again, _) = mark(&f, book, "unarchive", Some(&new_tag)).await;
    assert_eq!(s, 200, "an unarchived book is answered as it is: {again}");
}

/// The door drives its ops a few at a time under one deadline for the whole door and leaves the
/// rest to the ticker (D-522, review RF-P item 3): with Products stalling every release far past
/// that deadline, the archive still answers within seconds, its entries `released` and their
/// releases open, and the ticker finishes them once Products answers.
#[tokio::test]
async fn the_archive_answers_within_its_deadline_while_products_stalls() {
    let (f, catalog) = plan_support::setup().await;
    let book = new_book(&f, "stalled").await;
    let entries = [
        door_entry(&f, book, catalog.sku(SkuType::Recurring)).await,
        door_entry(&f, book, catalog.sku(SkuType::Recurring)).await,
    ];
    catalog.stall_releases_ms.store(15_000, Ordering::SeqCst);
    let started = std::time::Instant::now();
    archive(&f, book).await;
    let took = started.elapsed();
    assert!(
        took < std::time::Duration::from_secs(10),
        "the door answered after {took:?}"
    );
    for entry in entries {
        assert_eq!(reference_state(&f, entry).await, "released");
        assert_eq!(held(&catalog, entry), ReferenceState::Confirmed, "{entry}");
        let ops = ops_for(&f, entry).await;
        let release = ops.iter().find(|op| op.kind == "release").unwrap();
        assert_eq!(release.state, "releasing", "{release:?}");
    }
    assert_eq!(
        catalog.releases(),
        0,
        "no release answered within the deadline"
    );

    catalog.stall_releases_ms.store(0, Ordering::SeqCst);
    Ticker::new(f.state.clone(), Arc::new(Later), 10, 100)
        .tick()
        .await
        .unwrap();
    for entry in entries {
        assert_eq!(held(&catalog, entry), ReferenceState::Released, "{entry}");
        let ops = ops_for(&f, entry).await;
        let release = ops.iter().find(|op| op.kind == "release").unwrap();
        assert_eq!(release.state, "done", "{release:?}");
    }
}

/// The release survives a failed drive: with Products down the archive still answers 200, its
/// entry reads `released`, and the ticker finishes the release once Products answers.
#[tokio::test]
async fn the_release_survives_a_failed_drive() {
    let (f, catalog) = plan_support::setup().await;
    let book = new_book(&f, "offline").await;
    let entry = door_entry(&f, book, catalog.sku(SkuType::Recurring)).await;
    catalog.down.store(true, Ordering::SeqCst);
    archive(&f, book).await;
    assert_eq!(reference_state(&f, entry).await, "released");
    assert_eq!(held(&catalog, entry), ReferenceState::Confirmed);
    let ops = ops_for(&f, entry).await;
    let release = ops.iter().find(|op| op.kind == "release").unwrap();
    assert_eq!(release.state, "releasing", "{release:?}");

    catalog.down.store(false, Ordering::SeqCst);
    Ticker::new(f.state.clone(), Arc::new(Later), 10, 100)
        .tick()
        .await
        .unwrap();
    assert_eq!(held(&catalog, entry), ReferenceState::Released);
    let ops = ops_for(&f, entry).await;
    assert_eq!(
        ops.iter().find(|op| op.kind == "release").unwrap().state,
        "done"
    );
}

/// A re-reservation that an unarchive started and could not finish never writes into a book
/// archived again meanwhile: its write is refused, its new reservation released, and the entry
/// stays `released`.
#[tokio::test]
async fn a_rereserve_never_writes_into_an_archived_book() {
    let (f, catalog) = plan_support::setup().await;
    let book = new_book(&f, "again").await;
    let entry = door_entry(&f, book, catalog.sku(SkuType::Recurring)).await;
    archive(&f, book).await;
    catalog.down.store(true, Ordering::SeqCst);
    let tag = book_tag(&f, book).await;
    let (s, b, _) = mark(&f, book, "unarchive", Some(&tag)).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        b["released_entries"],
        json!([entry]),
        "the drive did not finish: {b}"
    );
    archive(&f, book).await;
    catalog.down.store(false, Ordering::SeqCst);
    Ticker::new(f.state.clone(), Arc::new(Later), 10, 100)
        .tick()
        .await
        .unwrap();
    assert_eq!(reference_state(&f, entry).await, "released");
    assert_eq!(held(&catalog, entry), ReferenceState::Released);
    let ops = ops_for(&f, entry).await;
    let rereserve = ops.iter().find(|op| op.kind == "rereserve").unwrap();
    assert_eq!(rereserve.state, "done", "{rereserve:?}");
}
