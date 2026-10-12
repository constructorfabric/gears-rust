//! A book's description and the delete of an unused book (phase 7, run 7.2, ask 16), on `SQLite`.
//!
//! - `description`: optional free text on `POST`/`PATCH /price-books` (PATCH: omitted keeps it,
//!   `null` clears it), at most 2000 characters, carried by every book answer.
//! - `DELETE /price-books/{id}` under If-Match and the book write grant: 204 and an audit row for a
//!   book no entry, plan revision or unit keeps; else 409 in a stated order.
//! - The forward migration `m20260928_000015_book_description` adds the column to a deployed
//!   database, books seeded; the application reads, edits and deletes them. The Postgres twin
//!   is `postgres_book_writes.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod book_support;
mod plan_support;
use plan_support::entry_support::policy_support;
mod schema_dump;
use book_support::{bare_revision, code_of, get, ok};
use entry_support::Script;
use plan_support::{Fixture, entry_support, id_of, plan, publish, request, stranger};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

async fn fixture() -> (Fixture, Arc<Script>) {
    let script = Arc::new(Script::default());
    (Fixture::new(script.clone()).await, script)
}
async fn new_book(f: &Fixture, code: &str, description: Option<&str>) -> Value {
    let mut body = json!({"code":code,"name":format!("Book {code}"),"currency":"EUR"});
    if let Some(text) = description {
        body["description"] = json!(text);
    }
    let (s, b, tag) = f
        .call(
            "POST",
            "/price-books",
            body,
            None,
            Some(&format!("book-{code}")),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(tag, "\"1\"");
    b
}
async fn delete(f: &Fixture, book: &Value, tag: Option<&str>) -> (u16, Value, String) {
    f.call(
        "DELETE",
        &format!("/price-books/{}", book["id"].as_str().unwrap()),
        json!({}),
        tag,
        None,
    )
    .await
}
async fn door_entry(f: &Fixture, book: &Value) -> Value {
    let (s, entry, _) = f
        .call(
            "POST",
            &format!("/price-books/{}/entries", book["id"].as_str().unwrap()),
            json!({"usage_rating_policy":policy_support::input(),"sku_id":Uuid::new_v4(),"model":"per_unit"}),
            None,
            Some(&Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(s, 201, "{entry}");
    entry
}
async fn door_price(f: &Fixture, entry: &Value) -> Value {
    let (s, created, _) = f
        .call(
            "POST",
            &format!(
                "/price-book-entries/{}/prices",
                entry["id"].as_str().unwrap()
            ),
            json!({"price":{"rate":"0.10"},"eligibility":"all","effective_from":"2031-03-01"}),
            None,
            Some(&Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(s, 201, "{created}");
    created["items"][0].clone()
}
async fn quorum_one(f: &Fixture) {
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let (s, b, _) = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"quorum":1}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
}
async fn submit(f: &Fixture, price: &Value) -> Value {
    let (s, receipt, _) = f
        .call(
            "POST",
            &format!("/prices/{}/submit", price["id"].as_str().unwrap()),
            json!({}),
            None,
            Some(&Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(s, 201, "{receipt}");
    assert_eq!(receipt["unit"]["state"], "pending", "{receipt}");
    receipt["unit"].clone()
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
/// The audit actions of one subject, by revision.
async fn audited(f: &Fixture, id: &Value) -> Vec<(String, i64)> {
    Database::connect(&f.dsn)
        .await
        .unwrap()
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT action, subject_revision FROM pricing_audit WHERE subject_id = ? \
             ORDER BY subject_revision, action",
            [id.as_str().unwrap().parse::<Uuid>().unwrap().into()],
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.try_get::<String>("", "action").unwrap(),
                r.try_get::<Option<i64>>("", "subject_revision")
                    .unwrap()
                    .unwrap_or_default(),
            )
        })
        .collect()
}

// ------------------------------------------------------------------ the description

#[tokio::test]
async fn a_book_carries_an_optional_description_through_every_answer() {
    let (f, _) = fixture().await;
    let plain = new_book(&f, "plain", None).await;
    assert!(plain["description"].is_null(), "{plain}");
    assert!(plain.as_object().unwrap().contains_key("description"));
    let book = new_book(&f, "retail", Some("Retail list, EUR")).await;
    assert_eq!(book["description"], "Retail list, EUR");
    let id = book["id"].as_str().unwrap();
    // Every read of the book carries it: the book, the list, the export, publish-changes.
    assert_eq!(
        ok(&f, &format!("/price-books/{id}")).await["description"],
        "Retail list, EUR"
    );
    let listed = ok(&f, "/price-books?q=retail").await;
    assert_eq!(listed["items"][0]["description"], "Retail list, EUR");
    assert_eq!(
        ok(&f, &format!("/price-books/{id}/export")).await["book"]["description"],
        "Retail list, EUR"
    );
    assert_eq!(
        ok(&f, &format!("/price-books/{id}/publish-changes")).await["book"]["description"],
        "Retail list, EUR"
    );
    // PATCH: omitted keeps it, a value replaces it, null clears it.
    let path = format!("/price-books/{id}");
    let kept = f
        .call(
            "PATCH",
            &path,
            json!({"name":"Retail"}),
            Some("\"1\""),
            None,
        )
        .await;
    assert_eq!(kept.0, 200, "{kept:?}");
    assert_eq!(kept.1["description"], "Retail list, EUR");
    let changed = f
        .call(
            "PATCH",
            &path,
            json!({"description":"Retail list"}),
            Some("\"2\""),
            None,
        )
        .await;
    assert_eq!(changed.0, 200, "{changed:?}");
    assert_eq!(changed.1["description"], "Retail list");
    assert_eq!(changed.2, "\"3\"");
    let cleared = f
        .call(
            "PATCH",
            &path,
            json!({"description":null}),
            Some("\"3\""),
            None,
        )
        .await;
    assert_eq!(cleared.0, 200, "{cleared:?}");
    assert!(cleared.1["description"].is_null(), "{cleared:?}");
    assert!(ok(&f, &path).await["description"].is_null());
    // At most 2000 characters, counted as characters, not bytes.
    let longest = "\u{e9}".repeat(2000);
    let (s, b, _) = f
        .call(
            "POST",
            "/price-books",
            json!({"code":"long","name":"long","currency":"EUR","description":longest}),
            None,
            Some("long"),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(b["description"], longest);
    let too_long = "\u{e9}".repeat(2001);
    let answer = f
        .call(
            "POST",
            "/price-books",
            json!({"code":"longer","name":"longer","currency":"EUR","description":too_long}),
            None,
            Some("longer"),
        )
        .await;
    refused(&answer, 400, "BOOK_DESCRIPTION_TOO_LONG");
    assert!(code_of(&answer.1).contains("description"), "{answer:?}");
    let answer = f
        .call(
            "PATCH",
            &path,
            json!({"description":too_long}),
            Some("\"4\""),
            None,
        )
        .await;
    refused(&answer, 400, "BOOK_DESCRIPTION_TOO_LONG");
    let answer = f
        .call(
            "PATCH",
            &path,
            json!({"description":"a\u{0}b"}),
            Some("\"4\""),
            None,
        )
        .await;
    refused(&answer, 400, "VALIDATION");
    let (_, read, tag) = get(&f, &path).await;
    assert_eq!(tag, "\"4\"", "a refused PATCH writes nothing");
    assert!(read["description"].is_null());
}

// ------------------------------------------------------------------ the delete

#[tokio::test]
async fn an_unused_book_is_deleted_under_if_match_with_an_audit_row() {
    let (f, _) = fixture().await;
    let book = new_book(&f, "gone", Some("to delete")).await;
    let id = book["id"].as_str().unwrap().to_owned();
    let path = format!("/price-books/{id}");
    // Authorization first: a caller without the book write grant is 403 before If-Match.
    let denied = request(&f.denied, &f.ctx, "DELETE", &path, json!({}), None, None).await;
    assert_eq!(denied.0, 403, "{denied:?}");
    assert_eq!(delete(&f, &book, None).await.0, 400, "If-Match is required");
    assert_eq!(
        delete(&f, &book, Some("one")).await.0,
        400,
        "a malformed If-Match"
    );
    refused(
        &delete(&f, &book, Some("\"2\"")).await,
        409,
        "STALE_REVISION",
    );
    let foreign = request(
        &f.app,
        &stranger(),
        "DELETE",
        &path,
        json!({}),
        Some("\"1\""),
        None,
    )
    .await;
    assert_eq!(
        foreign.0, 403,
        "another tenant holds no write grant on the book: {foreign:?}"
    );
    assert_eq!(get(&f, &path).await.0, 200, "the book stays");
    let unknown = f
        .call(
            "DELETE",
            &format!("/price-books/{}", Uuid::now_v7()),
            json!({}),
            Some("\"1\""),
            None,
        )
        .await;
    assert_eq!(unknown.0, 404, "{unknown:?}");

    let gone = delete(&f, &book, Some("\"1\"")).await;
    assert_eq!(gone.0, 204, "{gone:?}");
    assert_eq!(gone.1, Value::Null, "no body");
    assert_eq!(get(&f, &path).await.0, 404);
    assert_eq!(ok(&f, "/price-books").await["items"], json!([]));
    assert_eq!(
        delete(&f, &book, Some("\"1\"")).await.0,
        404,
        "deleted once"
    );
    assert_eq!(
        audited(&f, &book["id"]).await,
        [
            ("price_book.create".to_owned(), 1),
            ("price_book.delete".to_owned(), 1)
        ]
    );
    // P-D-206's twin, documented rather than changed: the create's key replays its 201 for a day,
    // naming the deleted book; the code itself is free again.
    let (s, replay, _) = f
        .call(
            "POST",
            "/price-books",
            json!({"code":"gone","name":"Book gone","currency":"EUR","description":"to delete"}),
            None,
            Some("book-gone"),
        )
        .await;
    assert_eq!(s, 201, "{replay}");
    assert_eq!(replay["id"], book["id"]);
    assert_eq!(get(&f, &path).await.0, 404);
    let (s, again, _) = f
        .call(
            "POST",
            "/price-books",
            json!({"code":"gone","name":"Again","currency":"EUR"}),
            None,
            Some("gone-again"),
        )
        .await;
    assert_eq!(s, 201, "{again}");
    assert_ne!(again["id"], book["id"]);
    // An edited book is deleted at its current version.
    let edited = new_book(&f, "edited", None).await;
    let e = format!("/price-books/{}", edited["id"].as_str().unwrap());
    assert_eq!(
        f.call("PATCH", &e, json!({"name":"x"}), Some("\"1\""), None)
            .await
            .0,
        200
    );
    refused(
        &delete(&f, &edited, Some("\"1\"")).await,
        409,
        "STALE_REVISION",
    );
    assert_eq!(delete(&f, &edited, Some("\"2\"")).await.0, 204);
}

/// The refusals, in order: If-Match before use; an entry of any state (`BOOK_HAS_ENTRIES`), then a
/// plan with a draft, pending, scheduled or published revision on the book (`BOOK_IN_PLAN`, the
/// one read `stats.plans` counts, D-441), then a plan that names it only through superseded
/// revisions (`BOOK_IN_PLAN_HISTORY`: `stats.plans` is 0, the revision's history keeps the book).
#[tokio::test]
async fn a_book_in_use_is_refused_in_a_stated_order_and_kept() {
    let (f, _) = fixture().await;
    // An entry.
    let entries = new_book(&f, "entries", None).await;
    door_entry(&f, &entries).await;
    refused(
        &delete(&f, &entries, Some("\"2\"")).await,
        409,
        "STALE_REVISION",
    );
    refused(
        &delete(&f, &entries, Some("\"1\"")).await,
        409,
        "BOOK_HAS_ENTRIES",
    );
    // An entry and a plan: the entry first.
    let (_, _) = plan(&f, "on-entries", id_of(&entries["id"])).await;
    refused(
        &delete(&f, &entries, Some("\"1\"")).await,
        409,
        "BOOK_HAS_ENTRIES",
    );
    // A plan's draft revision.
    let planned = new_book(&f, "planned", None).await;
    let (_, _) = plan(&f, "draft", id_of(&planned["id"])).await;
    let stats = ok(
        &f,
        &format!("/price-books/{}", planned["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(stats["stats"]["plans"], 1);
    assert_eq!(stats["stats"]["plans_superseded_only"], 0);
    assert_eq!(stats["stats"]["entries"], 0);
    refused(
        &delete(&f, &planned, Some("\"1\"")).await,
        409,
        "BOOK_IN_PLAN",
    );
    // Only a superseded revision names the book: stats.plans is 0 and BOOK_IN_PLAN does not fire;
    // stats.plans_superseded_only says why the delete is refused.
    let history = new_book(&f, "history", None).await;
    let other = new_book(&f, "other", None).await;
    let (p, first) = plan(&f, "moved", id_of(&history["id"])).await;
    let p = id_of(&p["id"]);
    publish(&f, p, first).await;
    let second = bare_revision(&f, p, 2, id_of(&other["id"])).await;
    publish(&f, p, second).await;
    let stats = ok(
        &f,
        &format!("/price-books/{}", history["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(stats["stats"]["plans"], 0, "{stats}");
    assert_eq!(stats["stats"]["plans_superseded_only"], 1, "{stats}");
    let answer = delete(&f, &history, Some("\"1\"")).await;
    refused(&answer, 409, "BOOK_IN_PLAN_HISTORY");
    // Every refused book is still there.
    for book in [&entries, &planned, &history] {
        assert_eq!(
            get(
                &f,
                &format!("/price-books/{}", book["id"].as_str().unwrap())
            )
            .await
            .0,
            200
        );
    }
}

/// A book's stats say whether its delete succeeds (D-441, D-444): `stats.plans`,
/// `stats.plans_superseded_only` and `stats.entries` are all 0 exactly when the delete answers
/// 204. Each case reads the book's stats, then deletes it at its version.
#[tokio::test]
async fn a_books_stats_say_whether_its_delete_succeeds() {
    let (f, _) = fixture().await;
    quorum_one(&f).await;
    // The book every moved plan moves to; it is not one of the cases.
    let elsewhere = id_of(&new_book(&f, "elsewhere", None).await["id"]);
    let moved = |code: &'static str, book: Uuid| {
        let f = &f;
        async move {
            let (p, first) = plan(f, code, book).await;
            let p = id_of(&p["id"]);
            publish(f, p, first).await;
            let second = bare_revision(f, p, 2, elsewhere).await;
            publish(f, p, second).await;
        }
    };
    let mut cases: Vec<(&str, Value, Option<&str>)> = Vec::new();

    cases.push(("nothing uses it", new_book(&f, "unused", None).await, None));

    let book = new_book(&f, "entry", None).await;
    door_entry(&f, &book).await;
    cases.push(("an entry", book, Some("BOOK_HAS_ENTRIES")));

    // A price submitted and withdrawn, then its entry deleted: the decided unit keeps nothing.
    let book = new_book(&f, "emptied", None).await;
    let entry = door_entry(&f, &book).await;
    let unit = submit(&f, &door_price(&f, &entry).await).await;
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/approval-units/{}/withdraw", unit["id"].as_str().unwrap()),
            json!({}),
            None,
            Some("emptied-withdraw"),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let entry_path = format!("/price-book-entries/{}", entry["id"].as_str().unwrap());
    assert_eq!(
        f.call("DELETE", &entry_path, json!({}), None, None).await.0,
        204
    );
    cases.push(("its entry deleted, a decided unit", book, None));

    let book = new_book(&f, "pending", None).await;
    submit(&f, &door_price(&f, &door_entry(&f, &book).await).await).await;
    cases.push(("a pending prices unit", book, Some("BOOK_HAS_ENTRIES")));

    let book = new_book(&f, "draft", None).await;
    plan(&f, "draft", id_of(&book["id"])).await;
    cases.push(("a draft revision", book, Some("BOOK_IN_PLAN")));

    let book = new_book(&f, "published", None).await;
    let (p, first) = plan(&f, "published", id_of(&book["id"])).await;
    publish(&f, id_of(&p["id"]), first).await;
    cases.push(("a published revision", book, Some("BOOK_IN_PLAN")));

    // One plan with a published and a superseded revision on the book counts once, as live.
    let book = new_book(&f, "republished", None).await;
    let (p, first) = plan(&f, "republished", id_of(&book["id"])).await;
    let p = id_of(&p["id"]);
    publish(&f, p, first).await;
    let second = bare_revision(&f, p, 2, id_of(&book["id"])).await;
    publish(&f, p, second).await;
    cases.push((
        "a live and a superseded revision",
        book,
        Some("BOOK_IN_PLAN"),
    ));

    let book = new_book(&f, "history", None).await;
    moved("history", id_of(&book["id"])).await;
    cases.push((
        "only a superseded revision",
        book,
        Some("BOOK_IN_PLAN_HISTORY"),
    ));

    let book = new_book(&f, "mixed", None).await;
    plan(&f, "mixed-live", id_of(&book["id"])).await;
    moved("mixed-moved", id_of(&book["id"])).await;
    cases.push(("a live plan and a moved one", book, Some("BOOK_IN_PLAN")));

    let mut seen = Vec::new();
    for (label, book, refusal) in cases {
        let id = book["id"].as_str().unwrap();
        let stats = ok(&f, &format!("/price-books/{id}")).await["stats"].clone();
        let free =
            stats["plans"] == 0 && stats["plans_superseded_only"] == 0 && stats["entries"] == 0;
        let answer = delete(&f, &book, Some("\"1\"")).await;
        assert_eq!(free, answer.0 == 204, "{label}: {stats} vs {answer:?}");
        match refusal {
            None => assert_eq!(answer.0, 204, "{label}: {answer:?}"),
            Some(code) => refused(&answer, 409, code),
        }
        seen.push((
            label,
            stats["entries"].as_u64().unwrap(),
            stats["plans"].as_u64().unwrap(),
            stats["plans_superseded_only"].as_u64().unwrap(),
        ));
    }
    assert_eq!(
        seen,
        [
            ("nothing uses it", 0, 0, 0),
            ("an entry", 1, 0, 0),
            ("its entry deleted, a decided unit", 0, 0, 0),
            ("a pending prices unit", 1, 0, 0),
            ("a draft revision", 0, 1, 0),
            ("a published revision", 0, 1, 0),
            ("a live and a superseded revision", 0, 1, 0),
            ("only a superseded revision", 0, 0, 1),
            ("a live plan and a moved one", 0, 1, 1),
        ]
    );
}

/// `BOOK_LOCKED_PENDING` is not a refusal: a pending prices unit holds pending prices, a pending
/// price keeps its entry (`ENTRY_PRICES_IN_USE`), and the book is refused for its entries first.
#[tokio::test]
async fn a_pending_unit_keeps_its_entry_so_its_book_is_refused_for_the_entry() {
    let (f, _) = fixture().await;
    quorum_one(&f).await;
    let book = new_book(&f, "pending", None).await;
    let entry = door_entry(&f, &book).await;
    let price = door_price(&f, &entry).await;
    submit(&f, &price).await;
    let entry_path = format!("/price-book-entries/{}", entry["id"].as_str().unwrap());
    refused(
        &f.call("DELETE", &entry_path, json!({}), None, None).await,
        409,
        "ENTRY_PRICES_IN_USE",
    );
    let stats = ok(
        &f,
        &format!("/price-books/{}", book["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(stats["stats"]["pending_units"], 1);
    refused(
        &delete(&f, &book, Some("\"1\"")).await,
        409,
        "BOOK_HAS_ENTRIES",
    );
}

/// A rejected and a withdrawn unit that named the book stay readable once their entry and then
/// the book are deleted: the card and the list answer without the book, with their decisions and
/// their impact from the stored items (P-D-206's rule for a deleted draft SKU).
#[tokio::test]
async fn a_decided_unit_that_named_a_deleted_book_stays_readable() {
    let (f, _) = fixture().await;
    quorum_one(&f).await;
    let book = new_book(&f, "decided", None).await;
    let entry = door_entry(&f, &book).await;
    let rejected = submit(&f, &door_price(&f, &entry).await).await;
    let reviewer = f.user();
    let (s, b, _) = f
        .call_as(
            &reviewer,
            "POST",
            &format!(
                "/approval-units/{}/reject",
                rejected["id"].as_str().unwrap()
            ),
            json!({"generation":1,"note":"too cheap"}),
            None,
            Some("reject"),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let withdrawn = submit(&f, &door_price(&f, &entry).await).await;
    let (s, b, _) = f
        .call(
            "POST",
            &format!(
                "/approval-units/{}/withdraw",
                withdrawn["id"].as_str().unwrap()
            ),
            json!({}),
            None,
            Some("withdraw"),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let entry_path = format!("/price-book-entries/{}", entry["id"].as_str().unwrap());
    let (s, b, _) = f.call("DELETE", &entry_path, json!({}), None, None).await;
    assert_eq!(s, 204, "draft and rejected prices go with their entry: {b}");
    assert_eq!(delete(&f, &book, Some("\"1\"")).await.0, 204);

    for (unit, state) in [(&rejected, "rejected"), (&withdrawn, "withdrawn")] {
        let card = ok(
            &f,
            &format!("/approval-units/{}", unit["id"].as_str().unwrap()),
        )
        .await;
        assert_eq!(card["state"], state, "{card}");
        assert_eq!(card["ref_id"], book["id"], "{card}");
        assert_eq!(card["impact"]["prices"], 1, "{card}");
        assert_eq!(card["impact"]["entries"], 1, "{card}");
        assert_eq!(card["impact"]["plans"], json!([]), "{card}");
    }
    let card = ok(
        &f,
        &format!("/approval-units/{}", rejected["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(card["decisions"].as_array().unwrap().len(), 1, "{card}");
    let listed = f
        .all_units(&format!("book_id={}", book["id"].as_str().unwrap()))
        .await;
    assert_eq!(listed.len(), 2, "{listed:?}");
}

/// An entry create in flight when its book is deleted loses cleanly: its Tx B finds no book
/// (`BOOK_NOT_FOUND`), which cancels the op and releases the reservation.
#[tokio::test]
async fn an_entry_create_in_flight_loses_to_the_books_delete() {
    let (f, script) = fixture().await;
    let book = new_book(&f, "race", None).await;
    script.set(1);
    let parked = script.parked.notified();
    let (app, ctx, path) = (
        f.app.clone(),
        f.ctx.clone(),
        format!("/price-books/{}/entries", book["id"].as_str().unwrap()),
    );
    let create = tokio::spawn(async move {
        request(
            &app,
            &ctx,
            "POST",
            &path,
            json!({"usage_rating_policy":policy_support::input(),"sku_id":Uuid::new_v4(),"model":"per_unit"}),
            None,
            Some("in-flight"),
        )
        .await
    });
    parked.await;
    script.set(0);
    assert_eq!(delete(&f, &book, Some("\"1\"")).await.0, 204);
    script.resume.notify_one();
    let answer = create.await.unwrap();
    refused(&answer, 409, "BOOK_NOT_FOUND");
    assert_eq!(
        Script::count(&script.releases),
        1,
        "the reservation is released"
    );
}

// ------------------------------------------------------------------ the forward migration

const MIGRATION: &str = "m20260928_000015_book_description";
const TENANT: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0015_0001);
const OPEN: Uuid = Uuid::from_u128(0xb00c_0001);
const DATED: Uuid = Uuid::from_u128(0xb00c_0002);

/// A file database, so the runner's pool, the repositories and a raw connection see one schema.
struct Lite {
    /// The database's own temporary directory, removed with the `Lite` (the file, its `-wal` and
    /// its `-shm`).
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}
impl Lite {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("pricing-book-")
            .tempdir()
            .unwrap();
        let path = dir.path().join("db.sqlite3");
        Self { _dir: dir, path }
    }
    fn dsn(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }
    async fn pool(&self) -> toolkit_db::Db {
        connect_db(
            &self.dsn(),
            ConnectOpts {
                max_conns: Some(1),
                min_conns: Some(1),
                ..ConnectOpts::default()
            },
        )
        .await
        .unwrap()
    }
    async fn migrate(&self, without: Option<&str>) -> Result<MigrationResult, MigrationError> {
        let chain = bss_pricing::module::BssPricingGear::default()
            .migrations()
            .into_iter()
            .filter(|m| Some(m.name()) != without)
            .collect();
        run_migrations_for_testing(&self.pool().await, chain).await
    }
    async fn raw(&self) -> DatabaseConnection {
        Database::connect(self.dsn()).await.unwrap()
    }
    async fn strings(&self, sql: &str) -> Vec<String> {
        let raw = self.raw().await;
        let rows = raw
            .query_all_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .iter()
            .map(|row| row.try_get::<String>("", "v").unwrap())
            .collect();
        raw.close().await.unwrap();
        rows
    }
    async fn dump(&self) -> Vec<String> {
        let raw = self.raw().await;
        let dump = schema_dump::sqlite_dump(&raw).await;
        raw.close().await.unwrap();
        stanza_lines(&dump)
    }
    async fn ddl(&self) -> String {
        self.strings(
            "SELECT sql AS v FROM sqlite_master WHERE type = 'table' AND name = 'pricing_price_book'",
        )
        .await
        .pop()
        .unwrap()
    }
    /// Every book row: every column, blobs as hex, as JSON, by id.
    async fn rows(&self) -> Vec<Value> {
        let columns = self
            .strings("SELECT name AS v FROM pragma_table_info('pricing_price_book') ORDER BY cid")
            .await;
        let pairs = columns
            .iter()
            .map(|c| {
                format!(
                    "'{c}', CASE typeof(\"{c}\") WHEN 'blob' THEN 'x:' || hex(\"{c}\") ELSE \"{c}\" END"
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.strings(&format!(
            "SELECT json_object({pairs}) AS v FROM pricing_price_book ORDER BY id"
        ))
        .await
        .iter()
        .map(|r| serde_json::from_str(r).unwrap())
        .collect()
    }
}
/// Each dump line prefixed with the stanza it belongs to; the runner's ledger left out.
fn stanza_lines(dump: &str) -> Vec<String> {
    let mut stanza = String::new();
    let mut lines = Vec::new();
    for line in dump.lines() {
        if !line.starts_with(' ') {
            line.clone_into(&mut stanza);
        }
        if stanza.contains("toolkit_migrations") {
            continue;
        }
        if line.starts_with(' ') {
            lines.push(format!("{stanza} ::{line}"));
        } else {
            lines.push(line.to_owned());
        }
    }
    lines
}
/// The deployed database before this run: the chain without 000015 and two books, one open and
/// one dated, written with the values the application binds (a `Uuid` is a 16-byte blob; the
/// dates and instants are bound as the entity binds them).
async fn seeded() -> Lite {
    let db = Lite::new();
    let before = db.migrate(Some(MIGRATION)).await.unwrap();
    assert!(
        !before.applied_names.iter().any(|n| n == MIGRATION),
        "the chain before this run"
    );
    let at = time::OffsetDateTime::parse(
        "2026-09-01T09:00:00.123456Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let day = |d: u8| time::Date::from_calendar_date(2027, time::Month::January, d).unwrap();
    let raw = db.raw().await;
    for (id, code, from, until) in [
        (OPEN, "open", None, None),
        (DATED, "dated", Some(day(1)), Some(day(31))),
    ] {
        raw.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO pricing_price_book (id, tenant_id, code, name, currency, valid_from, \
             valid_until, version, created_at, updated_at) VALUES (?, ?, ?, ?, 'EUR', ?, ?, 3, ?, ?)",
            [
                id.into(),
                TENANT.into(),
                code.into(),
                format!("Book {code}").into(),
                from.into(),
                until.into(),
                at.into(),
                at.into(),
            ],
        ))
        .await
        .unwrap();
    }
    raw.close().await.unwrap();
    db
}

#[tokio::test]
async fn the_forward_migration_adds_the_description_and_keeps_every_book() {
    let db = seeded().await;
    let dump_before = db.dump().await;
    let ddl_before = db.ddl().await;
    let rows_before = db.rows().await;
    assert_eq!(rows_before.len(), 2);

    let result = db.migrate(None).await.unwrap();
    assert_eq!(result.applied_names, [MIGRATION], "only 000015 was pending");

    let dump_after = db.dump().await;
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
        "000015 SQLite dump diff\nremoved:\n  {}\nadded:\n  {}",
        show(&removed),
        show(&added)
    );
    assert!(removed.is_empty(), "{removed:#?}");
    assert_eq!(
        added,
        ["TABLE pricing_price_book ::  COLUMN description TEXT NULL DEFAULT - PK 0"]
    );
    let ddl_after = db.ddl().await;
    eprintln!("000015 SQLite table text after:\n{ddl_after}");
    assert_eq!(
        ddl_after.replace(", description text", ""),
        ddl_before,
        "ADD COLUMN appends the column and nothing else"
    );
    // Every book survives: every old column as it was, the description NULL.
    let rows_after: Vec<Value> = db
        .rows()
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
    assert_eq!(rows_after, rows_before);
    // An upgraded database and a fresh one hold the same schema.
    let fresh = Database::connect("sqlite::memory:").await.unwrap();
    assert_eq!(
        dump_after,
        stanza_lines(&schema_dump::migrate_and_dump_sqlite(&fresh).await)
    );
    assert!(db.migrate(None).await.unwrap().applied_names.is_empty());
}

/// The migrated books through the application: they read with no description, take one, and an
/// unused one is deleted at its version.
#[tokio::test]
async fn the_application_reads_edits_and_deletes_a_migrated_book() {
    let db = seeded().await;
    db.migrate(None).await.unwrap();
    let state = entry_support::state_on(
        DBProvider::new(db.pool().await),
        Arc::new(Script::default()),
    )
    .await;
    let app = entry_support::app_for(state, TENANT);
    let ctx = entry_support::user_of(TENANT);
    let path = |id: Uuid| format!("/price-books/{id}");
    let (s, read, tag) = request(&app, &ctx, "GET", &path(DATED), json!({}), None, None).await;
    assert_eq!(s, 200, "{read}");
    assert_eq!(tag, "\"3\"");
    assert!(read["description"].is_null(), "{read}");
    assert_eq!(read["valid_from"], "2027-01-01");
    assert_eq!(read["created_at"], "2026-09-01T09:00:00.123456Z");
    let (s, saved, tag) = request(
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
    assert_eq!(saved["valid_until"], "2027-01-31");
    assert_eq!(tag, "\"4\"");
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
    assert_eq!(list["items"][0]["description"], "dated");
}

// ------------------------------------------------------------------ length caps (D-457)

/// One text over its cap: the request, and the field and code its refusal names.
struct Capped {
    method: &'static str,
    path: String,
    body: Value,
    tag: Option<String>,
    field: &'static str,
    code: &'static str,
}
impl Capped {
    fn new(
        method: &'static str,
        path: impl Into<String>,
        body: Value,
        field: &'static str,
    ) -> Self {
        Self {
            method,
            path: path.into(),
            body,
            tag: None,
            field,
            code: "FIELD_TOO_LONG",
        }
    }
    fn at(self, tag: &str) -> Self {
        Self {
            tag: Some(tag.to_owned()),
            ..self
        }
    }
    fn note(self) -> Self {
        Self {
            code: "NOTE_TOO_LONG",
            ..self
        }
    }
}
/// The resources the capped requests name.
struct Named {
    book: Uuid,
    entry: Uuid,
    plan: Uuid,
    draft: String,
    sku: Uuid,
}
/// A text of `n` ASCII characters.
fn ascii(n: usize) -> String {
    "x".repeat(n)
}
/// Every door's requests with one text over its cap. A text that must name a stored row (a price's
/// `dim_value`, an entry's `dimension_key`, the registry PATCH's `key` and `remove`) is not capped
/// (the second review of W1a, L1): its refusal is the registry's own, and
/// `dimension_values::a_stored_text_over_its_cap_never_locks_the_dimension_registry` covers it.
async fn over_the_caps(f: &Fixture, named: &Named) -> Vec<Capped> {
    let tag_of = |path: String| async move { f.call("GET", &path, json!({}), None, None).await.2 };
    let settings = |field: &str, value: Value| {
        let mut body = json!({
            "default_timing":"advance","default_rounding":"half_even","default_gl":null,
            "default_tax_category":null,"invoice_line_templates":{},"currencies":[]
        });
        body[field] = value;
        body
    };
    let Named {
        book,
        entry,
        plan,
        draft,
        sku,
    } = named;
    let book_tag = tag_of(format!("/price-books/{book}")).await;
    let plan_tag = tag_of(format!("/plans/{plan}")).await;
    // A draft's version is its create's ETag; the price read is the pinned consumer read.
    let price_tag = "\"1\"";
    let entry_tag = tag_of(format!("/price-book-entries/{entry}")).await;
    let settings_tag = tag_of("/settings".to_owned()).await;
    let dimensions_tag = tag_of("/dimension-keys".to_owned()).await;
    let price = |extra: (&str, Value)| {
        let mut body =
            json!({"price":{"rate":"0.10"},"eligibility":"all","effective_from":"2031-04-01"});
        body[extra.0] = extra.1;
        body
    };
    let entry_body = |extra: (&str, Value)| {
        let mut body =
            json!({"usage_rating_policy":policy_support::input(),"sku_id":sku,"model":"per_unit"});
        body[extra.0] = extra.1;
        body
    };
    vec![
        Capped::new(
            "POST",
            "/price-books",
            json!({"code":ascii(65),"name":"n","currency":"EUR"}),
            "code",
        ),
        Capped::new(
            "POST",
            "/price-books",
            json!({"code":"c","name":ascii(201),"currency":"EUR"}),
            "name",
        ),
        Capped::new(
            "PATCH",
            format!("/price-books/{book}"),
            json!({"name":ascii(201)}),
            "name",
        )
        .at(&book_tag),
        Capped::new(
            "POST",
            "/plans",
            json!({"code":ascii(65),"name":"n","book_id":book}),
            "code",
        ),
        Capped::new(
            "POST",
            "/plans",
            json!({"code":"c","name":ascii(201),"book_id":book}),
            "name",
        ),
        Capped::new(
            "POST",
            format!("/plans/{plan}/clone"),
            json!({"code":ascii(65),"name":"n"}),
            "code",
        ),
        Capped::new(
            "POST",
            format!("/plans/{plan}/clone"),
            json!({"code":"c","name":ascii(201)}),
            "name",
        ),
        Capped::new(
            "PATCH",
            format!("/plans/{plan}"),
            json!({"name":ascii(201)}),
            "name",
        )
        .at(&plan_tag),
        Capped::new(
            "POST",
            format!("/price-book-entries/{entry}/prices"),
            price(("note", json!(ascii(2001)))),
            "note",
        )
        .note(),
        Capped::new(
            "PATCH",
            format!("/prices/{draft}"),
            json!({"note":ascii(2001)}),
            "note",
        )
        .at(price_tag)
        .note(),
        Capped::new(
            "POST",
            format!("/price-books/{book}/entries"),
            entry_body(("invoice_line_override", json!(ascii(2001)))),
            "invoice_line_override",
        ),
        Capped::new(
            "PATCH",
            format!("/price-book-entries/{entry}"),
            json!({"invoice_line_override":ascii(2001)}),
            "invoice_line_override",
        )
        .at(&entry_tag),
        Capped::new(
            "PUT",
            "/settings",
            settings("default_gl", json!(ascii(65))),
            "default_gl",
        )
        .at(&settings_tag),
        Capped::new(
            "PUT",
            "/settings",
            settings("default_tax_category", json!(ascii(65))),
            "default_tax_category",
        )
        .at(&settings_tag),
        Capped::new(
            "PUT",
            "/settings",
            settings("invoice_line_templates", json!({"usage":ascii(2001)})),
            "invoice_line_templates",
        )
        .at(&settings_tag),
        Capped::new(
            "PUT",
            "/dimension-keys",
            json!({"items":[{"key":ascii(65),"values":[]}]}),
            "key",
        )
        .at(&dimensions_tag),
        Capped::new(
            "PUT",
            "/dimension-keys",
            json!({"items":[{"key":"region","values":[ascii(65),"eu"]}]}),
            "values",
        )
        .at(&dimensions_tag),
        Capped::new(
            "PATCH",
            "/dimension-keys",
            json!({"key":"region","add":[ascii(65),"eu"]}),
            "add",
        )
        .at(&dimensions_tag),
    ]
}

/// Every text a request writes has an explicit length cap, counted in characters (D-457, the
/// whole-branch review's PS-09 and PS-10): a code, a dimension key and value 64, a name 200, a
/// note 2000 (400 `NOTE_TOO_LONG`), a GL code and a tax category 64, an invoice line template
/// 2000. A longer one is 400 `FIELD_TOO_LONG` on the field and nothing is written: before anything
/// is read, except the two full-replace PUTs (the settings and the dimension registry), which judge
/// against the stored row after their If-Match. The caps themselves pass, in two-byte characters.
#[tokio::test]
async fn every_text_a_request_writes_has_a_length_cap() {
    use bss_products_sdk::models::SkuType;
    let (f, catalog) = plan_support::setup().await;
    let book = plan_support::book(&f, "eur").await;
    let entry = plan_support::entry(&f, book, catalog.sku(SkuType::Usage), "usage", None).await;
    let (created, _) = plan(&f, "pro", book).await;
    let (status, drafted, _) = f
        .call(
            "POST",
            &format!("/price-book-entries/{entry}/prices"),
            json!({"price":{"rate":"0.10"},"eligibility":"all","effective_from":"2031-03-01"}),
            None,
            Some("draft"),
        )
        .await;
    assert_eq!(status, 201, "{drafted}");
    let named = Named {
        book,
        entry,
        plan: id_of(&created["id"]),
        draft: drafted["items"][0]["id"].as_str().unwrap().to_owned(),
        sku: catalog.sku(SkuType::Usage),
    };
    for (i, case) in over_the_caps(&f, &named).await.into_iter().enumerate() {
        let Capped {
            method,
            path,
            body,
            tag,
            field,
            code,
        } = case;
        let key = format!("cap-{i}");
        let (status, b, _) = f
            .call(method, &path, body, tag.as_deref(), Some(&key))
            .await;
        assert_eq!(status, 400, "{method} {path} {field}: {b}");
        let text = b.to_string();
        assert!(text.contains(code), "{method} {path}: {b}");
        assert!(
            text.contains(&format!("\"{field}\"")),
            "{method} {path}: {b}"
        );
    }
    assert_eq!(
        ok(&f, &format!("/price-books/{book}")).await["version"],
        1,
        "nothing written"
    );
    // The caps themselves, two bytes a character. A plan code takes ASCII only (D-468), so the plan
    // meets the name's cap alone.
    let accented = |n: usize| "\u{e9}".repeat(n);
    for (path, body, key) in [
        (
            "/price-books".to_owned(),
            json!({"code":accented(64),"name":accented(200),"currency":"EUR"}),
            Some("book-at-cap"),
        ),
        (
            "/plans".to_owned(),
            json!({"code":"PLAN-AT-CAP","name":accented(200),"book_id":book}),
            Some("plan-at-cap"),
        ),
    ] {
        let (status, b, _) = f.call("POST", &path, body, None, key).await;
        assert_eq!(status, 201, "{path}: {b}");
    }
    let (status, b, _) = f
        .call(
            "PATCH",
            &format!("/prices/{}", named.draft),
            json!({"note":accented(2000)}),
            Some("\"1\""),
            None,
        )
        .await;
    assert_eq!(status, 200, "{b}");
}
