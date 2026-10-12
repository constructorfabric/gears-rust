//! The Price Books screen's reads (phase 7, run 7.1): an entry's prices with their date-derived
//! status and the entry reads' price in force and dated counts (D-440); every book read's stats
//! (D-441); the book list on the toolkit's `OData` pager, with `q` and `sku_id` (D-442) — each
//! list in a fixed number of statements, whatever the number of rows.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod book_support;
mod plan_support;
use book_support::{
    Row, bare_revision, code_of, codes, days, door_book, encode, get, ids, instant, ok,
    stored_book, stored_entry, stored_price, today, unit_on,
};
use bss_approval::UnitState;
use bss_pricing::infra::storage::{
    entity::price,
    repo::{price_book_entry_repo, price_repo},
};
use bss_products_sdk::models::SkuType;
use plan_support::{
    Catalog, Fixture, entry_support, holding, id_of, item, plan, publish, request, setup, stranger,
};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

// ------------------------------------------------------------------ the policies of the money

/// A policy where every subject of `tenant` holds every grant, `price_book read` narrowed to
/// `books` when given, or unavailable altogether when `unavailable`.
struct Money {
    tenant: Uuid,
    books: Option<Vec<Uuid>>,
    unavailable: bool,
}
#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverApi for Money {
    async fn evaluate(
        &self,
        _: toolkit_security::PlatformSecurityContext,
        request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, toolkit_canonical_errors::CanonicalError>
    {
        use authz_resolver_sdk::*;
        let money = request.resource.resource_type == "gts.cf.bss.pricing.price_book.v1~"
            && request.action.name == "read";
        if money && self.unavailable {
            return Err(toolkit_canonical_errors::CanonicalError::service_unavailable().create());
        }
        let mut predicates = vec![Predicate::In(InPredicate::new(
            toolkit_security::pep_properties::OWNER_TENANT_ID,
            vec![self.tenant],
        ))];
        if money && let Some(books) = &self.books {
            predicates.push(Predicate::In(InPredicate::new(
                toolkit_security::pep_properties::RESOURCE_ID,
                books.clone(),
            )));
        }
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint { predicates }],
                deny_reason: None,
            },
        })
    }
}
fn money_app(f: &Fixture, books: Option<Vec<Uuid>>, unavailable: bool) -> axum::Router {
    entry_support::production(f.state.clone()).layer(axum::Extension(
        authz_resolver_sdk::PolicyEnforcer::new(Arc::new(Money {
            tenant: f.ctx.subject_tenant_id(),
            books,
            unavailable,
        })),
    ))
}

// ------------------------------------------------------------------ a plan's book (PS-08)

/// The number of plans of the fixture tenant, read with every grant.
async fn plan_count(f: &Fixture) -> usize {
    ok(f, "/plans").await["items"].as_array().unwrap().len()
}

/// A plan names only a book its author may read (whole-branch review PS-08, D-456): plan create,
/// clone and a revision PATCH that names a book judge `price_book` read on that book, as D-440
/// judges the money. A grant that does not admit the book is 403 `PRICE_BOOK_READ_REQUIRED`, a
/// policy that cannot judge is 503, and nothing is written; a grant that admits it lets all three
/// through, and a PATCH that names no book asks nothing of the money's policy.
#[tokio::test]
async fn a_plan_names_only_a_book_its_author_may_read() {
    let (f, catalog) = setup().await;
    let eur = plan_support::book(&f, "eur").await;
    let other = plan_support::book(&f, "other").await;
    let sku = catalog.sku(SkuType::Usage);
    let entry = plan_support::entry(&f, eur, sku, "usage", None).await;
    let (source, rev) = plan(&f, "source", eur).await;
    let source = id_of(&source["id"]);
    item(&f, rev, sku, Some(entry), "paid").await;
    publish(&f, source, rev).await;
    let (_, draft) = plan(&f, "draft", eur).await;
    let plans = plan_count(&f).await;
    let tries = |app: axum::Router, tag: &'static str| {
        let f = &f;
        async move {
            [
                request(
                    &app,
                    &f.ctx,
                    "POST",
                    "/plans",
                    json!({"code":format!("new-{tag}").to_uppercase(),"name":"New","book_id":eur}),
                    None,
                    Some(&format!("create-{tag}")),
                )
                .await,
                request(
                    &app,
                    &f.ctx,
                    "POST",
                    &format!("/plans/{source}/clone"),
                    json!({"code":format!("clone-{tag}").to_uppercase(),"name":"Clone"}),
                    None,
                    Some(&format!("clone-{tag}")),
                )
                .await,
                request(
                    &app,
                    &f.ctx,
                    "PATCH",
                    &format!("/plan-revisions/{draft}"),
                    json!({"book_id":eur}),
                    Some("\"1\""),
                    None,
                )
                .await,
            ]
        }
    };
    for (app, status, tag) in [
        (money_app(&f, Some(vec![other]), false), 403, "narrow"),
        (money_app(&f, None, true), 503, "down"),
    ] {
        for (s, b, _) in tries(app, tag).await {
            assert_eq!(s, status, "{tag}: {b}");
            if status == 403 {
                assert!(code_of(&b).contains("PRICE_BOOK_READ_REQUIRED"), "{b}");
            }
        }
        assert_eq!(plan_count(&f).await, plans, "{tag}: nothing was written");
    }
    let revision = ok(&f, &format!("/plan-revisions/{draft}")).await;
    assert_eq!(
        revision["version"], 1,
        "the PATCH wrote nothing: {revision}"
    );
    // A PATCH that names no book needs no judgement of the money: the policy may be down.
    let (s, b, _) = request(
        &money_app(&f, None, true),
        &f.ctx,
        "PATCH",
        &format!("/plan-revisions/{draft}"),
        json!({"available_from":null}),
        Some("\"1\""),
        None,
    )
    .await;
    assert_eq!(s, 200, "{b}");
    let admitted = money_app(&f, Some(vec![eur]), false);
    let [created, cloned, patched] = tries(admitted, "admitted").await;
    assert_eq!(created.0, 201, "{created:?}");
    assert_eq!(cloned.0, 201, "{cloned:?}");
    assert_eq!(patched.0, 409, "the draft moved to version 2: {patched:?}");
    assert!(
        code_of(&patched.1).contains("STALE_REVISION"),
        "{patched:?}"
    );
    assert_eq!(plan_count(&f).await, plans + 2);
    // The second review of W1a, L2: the PATCH judges the book it names, not the draft's current
    // one. Under a grant for `eur` alone, moving the `eur` draft to `other` is 403 and writes
    // nothing; naming `eur` at the draft's current version passes.
    let only_eur = money_app(&f, Some(vec![eur]), false);
    let (s, b, _) = request(
        &only_eur,
        &f.ctx,
        "PATCH",
        &format!("/plan-revisions/{draft}"),
        json!({"book_id":other}),
        Some("\"2\""),
        None,
    )
    .await;
    assert_eq!(s, 403, "{b}");
    assert!(code_of(&b).contains("PRICE_BOOK_READ_REQUIRED"), "{b}");
    let revision = ok(&f, &format!("/plan-revisions/{draft}")).await;
    assert_eq!(
        revision["version"], 2,
        "the refused move wrote nothing: {revision}"
    );
    let (s, b, _) = request(
        &only_eur,
        &f.ctx,
        "PATCH",
        &format!("/plan-revisions/{draft}"),
        json!({"book_id":eur}),
        Some("\"2\""),
        None,
    )
    .await;
    assert_eq!(s, 200, "{b}");
}

// ------------------------------------------------------------------ D-440: an entry's prices

/// An entry whose default chain holds one price of every status and whose value chains `eu` and
/// `apac` hold one price each: `(entry, the stored prices by name)`.
async fn priced_entry(f: &Fixture, catalog: &Catalog, book: Uuid) -> (Uuid, Vec<price::Model>) {
    let t = today();
    let e = stored_entry(
        f,
        book,
        catalog.sku(SkuType::Usage),
        "per_unit",
        time::OffsetDateTime::now_utc(),
    )
    .await;
    let mut prices = Vec::new();
    for row in [
        // 0: superseded, 1: active (the price in force), 2: scheduled — the approved default chain.
        Row::new(1, "approved", t - days(30)).to(t - days(10)),
        Row::new(2, "approved", t - days(10)).to(t + days(10)),
        Row::new(3, "approved", t + days(10)),
        // 3: a draft on the scheduled price's start, after it by version.
        Row::new(9, "draft", t + days(10)),
        Row::new(4, "draft", t + days(20)),
        Row::new(5, "pending", t + days(21)),
        Row::new(6, "rejected", t + days(22)),
        // 7: eu's active price; 8: apac's draft.
        Row::new(7, "approved", t - days(5)).on("eu"),
        Row::new(8, "draft", t + days(5)).on("apac"),
    ] {
        prices.push(stored_price(f, e, row).await);
    }
    (e, prices)
}

#[tokio::test]
async fn an_entrys_prices_are_every_state_with_its_status_default_chain_first() {
    let (f, catalog) = setup().await;
    let eur = plan_support::book(&f, "eur").await;
    let (entry, prices) = priced_entry(&f, &catalog, eur).await;
    let b = ok(&f, &format!("/price-book-entries/{entry}/prices")).await;
    // The default chain by start then version, then apac, then eu.
    let expected: Vec<String> = [0, 1, 2, 3, 4, 5, 6, 8, 7]
        .into_iter()
        .map(|i| prices[i].id.to_string())
        .collect();
    assert_eq!(ids(&b), expected, "{b:#}");
    let statuses: Vec<&str> = b["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        statuses,
        [
            "superseded",
            "active",
            "scheduled",
            "draft",
            "draft",
            "pending",
            "rejected",
            "draft",
            "active"
        ]
    );
    // Each item is the price as every price read answers it, with its entry's model.
    let first = &b["items"][0];
    assert_eq!(first["price_book_entry_id"], entry.to_string());
    assert_eq!(first["model"], "per_unit");
    assert_eq!(first["state"], "approved");
    assert_eq!(first["version_no"], 1);
    assert!(first["dim_value"].is_null());
    assert_eq!(b["items"][8]["dim_value"], "eu");
    // An entry with no prices lists none; another tenant's entry and an unknown one are 404.
    let bare = stored_entry(
        &f,
        eur,
        catalog.sku(SkuType::Usage),
        "per_unit",
        time::OffsetDateTime::now_utc(),
    )
    .await;
    assert_eq!(
        ok(&f, &format!("/price-book-entries/{bare}/prices")).await,
        json!({"items": []})
    );
    for (who, id) in [(stranger(), entry), (f.ctx.clone(), Uuid::new_v4())] {
        let (s, b, _) = f
            .call_as(
                &who,
                "GET",
                &format!("/price-book-entries/{id}/prices"),
                json!({}),
                None,
                None,
            )
            .await;
        assert_eq!(s, 404, "{b}");
        assert!(code_of(&b).contains("ENTRY_NOT_FOUND"), "{b}");
    }
}

