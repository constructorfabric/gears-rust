//! Usage counts (D-428, run 5.3): the two entry reads carry `usage` — the entry's prices by
//! state, the distinct plans whose draft, pending, scheduled or published revisions name it, and
//! the plans that name it only through superseded revisions — with a fixed number of statements
//! however many entries a book holds; and the SKU usage port pricing fills for Products (P-D-197)
//! adds the counts up per SKU, with plans distinct across the SKU's entries.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
use bss_pricing::api::sku_usage::PricingSkuUsage;
use bss_pricing::infra::storage::{
    entity::{plan_revision, price_book, price_book_entry},
    repo::{book_repo, plan_revision_repo, price_book_entry_repo, price_repo},
};
use bss_products_sdk::models::SkuType;
use bss_products_sdk::sku_usage::{PriceCounts, SkuUsage, SkuUsageSets, SkuUsageV1};
use plan_support::{
    Catalog, Fixture, book, entry, entry_in, entry_support, holding, id_of, item, lock, plan,
    publish, scope, setup,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use toolkit_db::secure::AccessScope;
use uuid::Uuid;

/// A price of `entry` in `state`, written straight through the repository: the doors cannot
/// write an approved, pending or rejected price without a unit, and the counts read the state
/// alone. Each price starts on its own day, so approved starts never collide.
async fn price_in(f: &Fixture, entry: Uuid, state: &str, version_no: i32) {
    let conn = f.db.conn().unwrap();
    let e = price_book_entry_repo::find(&conn, &scope(f), f.ctx.subject_tenant_id(), entry)
        .await
        .unwrap()
        .unwrap();
    let mut p = entry_support::price(&e);
    // The money in the entry's model (D-427): an entry read chooses its price in force from its
    // approved prices, read as the model's money (D-440).
    if e.model == "flat" {
        p.price_json = json!({"amount": "10.00"});
    }
    p.version_no = version_no;
    p.state = state.into();
    p.effective_from = time::Date::from_calendar_date(2031, time::Month::January, 1).unwrap()
        + time::Duration::days(i64::from(version_no));
    price_repo::insert(&conn, &scope(f), p).await.unwrap();
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
async fn read_entry(f: &Fixture, entry: Uuid) -> Value {
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
    b
}
/// The book's entries by id, as the list door answers them.
async fn list_entries(f: &Fixture, book: Uuid) -> BTreeMap<String, Value> {
    let (s, b, _) = f
        .call(
            "GET",
            &format!("/price-books/{book}/entries"),
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
        .map(|e| (e["id"].as_str().unwrap().to_owned(), e.clone()))
        .collect()
}
/// An entry's usage (D-428). Every approved price here starts in 2031 (`price_in`), so each one
/// is `scheduled` today (D-440).
fn usage(approved: u64, pending: u64, draft: u64, plans: u64, superseded_only: u64) -> Value {
    json!({
        "prices": {
            "approved": approved,
            "pending": pending,
            "draft": draft,
            "scheduled": approved,
            "active": 0,
            "superseded": 0,
        },
        "plans": plans,
        "plans_superseded_only": superseded_only,
    })
}
/// The port as the gear registers it, over the fixture's state and policy.
fn port(f: &Fixture) -> PricingSkuUsage {
    PricingSkuUsage::new(
        f.state.clone(),
        entry_support::enforcer_for(f.ctx.subject_tenant_id()),
    )
}

// ------------------------------------------------------------------ the entry reads

#[tokio::test]
async fn an_entry_counts_its_prices_by_state_and_leaves_a_rejected_price_out() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let used = entry(&f, eur, catalog.sku(SkuType::Usage), "usage", None).await;
    let quiet = entry(&f, eur, catalog.sku(SkuType::Usage), "usage", None).await;
    for (n, state) in [
        (1, "approved"),
        (2, "approved"),
        (3, "pending"),
        (4, "draft"),
        (5, "draft"),
        (6, "draft"),
        (7, "rejected"),
        (8, "rejected"),
    ] {
        price_in(&f, used, state, n).await;
    }
    let (s, b, tag) = f
        .call(
            "GET",
            &format!("/price-book-entries/{used}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["usage"], usage(2, 1, 3, 0, 0), "{b}");
    assert_eq!(
        b["id"],
        used.to_string(),
        "the entry's own fields stay flat"
    );
    assert_eq!(b["model"], "per_unit");
    assert_eq!(tag, "\"1\"", "the read keeps its ETag");
    let listed = list_entries(&f, eur).await;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[&used.to_string()]["usage"], usage(2, 1, 3, 0, 0));
    assert_eq!(
        listed[&quiet.to_string()]["usage"],
        usage(0, 0, 0, 0, 0),
        "an entry nothing uses reads zeros, never null"
    );
    assert_eq!(read_entry(&f, quiet).await["usage"], usage(0, 0, 0, 0, 0));
}

#[tokio::test]
async fn a_plan_counts_once_however_many_of_its_revisions_name_the_entry() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    let e = entry(&f, eur, sku, "usage", None).await;
    // Plan pro: revision 1 names the entry and is published; revision 2, the door's copy of it,
    // names it again (an unreserved copied item still counts).
    let (pro, r1) = plan(&f, "pro", eur).await;
    let pro = id_of(&pro["id"]);
    item(&f, r1, sku, Some(e), "paid").await;
    publish(&f, pro, r1).await;
    let copied = f
        .call(
            "POST",
            &format!("/plans/{pro}/revisions"),
            json!({}),
            None,
            Some("copy"),
        )
        .await;
    assert_eq!(copied.0, 201, "{copied:?}");
    assert_eq!(
        copied.1["items"][0]["price_book_entry_id"],
        e.to_string(),
        "{copied:?}"
    );
    assert_eq!(read_entry(&f, e).await["usage"], usage(0, 0, 0, 1, 0));
    // Plan trial: its only revision names the entry and is pending.
    let (_, t1) = plan(&f, "trial", eur).await;
    item(&f, t1, sku, Some(e), "optional").await;
    lock(&f, t1).await;
    assert_eq!(read_entry(&f, e).await["usage"], usage(0, 0, 0, 2, 0));
    assert_eq!(
        list_entries(&f, eur).await[&e.to_string()]["usage"],
        usage(0, 0, 0, 2, 0)
    );
}

#[tokio::test]
async fn a_plan_that_names_the_entry_only_through_superseded_revisions_is_counted_apart() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    let e = entry(&f, eur, sku, "usage", None).await;
    // Plan legacy: revision 1 names the entry; revision 2, without it, supersedes it.
    let (legacy, l1) = plan(&f, "legacy", eur).await;
    let legacy = id_of(&legacy["id"]);
    item(&f, l1, sku, Some(e), "paid").await;
    publish(&f, legacy, l1).await;
    let l2 = bare_revision(&f, legacy, 2, eur).await;
    publish(&f, legacy, l2).await;
    assert_eq!(read_entry(&f, e).await["usage"], usage(0, 0, 0, 0, 1));
    // That plan is history, and it still keeps the entry in use (D-408, D-414).
    let (s, b, _) = f
        .call(
            "DELETE",
            &format!("/price-book-entries/{e}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 409, "{b}");
    assert!(b.to_string().contains("ENTRY_IN_USE"), "{b}");
    // Plan pro names it through a superseded AND a published revision: counted in plans only.
    let (pro, p1) = plan(&f, "pro", eur).await;
    let pro = id_of(&pro["id"]);
    item(&f, p1, sku, Some(e), "paid").await;
    publish(&f, pro, p1).await;
    let copied = f
        .call(
            "POST",
            &format!("/plans/{pro}/revisions"),
            json!({}),
            None,
            Some("copy"),
        )
        .await;
    assert_eq!(copied.0, 201, "{copied:?}");
    publish(&f, pro, id_of(&copied.1["id"])).await;
    assert_eq!(read_entry(&f, e).await["usage"], usage(0, 0, 0, 1, 1));
}

/// A book of `n` entries of their own SKUs, each with a draft and an approved price and named
/// by the paid item of one draft revision.
async fn seeded_book(f: &Fixture, catalog: &Catalog, code: &str, n: usize) -> Uuid {
    let b = book(f, code).await;
    let (_, revision) = plan(f, &format!("plan-{code}"), b).await;
    for _ in 0..n {
        let sku = catalog.sku(SkuType::Usage);
        let e = entry(f, b, sku, "usage", None).await;
        price_in(f, e, "draft", 1).await;
        price_in(f, e, "approved", 2).await;
        item(f, revision, sku, Some(e), "paid").await;
    }
    b
}
/// The statements on pricing's tables that one read of the book's entries makes.
async fn statements(
    f: &Fixture,
    recorder: &toolkit_db::test_support::QueryRecorder,
    book: Uuid,
    n: usize,
) -> Vec<String> {
    recorder.clear();
    let listed = list_entries(f, book).await;
    let seen: Vec<String> = recorder
        .events()
        .into_iter()
        .filter(|q| {
            q.table
                .as_deref()
                .is_some_and(|t| t.starts_with("pricing_"))
        })
        .map(|q| q.sql)
        .collect();
    assert_eq!(listed.len(), n);
    for e in listed.values() {
        assert_eq!(e["usage"], usage(1, 0, 1, 1, 0), "{e}");
    }
    seen
}

#[tokio::test]
async fn the_entry_list_reads_usage_in_the_same_number_of_statements_for_10_and_100_entries() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog.clone()).await;
    let small = seeded_book(&f, &catalog, "small", 10).await;
    let large = seeded_book(&f, &catalog, "large", 100).await;
    let ten = statements(&f, &recorder, small, 10).await;
    let hundred = statements(&f, &recorder, large, 100).await;
    for (i, sql) in hundred.iter().enumerate() {
        eprintln!("statement {i}: {sql}");
    }
    assert_eq!(
        ten.len(),
        hundred.len(),
        "the list's statements grow with its entries: {ten:#?} vs {hundred:#?}"
    );
    assert_eq!(ten, hundred, "the same statements, whatever the size");
}

// ------------------------------------------------------------------ the SKU usage port

#[tokio::test]
async fn the_sku_usage_counts_plans_distinct_across_the_skus_entries() {
    let (f, catalog) = setup().await;
    let tenant = f.ctx.subject_tenant_id();
    let eur = book(&f, "eur").await;
    let (s, usd, _) = f
        .call(
            "POST",
            "/price-books",
            json!({"code":"usd","name":"usd","currency":"USD"}),
            None,
            Some("book-usd"),
        )
        .await;
    assert_eq!(s, 201, "{usd}");
    let usd = id_of(&usd["id"]);
    let sku = catalog.sku(SkuType::Recurring);
    let e_eur = entry(&f, eur, sku, "recurring", Some("month")).await;
    let e_usd = entry(&f, usd, sku, "recurring", Some("month")).await;
    let e_flat = entry_in(&f, eur, sku, "recurring", Some("month"), "flat").await;
    price_in(&f, e_eur, "approved", 1).await;
    price_in(&f, e_eur, "draft", 2).await;
    price_in(&f, e_usd, "pending", 1).await;
    price_in(&f, e_flat, "approved", 1).await;
    price_in(&f, e_flat, "rejected", 2).await;
    // Plan pro names the SKU through two entries: revision 1 (EUR, published) and revision 2
    // (USD, a draft).
    let (pro, r1) = plan(&f, "pro", eur).await;
    let pro = id_of(&pro["id"]);
    item(&f, r1, sku, Some(e_eur), "paid").await;
    publish(&f, pro, r1).await;
    let r2 = bare_revision(&f, pro, 2, usd).await;
    item(&f, r2, sku, Some(e_usd), "paid").await;
    // Plan basic names it once, through the flat entry.
    let (basic, b1) = plan(&f, "basic", eur).await;
    item(&f, b1, sku, Some(e_flat), "paid").await;
    publish(&f, id_of(&basic["id"]), b1).await;
    // Plan old names it only through a superseded revision: not a plan of the SKU.
    let (old, o1) = plan(&f, "old", eur).await;
    let old = id_of(&old["id"]);
    item(&f, o1, sku, Some(e_eur), "paid").await;
    publish(&f, old, o1).await;
    let o2 = bare_revision(&f, old, 2, eur).await;
    publish(&f, old, o2).await;
    // Each entry alone says one plan: their sum is three.
    let mut sum = 0;
    for e in [e_eur, e_usd, e_flat] {
        let plans = read_entry(&f, e).await["usage"]["plans"].as_u64().unwrap();
        assert_eq!(plans, 1);
        sum += plans;
    }
    assert_eq!(sum, 3);
    let answer = port(&f).usage(&f.ctx, tenant, &[sku]).await.unwrap();
    assert_eq!(
        answer,
        vec![SkuUsage {
            sku_id: sku,
            entries: 3,
            currencies: vec!["EUR".into(), "USD".into()],
            prices: PriceCounts {
                approved: 2,
                pending: 1,
                draft: 1,
            },
            plans: 2,
        }]
    );
    assert!(answer[0].plans < sum, "plans are distinct, never a sum");
}

#[tokio::test]
async fn unknown_foreign_and_bundle_skus_answer_zeros_and_each_id_is_answered_once() {
    let (f, catalog) = setup().await;
    let tenant = f.ctx.subject_tenant_id();
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    let e = entry(&f, eur, sku, "usage", None).await;
    price_in(&f, e, "approved", 1).await;
    let bundle = catalog.sku(SkuType::Bundle);
    let unknown = Uuid::new_v4();
    // Another tenant prices a SKU of its own.
    let other = Uuid::new_v4();
    let foreign = catalog.sku(SkuType::Usage);
    let (conn, theirs) = (f.db.conn().unwrap(), AccessScope::for_tenant(other));
    let now = time::OffsetDateTime::now_utc();
    let their_book = book_repo::insert(
        &conn,
        &theirs,
        price_book::Model {
            id: Uuid::now_v7(),
            tenant_id: other,
            code: "eur".into(),
            name: "eur".into(),
            currency: "EUR".into(),
            valid_from: None,
            valid_until: None,
            description: None,
            version: 1,
            created_at: now,
            updated_at: now,
            archived_at: None,
            archived_by: None,
        },
    )
    .await
    .unwrap();
    price_book_entry_repo::insert(
        &conn,
        &theirs,
        price_book_entry::Model {
            id: Uuid::now_v7(),
            tenant_id: other,
            book_id: their_book.id,
            sku_id: foreign,
            charge_kind: "usage".into(),
            period: None,
            model: "per_unit".into(),
            usage_policy_id: None,
            usage_policy_version: None,
            usage_policy_digest: None,
            usage_sku_version: None,
            dimension_key: None,
            invoice_line_override: None,
            reservation_id: Uuid::new_v4(),
            reference_state: "confirmed".into(),
            version: 1,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();
    let zero = |sku_id| SkuUsage {
        sku_id,
        ..SkuUsage::default()
    };
    let answer = port(&f)
        .usage(
            &f.ctx,
            tenant,
            &[sku, unknown, sku, bundle, foreign, unknown],
        )
        .await
        .unwrap();
    assert_eq!(
        answer,
        vec![
            SkuUsage {
                sku_id: sku,
                entries: 1,
                currencies: vec!["EUR".into()],
                prices: PriceCounts {
                    approved: 1,
                    pending: 0,
                    draft: 0,
                },
                plans: 0,
            },
            zero(unknown),
            zero(bundle),
            zero(foreign),
        ],
        "each distinct id once, in the order first asked"
    );
    // The other tenant's own reader sees its entry: the zero above is the tenant boundary.
    let theirs = PricingSkuUsage::new(f.state.clone(), entry_support::enforcer_for(other));
    let reader = entry_support::user_of(other);
    assert_eq!(
        theirs.usage(&reader, other, &[foreign]).await.unwrap()[0].entries,
        1
    );
    // Naming the other tenant does not widen this caller's reach: the tenant argument narrows.
    assert_eq!(
        port(&f).usage(&f.ctx, other, &[foreign]).await.unwrap(),
        vec![zero(foreign)]
    );
    assert_eq!(port(&f).usage(&f.ctx, tenant, &[]).await.unwrap(), vec![]);
}

#[tokio::test]
async fn a_caller_without_entry_read_is_refused_with_403() {
    let (f, catalog) = setup().await;
    let tenant = f.ctx.subject_tenant_id();
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    entry(&f, eur, sku, "usage", None).await;
    let refused = port(&f)
        .usage(&holding(&f, "price_book:read"), tenant, &[sku])
        .await
        .unwrap_err();
    let (status, body, _) = entry_support::answer(Err(refused)).await;
    assert_eq!(status, 403, "{body}");
    let granted = port(&f)
        .usage(&holding(&f, "price_book_entry:read"), tenant, &[sku])
        .await
        .unwrap();
    assert_eq!(granted[0].entries, 1);
}

// ------------------------------------------------------------------ the usage sets (P-D-212)

/// P-D-212: the sets are exactly the SKUs whose usage counts an entry (`priced`) and a plan
/// (`in_plan`), on the same data. A plan that names a SKU only through a superseded revision,
/// and an `included` item that names a SKU without an entry, do not put it in plan; another
/// tenant's entries are not this tenant's.
#[tokio::test]
async fn the_usage_sets_are_the_skus_whose_usage_counts_entries_and_plans() {
    let (f, catalog) = setup().await;
    let tenant = f.ctx.subject_tenant_id();
    let eur = book(&f, "eur").await;
    let unpriced = catalog.sku(SkuType::Usage);
    let priced = catalog.sku(SkuType::Usage);
    entry(&f, eur, priced, "usage", None).await;
    let live = catalog.sku(SkuType::Usage);
    let e_live = entry(&f, eur, live, "usage", None).await;
    let old = catalog.sku(SkuType::Usage);
    let e_old = entry(&f, eur, old, "usage", None).await;
    let included = catalog.sku(SkuType::Usage);
    // A draft revision names `live` through its entry, and `included` with no entry at all.
    let (_, draft) = plan(&f, "pro", eur).await;
    item(&f, draft, live, Some(e_live), "paid").await;
    item(&f, draft, included, None, "included").await;
    // Plan old names `old` only through a superseded revision.
    let (old_plan, o1) = plan(&f, "old", eur).await;
    let old_plan = id_of(&old_plan["id"]);
    item(&f, o1, old, Some(e_old), "paid").await;
    publish(&f, old_plan, o1).await;
    let o2 = bare_revision(&f, old_plan, 2, eur).await;
    publish(&f, old_plan, o2).await;
    // Another tenant prices a SKU of its own.
    let other = Uuid::new_v4();
    let foreign = catalog.sku(SkuType::Usage);
    let (conn, theirs) = (f.db.conn().unwrap(), AccessScope::for_tenant(other));
    let now = time::OffsetDateTime::now_utc();
    let their_book = book_repo::insert(
        &conn,
        &theirs,
        price_book::Model {
            id: Uuid::now_v7(),
            tenant_id: other,
            code: "eur".into(),
            name: "eur".into(),
            currency: "EUR".into(),
            valid_from: None,
            valid_until: None,
            description: None,
            version: 1,
            created_at: now,
            updated_at: now,
            archived_at: None,
            archived_by: None,
        },
    )
    .await
    .unwrap();
    price_book_entry_repo::insert(
        &conn,
        &theirs,
        price_book_entry::Model {
            id: Uuid::now_v7(),
            tenant_id: other,
            book_id: their_book.id,
            sku_id: foreign,
            charge_kind: "usage".into(),
            period: None,
            model: "per_unit".into(),
            usage_policy_id: None,
            usage_policy_version: None,
            usage_policy_digest: None,
            usage_sku_version: None,
            dimension_key: None,
            invoice_line_override: None,
            reservation_id: Uuid::new_v4(),
            reference_state: "confirmed".into(),
            version: 1,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap();

    let all = [unpriced, priced, live, old, included, foreign];
    let usage = port(&f).usage(&f.ctx, tenant, &all).await.unwrap();
    let sets = port(&f).usage_sets(&f.ctx, tenant).await.unwrap();
    assert_eq!(usage.len(), all.len());
    for u in &usage {
        assert_eq!(
            sets.priced.contains(&u.sku_id),
            u.entries > 0,
            "priced iff entries > 0: {u:?}"
        );
        assert_eq!(
            sets.in_plan.contains(&u.sku_id),
            u.plans > 0,
            "in_plan iff plans > 0: {u:?}"
        );
    }
    let mut expected = vec![priced, live, old];
    expected.sort_unstable();
    assert_eq!(
        sets,
        SkuUsageSets {
            priced: expected,
            in_plan: vec![live],
        },
        "sorted and distinct; the included item and the superseded-only plan count neither"
    );
    // The other tenant's own reader has its SKU in its own set.
    let theirs = PricingSkuUsage::new(f.state.clone(), entry_support::enforcer_for(other));
    let reader = entry_support::user_of(other);
    assert_eq!(
        theirs.usage_sets(&reader, other).await.unwrap().priced,
        vec![foreign]
    );
    // Naming the other tenant does not widen this caller's reach.
    assert_eq!(
        port(&f).usage_sets(&f.ctx, other).await.unwrap(),
        SkuUsageSets::default()
    );
}

/// P-D-212: the sets are refused to a caller without `price_book_entry:read`, as the usage is.
#[tokio::test]
async fn the_usage_sets_refuse_a_caller_without_entry_read_with_403() {
    let (f, catalog) = setup().await;
    let tenant = f.ctx.subject_tenant_id();
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    entry(&f, eur, sku, "usage", None).await;
    let refused = port(&f)
        .usage_sets(&holding(&f, "price_book:read"), tenant)
        .await
        .unwrap_err();
    let (status, body, _) = entry_support::answer(Err(refused)).await;
    assert_eq!(status, 403, "{body}");
    let granted = port(&f)
        .usage_sets(&holding(&f, "price_book_entry:read"), tenant)
        .await
        .unwrap();
    assert_eq!(granted.priced, vec![sku]);
}

/// The statements on pricing's tables one call of `usage_sets` makes.
async fn set_statements(
    f: &Fixture,
    recorder: &toolkit_db::test_support::QueryRecorder,
    n: usize,
) -> Vec<(String, usize)> {
    recorder.clear();
    let sets = port(f)
        .usage_sets(&f.ctx, f.ctx.subject_tenant_id())
        .await
        .unwrap();
    assert_eq!((sets.priced.len(), sets.in_plan.len()), (n, n));
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

/// P-D-212: the sets are two statements, with the same binds, for 10 and for 100 SKUs.
#[tokio::test]
async fn the_usage_sets_read_in_two_statements_for_10_and_100_skus() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog.clone()).await;
    seeded_book(&f, &catalog, "small", 10).await;
    let ten = set_statements(&f, &recorder, 10).await;
    seeded_book(&f, &catalog, "large", 90).await;
    let hundred = set_statements(&f, &recorder, 100).await;
    for (i, (sql, binds)) in hundred.iter().enumerate() {
        eprintln!("statement {i} ({binds} binds): {sql}");
    }
    assert_eq!(ten.len(), 2, "one read per set: {ten:#?}");
    assert_eq!(ten, hundred, "the same statements, whatever the size");
}
