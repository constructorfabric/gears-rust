//! The book's entries list on the toolkit's pager (D-483, ask 51): the order `(sku_id,
//! charge_kind, model, id)`, `limit` 500 by default and at most 500, `cursor`, `$filter` over
//! `sku_id`, `charge_kind`, `model` and `reference_state`, a cursor bound to the filter and the
//! day, D-473's `as_of` refusal judged before any entry is read, and seven statements per page.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod book_support;
mod entry_paging_support;
mod plan_support;
use book_support::{Row, code_of, days, get, ok, stored_book, stored_price, today};
use entry_paging_support::{entry_at, ids_of, walk, with};
use plan_support::{Catalog, Fixture, entry_support, holding, item, plan, setup};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

async fn recorded() -> (Fixture, toolkit_db::test_support::QueryRecorder) {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let f = Fixture::on(db, tenant, dsn, Arc::new(Catalog::default())).await;
    (f, recorder)
}

/// The statements on pricing's tables one read makes, and the tables they read.
async fn statements(
    f: &Fixture,
    recorder: &toolkit_db::test_support::QueryRecorder,
    path: &str,
    n: usize,
) -> Vec<(String, Option<String>)> {
    recorder.clear();
    let b = ok(f, path).await;
    assert_eq!(b["items"].as_array().unwrap().len(), n, "{path}");
    recorder
        .events()
        .into_iter()
        .filter(|q| {
            q.table
                .as_deref()
                .is_some_and(|t| t.starts_with("pricing_"))
        })
        .map(|q| (q.sql, q.table))
        .collect()
}

/// `n` entries of `book`, one per SKU, the SKUs and the ids rising with `i`: the list's order is
/// the seed's.
async fn many(f: &Fixture, book: Uuid, n: usize) -> Vec<Uuid> {
    let base = Uuid::new_v4().as_u128() & !0xffff_ffff;
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let i = u128::try_from(i).unwrap();
        ids.push(
            entry_at(
                f,
                book,
                Uuid::from_u128(base | (0x8000_0000 + i)),
                Uuid::from_u128(base | i),
                "usage",
                None,
                "per_unit",
                "confirmed",
            )
            .await,
        );
    }
    ids
}

// ------------------------------------------------------------------ the order and the pages

/// D-483: the pages cross every boundary of the new order, a usage SKU's and a recurring group's.
// Probed (PROBE-9-8-1): the old in-memory order (`period` before `model`) on the pager.
#[tokio::test]
async fn the_entries_page_in_the_new_order_across_usage_and_recurring_boundaries() {
    let (f, _) = setup().await;
    entry_paging_support::the_pages_cross_every_boundary(&f).await;
}

/// D-483: `$filter` narrows the list and every page of it, and refuses what it does not take.
#[tokio::test]
async fn the_entries_filter_by_sku_charge_kind_model_and_reference_state() {
    let (f, _) = setup().await;
    entry_paging_support::the_filter_narrows_every_page(&f).await;
}