#[tokio::test]
async fn the_status_filter_takes_one_or_several_values_and_refuses_the_rest() {
    let (f, catalog) = setup().await;
    let eur = plan_support::book(&f, "eur").await;
    let (entry, prices) = priced_entry(&f, &catalog, eur).await;
    let path = format!("/price-book-entries/{entry}/prices");
    let of = |i: &[usize]| -> Vec<String> { i.iter().map(|i| prices[*i].id.to_string()).collect() };
    for (status, expected) in [
        ("active", of(&[1, 7])),
        ("scheduled", of(&[2])),
        ("superseded", of(&[0])),
        ("pending", of(&[5])),
        ("rejected", of(&[6])),
        ("draft", of(&[3, 4, 8])),
        // Several values, in chain order whatever order they are named in.
        ("draft,superseded", of(&[0, 3, 4, 8])),
        ("active,active", of(&[1, 7])),
        (
            "rejected,pending,draft,scheduled,active,superseded",
            of(&[0, 1, 2, 3, 4, 5, 6, 8, 7]),
        ),
    ] {
        let b = ok(&f, &format!("{path}?status={status}")).await;
        assert_eq!(ids(&b), expected, "status={status}: {b:#}");
    }
    for query in [
        "?status=live",
        "?status=",
        "?status=active,",
        "?status=ACTIVE",
        "?status=active&status=draft",
        "?state=draft",
        "?limit=10",
    ] {
        let (s, b, _) = get(&f, &format!("{path}{query}")).await;
        assert_eq!(s, 400, "{query}: {b}");
        assert!(code_of(&b).contains("QUERY_INVALID"), "{query}: {b}");
    }
    // Authorization is judged first.
    let (s, _, _) = request(
        &f.denied,
        &f.ctx,
        "GET",
        &format!("{path}?status=live"),
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(s, 403);
}

// Probed in run 7.1: the prices list without the second grant.
#[tokio::test]
async fn an_entrys_prices_are_money_read_with_price_book_read_on_its_book() {
    let (f, catalog) = setup().await;
    let eur = plan_support::book(&f, "eur").await;
    let usd = plan_support::book(&f, "usd").await;
    let (e, _) = priced_entry(&f, &catalog, eur).await;
    let path = format!("/price-book-entries/{e}/prices");
    // Entry read alone reaches the entry — an unknown one is 404 — but not its money: 403.
    let entry_reader = holding(&f, "price_book_entry:read");
    let (s, b, _) = f
        .call_as(&entry_reader, "GET", &path, json!({}), None, None)
        .await;
    assert_eq!(s, 403, "{b}");
    let (s, _, _) = f
        .call_as(
            &entry_reader,
            "GET",
            &format!("/price-book-entries/{}/prices", Uuid::new_v4()),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 404, "the entry is judged before its money");
    // Price-book read alone does not reach entries.
    let (s, _, _) = f
        .call_as(
            &holding(&f, "price_book:read"),
            "GET",
            &path,
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 403);
    // A price_book grant scoped to other books does not show this book's money.
    let app = money_app(&f, Some(vec![usd]), false);
    let (s, b, _) = request(&app, &f.ctx, "GET", &path, json!({}), None, None).await;
    assert_eq!(s, 403, "{b}");
    let app = money_app(&f, Some(vec![eur]), false);
    let (s, b, _) = request(&app, &f.ctx, "GET", &path, json!({}), None, None).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["items"].as_array().unwrap().len(), 9);
    // A policy that cannot judge the money fails the read.
    let app = money_app(&f, None, true);
    let (s, b, _) = request(&app, &f.ctx, "GET", &path, json!({}), None, None).await;
    assert_eq!(s, 503, "{b}");
}

// ------------------------------------------------------------------ D-440: the entry reads

