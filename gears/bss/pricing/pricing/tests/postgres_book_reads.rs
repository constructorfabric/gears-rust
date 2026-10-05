//! The Price Books screen's reads on `PostgreSQL` (D-440 to D-442): `q` folds Unicode case through
//! the ICU root collation — on a `C`-locale database too — and the date filters, the order and
//! the cursor hold on the engine production runs; the stats' grouped statements, the last change
//! compared as instants to the microsecond, the dated counts and the prices list read the same
//! on Postgres as on `SQLite`, a plan only history holds among the counts.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod book_support;
mod pg_support;
mod plan_support;
use book_support::{
    Row, bare_revision, codes, days, door_book, encode, instant, ok, stored_book, stored_entry,
    stored_price, today, unit_on,
};
use bss_approval::UnitState;
use bss_products_sdk::models::SkuType;
use plan_support::{Catalog, Fixture, id_of, item, plan, publish};
use serde_json::json;
use std::sync::Arc;
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

async fn fixture(pg: &pg_support::Pg) -> (Fixture, Arc<Catalog>) {
    let catalog = Arc::new(Catalog::default());
    let db = DBProvider::<DbError>::new(pg.db().await);
    let f = Fixture::on(
        db,
        Uuid::new_v4(),
        plan_support::entry_support::TestDsn::of(pg.url(true)),
        catalog.clone(),
    )
    .await;
    (f, catalog)
}

/// D-442 on a `C`-locale database — `initdb --locale=C`, `CloudNativePG`'s default — where the
/// database's own `lower()` folds ASCII only: `q` still folds Unicode case through `und-x-icu`,
/// and takes `%` and `_` literally; the date filters, `eq null`, the order and the cursor hold.
// Probed in run 7.1: the Postgres fold through the database's own lower().
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_book_list_searches_filters_and_pages_on_a_c_locale_postgres() {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let pg = pg_support::Pg::applied_in_c_locale().await;
    let raw = pg.raw().await;
    let row = raw
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT datctype, lower('\u{411}\u{415}\u{422}\u{410}') = '\u{431}\u{435}\u{442}\u{430}' AS folds \
             FROM pg_database WHERE datname = current_database()",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            row.try_get::<String>("", "datctype").unwrap(),
            row.try_get::<bool>("", "folds").unwrap()
        ),
        ("C".to_owned(), false),
        "the database's lower() folds ASCII only"
    );
    raw.close().await.unwrap();
    let (f, _) = fixture(&pg).await;
    // Beta, in capital Cyrillic, as a code and a name; Latin books beside it.
    let beta = "\u{411}\u{415}\u{422}\u{410}";
    door_book(&f, beta, beta, "EUR", Some("2026-01-01"), None).await;
    door_book(&f, "stor", "Storage 100%", "EUR", None, Some("2027-01-01")).await;
    door_book(&f, "u_s", "Dollars", "USD", Some("2027-01-01"), None).await;
    for (q, expected) in [
        ("\u{431}\u{435}\u{442}\u{430}", vec![beta]),
        ("\u{411}\u{435}\u{442}", vec![beta]),
        (beta, vec![beta]),
        ("STOR", vec!["stor"]),
        ("100%", vec!["stor"]),
        ("_", vec!["u_s"]),
        ("dollars", vec!["u_s"]),
    ] {
        let b = ok(&f, &format!("/price-books?q={}", encode(q))).await;
        assert_eq!(codes(&b), expected, "q={q}: {b:#}");
    }
    for (query, expected) in [
        ("$filter=valid_from ge 2026-06-01", vec!["u_s"]),
        ("$filter=valid_from eq null", vec!["stor"]),
        ("$filter=valid_until lt 2028-01-01", vec!["stor"]),
        (
            "$filter=currency eq 'EUR' and valid_from ne null",
            vec![beta],
        ),
        // The `C` collation orders bytes: Latin before Cyrillic.
        ("$orderby=name desc", vec![beta, "stor", "u_s"]),
    ] {
        let b = ok(&f, &format!("/price-books?{}", encode(query))).await;
        assert_eq!(codes(&b), expected, "{query}: {b:#}");
    }
    let first = ok(&f, "/price-books?$top=2").await;
    assert_eq!(codes(&first), ["stor", "u_s"]);
    let cursor = first["page_info"]["next_cursor"].as_str().unwrap();
    assert_eq!(
        codes(&ok(&f, &format!("/price-books?$top=2&cursor={cursor}")).await),
        [beta]
    );
    let (s, b, _) = f
        .call(
            "GET",
            &format!("/price-books?q=s&cursor={cursor}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 400, "{b}");
    assert!(b.to_string().contains("FILTER_MISMATCH"), "{b}");
}

/// D-440 and D-441 on Postgres: the stats of a hand-built book, its last change as the latest
/// instant to the microsecond, the dated counts, the price in force and the prices list.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_stats_the_dated_counts_and_the_prices_hold_on_postgres() {
    let pg = pg_support::Pg::applied().await;
    let (f, catalog) = fixture(&pg).await;
    let day = today();
    let at = |s: &str| instant(s);
    let book = stored_book(&f, "pg", at("2026-09-01T09:00:00Z")).await;
    let sku = catalog.sku(SkuType::Usage);
    let entry = stored_entry(&f, book, sku, "per_unit", at("2026-09-02T09:00:00.418681Z")).await;
    let e2 = stored_entry(
        &f,
        book,
        catalog.sku(SkuType::Usage),
        "per_unit",
        at("2026-09-02T09:00:00Z"),
    )
    .await;
    let old = at("2026-09-02T09:00:00.41868Z");
    let mut p = Vec::new();
    for row in [
        Row::new(1, "approved", day - days(30)).to(day - days(10)),
        Row::new(2, "approved", day - days(10)).to(day + days(10)),
        Row::new(3, "approved", day + days(10)),
        Row::new(4, "draft", day + days(20)),
        Row::new(5, "pending", day + days(21)),
        Row::new(6, "rejected", day + days(22)),
        Row::new(7, "approved", day - days(5)).on("eu"),
    ] {
        p.push(stored_price(&f, entry, row.updated(old)).await);
    }
    stored_price(&f, e2, Row::new(1, "rejected", day + days(1)).updated(old)).await;
    let (plan_a, a1) = plan(&f, "a", book).await;
    let plan_a = id_of(&plan_a["id"]);
    item(&f, a1, sku, Some(entry), "paid").await;
    publish(&f, plan_a, a1).await;
    plan(&f, "b", book).await;
    // c names the book only through a superseded revision: it moved to another book.
    let away = stored_book(&f, "away", at("2026-09-01T09:00:00Z")).await;
    let (plan_c, c1) = plan(&f, "c", book).await;
    let plan_c = id_of(&plan_c["id"]);
    publish(&f, plan_c, c1).await;
    let c2 = bare_revision(&f, plan_c, 2, away).await;
    publish(&f, plan_c, c2).await;
    unit_on(
        &f,
        "prices",
        book,
        UnitState::Pending,
        at("2026-09-01T10:00:00Z"),
        None,
    )
    .await;
    let read = ok(&f, &format!("/price-books/{book}")).await;
    assert_eq!(
        read["stats"],
        json!({
            "entries": 2,
            "skus": 2,
            "plans": 2,
            "plans_superseded_only": 1,
            "prices": {
                "draft": 1,
                "pending": 1,
                "approved": 4,
                "scheduled": 1,
                "active": 2,
                "superseded": 1,
                "rejected": 2,
            },
            "pending_units": 1,
            "last_change_at": "2026-09-02T09:00:00.418681Z",
        }),
        "{read:#}"
    );
    // A decision half a second later than the entry's second moves it; the list agrees.
    unit_on(
        &f,
        "prices",
        book,
        UnitState::Approved,
        at("2026-09-02T09:00:00Z"),
        Some(at("2026-09-02T09:00:00.5Z")),
    )
    .await;
    let listed = ok(&f, "/price-books?q=pg").await;
    assert_eq!(
        listed["items"][0]["stats"]["last_change_at"], "2026-09-02T09:00:00.5Z",
        "{listed:#}"
    );
    // The entry's dated counts, its price in force and its prices in chain order.
    let read = ok(&f, &format!("/price-book-entries/{entry}")).await;
    assert_eq!(
        read["usage"]["prices"],
        json!({"approved": 4, "pending": 1, "draft": 1, "scheduled": 1, "active": 2, "superseded": 1})
    );
    assert_eq!(read["current_price"]["id"], p[1].id.to_string());
    let prices = ok(
        &f,
        &format!("/price-book-entries/{entry}/prices?status=active,superseded"),
    )
    .await;
    let listed: Vec<(String, String)> = prices["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["id"].as_str().unwrap().to_owned(),
                i["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            (p[0].id.to_string(), "superseded".to_owned()),
            (p[1].id.to_string(), "active".to_owned()),
            (p[6].id.to_string(), "active".to_owned()),
        ]
    );
    // The book list's sku_id narrowing on Postgres.
    assert_eq!(
        codes(&ok(&f, &format!("/price-books?sku_id={sku}")).await),
        ["pg"]
    );
}