/// D-483: `limit` (alias `$top`) defaults to 500 and is clamped at 500; a book of 501 entries
/// answers 500 and a `next_cursor`, then the last one. A caller that does not follow the cursor
/// sees the first 500 (the breaking change).
#[tokio::test]
async fn a_page_is_500_entries_by_default_and_at_most_500() {
    let (f, _) = setup().await;
    let book = stored_book(&f, "big", time::OffsetDateTime::now_utc()).await;
    let ids = many(&f, book, 501).await;
    let path = format!("/price-books/{book}/entries");
    let first = ok(&f, &path).await;
    assert_eq!(ids_of(&first), ids[..500].to_vec());
    assert_eq!(first["page_info"]["limit"], 500, "{}", first["page_info"]);
    let next = first["page_info"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let last = ok(&f, &with(&path, &[("cursor", &next)])).await;
    assert_eq!(ids_of(&last), vec![ids[500]]);
    assert!(last["page_info"]["next_cursor"].is_null(), "{last}");
    for limit in [("limit", "600"), ("$top", "501"), ("limit", "500")] {
        let page = ok(&f, &with(&path, &[limit])).await;
        assert_eq!(page["page_info"]["limit"], 500, "{limit:?}");
        assert_eq!(page["items"].as_array().unwrap().len(), 500, "{limit:?}");
    }
    let small = ok(&f, &with(&path, &[("$top", "7")])).await;
    assert_eq!(ids_of(&small), ids[..7].to_vec(), "$top is limit's alias");
    let pages = walk(&f, book, &[], 200).await;
    assert_eq!(
        pages.iter().map(Vec::len).collect::<Vec<_>>(),
        [200, 200, 101]
    );
}

/// D-483: the cursor carries a hash of `$filter` and of the day the page is judged on, so a
/// cursor replayed under another filter or another `as_of` is 400 `FILTER_MISMATCH`; the same
/// day spelled as today's `as_of` or left out is one narrowing. `$skiptoken` is `cursor`'s alias.
#[tokio::test]
async fn a_cursor_carries_the_filter_and_the_day() {
    let (f, _) = setup().await;
    let s = entry_paging_support::seeded(&f).await;
    let path = format!("/price-books/{}/entries", s.book);
    let t = today().to_string();
    let filter = format!("sku_id ne {}", s.one_time_sku);
    let first = ok(&f, &with(&path, &[("limit", "2"), ("$filter", &filter)])).await;
    let cursor = first["page_info"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    for (k, v) in [("cursor", cursor.as_str()), ("$skiptoken", cursor.as_str())] {
        let next = ok(
            &f,
            &with(&path, &[("limit", "2"), ("$filter", &filter), (k, v)]),
        )
        .await;
        assert_eq!(ids_of(&next), s.ordered[2..4].to_vec(), "{k}");
    }
    let same_day = ok(
        &f,
        &with(
            &path,
            &[
                ("limit", "2"),
                ("$filter", &filter),
                ("as_of", &t),
                ("cursor", &cursor),
            ],
        ),
    )
    .await;
    assert_eq!(ids_of(&same_day), s.ordered[2..4].to_vec(), "today's as_of");
    let tomorrow = (today() + days(1)).to_string();
    for replay in [
        vec![("cursor", cursor.as_str())],
        vec![("$filter", "model eq 'flat'"), ("cursor", cursor.as_str())],
        vec![
            ("$filter", filter.as_str()),
            ("as_of", tomorrow.as_str()),
            ("cursor", cursor.as_str()),
        ],
    ] {
        let (status, body, _) = get(&f, &with(&path, &replay)).await;
        assert_eq!(status, 400, "{replay:?}: {body}");
        assert!(
            code_of(&body).contains("FILTER_MISMATCH"),
            "{replay:?}: {body}"
        );
    }
    // A cursor minted on another day continues only on that day.
    let dated = ok(&f, &with(&path, &[("limit", "2"), ("as_of", &tomorrow)])).await;
    let dated_cursor = dated["page_info"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body, _) = get(&f, &with(&path, &[("cursor", &dated_cursor)])).await;
    assert_eq!(status, 400, "{body}");
    assert!(code_of(&body).contains("FILTER_MISMATCH"), "{body}");
    let on = ok(
        &f,
        &with(&path, &[("as_of", &tomorrow), ("cursor", &dated_cursor)]),
    )
    .await;
    assert_eq!(ids_of(&on), s.ordered[2..].to_vec());
}

/// D-483 and D-473: the list takes `as_of`, `limit` (`$top`), `cursor` (`$skiptoken`) and
/// `$filter`. Any other plain key and a repeated one are 400 `QUERY_INVALID`; `$orderby` (the
/// order is fixed), `$select` and `$count` are 400; `limit=0`, a cursor that does not read and
/// `$orderby` beside a cursor are the pager's 400s — all before the book is read.
#[tokio::test]
async fn the_entries_list_refuses_what_its_pager_does_not_take() {
    let (f, _) = setup().await;
    let s = entry_paging_support::seeded(&f).await;
    let path = format!("/price-books/{}/entries", s.book);
    let first = ok(&f, &with(&path, &[("limit", "2")])).await;
    let cursor = first["page_info"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    for (query, code) in [
        ("?sku_id=1".to_owned(), "QUERY_INVALID"),
        ("?limit=2&limit=3".to_owned(), "QUERY_INVALID"),
        (
            "?as_of=2026-01-05&as_of=2026-01-06".to_owned(),
            "QUERY_INVALID",
        ),
        ("?status=active".to_owned(), "QUERY_INVALID"),
        ("?$orderby=sku_id%20desc".to_owned(), "400"),
        ("?$select=id".to_owned(), "400"),
        ("?$count=true".to_owned(), "400"),
        ("?limit=0".to_owned(), "400"),
        ("?cursor=not-a-cursor".to_owned(), "400"),
        (format!("?$orderby=id&cursor={cursor}"), "ORDER_WITH_CURSOR"),
    ] {
        let (status, body, _) = get(&f, &format!("{path}{query}")).await;
        assert_eq!(status, 400, "{query}: {body}");
        assert!(
            code == "400" || code_of(&body).contains(code),
            "{query}: {code}: {body}"
        );
        let unknown = format!("/price-books/{}/entries{query}", Uuid::new_v4());
        let (status, body, _) = get(&f, &unknown).await;
        assert_eq!(status, 400, "the query before the book: {query}: {body}");
    }
    let (status, body, _) = get(
        &f,
        &format!("/price-books/{}/entries?limit=2", Uuid::new_v4()),
    )
    .await;
    assert_eq!(status, 404, "{body}");
}

// ------------------------------------------------------------------ the money and the day per page

/// D-473 kept first (D-483): an `as_of` other than today without `price_book` read on the book is
/// 403 `PRICE_BOOK_READ_REQUIRED` before any entry, price or usage is read — the book and the
/// money's book are the only tables the refused read touches.
#[tokio::test]
async fn a_dated_read_without_the_money_is_refused_before_any_entry_is_read() {
    let (f, recorder) = recorded().await;
    let book = stored_book(&f, "dated", time::OffsetDateTime::now_utc()).await;
    many(&f, book, 3).await;
    let reader = holding(&f, "price_book_entry:read");
    let path = format!("/price-books/{book}/entries?as_of={}", today() + days(3));
    recorder.clear();
    let (s, b, _) = f
        .call_as(&reader, "GET", &path, json!({}), None, None)
        .await;
    assert_eq!(s, 403, "{b}");
    assert!(code_of(&b).contains("PRICE_BOOK_READ_REQUIRED"), "{b}");
    let tables: Vec<String> = recorder
        .events()
        .into_iter()
        .filter_map(|q| q.table)
        .filter(|t| t.starts_with("pricing_"))
        .collect();
    assert!(
        tables.iter().all(|t| t == "pricing_price_book"),
        "only the book is read before the refusal: {tables:?}"
    );
    assert!(!tables.is_empty(), "the book's 404 comes first: {tables:?}");
}

/// D-483: each page is judged on the one day of the read — the price in force, the next price,
/// each price's status and the usage split hold on a second page as on the first.
#[tokio::test]
async fn every_page_is_judged_on_its_as_of() {
    let (f, _) = setup().await;
    let book = stored_book(&f, "days", time::OffsetDateTime::now_utc()).await;
    let ids = many(&f, book, 3).await;
    let t = today();
    for id in &ids {
        stored_price(
            &f,
            *id,
            Row::new(1, "approved", t - days(10)).to(t + days(5)),
        )
        .await;
        stored_price(&f, *id, Row::new(2, "approved", t + days(5))).await;
    }
    let path = format!("/price-books/{book}/entries");
    let on = (t + days(6)).to_string();
    let whole = ok(&f, &with(&path, &[("as_of", &on)])).await;
    let pages = {
        let first = ok(&f, &with(&path, &[("as_of", &on), ("limit", "1")])).await;
        let mut pages = vec![first.clone()];
        let mut next = first["page_info"]["next_cursor"]
            .as_str()
            .map(str::to_owned);
        while let Some(cursor) = next {
            let page = ok(
                &f,
                &with(
                    &path,
                    &[("as_of", &on), ("limit", "1"), ("cursor", &cursor)],
                ),
            )
            .await;
            next = page["page_info"]["next_cursor"].as_str().map(str::to_owned);
            pages.push(page);
        }
        pages
    };
    assert_eq!(pages.len(), 3);
    for (i, page) in pages.iter().enumerate() {
        let item = &page["items"][0];
        assert_eq!(item, &whole["items"][i], "page {i} reads as the whole list");
        assert_eq!(item["current_price"]["version_no"], 2, "{item}");
        assert_eq!(item["current_price"]["status"], "active", "{item}");
        assert_eq!(item["usage"]["prices"]["active"], 1, "{item}");
        assert_eq!(item["usage"]["prices"]["superseded"], 1, "{item}");
    }
}

// ------------------------------------------------------------------ fixed statements per page

/// D-483 (D-472's seven): a page makes the same seven statements whatever its size and place —
/// the book, the book under the money's grant, the page, the three usage reads and the default
/// chain — for the first page of 501 entries (500), its last page (1), a filtered page and a
/// whole book of 10. The usage and the prices are read for the page's entries only.
#[tokio::test]
async fn each_page_reads_in_seven_statements() {
    let (f, recorder) = recorded().await;
    let book = stored_book(&f, "pinned", time::OffsetDateTime::now_utc()).await;
    let ids = many(&f, book, 501).await;
    // A plan's draft names the first and the last entry, so each page reads the items' revisions.
    let (_, revision) = plan(&f, "pinned", book).await;
    let conn = f.db.conn().unwrap();
    for id in [ids[0], ids[500]] {
        let e = bss_pricing::infra::storage::repo::price_book_entry_repo::find(
            &conn,
            &plan_support::scope(&f),
            f.ctx.subject_tenant_id(),
            id,
        )
        .await
        .unwrap()
        .unwrap();
        item(&f, revision, e.sku_id, Some(id), "paid").await;
    }
    let path = format!("/price-books/{book}/entries");
    let first = statements(&f, &recorder, &path, 500).await;
    let page = ok(&f, &path).await;
    let next = page["page_info"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let last = statements(&f, &recorder, &with(&path, &[("cursor", &next)]), 1).await;
    let sku = bss_pricing::infra::storage::repo::price_book_entry_repo::find(
        &conn,
        &plan_support::scope(&f),
        f.ctx.subject_tenant_id(),
        ids[0],
    )
    .await
    .unwrap()
    .unwrap()
    .sku_id;
    let filtered = statements(
        &f,
        &recorder,
        &with(&path, &[("$filter", &format!("sku_id eq {sku}"))]),
        1,
    )
    .await;
    for (what, seen) in [("first", &first), ("last", &last), ("filtered", &filtered)] {
        for (i, (sql, table)) in seen.iter().enumerate() {
            eprintln!(
                "{what} statement {i} ({}): {sql}",
                table.as_deref().unwrap_or("-")
            );
        }
        assert_eq!(seen.len(), 7, "{what}: {seen:#?}");
    }
    let tables = |seen: &[(String, Option<String>)]| -> Vec<Option<String>> {
        seen.iter().map(|(_, t)| t.clone()).collect()
    };
    assert_eq!(
        tables(&first),
        tables(&last),
        "the same reads on every page"
    );
    assert_eq!(tables(&first), tables(&filtered));
    // The usage and the prices read the page's 500 entries, never the book's 501.
    let page_reads = first
        .iter()
        .filter(|(_, t)| t.as_deref() == Some("pricing_price_book_entry"))
        .count();
    assert_eq!(page_reads, 1, "one read of the entries: {first:#?}");
}

// ------------------------------------------------------------------ the served contract

/// D-483: the served list declares `limit` and `cursor` beside `as_of`, publishes its `$filter`
/// vocabulary and no `$orderby`, names its order, its page and what it refuses, and points a SKU
/// search at Products; its answer carries `page_info`.
#[tokio::test]
async fn the_served_list_says_how_it_orders_filters_and_pages() {
    let (f, _) = setup().await;
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    let _router = bss_pricing::api::rest::authoring::router(f.state, &openapi);
    let api = serde_json::to_value(
        openapi
            .build_openapi(&toolkit::api::OpenApiInfo::default())
            .unwrap(),
    )
    .unwrap();
    let op = &api["paths"]["/bss-pricing/v1/price-books/{id}/entries"]["get"];
    let mut names: Vec<&str> = op["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["in"] == "query")
        .filter_map(|p| p["name"].as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["$filter", "as_of", "cursor", "limit"], "{op}");
    let mut fields: Vec<&str> = op["x-odata-filter"]["allowedFields"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        ["charge_kind", "model", "reference_state", "sku_id"]
    );
    assert!(op["x-odata-orderby"].is_null(), "the order is fixed: {op}");
    let text = op["description"].as_str().unwrap();
    for said in [
        "sku_id, charge_kind, model and id",
        "500",
        "next_cursor",
        "FILTER_MISMATCH",
        "$filter=sku_id in",
        "GET /bss-products/v1/skus?q=",
        "QUERY_INVALID",
        "PRICE_BOOK_READ_REQUIRED",
        "before any entry is read",
    ] {
        assert!(text.contains(said), "the list says {said}: {text}");
    }
    let list = &api["components"]["schemas"]["PricingPriceBookEntryList"];
    assert!(
        list["properties"]["page_info"].is_object(),
        "the list pages: {list}"
    );
}