fn dated(
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

#[tokio::test]
async fn the_entry_reads_carry_the_price_in_force_and_the_approved_prices_by_date() {
    let (f, catalog) = setup().await;
    let eur = plan_support::book(&f, "eur").await;
    let (entry, prices) = priced_entry(&f, &catalog, eur).await;
    // An entry whose default chain has nothing in force: only a value chain's price and a
    // scheduled one.
    let quiet = stored_entry(
        &f,
        eur,
        catalog.sku(SkuType::Usage),
        "per_unit",
        time::OffsetDateTime::now_utc(),
    )
    .await;
    stored_price(
        &f,
        quiet,
        Row::new(1, "approved", today() - days(5)).on("eu"),
    )
    .await;
    // Five days off today, never one: the server reads its own day per request, so a test that
    // straddles 00:00 UTC must not move a price across it (the phase 9 review's R25).
    stored_price(&f, quiet, Row::new(2, "approved", today() + days(5))).await;
    let read = ok(&f, &format!("/price-book-entries/{entry}")).await;
    assert_eq!(read["usage"], dated((1, 2, 1), 1, 3, 0, 0), "{read:#}");
    assert_eq!(
        read["current_price"]["id"],
        prices[1].id.to_string(),
        "{read:#}"
    );
    assert_eq!(read["current_price"]["status"], "active");
    assert_eq!(read["current_price"]["model"], "per_unit");
    let listed = ok(&f, &format!("/price-books/{eur}/entries")).await;
    let item = |id: Uuid| {
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id.to_string())
            .unwrap()
            .clone()
    };
    assert_eq!(item(entry)["usage"], read["usage"]);
    assert_eq!(item(entry)["current_price"], read["current_price"]);
    assert_eq!(item(quiet)["usage"], dated((1, 1, 0), 0, 0, 0, 0));
    assert!(
        item(quiet)
            .as_object()
            .unwrap()
            .contains_key("current_price")
            && item(quiet)["current_price"].is_null(),
        "no default-chain price in force: null, never absent"
    );
    // The SKU's entries (D-434) carry the same dated counts.
    let sku = read["sku_id"].as_str().unwrap();
    let across = ok(&f, &format!("/price-book-entries?sku_id={sku}")).await;
    assert_eq!(across["items"][0]["usage"], read["usage"]);
    // Entry read alone: the entry and its counts, and no money.
    let entry_reader = holding(&f, "price_book_entry:read");
    for path in [
        format!("/price-book-entries/{entry}"),
        format!("/price-books/{eur}/entries"),
    ] {
        let (s, b, _) = f
            .call_as(&entry_reader, "GET", &path, json!({}), None, None)
            .await;
        assert_eq!(s, 200, "{path}: {b}");
        let body = if b["items"].is_array() {
            b["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["id"] == entry.to_string())
                .unwrap()
                .clone()
        } else {
            b
        };
        assert!(body["current_price"].is_null(), "{path}: {body}");
        assert_eq!(body["usage"], read["usage"], "{path}");
    }
    // A price_book grant on another book hides the money; an unavailable policy fails the read.
    let other = plan_support::book(&f, "other").await;
    for (app, status) in [
        (money_app(&f, Some(vec![other]), false), 200),
        (money_app(&f, None, true), 503),
    ] {
        for path in [
            format!("/price-book-entries/{entry}"),
            format!("/price-books/{eur}/entries"),
        ] {
            let (s, b, _) = request(&app, &f.ctx, "GET", &path, json!({}), None, None).await;
            assert_eq!(s, status, "{path}: {b}");
            if s == 200 {
                assert!(
                    !code_of(&b).contains(&prices[1].id.to_string()),
                    "{path}: {b}"
                );
            }
        }
    }
}

/// `approved` is the sum of `scheduled`, `active` and `superseded` for every entry, whatever its
/// chains hold: value chains, an explicitly closed price, a temporary pair and a gap.
// Probed in run 7.1: approved counted apart from its three dates.
#[tokio::test]
async fn approved_is_always_scheduled_plus_active_plus_superseded() {
    let (f, catalog) = setup().await;
    let eur = plan_support::book(&f, "eur").await;
    let t = today();
    let mut entries = Vec::new();
    for rows in [
        vec![
            Row::new(1, "approved", t - days(40)).to(t - days(20)),
            Row::new(2, "approved", t - days(20)).to(t),
            // Five days, never one (R25): a run across 00:00 UTC keeps every count.
            Row::new(3, "approved", t).to(t + days(5)),
            Row::new(4, "approved", t + days(5)),
        ],
        // A temporary promo in force with its scheduled return, and an ended gap before it.
        vec![
            Row::new(1, "approved", t - days(9)).to(t - days(5)),
            Row::new(2, "approved", t - days(2)).to(t + days(3)),
            Row::new(3, "approved", t + days(3)),
            Row::new(4, "approved", t - days(3))
                .on("eu")
                .to(t - days(1)),
            Row::new(5, "approved", t + days(30)).on("us"),
        ],
        vec![Row::new(1, "draft", t + days(1))],
        vec![],
    ] {
        let e = stored_entry(
            &f,
            eur,
            catalog.sku(SkuType::Usage),
            "per_unit",
            time::OffsetDateTime::now_utc(),
        )
        .await;
        for row in rows {
            stored_price(&f, e, row).await;
        }
        entries.push(e);
    }
    let listed = ok(&f, &format!("/price-books/{eur}/entries")).await;
    let mut seen = Vec::new();
    for item in listed["items"].as_array().unwrap() {
        let p = &item["usage"]["prices"];
        let split = ["scheduled", "active", "superseded"]
            .iter()
            .map(|k| p[k].as_u64().unwrap())
            .sum::<u64>();
        assert_eq!(p["approved"].as_u64().unwrap(), split, "{item}");
        seen.push((
            item["id"].as_str().unwrap().to_owned(),
            p["scheduled"].as_u64().unwrap(),
            p["active"].as_u64().unwrap(),
            p["superseded"].as_u64().unwrap(),
        ));
    }
    seen.sort();
    let mut expected = vec![
        (entries[0].to_string(), 1, 1, 2),
        (entries[1].to_string(), 2, 1, 2),
        (entries[2].to_string(), 0, 0, 0),
        (entries[3].to_string(), 0, 0, 0),
    ];
    expected.sort();
    assert_eq!(seen, expected);
    // The book's own counts add up the same way.
    let book = ok(&f, &format!("/price-books/{eur}")).await;
    let p = &book["stats"]["prices"];
    assert_eq!(
        (
            p["approved"].as_u64(),
            p["scheduled"].as_u64(),
            p["active"].as_u64(),
            p["superseded"].as_u64()
        ),
        (Some(9), Some(3), Some(2), Some(4)),
        "{book:#}"
    );
}

// ------------------------------------------------------------------ D-441: the book stats

fn stats(
    entries: u64,
    skus: u64,
    [plans, plans_superseded_only]: [u64; 2],
    prices: [u64; 7],
    pending_units: u64,
    last_change_at: &str,
) -> Value {
    let [
        draft,
        pending,
        approved,
        scheduled,
        active,
        superseded,
        rejected,
    ] = prices;
    json!({
        "entries": entries,
        "skus": skus,
        "plans": plans,
        "plans_superseded_only": plans_superseded_only,
        "prices": {
            "draft": draft,
            "pending": pending,
            "approved": approved,
            "scheduled": scheduled,
            "active": active,
            "superseded": superseded,
            "rejected": rejected,
        },
        "pending_units": pending_units,
        "last_change_at": last_change_at,
    })
}
/// The book as the list answers it (`q` = its unique code).
async fn listed_book(f: &Fixture, code: &str) -> Value {
    let b = ok(f, &format!("/price-books?q={code}")).await;
    let found: Vec<&Value> = b["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["code"] == code)
        .collect();
    assert_eq!(found.len(), 1, "{b:#}");
    found[0].clone()
}

#[tokio::test]
async fn a_books_stats_count_its_entries_skus_plans_prices_and_units() {
    let (f, catalog) = setup().await;
    let day = today();
    let at = |s: &str| instant(s);
    let book = stored_book(&f, "stats", at("2026-09-01T09:00:00Z")).await;
    let other = stored_book(&f, "other", at("2026-09-01T09:00:00Z")).await;
    // Three entries over two SKUs; another book's entry of the same SKU is not counted.
    let sku = catalog.sku(SkuType::Usage);
    let e1 = stored_entry(&f, book, sku, "per_unit", at("2026-09-02T09:00:00Z")).await;
    let e2 = stored_entry(&f, book, sku, "graduated", at("2026-09-02T09:00:00Z")).await;
    let e3 = stored_entry(
        &f,
        book,
        catalog.sku(SkuType::Usage),
        "per_unit",
        at("2026-09-02T09:00:00Z"),
    )
    .await;
    stored_entry(&f, other, sku, "per_unit", at("2026-09-20T09:00:00Z")).await;
    let old = at("2026-09-02T09:00:00Z");
    for (entry, row) in [
        (
            e1,
            Row::new(1, "approved", day - days(30)).to(day - days(10)),
        ),
        (e1, Row::new(2, "approved", day - days(10))),
        (e1, Row::new(3, "draft", day + days(3))),
        (e1, Row::new(4, "rejected", day + days(4))),
        (e2, Row::new(1, "approved", day + days(10))),
        (e2, Row::new(2, "pending", day + days(11))),
        (e2, Row::new(3, "draft", day + days(12))),
        (e3, Row::new(1, "rejected", day + days(13))),
    ] {
        stored_price(&f, entry, row.updated(old)).await;
    }
    // Plans: a (a published and a superseded revision on the book: once), b (a draft on it),
    // c (on it only through a superseded revision: not in `plans`, the one plan of
    // `plans_superseded_only`), d (on the other book).
    let (plan_a, a1) = plan(&f, "a", book).await;
    let plan_a = id_of(&plan_a["id"]);
    item(&f, a1, sku, Some(e1), "paid").await;
    publish(&f, plan_a, a1).await;
    let a2 = bare_revision(&f, plan_a, 2, book).await;
    publish(&f, plan_a, a2).await;
    plan(&f, "b", book).await;
    let (plan_c, c1) = plan(&f, "c", book).await;
    let plan_c = id_of(&plan_c["id"]);
    publish(&f, plan_c, c1).await;
    let c2 = bare_revision(&f, plan_c, 2, other).await;
    publish(&f, plan_c, c2).await;
    plan(&f, "d", other).await;
    // Units: two pending prices units, a decided one, and a plan revision unit naming the id.
    let submitted = at("2026-09-03T09:00:00Z");
    unit_on(&f, "prices", book, UnitState::Pending, submitted, None).await;
    unit_on(&f, "prices", book, UnitState::Pending, submitted, None).await;
    unit_on(
        &f,
        "prices",
        book,
        UnitState::Approved,
        submitted,
        Some(submitted),
    )
    .await;
    unit_on(
        &f,
        "plan_revision",
        book,
        UnitState::Pending,
        at("2026-09-09T09:00:00Z"),
        None,
    )
    .await;
    let expected = stats(
        3,
        2,
        [2, 1],
        [2, 1, 3, 1, 1, 1, 2],
        2,
        "2026-09-03T09:00:00Z",
    );
    assert_eq!(listed_book(&f, "stats").await["stats"], expected);
    let read = ok(&f, &format!("/price-books/{book}")).await;
    assert_eq!(read["stats"], expected, "{read:#}");
    assert_eq!(
        read["id"],
        book.to_string(),
        "the book's own fields stay flat"
    );
    assert_eq!(read["code"], "stats");
    // The other book: its own entry and plans (c's live revision, d).
    assert_eq!(
        ok(&f, &format!("/price-books/{other}")).await["stats"],
        stats(1, 1, [2, 0], [0; 7], 0, "2026-09-20T09:00:00Z")
    );
    // A book with nothing reads zeros and its own last change.
    let empty = stored_book(&f, "empty", at("2026-08-01T09:00:00.25Z")).await;
    assert_eq!(
        ok(&f, &format!("/price-books/{empty}")).await["stats"],
        stats(0, 0, [0, 0], [0; 7], 0, "2026-08-01T09:00:00.25Z")
    );
    // The write answers keep the book alone.
    let (s, created, _) = f
        .call(
            "POST",
            "/price-books",
            json!({"code":"new","name":"New","currency":"EUR"}),
            None,
            Some("book-new"),
        )
        .await;
    assert_eq!(s, 201, "{created}");
    assert!(created.get("stats").is_none(), "{created}");
    let (_, _, tag) = get(
        &f,
        &format!("/price-books/{}", created["id"].as_str().unwrap()),
    )
    .await;
    let (s, patched, _) = f
        .call(
            "PATCH",
            &format!("/price-books/{}", created["id"].as_str().unwrap()),
            json!({"name":"Renamed"}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{patched}");
    assert!(patched.get("stats").is_none(), "{patched}");
}

/// `last_change_at` is the latest of the book's, its entries' and its prices' `updated_at` and
/// its units' `submitted_at` and `decided_at` — compared as instants, within one source too, where
/// `SQLite`'s RFC 3339 text does not sort as time within one second (`…00.41868Z` after
/// `…00.418681Z`, `…00Z` after `…00.5Z`).
// Probed in run 7.1: the maximum of a source's stored text.
#[tokio::test]
async fn the_last_change_is_the_latest_instant_of_every_source() {
    let (f, catalog) = setup().await;
    let at = |s: &str| instant(s);
    let book = stored_book(&f, "dated", at("2026-09-01T09:00:00Z")).await;
    let last = || async { listed_book(&f, "dated").await["stats"]["last_change_at"].clone() };
    assert_eq!(last().await, "2026-09-01T09:00:00Z");
    let entry = stored_entry(
        &f,
        book,
        catalog.sku(SkuType::Usage),
        "per_unit",
        at("2026-09-02T08:00:00Z"),
    )
    .await;
    assert_eq!(last().await, "2026-09-02T08:00:00Z");
    // Two prices in one second: the later one's text sorts first.
    for (n, updated) in [
        (1, "2026-09-02T09:00:00.418681Z"),
        (2, "2026-09-02T09:00:00.41868Z"),
    ] {
        stored_price(
            &f,
            entry,
            Row::new(n, "draft", today() + days(i64::from(n))).updated(at(updated)),
        )
        .await;
    }
    assert_eq!(last().await, "2026-09-02T09:00:00.418681Z");
    // A unit's submission, then decisions in one second: the later one's text sorts first.
    unit_on(
        &f,
        "prices",
        book,
        UnitState::Pending,
        at("2026-09-03T09:00:00Z"),
        None,
    )
    .await;
    assert_eq!(last().await, "2026-09-03T09:00:00Z");
    for (state, decided) in [
        (UnitState::Rejected, "2026-09-04T09:00:00.5Z"),
        (UnitState::Approved, "2026-09-04T09:00:00Z"),
    ] {
        unit_on(
            &f,
            "prices",
            book,
            state,
            at("2026-09-03T08:00:00Z"),
            Some(at(decided)),
        )
        .await;
    }
    assert_eq!(last().await, "2026-09-04T09:00:00.5Z");
    // The entry again, a quarter second later: the latest across the sources.
    let mut changed = bss_pricing::infra::storage::repo::price_book_entry_repo::find(
        &f.db.conn().unwrap(),
        &plan_support::scope(&f),
        f.ctx.subject_tenant_id(),
        entry,
    )
    .await
    .unwrap()
    .unwrap();
    changed.updated_at = at("2026-09-04T09:00:00.75Z");
    bss_pricing::infra::storage::repo::price_book_entry_repo::update(
        &f.db.conn().unwrap(),
        &plan_support::scope(&f),
        changed,
    )
    .await
    .unwrap();
    assert_eq!(last().await, "2026-09-04T09:00:00.75Z");
    // Another book's rows and a plan revision unit naming the id move nothing.
    let other = stored_book(&f, "elsewhere", at("2026-09-30T09:00:00Z")).await;
    stored_entry(
        &f,
        other,
        catalog.sku(SkuType::Usage),
        "per_unit",
        at("2026-10-01T09:00:00Z"),
    )
    .await;
    unit_on(
        &f,
        "plan_revision",
        book,
        UnitState::Pending,
        at("2026-10-02T09:00:00Z"),
        None,
    )
    .await;
    assert_eq!(last().await, "2026-09-04T09:00:00.75Z");
    assert_eq!(
        ok(&f, &format!("/price-books/{book}")).await["stats"]["last_change_at"],
        "2026-09-04T09:00:00.75Z"
    );
}

// ------------------------------------------------------------------ D-442: the book list

#[tokio::test]
async fn the_book_list_filters_orders_and_pages() {
    let (f, _) = setup().await;
    door_book(&f, "c-usd", "Dollars", "USD", Some("2026-01-01"), None).await;
    door_book(
        &f,
        "a-eur",
        "Euro 2026",
        "EUR",
        Some("2026-01-01"),
        Some("2027-01-01"),
    )
    .await;
    door_book(&f, "b-eur", "Euro open", "EUR", None, None).await;
    door_book(&f, "d-gbp", "Alpha pounds", "GBP", Some("2027-01-01"), None).await;
    // The default order is the code; the page is 200 and a larger $top is clamped at 500.
    let all = ok(&f, "/price-books").await;
    assert_eq!(codes(&all), ["a-eur", "b-eur", "c-usd", "d-gbp"]);
    assert_eq!(all["page_info"]["limit"], 200, "{all}");
    assert!(all["page_info"]["next_cursor"].is_null(), "{all}");
    assert_eq!(
        ok(&f, "/price-books?$top=1000").await["page_info"]["limit"],
        500
    );
    for (query, expected) in [
        ("$filter=currency eq 'EUR'", vec!["a-eur", "b-eur"]),
        ("$filter=code eq 'c-usd'", vec!["c-usd"]),
        ("$filter=startswith(name, 'Euro')", vec!["a-eur", "b-eur"]),
        ("$filter=valid_from eq null", vec!["b-eur"]),
        ("$filter=valid_until ne null", vec!["a-eur"]),
        ("$filter=valid_from ge 2026-06-01", vec!["d-gbp"]),
        (
            "$filter=valid_from le 2026-01-01 and currency ne 'EUR'",
            vec!["c-usd"],
        ),
        ("$orderby=name", vec!["d-gbp", "c-usd", "a-eur", "b-eur"]),
        (
            "$orderby=code desc",
            vec!["d-gbp", "c-usd", "b-eur", "a-eur"],
        ),
        (
            "$orderby=name desc",
            vec!["b-eur", "a-eur", "c-usd", "d-gbp"],
        ),
    ] {
        let b = ok(&f, &format!("/price-books?{}", encode(query))).await;
        assert_eq!(codes(&b), expected, "{query}: {b:#}");
    }
    // Paging: `$top` or its alias `limit`, then the cursor from page_info.
    for top in ["$top", "limit"] {
        let first = ok(&f, &format!("/price-books?{top}=3")).await;
        assert_eq!(codes(&first), ["a-eur", "b-eur", "c-usd"], "{top}");
        let cursor = first["page_info"]["next_cursor"].as_str().unwrap();
        let second = ok(&f, &format!("/price-books?{top}=3&cursor={cursor}")).await;
        assert_eq!(codes(&second), ["d-gbp"], "{top}: {second:#}");
        assert!(second["page_info"]["next_cursor"].is_null());
    }
    // Every item is the book with its stats.
    let item = &all["items"][0];
    assert_eq!(item["currency"], "EUR");
    assert_eq!(item["valid_from"], "2026-01-01");
    assert_eq!(item["valid_until"], "2027-01-01");
    assert_eq!(item["stats"]["entries"], 0);
    // Another tenant lists nothing.
    let (s, b, _) = f
        .call_as(&stranger(), "GET", "/price-books", json!({}), None, None)
        .await;
    assert_eq!(s, 200);
    assert_eq!(b["items"], json!([]));
}

#[tokio::test]
async fn q_matches_a_code_or_a_name_whatever_its_case_and_literally() {
    let (f, _) = setup().await;
    door_book(&f, "eur-main", "Main", "EUR", None, None).await;
    door_book(&f, "usd", "Dollars for EUROPE", "USD", None, None).await;
    door_book(&f, "gbp", "Pounds", "GBP", None, None).await;
    door_book(&f, "pct", "Ten 100% off", "EUR", None, None).await;
    door_book(&f, "under_score", "Under", "EUR", None, None).await;
    for (q, expected) in [
        ("EUR", vec!["eur-main", "usd"]),
        ("eUr", vec!["eur-main", "usd"]),
        ("pounds", vec!["gbp"]),
        ("100%", vec!["pct"]),
        ("%", vec!["pct"]),
        ("_", vec!["under_score"]),
        ("r_s", vec!["under_score"]),
        ("nothing", vec![]),
    ] {
        let b = ok(&f, &format!("/price-books?q={}", encode(q))).await;
        assert_eq!(codes(&b), expected, "q={q}: {b:#}");
    }
    // An empty q is no search.
    assert_eq!(codes(&ok(&f, "/price-books?q=").await).len(), 5);
}

#[tokio::test]
async fn sku_id_keeps_the_books_that_price_the_sku() {
    let (f, catalog) = setup().await;
    let (a, b, c) = (
        plan_support::book(&f, "a").await,
        plan_support::book(&f, "b").await,
        plan_support::book(&f, "c").await,
    );
    let sku = catalog.sku(SkuType::Usage);
    let other = catalog.sku(SkuType::Usage);
    plan_support::entry(&f, a, sku, "usage", None).await;
    plan_support::entry_in(&f, a, sku, "usage", None, "graduated").await;
    plan_support::entry(&f, c, sku, "usage", None).await;
    plan_support::entry(&f, b, other, "usage", None).await;
    let listed = ok(&f, &format!("/price-books?sku_id={sku}")).await;
    assert_eq!(codes(&listed), ["a", "c"], "each book once: {listed:#}");
    assert_eq!(listed["items"][0]["stats"]["entries"], 2);
    assert_eq!(
        codes(&ok(&f, &format!("/price-books?sku_id={other}&q=b")).await),
        ["b"]
    );
    assert_eq!(
        codes(&ok(&f, &format!("/price-books?sku_id={other}&q=a")).await),
        Vec::<String>::new()
    );
    assert_eq!(
        codes(&ok(&f, &format!("/price-books?sku_id={}", Uuid::new_v4())).await),
        Vec::<String>::new()
    );
}

// Probed in run 7.1: a cursor replayed with another q or sku_id accepted.
#[tokio::test]
async fn a_cursor_replayed_under_another_narrowing_is_refused() {
    let (f, catalog) = setup().await;
    let sku = catalog.sku(SkuType::Usage);
    for code in ["x-1", "x-2", "x-3"] {
        let b = plan_support::book(&f, code).await;
        plan_support::entry(&f, b, sku, "usage", None).await;
    }
    let first = ok(&f, &format!("/price-books?q=x&sku_id={sku}&$top=1")).await;
    let cursor = first["page_info"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let next = ok(
        &f,
        &format!("/price-books?q=x&sku_id={sku}&cursor={cursor}"),
    )
    .await;
    assert_eq!(codes(&next), ["x-2", "x-3"]);
    for query in [
        format!("q=x-&sku_id={sku}&cursor={cursor}"),
        format!("sku_id={sku}&cursor={cursor}"),
        format!("q=x&sku_id={}&cursor={cursor}", Uuid::new_v4()),
        format!("q=x&cursor={cursor}"),
        format!("q=x&sku_id={sku}&$filter=currency eq 'EUR'&cursor={cursor}"),
    ] {
        let (s, b, _) = get(&f, &format!("/price-books?{}", encode(&query))).await;
        assert_eq!(s, 400, "{query}: {b}");
        assert!(code_of(&b).contains("FILTER_MISMATCH"), "{query}: {b}");
    }
}

/// D-480 (amending D-442), pinned again by D-516: `$filter` names `id`, with `eq` and `in`, on this
/// backend. The filter is not rebuilt. A malformed
/// uuid is 400. The cursor's hash covers the filter, so replaying it under another `id` is 400
/// `FILTER_MISMATCH`.
#[tokio::test]
async fn the_book_list_filters_by_id() {
    let (f, _) = setup().await;
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
    let (s, body, _) = get(
        &f,
        &format!("/price-books?{}", encode("$filter=id eq not-a-uuid")),
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
    let (s, body, _) = get(
        &f,
        &format!(
            "/price-books?{}",
            encode(&format!("$filter=id eq {dollars}&cursor={cursor}"))
        ),
    )
    .await;
    assert_eq!(s, 400, "{body}");
    assert!(code_of(&body).contains("FILTER_MISMATCH"), "{body}");
}

#[tokio::test]
async fn the_book_list_refuses_what_it_does_not_take() {
    let (f, _) = setup().await;
    for (query, code) in [
        ("book=1", "QUERY_INVALID"),
        ("q=a&q=b", "QUERY_INVALID"),
        ("sku_id=nope", "QUERY_INVALID"),
        ("sku_id=", "QUERY_INVALID"),
        ("$select=code", "UNSUPPORTED_QUERY_PARAM"),
        ("$count=true", "UNSUPPORTED_QUERY_PARAM"),
        ("$orderby=currency", "INVALID_ORDERBY_FIELD"),
        ("$orderby=valid_from", "INVALID_ORDERBY_FIELD"),
        ("$filter=version eq 1", "INVALID_FILTER"),
        ("$filter=code eq null", "INVALID_FILTER"),
        ("cursor=garbage", "INVALID_CURSOR"),
    ] {
        let (s, b, _) = get(&f, &format!("/price-books?{}", encode(query))).await;
        assert_eq!(s, 400, "{query}: {b}");
        assert!(code_of(&b).contains(code), "{query}: {b}");
    }
    // Authorization is judged first.
    let (s, _, _) = request(
        &f.denied,
        &f.ctx,
        "GET",
        "/price-books?book=1",
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(s, 403);
}

// ------------------------------------------------------------------ fixed statements

/// The statements on pricing's tables one read makes, and the items it answered.
async fn statements(
    f: &Fixture,
    recorder: &toolkit_db::test_support::QueryRecorder,
    path: &str,
    n: usize,
) -> Vec<(String, usize)> {
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
        .map(|q| (q.sql, q.param_count))
        .collect()
}
fn same(what: &str, ten: &[(String, usize)], hundred: &[(String, usize)]) {
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
async fn recorded() -> (
    Fixture,
    Arc<Catalog>,
    toolkit_db::test_support::QueryRecorder,
) {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog.clone()).await;
    (f, catalog, recorder)
}

#[tokio::test]
async fn an_entrys_prices_read_in_the_same_statements_for_10_and_100_prices() {
    let (f, catalog, recorder) = recorded().await;
    let eur = plan_support::book(&f, "eur").await;
    let mut lists = Vec::new();
    for n in [10, 100] {
        let e = stored_entry(
            &f,
            eur,
            catalog.sku(SkuType::Usage),
            "per_unit",
            time::OffsetDateTime::now_utc(),
        )
        .await;
        for i in 0..n {
            let version = i32::try_from(i).unwrap() + 1;
            let from = today() + days(i64::from(version));
            let state = ["approved", "draft", "pending"][i % 3];
            stored_price(&f, e, Row::new(version, state, from)).await;
        }
        lists.push(statements(&f, &recorder, &format!("/price-book-entries/{e}/prices"), n).await);
    }
    same("prices", &lists[0], &lists[1]);
}

/// `n` more books, each with an entry, an approved price in force, a draft and a pending unit.
/// An even book is in a plan (a draft revision with an item); an odd one is named only by a plan's
/// superseded revision, the plan having moved to `moved`: `plans` 0, `plans_superseded_only` 1.
async fn seed_books(f: &Fixture, catalog: &Catalog, tag: &str, n: usize, moved: Uuid) {
    for i in 0..n {
        let code = format!("{tag}-{i:03}");
        let b = plan_support::book(f, &code).await;
        let sku = catalog.sku(SkuType::Usage);
        let e = plan_support::entry(f, b, sku, "usage", None).await;
        stored_price(f, e, Row::new(1, "approved", today() - days(1))).await;
        stored_price(f, e, Row::new(2, "draft", today() + days(1))).await;
        let (created, revision) = plan(f, &format!("plan-{code}"), b).await;
        if i % 2 == 0 {
            item(f, revision, sku, Some(e), "paid").await;
        } else {
            let plan_id = id_of(&created["id"]);
            publish(f, plan_id, revision).await;
            let second = bare_revision(f, plan_id, 2, moved).await;
            publish(f, plan_id, second).await;
        }
        unit_on(
            f,
            "prices",
            b,
            UnitState::Pending,
            time::OffsetDateTime::now_utc(),
            None,
        )
        .await;
    }
}

// Probed in run 7.1: a per-book statement in the stats.
#[tokio::test]
async fn the_book_reads_count_their_stats_in_the_same_statements_for_10_and_100_books() {
    let (f, catalog, recorder) = recorded().await;
    // The plans that moved away land on a book the seeded books' search (`q=-`) leaves out.
    let moved = plan_support::book(&f, "moved").await;
    seed_books(&f, &catalog, "s", 10, moved).await;
    let seeded = "/price-books?q=-&$top=200";
    let ten = statements(&f, &recorder, seeded, 10).await;
    seed_books(&f, &catalog, "l", 90, moved).await;
    let hundred = statements(&f, &recorder, seeded, 100).await;
    same("books", &ten, &hundred);
    // The page, then one grouped statement per source: entries, prices, plans (the live ones and
    // those only history holds, together), units.
    assert_eq!(ten.len(), 5, "{ten:#?}");
    let listed = ok(&f, seeded).await;
    for b in listed["items"].as_array().unwrap() {
        let s = &b["stats"];
        let odd = b["code"]
            .as_str()
            .unwrap()
            .ends_with(['1', '3', '5', '7', '9']);
        assert_eq!(
            (
                s["entries"].as_u64(),
                s["skus"].as_u64(),
                s["plans"].as_u64(),
                s["plans_superseded_only"].as_u64(),
                s["prices"]["active"].as_u64(),
                s["prices"]["draft"].as_u64(),
                s["pending_units"].as_u64()
            ),
            (
                Some(1),
                Some(1),
                Some(u64::from(!odd)),
                Some(u64::from(odd)),
                Some(1),
                Some(1),
                Some(1)
            ),
            "{b}"
        );
    }
    // One book's read counts it with the same statements, a book in a plan (`l-000`) and a book
    // only history holds (`l-001`) alike.
    let mut reads = Vec::new();
    for book in &listed["items"].as_array().unwrap()[..2] {
        recorder.clear();
        let read = ok(
            &f,
            &format!("/price-books/{}", book["id"].as_str().unwrap()),
        )
        .await;
        assert_eq!(read["stats"], book["stats"], "{read:#}");
        reads.push(
            recorder
                .events()
                .into_iter()
                .filter(|q| {
                    q.table
                        .as_deref()
                        .is_some_and(|t| t.starts_with("pricing_"))
                })
                .map(|q| q.sql)
                .collect::<Vec<String>>(),
        );
    }
    assert_eq!(reads[0].len(), 5, "{:#?}", reads[0]);
    assert_eq!(reads[0], reads[1], "the same statements for either book");
    assert_eq!(
        (
            listed["items"][0]["stats"]["plans_superseded_only"].as_u64(),
            listed["items"][1]["stats"]["plans_superseded_only"].as_u64()
        ),
        (Some(0), Some(1))
    );
    // The entry reads stay set-based with their price in force (D-434's two extra reads).
    let mut lists = Vec::new();
    for n in [10, 100] {
        let b = plan_support::book(&f, &format!("entries-{n}")).await;
        for _ in 0..n {
            let e = plan_support::entry(&f, b, catalog.sku(SkuType::Usage), "usage", None).await;
            stored_price(&f, e, Row::new(1, "approved", today() - days(1))).await;
        }
        lists.push(statements(&f, &recorder, &format!("/price-books/{b}/entries"), n).await);
    }
    same("entries", &lists[0], &lists[1]);
}

// ------------------------------------------------------------------ set-based reads (whole-branch review)

/// The statements on pricing's tables the last request made, in order, with their binds.
fn pricing_statements(recorder: &toolkit_db::test_support::QueryRecorder) -> Vec<(String, usize)> {
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

/// `GET /resolve` reads the revision's entries and their prices set-based (PS-15): the same
/// statements for 10 and for 100 items, each item on its own entry with its own price.
#[tokio::test]
async fn resolve_reads_in_the_same_statements_for_10_and_100_items() {
    let (f, catalog, recorder) = recorded().await;
    let eur = plan_support::book(&f, "eur").await;
    let now = time::OffsetDateTime::now_utc();
    let mut runs = Vec::new();
    for n in [10, 100] {
        let (created, revision) = plan(&f, &format!("resolve-{n}"), eur).await;
        for _ in 0..n {
            let sku = catalog.sku(SkuType::Usage);
            let e = stored_entry(&f, eur, sku, "per_unit", now).await;
            stored_price(&f, e, Row::new(1, "approved", today() - days(1))).await;
            item(&f, revision, sku, Some(e), "paid").await;
        }
        publish(&f, id_of(&created["id"]), revision).await;
        recorder.clear();
        let b = ok(
            &f,
            &format!("/resolve?plan_revision_id={revision}&date={}", today()),
        )
        .await;
        assert_eq!(b["items"].as_array().unwrap().len(), n);
        runs.push(pricing_statements(&recorder));
    }
    same("resolve", &runs[0], &runs[1]);
}

/// A book's publish-changes listing and its export read the book's prices, and the listing the
/// plans that read its entries, set-based (PS-14, PS-16): the same statements for 10 and for 100
/// entries, each with an approved price and a draft, and each named by its own plan.
#[tokio::test]
async fn publish_changes_and_the_export_read_in_the_same_statements_for_10_and_100_entries() {
    let (f, catalog, recorder) = recorded().await;
    let now = time::OffsetDateTime::now_utc();
    let (mut listings, mut exports) = (Vec::new(), Vec::new());
    for n in [10, 100] {
        let b = plan_support::book(&f, &format!("changes-{n}")).await;
        for i in 0..n {
            let sku = catalog.sku(SkuType::Usage);
            let e = stored_entry(&f, b, sku, "per_unit", now).await;
            stored_price(&f, e, Row::new(1, "approved", today() - days(1))).await;
            stored_price(&f, e, Row::new(2, "draft", today() + days(1))).await;
            let (_, revision) = plan(&f, &format!("reader-{n}-{i}"), b).await;
            item(&f, revision, sku, Some(e), "paid").await;
        }
        recorder.clear();
        let listing = ok(&f, &format!("/price-books/{b}/publish-changes")).await;
        assert_eq!(
            listing["prices"].as_array().unwrap().len(),
            n,
            "{listing:#}"
        );
        assert_eq!(listing["impact"]["plans"].as_array().unwrap().len(), n);
        listings.push(pricing_statements(&recorder));
        recorder.clear();
        let export = ok(&f, &format!("/price-books/{b}/export")).await;
        assert_eq!(export["entries"].as_array().unwrap().len(), n);
        exports.push(pricing_statements(&recorder));
    }
    same("publish-changes", &listings[0], &listings[1]);
    same("export", &exports[0], &exports[1]);
}

/// A submit reads its prices set-based (PS-39): publishing 10 or 100 drafts of one entry makes
/// the same reads of `pricing_price`, under quorum 1 (recorded) and quorum 0 (applied at once,
/// with its event); only its per-price writes (the locks, the approvals) grow.
#[tokio::test]
async fn a_submit_reads_its_prices_in_the_same_statements_for_10_and_100_drafts() {
    let (f, catalog, recorder) = recorded().await;
    let now = time::OffsetDateTime::now_utc();
    for quorum in [1, 0] {
        let (_, _, tag) = f
            .call("GET", "/approval-policy", json!({}), None, None)
            .await;
        let (s, b, _) = f
            .call(
                "PUT",
                "/approval-policy",
                json!({ "quorum": quorum }),
                Some(&tag),
                None,
            )
            .await;
        assert_eq!(s, 200, "{b}");
        submit_reads(&f, &catalog, &recorder, now, quorum).await;
    }
}
async fn submit_reads(
    f: &Fixture,
    catalog: &Catalog,
    recorder: &toolkit_db::test_support::QueryRecorder,
    _now: time::OffsetDateTime,
    quorum: u32,
) {
    let mut runs = Vec::new();
    for n in [10_i32, 100] {
        let b = plan_support::book(f, &format!("submit-{quorum}-{n}")).await;
        let e = plan_support::policy_entry(f, b, catalog.sku(SkuType::Usage), "usage", None).await;
        for i in 1..=n {
            stored_price(f, e, Row::new(i, "draft", today() + days(i64::from(i)))).await;
        }
        recorder.clear();
        let (s, body, _) = f
            .call(
                "POST",
                &format!("/price-books/{b}/publish-changes"),
                json!({}),
                None,
                Some(&format!("submit-{quorum}-{n}")),
            )
            .await;
        assert_eq!(s, 201, "{body}");
        assert_eq!(body["applied"], quorum == 0, "{body}");
        // The phase 9 review's R44: the receipt reads the unit's items and its decisions once
        // each and builds its prices and its unit from them. Under quorum 0 the apply's event reads
        // the items once more, and the decided event the decisions.
        let selects = |table: &str| {
            recorder
                .events()
                .into_iter()
                .filter(|q| {
                    q.table.as_deref() == Some(table)
                        && q.sql
                            .trim_start()
                            .to_ascii_uppercase()
                            .starts_with("SELECT")
                })
                .count()
        };
        let once = if quorum == 0 { 2 } else { 1 };
        assert_eq!(
            (
                selects("pricing_approval_unit_item"),
                selects("pricing_approval_decision")
            ),
            (once, once),
            "quorum {quorum}: the unit's items and decisions, read once"
        );
        runs.push(
            recorder
                .events()
                .into_iter()
                .filter(|q| {
                    q.table.as_deref() == Some("pricing_price")
                        && q.sql
                            .trim_start()
                            .to_ascii_uppercase()
                            .starts_with("SELECT")
                })
                .map(|q| (q.sql, q.param_count))
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(
        runs[0].len(),
        runs[1].len(),
        "quorum {quorum}: the price reads grow with the drafts: {:#?}",
        runs[1]
    );
}

// ------------------------------------------------------------------ the unit list (PS-13, D-458)

/// A `prices` unit on `book` whose one price item names `entry`, with one current vote.
async fn priced_unit(
    f: &Fixture,
    book: Uuid,
    entry: Uuid,
    submitted: time::OffsetDateTime,
) -> Uuid {
    use bss_approval::{Decision, ItemRef, Store, Unit, Verdict};
    use bss_pricing::infra::storage::repo::{approval_repo::PricingApprovalStore, price_repo};
    let (id, tenant) = (Uuid::now_v7(), f.ctx.subject_tenant_id());
    let scope = plan_support::scope(f);
    price_repo::transaction(&f.db.db(), move |tx| {
        let scope = scope.clone();
        Box::pin(async move {
            let store = PricingApprovalStore {
                scope,
                tenant_id: tenant,
            };
            let err = |e: bss_approval::ApprovalError| {
                bss_pricing::infra::storage::RepoError::Db(e.to_string())
            };
            store
                .insert_unit(
                    tx,
                    &Unit {
                        id,
                        tenant_id: tenant,
                        kind: "prices".into(),
                        ref_type: "price_book".into(),
                        ref_id: book,
                        state: UnitState::Pending,
                        common_effective_date: None,
                        quorum_required: 2,
                        generation: 1,
                        submitted_by: Uuid::new_v4(),
                        submitted_at: submitted,
                        submit_note: None,
                        decided_at: None,
                        decided_note: None,
                        snapshot: json!({}),
                        snapshot_hash: "hash".into(),
                        version: 1,
                    },
                    &[ItemRef {
                        item_type: "price".into(),
                        item_id: Uuid::now_v7(),
                        created_by: Uuid::new_v4(),
                        before: None,
                        after: json!({ "price_book_entry_id": entry }),
                    }],
                )
                .await
                .map_err(err)?;
            store
                .insert_decision(
                    tx,
                    &Decision {
                        unit_id: id,
                        actor: Uuid::new_v4(),
                        generation: 1,
                        verdict: Verdict::Approve,
                        note: None,
                        at: submitted,
                        stale: false,
                    },
                )
                .await
                .map_err(err)
        })
    })
    .await
    .unwrap();
    id
}

/// `GET /approval-units` reads one page set-based (PS-13, D-458): its units, their items, their
/// decisions and their impact's plans in the same statements for 10 and for 100 units, each with
/// an item on its own entry, a vote, and a plan that names the entry.
#[tokio::test]
async fn the_unit_list_reads_a_page_in_the_same_statements_for_10_and_100_units() {
    let (f, catalog, recorder) = recorded().await;
    let now = time::OffsetDateTime::now_utc();
    let mut runs = Vec::new();
    for n in [10, 100] {
        let b = plan_support::book(&f, &format!("units-{n}")).await;
        for i in 0..n {
            let sku = catalog.sku(SkuType::Usage);
            let e = stored_entry(&f, b, sku, "per_unit", now).await;
            let (_, revision) = plan(&f, &format!("reads-{n}-{i}"), b).await;
            item(&f, revision, sku, Some(e), "paid").await;
            priced_unit(&f, b, e, now).await;
        }
        recorder.clear();
        let page = ok(&f, &format!("/approval-units?book_id={b}")).await;
        let items = page["items"].as_array().unwrap();
        assert_eq!(items.len(), n);
        for unit in items {
            assert_eq!(unit["decisions"].as_array().unwrap().len(), 1, "{unit}");
            assert_eq!(
                unit["impact"]["plans"].as_array().unwrap().len(),
                1,
                "{unit}"
            );
        }
        runs.push(pricing_statements(&recorder));
    }
    same("approval units", &runs[0], &runs[1]);
}

/// The unit list pages (PS-13, D-458): `limit` (default 200, clamped at 500) and the opaque
/// `cursor` of `page_info`, in submission order with the id breaking a tie; the pages together are
/// the whole list, and a cursor replayed under another filter is 400.
#[tokio::test]
async fn the_unit_list_pages_in_submission_order() {
    let (f, _catalog) = setup().await;
    let book = plan_support::book(&f, "paged").await;
    let t0 = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    let mut submitted = Vec::new();
    for minutes in [0_i64, 1, 1, 2, 3] {
        submitted.push(
            unit_on(
                &f,
                "prices",
                book,
                UnitState::Pending,
                t0 + time::Duration::minutes(minutes),
                None,
            )
            .await,
        );
    }
    // Minute 1 holds two units: the id orders them.
    submitted[1..3].sort();
    let whole = ok(&f, &format!("/approval-units?book_id={book}")).await;
    assert_eq!(
        ids(&whole),
        submitted.iter().map(Uuid::to_string).collect::<Vec<_>>()
    );
    assert_eq!(whole["page_info"]["limit"], 200, "{whole}");
    assert!(whole["page_info"]["next_cursor"].is_null(), "{whole}");
    let mut seen = Vec::new();
    let mut path = format!("/approval-units?book_id={book}&limit=2");
    let mut pages = 0;
    loop {
        let page = ok(&f, &path).await;
        pages += 1;
        assert!(page["items"].as_array().unwrap().len() <= 2, "{page}");
        seen.extend(ids(&page));
        // Bounded: a cursor that does not advance fails here instead of hanging.
        assert!(pages <= 3, "the cursor does not advance: {seen:?}");
        match page["page_info"]["next_cursor"].as_str() {
            Some(cursor) => {
                path = format!(
                    "/approval-units?book_id={book}&limit=2&cursor={}",
                    encode(cursor)
                );
            }
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen, ids(&whole), "the pages are the whole list, in order");
    let first = ok(&f, &format!("/approval-units?book_id={book}&limit=2")).await;
    let cursor = encode(first["page_info"]["next_cursor"].as_str().unwrap());
    let (s, b, _) = get(
        &f,
        &format!("/approval-units?book_id={book}&state=pending&limit=2&cursor={cursor}"),
    )
    .await;
    assert_eq!(s, 400, "a cursor of another filter: {b}");
    assert!(code_of(&b).contains("FILTER_MISMATCH"), "{b}");
    let (s, b, _) = get(&f, "/approval-units?cursor=not-a-cursor").await;
    assert_eq!(s, 400, "{b}");
    let (s, b, _) = get(&f, "/approval-units?limit=many").await;
    assert_eq!(s, 400, "{b}");
    assert!(code_of(&b).contains("QUERY_INVALID"), "{b}");
    let clamped = ok(&f, &format!("/approval-units?book_id={book}&limit=1000")).await;
    assert_eq!(clamped["page_info"]["limit"], 500, "{clamped}");
}

// ------------------------------------------------------------------ counts, the order, a light list (D-470)

/// What `GET /approval-units/counts` answers for `items`, the units the list pages through under
/// the same narrowing: every state and every kind named, 0 when none, and the total.
fn counted(items: &[Value]) -> Value {
    let n = |key: &str, value: &str| items.iter().filter(|u| u[key] == value).count();
    json!({
        "by_state": {
            "pending": n("state", "pending"),
            "approved": n("state", "approved"),
            "rejected": n("state", "rejected"),
            "withdrawn": n("state", "withdrawn"),
        },
        "by_kind": {
            "prices": n("kind", "prices"),
            "plan_revision": n("kind", "plan_revision"),
        },
        "total": items.len(),
    })
}
/// A whole second, so a stored instant and one a test writes into a cursor are the same.
fn whole_second(at: time::OffsetDateTime) -> time::OffsetDateTime {
    at.replace_nanosecond(0).unwrap()
}

/// D-470 (ask 42, plan review L11): `GET /approval-units/counts` counts what the list pages
/// through under the list's whole narrowing (`state`, `kind`, and `ref_id` or `book_id`), by state
/// and by kind, every state and kind named; `total` is the list's length. A narrowing the list
/// refuses is refused the same way, and the counts take nothing but the narrowing.
#[tokio::test]
async fn the_unit_counts_count_what_the_list_pages_under_each_narrowing() {
    let (f, _catalog) = setup().await;
    let (a, b) = (
        plan_support::book(&f, "counted-a").await,
        plan_support::book(&f, "counted-b").await,
    );
    let t0 = whole_second(time::OffsetDateTime::now_utc()) - time::Duration::hours(1);
    let decided = Some(t0 + time::Duration::minutes(30));
    for (i, (kind, reference, state)) in [
        ("prices", a, UnitState::Pending),
        ("prices", a, UnitState::Pending),
        ("prices", a, UnitState::Approved),
        ("prices", a, UnitState::Rejected),
        ("prices", b, UnitState::Pending),
        ("prices", b, UnitState::Withdrawn),
        ("plan_revision", a, UnitState::Pending),
        ("plan_revision", b, UnitState::Approved),
    ]
    .into_iter()
    .enumerate()
    {
        let done = (state != UnitState::Pending).then_some(decided).flatten();
        let at = t0 + time::Duration::minutes(i64::try_from(i).unwrap());
        unit_on(&f, kind, reference, state, at, done).await;
    }
    let whole = ok(&f, "/approval-units/counts").await;
    assert_eq!(
        whole,
        json!({
            "by_state": {"pending": 4, "approved": 2, "rejected": 1, "withdrawn": 1},
            "by_kind": {"prices": 6, "plan_revision": 2},
            "total": 8,
        })
    );
    for narrowing in [
        String::new(),
        "state=pending".into(),
        "state=approved&kind=plan_revision".into(),
        "kind=prices".into(),
        format!("ref_id={a}"),
        format!("book_id={b}"),
        format!("ref_id={a}&book_id={a}"),
        format!("state=pending&kind=prices&book_id={a}"),
    ] {
        let listed = f.all_units(&narrowing).await;
        let counts = ok(&f, &format!("/approval-units/counts?{narrowing}")).await;
        assert_eq!(counts, counted(&listed), "{narrowing}");
    }
    // The list's refusals, the same code on the same field. A kind is one pricing records
    // (phase 9 review R6, R24): any other, an empty one included, is 400 QUERY_INVALID on kind.
    for narrowing in [
        "state=bogus".to_owned(),
        format!("ref_id={a}&book_id={b}"),
        "ref_id=not-a-uuid".into(),
        "book_id=7".into(),
        "kind=promotion".into(),
        "kind=".into(),
        "kind=PRICES".into(),
        format!("kind={}", "p".repeat(5000)),
    ] {
        let (ls, lb, _) = get(&f, &format!("/approval-units?{narrowing}")).await;
        let (cs, cb, _) = get(&f, &format!("/approval-units/counts?{narrowing}")).await;
        assert_eq!(ls, 400, "{narrowing}: {lb}");
        assert_eq!(cs, 400, "{narrowing}: {cb}");
        assert!(!lb["context"].is_null(), "{narrowing}: {lb}");
        assert_eq!(cb["context"], lb["context"], "{narrowing}");
        if narrowing.starts_with("kind=") {
            let violation = &lb["context"]["field_violations"][0];
            assert_eq!(
                (&violation["field"], &violation["reason"]),
                (&json!("kind"), &json!("QUERY_INVALID")),
                "{narrowing}: {lb}"
            );
        }
    }
    // Only the narrowing: no page, no order, no impact.
    for extra in [
        "limit=5",
        "cursor=abc",
        "$orderby=submitted_at%20desc",
        "impact=false",
        "q=x",
    ] {
        let (s, b, _) = get(&f, &format!("/approval-units/counts?{extra}")).await;
        assert_eq!(s, 400, "{extra}: {b}");
        assert!(code_of(&b).contains("QUERY_INVALID"), "{extra}: {b}");
        // The phase 9 review's R43: the refusal names the key the counts do not take.
        let key = extra.split('=').next().unwrap();
        let said = b["context"]["field_violations"][0]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(said.contains(key), "{extra}: {b}");
    }
}

/// D-470: the counts are ONE grouped statement whatever the number of units, read outside any
/// transaction.
#[tokio::test]
async fn the_unit_counts_read_one_grouped_statement_for_10_and_100_units() {
    let (f, _catalog, recorder) = recorded().await;
    let now = whole_second(time::OffsetDateTime::now_utc());
    let states = [
        UnitState::Pending,
        UnitState::Approved,
        UnitState::Rejected,
        UnitState::Withdrawn,
    ];
    let mut runs = Vec::new();
    for n in [10, 100] {
        let b = plan_support::book(&f, &format!("counted-{n}")).await;
        for i in 0..n {
            let state = states[i % 4];
            let kind = if i % 3 == 0 {
                "plan_revision"
            } else {
                "prices"
            };
            let done = (state != UnitState::Pending).then_some(now);
            unit_on(&f, kind, b, state, now, done).await;
        }
        recorder.clear();
        let counts = ok(&f, &format!("/approval-units/counts?book_id={b}")).await;
        assert_eq!(counts["total"], n, "{counts}");
        let statements = pricing_statements(&recorder);
        assert_eq!(statements.len(), 1, "{statements:#?}");
        assert!(
            statements[0].0.to_ascii_uppercase().contains("GROUP BY"),
            "{statements:#?}"
        );
        // The phase 9 review's R32: one statement is its own snapshot, so the counts read it on
        // the plain connection, never in the doors' serializable transaction.
        let in_tx: Vec<bool> = recorder
            .events()
            .into_iter()
            .filter(|q| {
                q.table
                    .as_deref()
                    .is_some_and(|t| t.starts_with("pricing_"))
            })
            .map(|q| q.in_tx)
            .collect();
        assert_eq!(in_tx, [false], "the counts run outside any transaction");
        runs.push(statements);
    }
    same("unit counts", &runs[0], &runs[1]);
}

/// D-470 (ask 42): `$orderby=submitted_at desc` pages the units newest first, the id breaking a
/// tie in the same direction; `submitted_at asc`, `submitted_at` alone and no `$orderby` are the
/// submission order of D-458. Every page size walks the same whole list, so a tie split by a page
/// boundary is neither lost nor repeated.
#[tokio::test]
async fn the_unit_list_pages_newest_first_with_the_id_breaking_a_tie_the_same_way() {
    let (f, _catalog) = setup().await;
    let book = plan_support::book(&f, "newest").await;
    let t0 = whole_second(time::OffsetDateTime::now_utc()) - time::Duration::hours(1);
    let mut submitted = Vec::new();
    for minutes in [0_i64, 1, 1, 1, 2, 3] {
        let at = t0 + time::Duration::minutes(minutes);
        let id = unit_on(&f, "prices", book, UnitState::Pending, at, None).await;
        submitted.push((at, id));
    }
    submitted.sort();
    let ascending: Vec<String> = submitted.iter().map(|(_, id)| id.to_string()).collect();
    let descending: Vec<String> = ascending.iter().rev().cloned().collect();
    let narrowing = format!("book_id={book}");
    for (order, expected) in [
        ("", &ascending),
        ("&$orderby=submitted_at", &ascending),
        ("&$orderby=submitted_at%20asc", &ascending),
        ("&$orderby=submitted_at%20desc", &descending),
    ] {
        let whole = ok(&f, &format!("/approval-units?{narrowing}{order}")).await;
        assert_eq!(&ids(&whole), expected, "{order}");
        for limit in 1..=5 {
            let mut seen = Vec::new();
            let mut path = format!("/approval-units?{narrowing}{order}&limit={limit}");
            let mut pages = 0;
            loop {
                let page = ok(&f, &path).await;
                seen.extend(ids(&page));
                // Bounded: a cursor that does not advance fails here instead of hanging.
                pages += 1;
                assert!(
                    seen.len() <= expected.len() && pages <= expected.len() + 1,
                    "{order} by {limit}: the cursor does not advance: {seen:?}"
                );
                match page["page_info"]["next_cursor"].as_str() {
                    // A continuation sends its cursor alone: the cursor carries the order.
                    Some(cursor) => {
                        path = format!(
                            "/approval-units?{narrowing}&limit={limit}&cursor={}",
                            encode(cursor)
                        );
                    }
                    None => break,
                }
            }
            assert_eq!(&seen, expected, "{order} by {limit}");
        }
    }
}

/// The phase 9 review's R67 (products) and its twin here: a refused `$orderby` names the key it
/// refuses, never the whole order, so `submitted_at`, which the list takes, is never called
/// unsupported.
#[tokio::test]
async fn a_refused_order_names_the_key_it_refuses() {
    let (f, _) = setup().await;
    for (order, said) in [
        ("code", "field: code"),
        ("submitted_at%20desc,id%20desc", "field: id"),
        (
            "submitted_at%20desc,submitted_at%20asc",
            "only one key, submitted_at, is accepted",
        ),
    ] {
        let (s, b, _) = get(&f, &format!("/approval-units?$orderby={order}")).await;
        assert_eq!(s, 400, "{order}: {b}");
        let text = b.to_string();
        assert!(
            text.contains("INVALID_ORDERBY_FIELD") && text.contains(said),
            "{order}: {b}"
        );
        assert!(!text.contains("submitted_at desc,"), "{order}: {b}");
    }
}

/// D-470 (plan review M4): the order is not part of the narrowing's hash, so a cursor minted
/// before the descending order existed still continues; a cursor carries its order, and a
/// continuation follows it; `$orderby` beside a cursor is the toolkit's 400 `ORDER_WITH_CURSOR`;
/// an order the list does not take is 400 `INVALID_ORDERBY_FIELD`.
#[tokio::test]
async fn a_cursor_keeps_its_order_and_one_minted_before_the_order_still_continues() {
    use toolkit_odata::{CursorV1, SortDir};
    // The narrowing hash of `kind=prices` as the list minted it before run 9.3: the first 8 bytes
    // of the SHA-256 of {"kind":"prices","ref_id":null,"state":null}, with no order in it.
    const BEFORE: &str = "a1a21e85af067d2d";
    let (f, _catalog) = setup().await;
    let book = plan_support::book(&f, "cursors").await;
    let t0 = whole_second(time::OffsetDateTime::now_utc()) - time::Duration::hours(1);
    let mut units = Vec::new();
    for minutes in 0..5_i64 {
        let at = t0 + time::Duration::minutes(minutes);
        units.push((
            at,
            unit_on(&f, "prices", book, UnitState::Pending, at, None).await,
        ));
    }
    let named = |range: &[(time::OffsetDateTime, Uuid)]| -> Vec<String> {
        range.iter().map(|(_, id)| id.to_string()).collect()
    };
    let format = time::format_description::parse_borrowed::<2>(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:9]Z",
    )
    .unwrap();
    let (at, id) = units[2];
    let before = CursorV1 {
        k: vec![at.format(&format).unwrap(), id.to_string()],
        o: SortDir::Asc,
        s: "+submitted_at,+id".into(),
        f: Some(BEFORE.into()),
        d: "fwd".into(),
    }
    .encode()
    .unwrap();
    let rest = ok(&f, &format!("/approval-units?kind=prices&cursor={before}")).await;
    assert_eq!(ids(&rest), named(&units[3..]), "a cursor minted before 9.3");
    let mut newest_first: Vec<_> = units.clone();
    newest_first.reverse();
    for (order, signed, first, second) in [
        (
            "",
            "+submitted_at,+id",
            named(&units[..2]),
            named(&units[2..4]),
        ),
        (
            "&$orderby=submitted_at%20desc",
            "-submitted_at,-id",
            named(&newest_first[..2]),
            named(&newest_first[2..4]),
        ),
    ] {
        let page = ok(&f, &format!("/approval-units?kind=prices&limit=2{order}")).await;
        assert_eq!(ids(&page), first, "{order}");
        let token = page["page_info"]["next_cursor"].as_str().unwrap();
        let cursor = CursorV1::decode(token).unwrap();
        assert_eq!(
            (cursor.f.as_deref(), cursor.s.as_str()),
            (Some(BEFORE), signed),
            "{order}: the same narrowing hash, its own order"
        );
        let next = ok(
            &f,
            &format!("/approval-units?kind=prices&limit=2&cursor={token}"),
        )
        .await;
        assert_eq!(ids(&next), second, "{order}: the cursor's order");
        for orderby in ["submitted_at%20desc", "submitted_at%20asc", "submitted_at"] {
            let (s, b, _) = get(
                &f,
                &format!("/approval-units?kind=prices&cursor={token}&$orderby={orderby}"),
            )
            .await;
            assert_eq!(s, 400, "{orderby}: {b}");
            assert!(code_of(&b).contains("ORDER_WITH_CURSOR"), "{orderby}: {b}");
        }
    }
    for bad in [
        "id%20desc",
        "submitted_at%20up",
        "submitted_at%20desc,id%20desc",
        "kind",
    ] {
        let (s, b, _) = get(&f, &format!("/approval-units?kind=prices&$orderby={bad}")).await;
        assert_eq!(s, 400, "{bad}: {b}");
        assert!(code_of(&b).contains("INVALID_ORDERBY_FIELD"), "{bad}: {b}");
    }
    // A limit of 0 reads one unit, in either order, as the house pager does (D-486 says the same
    // of a SKU's entries). The toolkit's `OData` extractor would refuse it (400 INVALID_LIMIT), one
    // reason the list keeps its own parse (the phase 9 review's theme I).
    for (order, first) in [
        ("", named(&units[..1])),
        ("&$orderby=submitted_at%20desc", named(&newest_first[..1])),
    ] {
        let page = ok(&f, &format!("/approval-units?kind=prices&limit=0{order}")).await;
        assert_eq!(ids(&page), first, "{order}");
        assert_eq!(page["page_info"]["limit"], 1, "{order}: {page}");
    }
}

/// The tables the last request's statements on pricing's tables read, in order.
fn tables_read(recorder: &toolkit_db::test_support::QueryRecorder) -> Vec<String> {
    recorder
        .events()
        .into_iter()
        .filter_map(|q| q.table.filter(|t| t.starts_with("pricing_")))
        .collect()
}

/// D-470 (plan review M3): `impact=false` skips the live impact read. Each unit answers
/// `impact: null`; the page reads its units, their items (whether its reader may approve a unit
/// judges them, D-471) and their decisions, and no plan. `impact=true` is the default. A value that
/// is not a boolean is 400 `QUERY_INVALID`.
#[tokio::test]
async fn impact_false_serves_no_impact_and_reads_no_plan() {
    let (f, catalog, recorder) = recorded().await;
    let now = time::OffsetDateTime::now_utc();
    let b = plan_support::book(&f, "light").await;
    for i in 0..3 {
        let sku = catalog.sku(SkuType::Usage);
        let e = stored_entry(&f, b, sku, "per_unit", now).await;
        let (_, revision) = plan(&f, &format!("light-{i}"), b).await;
        item(&f, revision, sku, Some(e), "paid").await;
        priced_unit(&f, b, e, now).await;
    }
    recorder.clear();
    let heavy = ok(&f, &format!("/approval-units?book_id={b}")).await;
    let heavy_tables = tables_read(&recorder);
    recorder.clear();
    let light = ok(&f, &format!("/approval-units?book_id={b}&impact=false")).await;
    let light_tables = tables_read(&recorder);
    // The phase 9 review's R46: without the impact, the items are read for their authors only
    // (the flag's separation of duties), never their content.
    let items_read: Vec<String> = recorder
        .events()
        .into_iter()
        .filter(|q| q.table.as_deref() == Some("pricing_approval_unit_item"))
        .map(|q| q.sql)
        .collect();
    assert_eq!(items_read.len(), 1, "{items_read:#?}");
    assert!(
        items_read[0].contains("created_by")
            && !items_read[0].contains("before_json")
            && !items_read[0].contains("after_json"),
        "the authors alone: {items_read:#?}"
    );
    assert_eq!(
        light_tables,
        [
            "pricing_approval_unit",
            "pricing_approval_unit_item",
            "pricing_approval_decision",
        ],
        "the units, their items and their decisions, and no plan"
    );
    assert!(
        heavy_tables.len() > light_tables.len()
            && heavy_tables.iter().any(|t| t.starts_with("pricing_plan")),
        "the default reads the plans: {heavy_tables:?}"
    );
    assert_eq!(ids(&light), ids(&heavy));
    for (l, h) in light["items"]
        .as_array()
        .unwrap()
        .iter()
        .zip(heavy["items"].as_array().unwrap())
    {
        assert_eq!(l["impact"], Value::Null, "{l}");
        assert_eq!(h["impact"]["plans"].as_array().unwrap().len(), 1, "{h}");
        let mut without = h.clone();
        without["impact"] = Value::Null;
        assert_eq!(*l, without, "only the impact differs");
    }
    assert_eq!(
        ok(&f, &format!("/approval-units?book_id={b}&impact=true")).await,
        heavy
    );
    let (s, bad, _) = get(&f, &format!("/approval-units?book_id={b}&impact=maybe")).await;
    assert_eq!(s, 400, "{bad}");
    assert!(code_of(&bad).contains("QUERY_INVALID"), "{bad}");
}

// ------------------------------------------------------------------ the next price (D-472)

/// The display status of a price that waits for its day: an approved price is scheduled, a draft
/// or a pending one shows its state.
fn waiting(state: &str) -> &str {
    if state == "approved" {
        "scheduled"
    } else {
        state
    }
}
/// A temporary approved price of `entry` from `from` until `until` and its return from `until` to
/// `base`, linked as the pair builder links them (`domain::price::temporary`): the return names
/// the temporary price as its pair and `base` as the price it returns to. The temporary half's
/// own link to its return is left out (the pair's two links are circular foreign keys, and the
/// reads never follow them). `(temporary, return)`.
async fn stored_pair(
    f: &Fixture,
    entry: Uuid,
    base: &price::Model,
    version_no: i32,
    window: (time::Date, time::Date),
) -> (price::Model, price::Model) {
    let (conn, scope) = (f.db.conn().unwrap(), plan_support::scope(f));
    let e = price_book_entry_repo::find(&conn, &scope, f.ctx.subject_tenant_id(), entry)
        .await
        .unwrap()
        .unwrap();
    let mut temporary = entry_support::price(&e);
    temporary.id = Uuid::now_v7();
    temporary.version_no = version_no;
    temporary.state = "approved".into();
    temporary.effective_from = window.0;
    temporary.effective_to = Some(window.1);
    temporary.temporary_until = Some(window.1);
    let temporary = price_repo::insert(&conn, &scope, temporary).await.unwrap();
    let mut back = entry_support::price(&e);
    back.id = Uuid::now_v7();
    back.version_no = version_no + 1;
    back.state = "approved".into();
    back.effective_from = window.1;
    back.paired_price_id = Some(temporary.id);
    back.return_of_price_id = Some(base.id);
    let back = price_repo::insert(&conn, &scope, back).await.unwrap();
    (temporary, back)
}
/// The item of `entry` in a list answer, or the answer itself when it is a single read.
fn the_entry(body: Value, entry: Uuid) -> Value {
    if body["items"].is_array() {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == entry.to_string())
            .unwrap()
            .clone()
    } else {
        body
    }
}

/// A chain's shape: its name, its rows, the index of its current price and of its next price.
type Shape = (&'static str, Vec<Row>, Option<usize>, Option<usize>);
/// A shape as stored: its name, its entry, its SKU, its current price and its next price.
type Expected = (
    String,
    Uuid,
    Uuid,
    Option<price::Model>,
    Option<price::Model>,
);

/// D-472 (ask 26): each entry read names the default chain's next price: its earliest scheduled
/// price, else its newest draft or pending price (the highest `version_no`, then the latest
/// `created_at`), else null. A value chain never counts, nor a rejected price. The book's list,
/// the single read and the SKU's entries agree, and each headline price is the very price the
/// entry's prices list answers.
// Probed in run 9.4: the scheduled price chosen by storage order; a draft beating a scheduled
// price; the newest draft by created_at; a value chain's price as the next one.
#[tokio::test]
async fn an_entry_names_its_next_price_in_each_chain_shape() {
    let (f, catalog) = setup().await;
    let eur = plan_support::book(&f, "eur").await;
    let t = today();
    let earlier = time::OffsetDateTime::now_utc() - days(1);
    let shapes: Vec<Shape> = vec![
        (
            "a scheduled price after the current one",
            vec![
                Row::new(1, "approved", t - days(10)).to(t + days(10)),
                // Stored before the earlier start: the earliest start wins, not the first row.
                Row::new(3, "approved", t + days(20)),
                Row::new(2, "approved", t + days(10)).to(t + days(20)),
                // A draft or a pending price comes only after every scheduled one.
                Row::new(9, "draft", t + days(30)),
                Row::new(8, "pending", t + days(25)),
            ],
            Some(0),
            Some(2),
        ),
        (
            "only drafts",
            vec![
                Row::new(1, "draft", t + days(5)),
                // The highest version_no wins, though written earlier and starting earlier.
                Row::new(2, "draft", t + days(3)).updated(earlier),
            ],
            None,
            Some(1),
        ),
        (
            "only pending prices",
            vec![
                Row::new(3, "pending", t + days(2)),
                Row::new(2, "pending", t + days(9)),
            ],
            None,
            Some(0),
        ),
        (
            "drafts and pending prices",
            vec![
                Row::new(4, "pending", t + days(8)),
                Row::new(5, "draft", t + days(2)),
                // A rejected price never counts, whatever its version.
                Row::new(6, "rejected", t + days(1)),
            ],
            None,
            Some(1),
        ),
        (
            "nothing after the current one",
            vec![Row::new(1, "approved", t - days(5))],
            Some(0),
            None,
        ),
        ("no price", vec![], None, None),
        (
            "only a rejected price",
            vec![Row::new(1, "rejected", t + days(1))],
            None,
            None,
        ),
        (
            "a scheduled price and nothing in force",
            vec![
                Row::new(1, "approved", t + days(3)),
                Row::new(2, "draft", t + days(4)),
            ],
            None,
            Some(0),
        ),
        (
            "value chains beside the default",
            vec![
                Row::new(1, "approved", t - days(5)),
                Row::new(2, "approved", t + days(5)).on("eu"),
                Row::new(3, "draft", t + days(6)).on("eu"),
                Row::new(4, "pending", t + days(7)).on("apac"),
            ],
            Some(0),
            None,
        ),
        (
            "a value chain's scheduled price beside the default's draft",
            vec![
                Row::new(1, "draft", t + days(3)),
                Row::new(2, "approved", t + days(2)).on("eu"),
            ],
            None,
            Some(0),
        ),
    ];
    let mut expected: Vec<Expected> = Vec::new();
    for (shape, rows, current, next) in shapes {
        let sku = catalog.sku(SkuType::Usage);
        let e = stored_entry(&f, eur, sku, "per_unit", time::OffsetDateTime::now_utc()).await;
        let mut stored = Vec::new();
        for row in rows {
            stored.push(stored_price(&f, e, row).await);
        }
        expected.push((
            shape.to_owned(),
            e,
            sku,
            current.map(|i| stored[i].clone()),
            next.map(|i| stored[i].clone()),
        ));
    }
    // A temporary pair in force: the temporary price is current and its return is next. A pair
    // still to come: the base price is current and the temporary price is next.
    for (shape, window) in [
        ("a temporary pair in force", (t - days(2), t + days(3))),
        ("a temporary pair to come", (t + days(4), t + days(8))),
    ] {
        let sku = catalog.sku(SkuType::Usage);
        let e = stored_entry(&f, eur, sku, "per_unit", time::OffsetDateTime::now_utc()).await;
        let base = stored_price(&f, e, Row::new(1, "approved", t - days(30)).to(window.0)).await;
        let (temporary, back) = stored_pair(&f, e, &base, 2, window).await;
        let (current, next) = if window.0 <= t {
            (temporary, back)
        } else {
            (base, temporary)
        };
        expected.push((shape.to_owned(), e, sku, Some(current), Some(next)));
    }
    let listed = ok(&f, &format!("/price-books/{eur}/entries")).await;
    for (shape, e, sku, current, next) in &expected {
        let item = the_entry(listed.clone(), *e);
        assert!(
            item.as_object().unwrap().contains_key("next_price"),
            "{shape}: null, never absent: {item}"
        );
        let prices = ok(&f, &format!("/price-book-entries/{e}/prices")).await;
        let answered = |p: &Option<price::Model>| {
            p.as_ref().map_or(Value::Null, |p| {
                prices["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|i| i["id"] == p.id.to_string())
                    .unwrap()
                    .clone()
            })
        };
        assert_eq!(
            item["current_price"],
            answered(current),
            "{shape}: {item:#}"
        );
        assert_eq!(item["next_price"], answered(next), "{shape}: {item:#}");
        if let Some(n) = next {
            assert_eq!(item["next_price"]["status"], waiting(&n.state), "{shape}");
        }
        let headline = |b: &Value| (b["current_price"].clone(), b["next_price"].clone());
        let read = ok(&f, &format!("/price-book-entries/{e}")).await;
        assert_eq!(headline(&read), headline(&item), "{shape}: the single read");
        let across = ok(&f, &format!("/price-book-entries?sku_id={sku}")).await;
        assert_eq!(
            headline(&the_entry(across, *e)),
            headline(&item),
            "{shape}: the SKU's entries"
        );
    }
}

/// D-472: the next price is money, shown as the price in force is (D-434): null, never absent,
/// without `price_book` read on the entry's book, on each of the three entry reads.
// Probed in run 9.4: the next price shown without the money's grant.
#[tokio::test]
async fn the_next_price_is_shown_only_with_price_book_read_on_its_book() {
    let (f, catalog) = setup().await;
    let eur = plan_support::book(&f, "eur").await;
    let other = plan_support::book(&f, "other").await;
    let t = today();
    let sku = catalog.sku(SkuType::Usage);
    let entry = stored_entry(&f, eur, sku, "per_unit", time::OffsetDateTime::now_utc()).await;
    // Five days off today, never one (the phase 9 review's R25): the server reads its own day per
    // request, so a run across 00:00 UTC must not put the next price in force.
    stored_price(&f, entry, Row::new(1, "approved", t - days(5))).await;
    let next = stored_price(&f, entry, Row::new(2, "approved", t + days(5))).await;
    let paths = [
        format!("/price-book-entries/{entry}"),
        format!("/price-books/{eur}/entries"),
        format!("/price-book-entries?sku_id={sku}"),
    ];
    for path in &paths {
        assert_eq!(
            the_entry(ok(&f, path).await, entry)["next_price"]["id"],
            next.id.to_string(),
            "{path}"
        );
    }
    let entry_reader = holding(&f, "price_book_entry:read");
    for path in &paths {
        let (status, body, _) = f
            .call_as(&entry_reader, "GET", path, json!({}), None, None)
            .await;
        assert_eq!(status, 200, "{path}: {body}");
        let item = the_entry(body, entry);
        assert!(
            item.as_object().unwrap().contains_key("next_price") && item["next_price"].is_null(),
            "{path}: {item}"
        );
    }
    for (app, shown) in [
        (money_app(&f, Some(vec![other]), false), false),
        (money_app(&f, Some(vec![eur]), false), true),
    ] {
        for path in &paths {
            let (status, body, _) = request(&app, &f.ctx, "GET", path, json!({}), None, None).await;
            assert_eq!(status, 200, "{path}: {body}");
            let item = the_entry(body, entry);
            assert_eq!(item["next_price"].is_null(), !shown, "{path}: {item}");
        }
    }
}

/// D-472 (plan review L7): the next price comes from the read the price in force comes from,
/// widened to the default chain's drafts and pending prices, so the book's entries list keeps its
/// seven statements for 10 and for 100 entries — the book, its entries, the book under the
/// money's grant, the three usage reads (the price counts, the plan items that name the entries,
/// their revisions) and the default chain — with or without `as_of` (D-473).
// Probed in run 9.4: the next price read one entry at a time.
#[tokio::test]
async fn the_entries_list_reads_both_headline_prices_in_seven_statements_for_10_and_100_entries() {
    let (f, catalog, recorder) = recorded().await;
    let t = today();
    let mut lists = Vec::new();
    for n in [10, 100] {
        let b = plan_support::book(&f, &format!("headline-{n}")).await;
        // Two entries in a plan's draft, so the usage reads the items' revisions too.
        let (_, revision) = plan(&f, &format!("headline-{n}"), b).await;
        for i in 0..n {
            let sku = catalog.sku(SkuType::Usage);
            let e = plan_support::entry(&f, b, sku, "usage", None).await;
            if i < 2 {
                item(&f, revision, sku, Some(e), "paid").await;
            }
            stored_price(&f, e, Row::new(1, "approved", t - days(1))).await;
            if i % 2 == 0 {
                stored_price(&f, e, Row::new(2, "approved", t + days(5))).await;
            }
            stored_price(&f, e, Row::new(3, "draft", t + days(6))).await;
            stored_price(&f, e, Row::new(4, "pending", t + days(7))).await;
        }
        for query in [String::new(), format!("?as_of={}", t + days(6))] {
            let path = format!("/price-books/{b}/entries{query}");
            let seen = statements(&f, &recorder, &path, n).await;
            assert_eq!(seen.len(), 7, "{path}: {seen:#?}");
            lists.push(seen);
            for item in ok(&f, &path).await["items"].as_array().unwrap() {
                assert!(
                    !item["current_price"].is_null() && !item["next_price"].is_null(),
                    "{path}: {item}"
                );
            }
        }
    }
    same("entries", &lists[0], &lists[2]);
    same("entries on a date", &lists[1], &lists[3]);
}

// ------------------------------------------------------------------ the entries list on a date (D-473)

/// D-473 (ask 37, plan review M5): `as_of` dates the whole answer of the book's entries list —
/// the price in force, the next price, each price's status and the usage split are judged on
/// that one day (D-440) — and today is the default. The single read keeps today.
// Probed in run 9.4: the next price judged on today under as_of; the usage split judged on today
// under as_of.
#[tokio::test]
async fn the_entries_list_judges_every_price_on_its_as_of_date() {
    let (f, catalog) = setup().await;
    let eur = plan_support::book(&f, "eur").await;
    let t = today();
    let entry = stored_entry(
        &f,
        eur,
        catalog.sku(SkuType::Usage),
        "per_unit",
        time::OffsetDateTime::now_utc(),
    )
    .await;
    let mut prices = Vec::new();
    for row in [
        Row::new(1, "approved", t - days(30)).to(t - days(10)),
        Row::new(2, "approved", t - days(10)).to(t + days(10)),
        Row::new(3, "approved", t + days(10)).to(t + days(20)),
        Row::new(4, "approved", t + days(20)),
        Row::new(5, "pending", t + days(25)),
        Row::new(9, "draft", t + days(30)),
        // A value chain's price: counted on its own window, never a headline.
        Row::new(6, "approved", t - days(5)).on("eu"),
    ] {
        prices.push(stored_price(&f, entry, row).await);
    }
    let id = |i: Option<usize>| i.map_or(Value::Null, |i| json!(prices[i].id.to_string()));
    // (as_of, the current price, the next price, the approved (scheduled, active, superseded))
    for (as_of, current, next, split) in [
        (t - days(40), None, Some(0), (5, 0, 0)),
        (t - days(20), Some(0), Some(1), (4, 1, 0)),
        (t, Some(1), Some(2), (2, 2, 1)),
        // The day a price ends is its successor's.
        (t + days(10), Some(2), Some(3), (1, 2, 2)),
        // After a scheduled start (M5): that price is in force, the next is the one after it.
        (t + days(15), Some(2), Some(3), (1, 2, 2)),
        // After the last scheduled start: the next is the newest draft or pending price.
        (t + days(20), Some(3), Some(5), (0, 2, 3)),
    ] {
        let body = ok(&f, &format!("/price-books/{eur}/entries?as_of={as_of}")).await;
        let item = &body["items"][0];
        assert_eq!(
            item["current_price"]["id"],
            id(current),
            "{as_of}: {item:#}"
        );
        assert_eq!(item["next_price"]["id"], id(next), "{as_of}: {item:#}");
        // The statuses agree with the day: the current price is active on it, the next waits.
        if current.is_some() {
            assert_eq!(item["current_price"]["status"], "active", "{as_of}");
        }
        if let Some(n) = next {
            assert_eq!(
                item["next_price"]["status"],
                waiting(&prices[n].state),
                "{as_of}"
            );
        }
        assert_eq!(item["usage"], dated(split, 1, 1, 0, 0), "{as_of}: {item:#}");
    }
    // Without as_of the day is today, the same answer as as_of today.
    assert_eq!(
        ok(&f, &format!("/price-books/{eur}/entries")).await,
        ok(&f, &format!("/price-books/{eur}/entries?as_of={t}")).await
    );
    // The single read takes no as_of: it stays dated on today.
    let read = ok(
        &f,
        &format!("/price-book-entries/{entry}?as_of={}", t + days(15)),
    )
    .await;
    assert_eq!(read["current_price"]["id"], id(Some(1)), "{read:#}");
    assert_eq!(read["next_price"]["id"], id(Some(2)), "{read:#}");
}

/// D-473: a date before the book's `valid_from`, or on or after its `valid_until`, still answers
/// with the prices in force then (the served text says such a price is not sellable).
#[tokio::test]
async fn a_date_outside_the_books_validity_answers_the_prices_in_force_then() {
    let (f, catalog) = setup().await;
    let t = today();
    let (from, until) = (t + days(10), t + days(40));
    let book = door_book(
        &f,
        "VALID",
        "Valid",
        "EUR",
        Some(&from.to_string()),
        Some(&until.to_string()),
    )
    .await;
    let entry = stored_entry(
        &f,
        book,
        catalog.sku(SkuType::Usage),
        "per_unit",
        time::OffsetDateTime::now_utc(),
    )
    .await;
    let first = stored_price(
        &f,
        entry,
        Row::new(1, "approved", t - days(5)).to(t + days(20)),
    )
    .await;
    let second = stored_price(&f, entry, Row::new(2, "approved", t + days(20))).await;
    let draft = stored_price(&f, entry, Row::new(3, "draft", t + days(50))).await;
    for (as_of, current, next) in [
        (t - days(1), &first, &second),
        (t + days(15), &first, &second),
        (until, &second, &draft),
        (t + days(100), &second, &draft),
    ] {
        let body = ok(&f, &format!("/price-books/{book}/entries?as_of={as_of}")).await;
        let item = &body["items"][0];
        assert_eq!(
            (&item["current_price"]["id"], &item["next_price"]["id"]),
            (&json!(current.id.to_string()), &json!(next.id.to_string())),
            "{as_of}: {item:#}"
        );
    }
}

/// D-473 (plan review L6): `as_of` is a `YYYY-MM-DD` date, else 400 `DATE_INVALID`; any other key,
/// or `as_of` twice, is 400 `QUERY_INVALID`, the house rule. D-440's order: 403 for entry read,
/// 503 for the money's policy, 400 for the query, then 404 for the book.
// Probed in run 9.4: an unknown key ignored; the book judged before the date.
#[tokio::test]
async fn the_entries_list_refuses_a_date_it_cannot_read_and_any_other_key() {
    let (f, _) = setup().await;
    let eur = plan_support::book(&f, "eur").await;
    let path = format!("/price-books/{eur}/entries");
    for bad in [
        "2026-13-01",
        "2026-02-30",
        "20260105",
        "tomorrow",
        "",
        "2026-01-05T00:00:00Z",
        "%202026-01-05",
    ] {
        let (s, b, _) = get(&f, &format!("{path}?as_of={bad}")).await;
        assert_eq!(s, 400, "{bad}: {b}");
        assert!(
            code_of(&b).contains("DATE_INVALID") && code_of(&b).contains("as_of"),
            "{bad}: {b}"
        );
    }
    // D-483: `limit`, `cursor`, `$top`, `$skiptoken` and `$filter` are the pager's keys now.
    for query in [
        "?asof=2026-01-05",
        "?page=5",
        "?as_of=2026-01-05&as_of=2026-01-06",
        "?status=active",
        "?top=5",
    ] {
        let (s, b, _) = get(&f, &format!("{path}{query}")).await;
        assert_eq!(s, 400, "{query}: {b}");
        assert!(code_of(&b).contains("QUERY_INVALID"), "{query}: {b}");
    }
    let bad = "?as_of=tomorrow";
    let (s, _, _) = request(
        &f.denied,
        &f.ctx,
        "GET",
        &format!("{path}{bad}"),
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(s, 403, "authorization first");
    let (s, b, _) = request(
        &money_app(&f, None, true),
        &f.ctx,
        "GET",
        &format!("{path}{bad}"),
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(s, 503, "the money's policy before the query: {b}");
    let unknown = format!("/price-books/{}/entries", Uuid::new_v4());
    let (s, b, _) = get(&f, &format!("{unknown}{bad}")).await;
    assert_eq!(s, 400, "the query before the book: {b}");
    let (s, b, _) = get(&f, &format!("{unknown}?as_of=2026-01-05")).await;
    assert_eq!(s, 404, "{b}");
}

/// D-473 amended (phase 9 review R1): the usage split moves with the start and the end of every
/// approved price, so an `as_of` other than today is money (D-440). Without `price_book` read on
/// the book — entry read alone, or a grant narrowed to another book — it is 403
/// `PRICE_BOOK_READ_REQUIRED`, judged after the book's 404 and before any price or usage is read;
/// no `as_of`, or today's, answers 200 as before, and a grant that admits the book reads any day.
#[tokio::test]
async fn a_dated_entries_list_takes_price_book_read_on_its_book() {
    let (f, catalog) = setup().await;
    let eur = plan_support::book(&f, "eur").await;
    let other = plan_support::book(&f, "other").await;
    let t = today();
    let entry = stored_entry(
        &f,
        eur,
        catalog.sku(SkuType::Usage),
        "per_unit",
        time::OffsetDateTime::now_utc(),
    )
    .await;
    stored_price(&f, entry, Row::new(1, "approved", t - days(1))).await;
    stored_price(&f, entry, Row::new(2, "approved", t + days(5))).await;
    let path = format!("/price-books/{eur}/entries");
    let entry_reader = holding(&f, "price_book_entry:read");
    let today_only = |s: u16, b: &Value, query: &str, dated: bool| {
        if dated {
            assert_eq!(s, 403, "{query}: {b}");
            assert!(
                code_of(b).contains("PRICE_BOOK_READ_REQUIRED"),
                "{query}: {b}"
            );
        } else {
            assert_eq!(s, 200, "{query}: {b}");
            let item = &b["items"][0];
            assert!(
                item["current_price"].is_null() && item["next_price"].is_null(),
                "{query}: {b}"
            );
        }
    };
    let queries = [
        (String::new(), false),
        (format!("?as_of={t}"), false),
        (format!("?as_of={}", t + days(5)), true),
        (format!("?as_of={}", t - days(1)), true),
        ("?as_of=2020-01-01".to_owned(), true),
    ];
    for (query, dated) in &queries {
        let (s, b, _) = f
            .call_as(
                &entry_reader,
                "GET",
                &format!("{path}{query}"),
                json!({}),
                None,
                None,
            )
            .await;
        today_only(s, &b, query, *dated);
        let narrowed = money_app(&f, Some(vec![other]), false);
        let (s, b, _) = request(
            &narrowed,
            &f.ctx,
            "GET",
            &format!("{path}{query}"),
            json!({}),
            None,
            None,
        )
        .await;
        today_only(s, &b, query, *dated);
        let admitted = money_app(&f, Some(vec![eur]), false);
        let (s, b, _) = request(
            &admitted,
            &f.ctx,
            "GET",
            &format!("{path}{query}"),
            json!({}),
            None,
            None,
        )
        .await;
        assert_eq!(s, 200, "{query}: {b}");
        let item = &b["items"][0];
        assert!(
            item["current_price"].is_object() || item["next_price"].is_object(),
            "{query}: {b}"
        );
    }
    // The book is judged before the money: an unknown book is 404 whatever the day.
    let unknown = format!(
        "/price-books/{}/entries?as_of={}",
        Uuid::new_v4(),
        t + days(5)
    );
    let (s, b, _) = f
        .call_as(&entry_reader, "GET", &unknown, json!({}), None, None)
        .await;
    assert_eq!(s, 404, "{b}");
}