/// D-480 on Postgres: `$filter` on `id` (`eq`, `in`), a malformed uuid (400) and the cursor hash.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_book_list_filters_by_id_on_postgres() {
    let pg = pg_support::Pg::applied_in_c_locale().await;
    let (f, _) = fixture(&pg).await;
    let euro = door_book(&f, "a-eur", "Euro", "EUR", None, None).await;
    let other = door_book(&f, "b-eur", "Other", "EUR", None, None).await;
    let dollars = door_book(&f, "c-usd", "Dollars", "USD", None, None).await;
    assert_eq!(
        codes(
            &ok(
                &f,
                &format!("/price-books?{}", encode(&format!("$filter=id eq {euro}")))
            )
            .await
        ),
        ["a-eur"]
    );
    assert_eq!(
        codes(
            &ok(
                &f,
                &format!(
                    "/price-books?{}",
                    encode(&format!("$filter=id in ({other}, {dollars})"))
                ),
            )
            .await
        ),
        ["b-eur", "c-usd"]
    );
    let (s, body, _) = f
        .call(
            "GET",
            &format!("/price-books?{}", encode("$filter=id eq not-a-uuid")),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 400, "{body}");
    let first = ok(
        &f,
        &format!(
            "/price-books?{}",
            encode(&format!("$filter=id in ({euro}, {other})&$top=1"))
        ),
    )
    .await;
    let cursor = first["page_info"]["next_cursor"].as_str().unwrap();
    let (s, body, _) = f
        .call(
            "GET",
            &format!(
                "/price-books?{}",
                encode(&format!("$filter=id eq {dollars}&cursor={cursor}"))
            ),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 400, "{body}");
    assert!(body.to_string().contains("FILTER_MISMATCH"), "{body}");
}
