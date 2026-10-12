//! Where a SKU is priced and sold (D-434): the SKU's entries across the tenant's books with their
//! usage and the default chain's price in force today, the plans that name the SKU through an
//! entry, and one plan item with its revision and plan — each list in a fixed number of
//! statements, whatever the number of rows.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
use bss_pricing::infra::storage::{
    entity::{plan_revision, price},
    repo::{plan_revision_repo, price_book_entry_repo, price_repo},
};
use bss_products_sdk::models::SkuType;
use plan_support::{
    Catalog, Fixture, book, entry, entry_support, holding, id_of, item, plan, publish, scope,
    setup, stranger,
};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

fn today() -> time::Date {
    time::OffsetDateTime::now_utc().date()
}
/// A price of `entry` written straight through the repository (the doors cannot write an
/// approved, pending or rejected price without a unit).
#[allow(
    clippy::too_many_arguments,
    reason = "a stored price row, spelled out where the test reads it"
)]
async fn price_at(
    f: &Fixture,
    entry: Uuid,
    version_no: i32,
    state: &str,
    dim: Option<&str>,
    from: time::Date,
    to: Option<time::Date>,
    rate: &str,
) -> price::Model {
    let conn = f.db.conn().unwrap();
    let e = price_book_entry_repo::find(&conn, &scope(f), f.ctx.subject_tenant_id(), entry)
        .await
        .unwrap()
        .unwrap();
    let mut p = entry_support::price(&e);
    p.version_no = version_no;
    p.state = state.into();
    p.dim_value = dim.map(str::to_owned);
    p.effective_from = from;
    p.effective_to = to;
    p.price_json = json!({ "rate": rate });
    price_repo::insert(&conn, &scope(f), p).await.unwrap()
}
/// A draft revision `rev_no` of `plan` on `book`, with no items, written through the repository.
async fn bare_revision(f: &Fixture, plan: Uuid, rev_no: i32, book: Uuid) -> Uuid {
    let now = time::OffsetDateTime::now_utc();
    plan_revision_repo::insert(
        &f.db.conn().unwrap(),
        &scope(f),
        plan_revision::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            plan_id: plan,
            rev_no,
            book_id: book,
            state: "draft".into(),
            available_from: None,
            pending_unit_id: None,
            approved_by_unit_id: None,
            published_at: None,
            version: 1,
            created_by: f.ctx.subject_id(),
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap()
    .id
}
async fn get(f: &Fixture, path: &str) -> (u16, Value, String) {
    f.call("GET", path, json!({}), None, None).await
}
/// The SKU's entries by id, as the list answers them.
async fn sku_entries(f: &Fixture, sku: Uuid) -> Vec<Value> {
    let (s, b, _) = get(f, &format!("/price-book-entries?sku_id={sku}")).await;
    assert_eq!(s, 200, "{b}");
    b["items"].as_array().unwrap().clone()
}
/// An entry's usage (D-428): its approved prices as `(scheduled, active, superseded)` today, whose
/// sum is `approved` (D-440), its pending and draft prices, and its plans.
fn usage(
    approved: (u64, u64, u64),
    pending: u64,
    draft: u64,
    plans: u64,
    superseded_only: u64,
) -> Value {
    let (scheduled, active, superseded) = approved;
    json!({
        "prices": {
            "approved": scheduled + active + superseded,
            "pending": pending,
            "draft": draft,
            "scheduled": scheduled,
            "active": active,
            "superseded": superseded,
        },
        "plans": plans,
        "plans_superseded_only": superseded_only,
    })
}
async fn usd_book(f: &Fixture) -> Uuid {
    let (s, b, _) = f
        .call(
            "POST",
            "/price-books",
            json!({"code":"a-usd","name":"Dollars","currency":"USD"}),
            None,
            Some("book-usd"),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    id_of(&b["id"])
}

// ------------------------------------------------------------------ GET /price-book-entries

#[tokio::test]
async fn a_skus_entries_are_listed_across_books_with_their_usage_and_the_price_in_force() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "b-eur").await;
    let usd = usd_book(&f).await;
    let sku = catalog.sku(SkuType::Usage);
    let other = catalog.sku(SkuType::Usage);
    let e_eur = entry(&f, eur, sku, "usage", None).await;
    let e_usd = entry(&f, usd, sku, "usage", None).await;
    let e_other = entry(&f, eur, other, "usage", None).await;
    let t = today();
    let day = time::Duration::days(1);
    // The default chain of e_eur: one price before, the one in force today, one scheduled; a
    // value chain's price and a pending price are never "the default chain's price in force".
    price_at(
        &f,
        e_eur,
        1,
        "approved",
        None,
        t - day * 30,
        Some(t - day * 10),
        "0.10",
    )
    .await;
    let current = price_at(
        &f,
        e_eur,
        2,
        "approved",
        None,
        t - day * 10,
        Some(t + day * 10),
        "0.20",
    )
    .await;
    price_at(&f, e_eur, 3, "approved", None, t + day * 10, None, "0.30").await;
    price_at(
        &f,
        e_eur,
        4,
        "approved",
        Some("eu"),
        t - day * 3,
        None,
        "0.25",
    )
    .await;
    price_at(&f, e_eur, 5, "pending", None, t + day * 20, None, "0.40").await;
    price_at(&f, e_eur, 6, "rejected", None, t + day * 21, None, "0.50").await;
    // e_usd: only a value chain and a draft — nothing of the default chain in force.
    price_at(
        &f,
        e_usd,
        1,
        "approved",
        Some("eu"),
        t - day * 3,
        None,
        "0.26",
    )
    .await;
    price_at(&f, e_usd, 2, "draft", None, t + day, None, "0.27").await;
    price_at(&f, e_other, 1, "approved", None, t - day, None, "9.99").await;
    // A live plan names e_eur; an included item of the SKU without an entry counts nothing.
    let (_, revision) = plan(&f, "pro", eur).await;
    item(&f, revision, sku, Some(e_eur), "paid").await;

    let items = sku_entries(&f, sku).await;
    assert_eq!(items.len(), 2, "{items:#?}");
    // Ordered by book name (D-486): "Dollars" before "b-eur".
    let (first, second) = (&items[0], &items[1]);
    assert_eq!(first["id"], e_usd.to_string());
    assert_eq!(first["book_id"], usd.to_string());
    assert_eq!(first["book_code"], "a-usd");
    assert_eq!(first["book_name"], "Dollars");
    assert_eq!(first["currency"], "USD");
    assert_eq!(first["sku_id"], sku.to_string());
    assert_eq!(first["charge_kind"], "usage");
    assert_eq!(first["model"], "per_unit");
    assert_eq!(first["reference_state"], "confirmed");
    assert!(first["period"].is_null());
    assert_eq!(first["usage"], usage((0, 1, 0), 0, 1, 0, 0));
    assert!(
        first["current_price"].is_null(),
        "no default-chain price in force: {first}"
    );
    assert_eq!(second["id"], e_eur.to_string());
    assert_eq!(second["book_name"], "b-eur");
    assert_eq!(second["currency"], "EUR");
    assert_eq!(second["usage"], usage((1, 2, 1), 1, 0, 1, 0));
    assert_eq!(second["current_price"]["id"], current.id.to_string());
    assert_eq!(
        second["current_price"]["price_json"],
        json!({"rate":"0.20"})
    );
    assert_eq!(second["current_price"]["status"], "active");
    assert_eq!(second["current_price"]["model"], "per_unit");
    assert!(second["current_price"]["dim_value"].is_null());

    // Another tenant's caller sees nothing of this SKU; an unknown SKU is an empty list.
    let (s, b, _) = f
        .call_as(
            &stranger(),
            "GET",
            &format!("/price-book-entries?sku_id={sku}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["items"], json!([]));
    assert_eq!(b["page_info"]["limit"], 500);
    assert!(sku_entries(&f, Uuid::new_v4()).await.is_empty());
}

// Probed in run 6.4: the money is shown only to a holder of price_book read (the export's grant).
#[tokio::test]
async fn the_price_in_force_is_shown_only_to_a_holder_of_price_book_read() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    let e = entry(&f, eur, sku, "usage", None).await;
    let in_force = price_at(
        &f,
        e,
        1,
        "approved",
        None,
        today() - time::Duration::days(1),
        None,
        "0.10",
    )
    .await;
    // The user holds every grant: the money is there.
    assert_eq!(
        sku_entries(&f, sku).await[0]["current_price"]["id"],
        in_force.id.to_string()
    );
    // Entry read alone: the entry, its book and its usage, and no money.
    let (s, b, _) = f
        .call_as(
            &holding(&f, "price_book_entry:read"),
            "GET",
            &format!("/price-book-entries?sku_id={sku}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["items"][0]["id"], e.to_string());
    assert_eq!(b["items"][0]["currency"], "EUR");
    assert_eq!(b["items"][0]["usage"], usage((0, 1, 0), 0, 0, 0, 0));
    assert!(
        b["items"][0]
            .as_object()
            .unwrap()
            .contains_key("current_price")
            && b["items"][0]["current_price"].is_null(),
        "the field is there, null: {b}"
    );
    // Price-book read alone does not read entries.
    let (s, _, _) = f
        .call_as(
            &holding(&f, "price_book:read"),
            "GET",
            &format!("/price-book-entries?sku_id={sku}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 403);
}

#[tokio::test]
async fn the_entry_list_needs_exactly_one_well_formed_sku_id() {
    let (f, _) = setup().await;
    let sku = Uuid::new_v4();
    for query in [
        String::new(),
        "?sku_id=".to_owned(),
        "?sku_id=not-a-uuid".to_owned(),
        format!("?sku_id={sku}&sku_id={sku}"),
        "?limit=10".to_owned(),
    ] {
        let (s, b, _) = get(&f, &format!("/price-book-entries{query}")).await;
        assert_eq!(s, 400, "{query}: {b}");
        assert!(b.to_string().contains("QUERY_INVALID"), "{query}: {b}");
    }
    // Authorization is judged first.
    let (s, _, _) = plan_support::request(
        &f.denied,
        &f.ctx,
        "GET",
        "/price-book-entries",
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(s, 403);
}

// ------------------------------------------------------------------ GET /plans?sku_id=

#[tokio::test]
async fn the_plans_of_a_sku_are_those_that_name_it_through_an_entry() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    let other = catalog.sku(SkuType::Usage);
    let e = entry(&f, eur, sku, "usage", None).await;
    let e_other = entry(&f, eur, other, "usage", None).await;
    // draft: a draft revision names the entry.
    let (draft, d1) = plan(&f, "c-draft", eur).await;
    item(&f, d1, sku, Some(e), "paid").await;
    // live: published, names it.
    let (live, l1) = plan(&f, "a-live", eur).await;
    let live_id = id_of(&live["id"]);
    item(&f, l1, sku, Some(e), "optional").await;
    publish(&f, live_id, l1).await;
    // history: named it only through a superseded revision.
    let (history, h1) = plan(&f, "b-history", eur).await;
    let history_id = id_of(&history["id"]);
    item(&f, h1, sku, Some(e), "paid").await;
    publish(&f, history_id, h1).await;
    let h2 = bare_revision(&f, history_id, 2, eur).await;
    publish(&f, history_id, h2).await;
    // included: names the SKU only through an included item without an entry.
    let (_, i1) = plan(&f, "d-included", eur).await;
    item(&f, i1, sku, None, "included").await;
    // elsewhere: names another SKU.
    let (_, o1) = plan(&f, "e-elsewhere", eur).await;
    item(&f, o1, other, Some(e_other), "paid").await;

    let (s, b, _) = get(&f, &format!("/plans?sku_id={sku}")).await;
    assert_eq!(s, 200, "{b}");
    let codes: Vec<&str> = b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["A-LIVE", "C-DRAFT"], "{b}");
    // One response schema: each plan as GET /plans answers it, with its revision headers.
    let (_, all, _) = get(&f, "/plans").await;
    let whole = |code: &str| {
        all["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["code"] == code)
            .unwrap()
            .clone()
    };
    assert_eq!(b["items"][0], whole("A-LIVE"));
    assert_eq!(b["items"][1], whole("C-DRAFT"));
    assert_eq!(b["items"][1]["id"], draft["id"]);
    assert_eq!(all["items"].as_array().unwrap().len(), 5);
    assert_eq!(
        whole("B-HISTORY")["revisions"].as_array().unwrap().len(),
        2,
        "the plain list keeps every revision header"
    );
    // The plans of the other SKU; an unknown SKU has none; another tenant sees none.
    let (_, b, _) = get(&f, &format!("/plans?sku_id={other}")).await;
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
    let (_, b, _) = get(&f, &format!("/plans?sku_id={}", Uuid::new_v4())).await;
    assert_eq!(
        b,
        json!({"items": [], "page_info": {"next_cursor": null, "prev_cursor": null, "limit": 500}})
    );
    let (s, b, _) = f
        .call_as(
            &stranger(),
            "GET",
            &format!("/plans?sku_id={sku}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(
        (s, b),
        (
            200,
            json!({"items": [], "page_info": {"next_cursor": null, "prev_cursor": null, "limit": 500}})
        )
    );
    for query in ["?sku_id=nope", "?sku=1", "?sku_id="] {
        let (s, b, _) = get(&f, &format!("/plans{query}")).await;
        assert_eq!(s, 400, "{query}: {b}");
        assert!(b.to_string().contains("QUERY_INVALID"), "{query}: {b}");
    }
}

// ------------------------------------------------------------------ a stored scheduled revision

/// D-446: a revision stored `scheduled` and not yet due reads as stored wherever a read renders a
/// revision state (its effective state is its stored one, D-447), and counts as a live revision
/// wherever a count reads one. No read answers 500 on it; `/resolve` refuses it before its date
/// (D-454).
#[tokio::test]
async fn a_stored_scheduled_revision_reads_as_stored_and_counts_as_live() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    let e = entry(&f, eur, sku, "usage", None).await;
    let (created, r1) = plan(&f, "pro", eur).await;
    let plan_id = id_of(&created["id"]);
    item(&f, r1, sku, Some(e), "paid").await;
    publish(&f, plan_id, r1).await;
    // Rev 2, available in two days, approved and waiting: scheduled through the repository.
    let r2 = bare_revision(&f, plan_id, 2, eur).await;
    let (conn, tenant) = (f.db.conn().unwrap(), f.ctx.subject_tenant_id());
    let mut dated = plan_revision_repo::find(&conn, &scope(&f), tenant, r2)
        .await
        .unwrap()
        .unwrap();
    dated.available_from = Some(today() + time::Duration::days(2));
    plan_revision_repo::update_draft(&conn, &scope(&f), dated)
        .await
        .unwrap();
    let waiting = item(&f, r2, sku, Some(e), "optional").await;
    let unit = plan_support::lock(&f, r2).await;
    plan_revision_repo::schedule(
        &conn,
        &scope(&f),
        tenant,
        r2,
        unit,
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();

    let (s, b, _) = get(&f, &format!("/plans/{plan_id}")).await;
    assert_eq!(s, 200, "{b}");
    let states: Vec<&str> = b["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["published", "scheduled"], "{b}");
    assert_eq!(b["published_rev"], 1);
    let (s, b, _) = get(&f, "/plans").await;
    assert_eq!(s, 200, "{b}");
    let (s, b, _) = get(&f, &format!("/plan-revisions/{r2}")).await;
    assert_eq!((s, b["state"].as_str()), (200, Some("scheduled")), "{b}");
    let (s, b, _) = get(&f, &format!("/plan-items/{}", waiting.id)).await;
    assert_eq!((s, b["state"].as_str()), (200, Some("scheduled")), "{b}");
    // The counts: the plan is on the SKU through both revisions, once.
    let (s, b, _) = get(&f, &format!("/plans?sku_id={sku}")).await;
    assert_eq!(
        (s, b["items"].as_array().map(Vec::len)),
        (200, Some(1)),
        "{b}"
    );
    assert_eq!(sku_entries(&f, sku).await[0]["usage"]["plans"], 1);
    let usage = bss_pricing::infra::usage::sku_usage(&conn, &scope(&f), tenant, &[sku])
        .await
        .unwrap();
    assert_eq!(usage[0].plans, 1);
    let (s, b, _) = get(
        &f,
        &format!("/resolve?plan_revision_id={r2}&date={}", today()),
    )
    .await;
    assert_eq!(s, 409, "{b}");
    assert!(b.to_string().contains("REVISION_NOT_YET_AVAILABLE"), "{b}");
}

// ------------------------------------------------------------------ GET /plan-items/{id}

#[tokio::test]
async fn a_plan_item_reads_with_its_revision_and_plan() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    let e = entry(&f, eur, sku, "usage", None).await;
    let (created, r1) = plan(&f, "pro", eur).await;
    let stored = item(&f, r1, sku, Some(e), "paid").await;
    let (s, b, tag) = get(&f, &format!("/plan-items/{}", stored.id)).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(tag, "\"1\"", "the item's version, the PATCH's If-Match");
    assert_eq!(b["id"], stored.id.to_string());
    assert_eq!(b["plan_id"], created["id"]);
    assert_eq!(b["revision_id"], r1.to_string());
    assert_eq!(b["rev_no"], 1);
    assert_eq!(b["state"], "draft");
    assert_eq!(b["sku_id"], sku.to_string());
    assert_eq!(b["price_book_entry_id"], e.to_string());
    assert!(b.get("treatment").is_none(), "D-467: {b}");
    assert_eq!(b["reference_state"], "confirmed");
    assert_eq!(b["version"], 1);
    // The tag is the one the PATCH takes.
    let patched = f
        .call(
            "PATCH",
            &format!("/plan-items/{}", stored.id),
            json!({"price_book_entry_id":e}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(patched.0, 200, "{patched:?}");
    publish(&f, id_of(&created["id"]), r1).await;
    let (_, b, tag) = get(&f, &format!("/plan-items/{}", stored.id)).await;
    assert_eq!(
        (b["state"].as_str(), tag.as_str()),
        (Some("published"), "\"2\"")
    );
    for (who, id) in [(stranger(), stored.id), (f.ctx.clone(), Uuid::new_v4())] {
        let (s, _, _) = f
            .call_as(
                &who,
                "GET",
                &format!("/plan-items/{id}"),
                json!({}),
                None,
                None,
            )
            .await;
        assert_eq!(s, 404);
    }
    let (s, _, _) = f
        .call_as(
            &holding(&f, "price_book_entry:read"),
            "GET",
            &format!("/plan-items/{}", stored.id),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 403, "plan read is the grant");
}

// ------------------------------------------------------------------ fixed statements

/// The statements on pricing's tables one read makes.
async fn statements(
    f: &Fixture,
    recorder: &toolkit_db::test_support::QueryRecorder,
    path: &str,
    n: usize,
) -> Vec<(String, usize)> {
    recorder.clear();
    let (s, b, _) = get(f, path).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["items"].as_array().unwrap().len(), n, "{path}");
    recorder
        .events()
        .into_iter()
        .filter(|q| {
            q.table
                .as_deref()
                .is_some_and(|t| t.starts_with("pricing_"))
        })
        .map(|q| (q.sql, q.param_count))
        .collect()
}
/// A SKU with `n` entries, one per book, each with an approved default-chain price in force and
/// named by the paid item of a draft revision of its own plan.
async fn seeded_sku(f: &Fixture, catalog: &Catalog, tag: &str, n: usize) -> Uuid {
    let sku = catalog.sku(SkuType::Usage);
    for i in 0..n {
        let own = book(f, &format!("{tag}-{i:03}")).await;
        let e = entry(f, own, sku, "usage", None).await;
        price_at(
            f,
            e,
            1,
            "approved",
            None,
            today() - time::Duration::days(1),
            None,
            "0.10",
        )
        .await;
        let (_, revision) = plan(f, &format!("{tag}-plan-{i:03}"), own).await;
        item(f, revision, sku, Some(e), "paid").await;
    }
    sku
}

// Probed in run 6.4: a per-row read in either list is red here.
#[tokio::test]
async fn the_two_sku_lists_read_in_the_same_statements_for_10_and_100_rows() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog.clone()).await;
    let small = seeded_sku(&f, &catalog, "s", 10).await;
    let large = seeded_sku(&f, &catalog, "l", 100).await;
    for (what, ten, hundred) in [
        (
            "entries",
            statements(
                &f,
                &recorder,
                &format!("/price-book-entries?sku_id={small}"),
                10,
            )
            .await,
            statements(
                &f,
                &recorder,
                &format!("/price-book-entries?sku_id={large}"),
                100,
            )
            .await,
        ),
        (
            "plans",
            statements(&f, &recorder, &format!("/plans?sku_id={small}"), 10).await,
            statements(&f, &recorder, &format!("/plans?sku_id={large}"), 100).await,
        ),
    ] {
        for (i, (sql, binds)) in hundred.iter().enumerate() {
            eprintln!("{what} statement {i} ({binds} binds): {sql}");
        }
        assert_eq!(
            ten.len(),
            hundred.len(),
            "{what}: the statements grow with the rows: {ten:#?} vs {hundred:#?}"
        );
        assert_eq!(
            ten.iter().map(|(sql, _)| sql).collect::<Vec<_>>(),
            hundred.iter().map(|(sql, _)| sql).collect::<Vec<_>>(),
            "{what}: the same statements, whatever the size"
        );
    }
    // The plain plan list is set-based too: 110 plans in five statements (D-485).
    let plain = statements(&f, &recorder, "/plans", 110).await;
    assert_eq!(plain.len(), 5, "{plain:#?}");
}

// ------------------------------------------------------------------ D-486: a SKU's entries, in memory

async fn book_named(f: &Fixture, code: &str, name: &str, currency: &str) -> Uuid {
    let key = format!("book-{code}");
    let (s, b, _) = f
        .call(
            "POST",
            "/price-books",
            json!({"code": code, "name": name, "currency": currency}),
            None,
            Some(&key),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    id_of(&b["id"])
}

fn item_ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect()
}

fn item_names(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["book_name"].as_str().unwrap().to_owned())
        .collect()
}

async fn sku_query(f: &Fixture, sku: Uuid, query: &str) -> Value {
    let path = if query.is_empty() {
        format!("/price-book-entries?sku_id={sku}")
    } else {
        format!("/price-book-entries?sku_id={sku}&{query}")
    };
    let (s, b, _) = get(f, &path).await;
    assert_eq!(s, 200, "{path}: {b}");
    b
}

fn id_set(ids: &[String]) -> std::collections::BTreeSet<String> {
    ids.iter().cloned().collect()
}

fn find_item(body: &Value, id: Uuid) -> &Value {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id.to_string())
        .unwrap()
}

/// The statements one read makes, and how many items it answered.
async fn recorded(
    f: &Fixture,
    recorder: &toolkit_db::test_support::QueryRecorder,
    path: &str,
) -> (usize, Vec<(String, usize)>) {
    recorder.clear();
    let (s, b, _) = get(f, path).await;
    assert_eq!(s, 200, "{path}: {b}");
    let n = b["items"].as_array().unwrap().len();
    let sql = recorder
        .events()
        .into_iter()
        .filter(|q| {
            q.table
                .as_deref()
                .is_some_and(|t| t.starts_with("pricing_"))
        })
        .map(|q| (q.sql, q.param_count))
        .collect();
    (n, sql)
}

#[tokio::test]
async fn sku_entries_query_refuses_what_it_does_not_take() {
    let (f, _) = setup().await;
    let sku = Uuid::new_v4();
    let books: String = (0..51)
        .map(|_| Uuid::new_v4().to_string())
        .collect::<Vec<_>>()
        .join(",");
    let cases = [
        ("/price-book-entries".to_owned(), "QUERY_INVALID", ""),
        (
            "/price-book-entries?sku_id=".to_owned(),
            "QUERY_INVALID",
            "",
        ),
        (
            "/price-book-entries?sku_id=not-a-uuid".to_owned(),
            "QUERY_INVALID",
            "",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&sku_id={sku}"),
            "QUERY_INVALID",
            "sku_id",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&book_id="),
            "QUERY_INVALID",
            "book_id",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&book_id=not-a-uuid"),
            "QUERY_INVALID",
            "book_id",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&book_id={sku},"),
            "QUERY_INVALID",
            "book_id",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&book_id={books}"),
            "QUERY_INVALID",
            "book_id",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&book_id={sku}&book_id={sku}"),
            "QUERY_INVALID",
            "book_id",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&currency=usd"),
            "QUERY_INVALID",
            "currency",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&currency=US"),
            "QUERY_INVALID",
            "currency",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&currency=USDD"),
            "QUERY_INVALID",
            "currency",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&status="),
            "QUERY_INVALID",
            "status",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&status=active"),
            "QUERY_INVALID",
            "status",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&status=priced,nope"),
            "QUERY_INVALID",
            "status",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&changing=yes"),
            "QUERY_INVALID",
            "changing",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&changing=TRUE"),
            "QUERY_INVALID",
            "changing",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&limit=nope"),
            "QUERY_INVALID",
            "limit",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&limit=-1"),
            "QUERY_INVALID",
            "limit",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&as_of=2026-01-01"),
            "QUERY_INVALID",
            "as_of",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&foo=1"),
            "QUERY_INVALID",
            "foo",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&$top=10"),
            "QUERY_INVALID",
            "$top",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&q=a&q=b"),
            "QUERY_INVALID",
            "q",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&$orderby=code"),
            "INVALID_ORDERBY_FIELD",
            "field: code",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&$orderby=id"),
            "INVALID_ORDERBY_FIELD",
            "field: id",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&$orderby=book_name%20desc,id%20desc"),
            "INVALID_ORDERBY_FIELD",
            "field: id",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&$orderby=book_name,status"),
            "INVALID_ORDERBY_FIELD",
            "only one key, book_name or status, is accepted",
        ),
        (
            format!("/price-book-entries?sku_id={sku}&cursor=abc&$orderby=book_name"),
            "ORDER_WITH_CURSOR",
            "",
        ),
    ];
    for (path, code, said) in cases {
        let (s, b, _) = get(&f, &path).await;
        let text = b.to_string();
        assert_eq!(s, 400, "{path}: {b}");
        assert!(text.contains(code), "{path}: {b}");
        if !said.is_empty() {
            assert!(text.contains(said), "{path}: want {said} in {b}");
        }
    }
    // Authorization is judged first, before a query this read refuses.
    let (s, _, _) = plan_support::request(
        &f.denied,
        &f.ctx,
        "GET",
        &format!("/price-book-entries?sku_id={sku}&$filter=x"),
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(s, 403);
}

#[tokio::test]
async fn sku_entries_filter_select_and_count_are_refused_before_any_read() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog).await;
    let sku = Uuid::new_v4();
    for key in ["$filter=x", "$select=id", "$count=true"] {
        recorder.clear();
        let (s, b, _) = get(&f, &format!("/price-book-entries?sku_id={sku}&{key}")).await;
        assert_eq!(s, 400, "{key}: {b}");
        assert!(b.to_string().contains("QUERY_INVALID"), "{key}: {b}");
        let reads = recorder
            .events()
            .into_iter()
            .filter(|q| {
                q.table
                    .as_deref()
                    .is_some_and(|t| t.starts_with("pricing_"))
            })
            .count();
        assert_eq!(reads, 0, "{key} is judged before any read");
    }
}

#[tokio::test]
async fn sku_entries_status_comes_from_todays_counts() {
    let (f, catalog) = setup().await;
    let sku = catalog.sku(SkuType::Usage);
    let t = today();
    let day = time::Duration::days(1);
    let value_book = book_named(&f, "v-priced", "a-value", "EUR").await;
    let later_book = book_named(&f, "s-later", "b-later", "EUR").await;
    let live_book = book_named(&f, "p-live", "c-live", "EUR").await;
    let none_book = book_named(&f, "u-none", "d-none", "EUR").await;
    let pend_book = book_named(&f, "n-pend", "e-pend", "EUR").await;
    let rej_book = book_named(&f, "r-rej", "f-rej", "EUR").await;
    let ended_book = book_named(&f, "e-end", "g-end", "EUR").await;
    let value = entry(&f, value_book, sku, "usage", None).await;
    let later = entry(&f, later_book, sku, "usage", None).await;
    let live = entry(&f, live_book, sku, "usage", None).await;
    let none = entry(&f, none_book, sku, "usage", None).await;
    let pending = entry(&f, pend_book, sku, "usage", None).await;
    let rejected = entry(&f, rej_book, sku, "usage", None).await;
    let ended = entry(&f, ended_book, sku, "usage", None).await;
    // A value chain in force prices the entry and is not its current_price (D-434).
    price_at(&f, value, 1, "approved", Some("eu"), t - day, None, "0.25").await;
    price_at(&f, later, 1, "approved", None, t + day, None, "0.30").await;
    price_at(&f, live, 1, "approved", None, t - day, None, "0.20").await;
    price_at(&f, live, 2, "draft", None, t + day, None, "0.40").await;
    price_at(&f, pending, 1, "pending", None, t, None, "0.50").await;
    price_at(&f, rejected, 1, "rejected", None, t - day, None, "0.60").await;
    price_at(
        &f,
        ended,
        1,
        "approved",
        None,
        t - day * 30,
        Some(t - day),
        "0.10",
    )
    .await;

    let (s, body, _) = get(&f, &format!("/price-book-entries?sku_id={sku}")).await;
    assert_eq!(s, 200, "{body}");
    let row = |id: Uuid| find_item(&body, id);
    let expect = |id: Uuid, status: &str, changing: bool, money: bool| {
        let item = row(id);
        assert_eq!(item["status"], status, "{item}");
        assert_eq!(item["changing"], changing, "{item}");
        assert_eq!(!item["current_price"].is_null(), money, "{item}");
    };
    expect(value, "priced", false, false);
    expect(later, "scheduled", false, false);
    expect(live, "priced", true, true);
    expect(none, "unpriced", false, false);
    expect(pending, "unpriced", true, false);
    expect(rejected, "unpriced", false, false);
    expect(ended, "unpriced", false, false);
    assert_eq!(
        item_names(&body),
        [
            "a-value", "b-later", "c-live", "d-none", "e-pend", "f-rej", "g-end"
        ]
    );

    // Status is the usage split, not money: an entry reader sees it, and no price.
    let (s, b, _) = f
        .call_as(
            &holding(&f, "price_book_entry:read"),
            "GET",
            &format!("/price-book-entries?sku_id={sku}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let shown = find_item(&b, value);
    assert_eq!(shown["status"], "priced", "{shown}");
    assert_eq!(shown["changing"], false, "{shown}");
    assert!(shown["current_price"].is_null(), "{shown}");
}

#[tokio::test]
async fn sku_entries_narrow_by_the_plain_keys() {
    let (f, catalog) = setup().await;
    let sku = catalog.sku(SkuType::Usage);
    let t = today();
    let dollars = book_named(&f, "a-usd", "Dollars", "USD").await;
    let euros = book_named(&f, "b-eur", "Euros", "EUR").await;
    let later = book_named(&f, "c-later", "Later", "EUR").await;
    let drafty = book_named(&f, "d-draft", "Drafty", "EUR").await;
    let d_entry = entry(&f, dollars, sku, "usage", None).await;
    let e_entry = entry(&f, euros, sku, "usage", None).await;
    let l_entry = entry(&f, later, sku, "usage", None).await;
    let r_entry = entry(&f, drafty, sku, "usage", None).await;
    price_at(
        &f,
        e_entry,
        1,
        "approved",
        None,
        t - time::Duration::days(1),
        None,
        "0.20",
    )
    .await;
    price_at(
        &f,
        l_entry,
        1,
        "approved",
        None,
        t + time::Duration::days(1),
        None,
        "0.30",
    )
    .await;
    price_at(
        &f,
        r_entry,
        1,
        "approved",
        None,
        t - time::Duration::days(1),
        None,
        "0.40",
    )
    .await;
    price_at(
        &f,
        r_entry,
        2,
        "draft",
        None,
        t + time::Duration::days(2),
        None,
        "0.41",
    )
    .await;

    assert_eq!(item_ids(&sku_query(&f, sku, "").await).len(), 4);
    assert_eq!(
        item_ids(&sku_query(&f, sku, "currency=USD").await),
        vec![d_entry.to_string()]
    );
    assert_eq!(item_ids(&sku_query(&f, sku, "currency=EUR").await).len(), 3);
    assert_eq!(
        item_ids(&sku_query(&f, sku, "q=doll").await),
        vec![d_entry.to_string()]
    );
    assert_eq!(
        item_ids(&sku_query(&f, sku, "q=B-EUR").await),
        vec![e_entry.to_string()]
    );
    assert!(
        item_ids(&sku_query(&f, sku, "q=%25").await).is_empty(),
        "q is a literal"
    );
    assert_eq!(
        item_ids(&sku_query(&f, sku, "q=").await).len(),
        4,
        "an empty q does not narrow"
    );
    assert_eq!(
        item_ids(&sku_query(&f, sku, &format!("book_id={euros}")).await),
        vec![e_entry.to_string()]
    );
    let both = item_ids(&sku_query(&f, sku, &format!("book_id={dollars},{euros}")).await);
    assert_eq!(
        id_set(&both),
        id_set(&[d_entry.to_string(), e_entry.to_string()])
    );
    assert_eq!(
        id_set(&both),
        id_set(&item_ids(
            &sku_query(&f, sku, &format!("book_id={euros},{dollars}")).await
        ))
    );
    assert_eq!(
        id_set(&item_ids(&sku_query(&f, sku, "status=priced").await)),
        id_set(&[e_entry.to_string(), r_entry.to_string()])
    );
    assert_eq!(
        id_set(&item_ids(
            &sku_query(&f, sku, "status=scheduled,unpriced").await
        )),
        id_set(&[l_entry.to_string(), d_entry.to_string()])
    );
    assert_eq!(
        item_ids(&sku_query(&f, sku, "changing=true").await),
        vec![r_entry.to_string()]
    );
    assert_eq!(
        item_ids(&sku_query(&f, sku, "changing=false").await).len(),
        3
    );
    let fifty: String = (0..50)
        .map(|_| Uuid::new_v4().to_string())
        .collect::<Vec<_>>()
        .join(",");
    assert!(item_ids(&sku_query(&f, sku, &format!("book_id={fifty}")).await).is_empty());
    let repeated = vec![euros.to_string(); 51].join(",");
    assert_eq!(
        item_ids(&sku_query(&f, sku, &format!("book_id={repeated}")).await),
        vec![e_entry.to_string()],
        "distinct ids, so fifty-one copies of one id are one book"
    );
}

#[tokio::test]
async fn sku_entries_order_by_book_name_and_status_both_ways() {
    let (f, catalog) = setup().await;
    let sku = catalog.sku(SkuType::Usage);
    let t = today();
    // Code order is the reverse of name order: the default is book_name, not book code.
    let z_book = book_named(&f, "a-code", "z-name", "EUR").await;
    let m_book = book_named(&f, "m-code", "m-name", "EUR").await;
    let n_book = book_named(&f, "z-code", "m-name", "EUR").await;
    let z = entry(&f, z_book, sku, "usage", None).await;
    let priced = entry(&f, m_book, sku, "usage", None).await;
    let other = entry(&f, n_book, sku, "usage", None).await;
    price_at(
        &f,
        priced,
        1,
        "approved",
        None,
        t - time::Duration::days(1),
        None,
        "0.20",
    )
    .await;
    let by_name = vec![priced.to_string(), other.to_string(), z.to_string()];
    assert_eq!(
        item_ids(&sku_query(&f, sku, "").await),
        by_name,
        "book_name asc, id the same way"
    );
    assert_eq!(
        item_ids(&sku_query(&f, sku, "$orderby=book_name").await),
        by_name
    );
    assert_eq!(
        item_ids(&sku_query(&f, sku, "$orderby=book_name%20asc").await),
        by_name
    );
    assert_eq!(
        item_ids(&sku_query(&f, sku, "$orderby=book_name%20desc").await),
        vec![z.to_string(), other.to_string(), priced.to_string()]
    );
    assert_eq!(
        item_ids(&sku_query(&f, sku, "$orderby=status").await),
        vec![priced.to_string(), z.to_string(), other.to_string()]
    );
    assert_eq!(
        item_ids(&sku_query(&f, sku, "$orderby=status%20desc").await),
        vec![other.to_string(), z.to_string(), priced.to_string()]
    );
}

#[tokio::test]
async fn sku_entries_page_on_the_cursor_and_clamp_the_limit() {
    let (f, catalog) = setup().await;
    let sku = catalog.sku(SkuType::Usage);
    let mut expected = Vec::new();
    for name in ["a", "b", "c", "d"] {
        let own = book_named(&f, name, name, "EUR").await;
        expected.push(entry(&f, own, sku, "usage", None).await.to_string());
    }
    let (s, whole, _) = get(&f, &format!("/price-book-entries?sku_id={sku}")).await;
    assert_eq!(s, 200, "{whole}");
    assert_eq!(item_ids(&whole), expected);
    assert_eq!(whole["page_info"]["limit"], 500, "{whole}");
    assert!(whole["page_info"]["next_cursor"].is_null(), "{whole}");
    let (s, clamped, _) = get(&f, &format!("/price-book-entries?sku_id={sku}&limit=1000")).await;
    assert_eq!(s, 200, "{clamped}");
    assert_eq!(clamped["page_info"]["limit"], 500, "{clamped}");
    let (s, one, _) = get(&f, &format!("/price-book-entries?sku_id={sku}&limit=0")).await;
    assert_eq!(s, 200, "{one}");
    assert_eq!(one["page_info"]["limit"], 1, "{one}");
    assert_eq!(item_ids(&one), vec![expected[0].clone()]);

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..6 {
        let path = match &cursor {
            None => format!("/price-book-entries?sku_id={sku}&limit=2"),
            Some(token) => format!("/price-book-entries?sku_id={sku}&limit=2&cursor={token}"),
        };
        let (s, page, _) = get(&f, &path).await;
        assert_eq!(s, 200, "{path}: {page}");
        assert_eq!(page["page_info"]["limit"], 2, "{page}");
        let ids = item_ids(&page);
        assert!(!ids.is_empty() && ids.len() <= 2, "{page}");
        seen.extend(ids);
        if let Some(token) = page["page_info"]["next_cursor"].as_str() {
            cursor = Some(token.to_owned());
        } else {
            cursor = None;
            break;
        }
    }
    assert!(cursor.is_none(), "the cursor does not advance");
    assert_eq!(seen, expected);

    let first = get(&f, &format!("/price-book-entries?sku_id={sku}&limit=2")).await;
    let token = first.1["page_info"]["next_cursor"].as_str().unwrap();
    let (s, b, _) = get(
        &f,
        &format!("/price-book-entries?sku_id={sku}&limit=2&cursor={token}&$orderby=status"),
    )
    .await;
    assert_eq!(s, 400, "{b}");
    assert!(b.to_string().contains("ORDER_WITH_CURSOR"), "{b}");
    let (s, b, _) = get(&f, &format!("/price-book-entries?sku_id={sku}&cursor=abc")).await;
    assert_eq!(s, 400, "{b}");
    assert!(b.to_string().contains("INVALID_CURSOR"), "{b}");
    let other = Uuid::new_v4();
    let (s, b, _) = get(
        &f,
        &format!("/price-book-entries?sku_id={other}&cursor={token}"),
    )
    .await;
    assert_eq!(s, 400, "{b}");
    assert!(b.to_string().contains("FILTER_MISMATCH"), "{b}");
    let (s, b, _) = get(
        &f,
        &format!("/price-book-entries?sku_id={sku}&status=priced&cursor={token}"),
    )
    .await;
    assert_eq!(s, 400, "{b}");
    assert!(b.to_string().contains("FILTER_MISMATCH"), "{b}");

    // The hash is the narrowing, so the two spellings of one book list continue each other.
    let left = book_named(&f, "p-left", "p-left", "EUR").await;
    let right = book_named(&f, "q-right", "q-right", "EUR").await;
    let left_entry = entry(&f, left, sku, "usage", None).await;
    let right_entry = entry(&f, right, sku, "usage", None).await;
    let narrowed = format!("book_id={left},{right}");
    let (s, page, _) = get(
        &f,
        &format!("/price-book-entries?sku_id={sku}&{narrowed}&limit=1"),
    )
    .await;
    assert_eq!(s, 200, "{page}");
    assert_eq!(item_ids(&page), vec![left_entry.to_string()]);
    let token = page["page_info"]["next_cursor"].as_str().unwrap();
    let (s, rest, _) = get(
        &f,
        &format!("/price-book-entries?sku_id={sku}&book_id={right},{left}&limit=1&cursor={token}"),
    )
    .await;
    assert_eq!(s, 200, "{rest}");
    assert_eq!(item_ids(&rest), vec![right_entry.to_string()]);
}

#[tokio::test]
async fn sku_entries_of_five_and_fifty_books_read_the_same_statements() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog.clone()).await;
    let small = seeded_sku(&f, &catalog, "five", 5).await;
    let large = seeded_sku(&f, &catalog, "fifty", 50).await;
    let five = recorded(
        &f,
        &recorder,
        &format!("/price-book-entries?sku_id={small}"),
    )
    .await;
    let fifty = recorded(
        &f,
        &recorder,
        &format!("/price-book-entries?sku_id={large}"),
    )
    .await;
    let priced = recorded(
        &f,
        &recorder,
        &format!("/price-book-entries?sku_id={large}&status=priced"),
    )
    .await;
    let unpriced = recorded(
        &f,
        &recorder,
        &format!("/price-book-entries?sku_id={large}&status=unpriced"),
    )
    .await;
    assert_eq!((five.0, fifty.0, priced.0, unpriced.0), (5, 50, 50, 0));
    assert_eq!(
        five.1.len(),
        7,
        "a SKU's entries stay at seven statements: {five:#?}"
    );
    let sql = |rows: &Vec<(String, usize)>| rows.iter().map(|(q, _)| q.clone()).collect::<Vec<_>>();
    assert_eq!(
        sql(&five.1),
        sql(&fifty.1),
        "5 and 50 books, the same statements"
    );
    assert_eq!(sql(&fifty.1), sql(&priced.1));
    assert_eq!(
        sql(&fifty.1),
        sql(&unpriced.1),
        "a narrowing that keeps nothing still reads the SKU's entries"
    );
}

fn filter_query(expr: &str) -> String {
    format!("/price-book-entries?$filter={}", expr.replace(' ', "%20"))
}

/// D-517: `$filter=id in (…)` lists those entries instead of `sku_id`, at most 200 ids. Another
/// field, `or`, more than 200 ids, and `sku_id` beside the filter are 400. Money and the tenant
/// stay as they are.
#[tokio::test]
async fn price_book_entries_can_be_read_by_id() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "b-eur").await;
    let usd = usd_book(&f).await;
    let sku = catalog.sku(SkuType::Usage);
    let other = catalog.sku(SkuType::Usage);
    let left = entry(&f, eur, sku, "usage", None).await;
    let right = entry(&f, usd, other, "usage", None).await;
    let hidden = entry(&f, eur, other, "usage", None).await;
    let (s, page, _) = get(&f, &filter_query(&format!("id in ({left},{right})"))).await;
    assert_eq!(s, 200, "{page}");
    assert_eq!(
        item_ids(&page),
        {
            let mut ids = vec![left.to_string(), right.to_string()];
            ids.sort();
            ids
        },
        "the named entries, in id order: {page}"
    );
    assert!(!item_ids(&page).contains(&hidden.to_string()));
    let (s, foreign, _) = f
        .call_as(
            &stranger(),
            "GET",
            &filter_query(&format!("id in ({left})")),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{foreign}");
    assert_eq!(foreign["items"], json!([]), "another tenant sees nothing");
    let (s, bare, _) = f
        .call_as(
            &holding(&f, "price_book_entry:read"),
            "GET",
            &filter_query(&format!("id in ({left})")),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{bare}");
    assert!(bare["items"][0]["current_price"].is_null(), "{bare}");
    let missing = Uuid::new_v4();
    let (s, partial, _) = get(&f, &filter_query(&format!("id in ({left},{missing})"))).await;
    assert_eq!(s, 200, "{partial}");
    assert_eq!(item_ids(&partial), vec![left.to_string()]);
    let (s, one, _) = get(&f, &filter_query(&format!("id eq {left}"))).await;
    assert_eq!(s, 200, "{one}");
    assert_eq!(item_ids(&one), vec![left.to_string()]);
    let too_many = (0..201)
        .map(|_| Uuid::new_v4().to_string())
        .collect::<Vec<_>>()
        .join(",");
    // Each refusal by its own description (review RF-P item 9): the problem's `type` URI already
    // holds "or" and "in", so a substring of the whole body told the refusals apart from nothing.
    let unparsed = "the filter is `id in (...)`, at most 200 ids: ";
    for (expr, said, whole) in [
        ("code eq 'EUR'", unparsed, false),
        (
            &format!("id in ({left}) or id in ({right})"),
            "`or` is not accepted; the filter is `id in (...)`",
            true,
        ),
        (
            &format!("id in ({too_many})"),
            "`id in (...)` lists at most 200 ids",
            true,
        ),
        (
            "id ne 00000000-0000-0000-0000-000000000001",
            "the filter is `id in (...)`, at most 200 ids",
            true,
        ),
    ] {
        let (s, body, _) = get(&f, &filter_query(expr)).await;
        assert_eq!(s, 400, "{expr}: {body}");
        let violation = &body["context"]["field_violations"][0];
        assert_eq!(violation["reason"], "QUERY_INVALID", "{expr}: {body}");
        let description = violation["description"].as_str().unwrap();
        if whole {
            assert_eq!(description, said, "{expr}: {body}");
        } else {
            assert!(
                description.starts_with(said) && description.len() > said.len(),
                "{expr}: the parser's own cause follows {said:?}: {body}"
            );
        }
    }
    let (s, both, _) = get(
        &f,
        &format!(
            "/price-book-entries?sku_id={sku}&{}",
            filter_query(&format!("id in ({left})")).trim_start_matches("/price-book-entries?")
        ),
    )
    .await;
    assert_eq!(s, 400, "{both}");
    assert!(both.to_string().contains("QUERY_INVALID"), "{both}");
}

/// D-517 (review RF-P item 1): the served contract of `GET /price-book-entries` declares `$filter`
/// as a plain parameter whose description names the two shapes the read accepts. It publishes no
/// `x-odata-filter`: that table would offer `id ne`, which the read refuses.
#[tokio::test]
async fn the_served_id_filter_names_only_what_the_read_accepts() {
    let (f, _) = setup().await;
    let openapi = toolkit::api::OpenApiRegistryImpl::new();
    let _router = bss_pricing::api::rest::authoring::router(f.state, &openapi);
    let api = serde_json::to_value(
        openapi
            .build_openapi(&toolkit::api::OpenApiInfo::default())
            .unwrap(),
    )
    .unwrap();
    let op = &api["paths"]["/bss-pricing/v1/price-book-entries"]["get"];
    assert!(op["x-odata-filter"].is_null(), "{op}");
    let filter = op["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["in"] == "query" && p["name"] == "$filter")
        .unwrap_or_else(|| panic!("$filter is declared: {op}"));
    let text = filter["description"].as_str().unwrap();
    for said in ["`id eq <id>`", "`id in (<id>, ...)`", "200", "8192 bytes"] {
        assert!(text.contains(said), "the filter says {said}: {text}");
    }
}

/// D-517 (review RF-P item 1): a raw `$filter` longer than the toolkit's `MAX_FILTER_LEN` is 400
/// `QUERY_INVALID` before it is parsed. The filter one byte over is a well-formed `id eq`, which
/// the read would otherwise answer; the filter at the limit itself is still read.
#[tokio::test]
async fn an_id_filter_past_the_length_cap_is_refused_before_it_is_parsed() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "b-eur").await;
    let left = entry(&f, eur, catalog.sku(SkuType::Usage), "usage", None).await;
    let padded = |len: usize| {
        let tail = format!("eq {left}");
        format!("id{}{tail}", " ".repeat(len - 2 - tail.len()))
    };
    let limit = toolkit::api::odata::MAX_FILTER_LEN;
    let at = padded(limit);
    assert_eq!(at.len(), limit);
    let (s, page, _) = get(&f, &filter_query(&at)).await;
    assert_eq!(s, 200, "a filter at the limit is read: {page}");
    assert_eq!(item_ids(&page), vec![left.to_string()]);
    let over = padded(limit + 1);
    assert_eq!(over.len(), limit + 1);
    let (s, body, _) = get(&f, &filter_query(&over)).await;
    assert_eq!(s, 400, "{body}");
    let violation = &body["context"]["field_violations"][0];
    assert_eq!(violation["reason"], "QUERY_INVALID", "{body}");
    assert_eq!(violation["field"], "$filter", "{body}");
    assert_eq!(
        violation["description"],
        format!("`$filter` is at most {limit} bytes"),
        "{body}"
    );
}
