//! The revision read, the reservations read and the effective policy (phase 9 run 9.6, D-480,
//! D-481): a revision read carries its entries and the price in force on its sale date, a draft
//! names the SKUs the plan sells, reservations settle without a per-item read, and the quorum a
//! submit needs is on the checks and on its own door.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]
mod plan_support;
use bss_pricing::infra::{
    reference_ticker::Ticker,
    reference_work::{self, WallClock},
    storage::{
        entity::{plan_revision, price_book_entry},
        repo::{
            plan_item_repo, plan_revision_repo, price_book_entry_repo, price_repo,
            reference_op_repo,
        },
    },
};
use bss_products_sdk::models::SkuType;
use plan_support::{
    Catalog, Fixture, book, entry, entry_support, holding, id_of, item, plan, policy_entry,
    request, scope, setup,
};
use serde_json::{Value, json};
use std::sync::Arc;
use time::{Date, Duration, OffsetDateTime};
use uuid::Uuid;

fn today() -> Date {
    OffsetDateTime::now_utc().date()
}
fn days(n: i64) -> Date {
    today() + Duration::days(n)
}
fn day(y: i32, m: time::Month, d: u8) -> Date {
    Date::from_calendar_date(y, m, d).unwrap()
}
async fn get(f: &Fixture, path: &str) -> Value {
    let (s, b, _) = f.call("GET", path, json!({}), None, None).await;
    assert_eq!(s, 200, "{path}: {b}");
    b
}
async fn policy(f: &Fixture, kind: &str, quorum: u32) {
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let (s, b, _) = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"kind": kind, "quorum": quorum}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
}
async fn price_from(f: &Fixture, entry: Uuid, version: i32, from: Date, dim: Option<&str>) -> Uuid {
    let conn = f.db.conn().unwrap();
    let e = price_book_entry_repo::find(&conn, &scope(f), f.ctx.subject_tenant_id(), entry)
        .await
        .unwrap()
        .unwrap();
    let mut p = entry_support::price(&e);
    p.version_no = version;
    p.state = "approved".into();
    p.effective_from = from;
    p.dim_value = dim.map(str::to_owned);
    let id = p.id;
    price_repo::insert(&conn, &scope(f), p).await.unwrap();
    id
}
async fn sale_date(f: &Fixture, revision: Uuid, from: Option<Date>) {
    let path = format!("/plan-revisions/{revision}");
    let (_, _, tag) = f.call("GET", &path, json!({}), None, None).await;
    let (s, b, _) = f
        .call(
            "PATCH",
            &path,
            json!({"available_from": from.map(|d| d.to_string())}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
}
/// A draft whose `n` items each name a priced usage entry of its book. Each entry carries a
/// rating policy, which a submit requires (D-502). The items share `vm-hours`, so submitting
/// more than one is `METER_DUPLICATE`.
async fn draft(f: &Fixture, catalog: &Catalog, code: &str, n: usize) -> (Uuid, Uuid, Vec<Uuid>) {
    let eur = book(f, code).await;
    let (created, rev) = plan(f, code, eur).await;
    let mut skus = Vec::new();
    for _ in 0..n {
        let sku = catalog.sku(SkuType::Usage);
        let priced = policy_entry(f, eur, sku, "usage", None).await;
        price_from(f, priced, 1, day(2020, time::Month::January, 1), None).await;
        item(f, rev, sku, Some(priced), "paid").await;
        skus.push(sku);
    }
    (id_of(&created["id"]), rev, skus)
}
/// A draft of `n` recurring items on one period, so a submit is not `METER_DUPLICATE`.
async fn draft_recurring(
    f: &Fixture,
    catalog: &Catalog,
    code: &str,
    n: usize,
) -> (Uuid, Uuid, Vec<Uuid>) {
    let eur = book(f, code).await;
    let (created, rev) = plan(f, code, eur).await;
    let mut skus = Vec::new();
    for _ in 0..n {
        let sku = catalog.sku(SkuType::Recurring);
        let priced = entry(f, eur, sku, "recurring", Some("month")).await;
        price_from(f, priced, 1, day(2020, time::Month::January, 1), None).await;
        item(f, rev, sku, Some(priced), "paid").await;
        skus.push(sku);
    }
    (id_of(&created["id"]), rev, skus)
}
fn entry_of(body: &Value, entry: Uuid) -> &Value {
    body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["price_book_entry_id"] == entry.to_string())
        .unwrap_or_else(|| panic!("no entry {entry} in {body}"))
}
fn sql(recorder: &toolkit_db::test_support::QueryRecorder) -> Vec<String> {
    recorder
        .events()
        .into_iter()
        .filter(|q| {
            q.table
                .as_deref()
                .is_some_and(|t| t.starts_with("pricing_"))
        })
        .map(|q| q.sql)
        .collect()
}

/// D-480: the price on the sale date is money, judged per book. A plan reader without `price_book`
/// read sees null. A grant narrowed to the revision's book nulls a foreign book's entry. A
/// value-only entry is null while its coverage check is green. A future sale date differs from
/// today's price; a past sale date is that past date.
#[tokio::test]
async fn the_sale_date_price_follows_the_book_grant_and_the_date() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let other = book(&f, "other").await;
    let (created, rev) = plan(&f, "pro", eur).await;
    let sku = catalog.sku(SkuType::Usage);
    let own = entry(&f, eur, sku, "recurring", Some("month")).await;
    let now_price = price_from(&f, own, 1, day(2020, time::Month::January, 1), None).await;
    let later = price_from(&f, own, 2, days(10), None).await;
    item(&f, rev, sku, Some(own), "paid").await;
    let foreign_sku = catalog.sku(SkuType::Usage);
    let foreign = entry(&f, other, foreign_sku, "usage", None).await;
    let foreign_price = price_from(&f, foreign, 1, day(2020, time::Month::January, 1), None).await;
    // The repository refuses a foreign entry on insert. Point the draft at that book for the
    // insert, then put it back: the item keeps the other book's entry.
    let conn = f.db.conn().unwrap();
    let mut stored = plan_revision_repo::find(&conn, &scope(&f), f.ctx.subject_tenant_id(), rev)
        .await
        .unwrap()
        .unwrap();
    stored.book_id = other;
    plan_revision_repo::update_draft(&conn, &scope(&f), stored)
        .await
        .unwrap();
    item(&f, rev, foreign_sku, Some(foreign), "paid").await;
    let mut stored = plan_revision_repo::find(&conn, &scope(&f), f.ctx.subject_tenant_id(), rev)
        .await
        .unwrap()
        .unwrap();
    stored.book_id = eur;
    plan_revision_repo::update_draft(&conn, &scope(&f), stored)
        .await
        .unwrap();

    let read = get(&f, &format!("/plan-revisions/{rev}")).await;
    assert_eq!(read["id"], created["revisions"][0]["id"]);
    assert_eq!(read["sale_date"], today().to_string(), "{read}");
    assert_eq!(
        entry_of(&read, own)["price_on_sale_date"]["id"],
        now_price.to_string(),
        "{read}"
    );
    assert!(read.get("entries").is_some());
    assert!(read["carried_sku_ids"].as_array().unwrap().is_empty());

    let reader = holding(&f, "plan:read");
    let (s, hidden, _) = f
        .call_as(
            &reader,
            "GET",
            &format!("/plan-revisions/{rev}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{hidden}");
    for e in hidden["entries"].as_array().unwrap() {
        assert!(
            e.as_object().unwrap().contains_key("price_on_sale_date")
                && e["price_on_sale_date"].is_null(),
            "{e}"
        );
    }

    let app = money_app(&f, Some(vec![eur]), false);
    let (s, narrowed, _) = request(
        &app,
        &f.ctx,
        "GET",
        &format!("/plan-revisions/{rev}"),
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(s, 200, "{narrowed}");
    assert_eq!(
        entry_of(&narrowed, own)["price_on_sale_date"]["id"],
        now_price.to_string()
    );
    assert!(entry_of(&narrowed, foreign)["price_on_sale_date"].is_null());
    assert_eq!(
        entry_of(&read, foreign)["price_on_sale_date"]["id"],
        foreign_price.to_string(),
        "the unnarrowed grant still names the foreign price: {read}"
    );

    sale_date(&f, rev, Some(days(10))).await;
    let future = get(&f, &format!("/plan-revisions/{rev}")).await;
    assert_eq!(future["sale_date"], days(10).to_string());
    assert_eq!(
        entry_of(&future, own)["price_on_sale_date"]["id"],
        later.to_string(),
        "the price that starts on the sale date: {future}"
    );
    let today_list = get(&f, &format!("/price-books/{eur}/entries")).await;
    let listed = today_list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == own.to_string())
        .unwrap();
    assert_eq!(listed["current_price"]["id"], now_price.to_string());

    let past = day(2020, time::Month::March, 15);
    sale_date(&f, rev, Some(past)).await;
    let aged = get(&f, &format!("/plan-revisions/{rev}")).await;
    assert_eq!(aged["sale_date"], past.to_string());
    assert_eq!(
        entry_of(&aged, own)["price_on_sale_date"]["id"],
        now_price.to_string(),
        "the later price has not started: {aged}"
    );

    // A dimension-keyed entry priced only on its value: null here, covered in the checks.
    let (s, _, tag) = f
        .call("GET", "/dimension-keys", json!({}), None, None)
        .await;
    assert_eq!(s, 200);
    let (s, b, _) = f
        .call(
            "PUT",
            "/dimension-keys",
            json!({"items":[{"key":"region","values":["eu","us"]}]}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let valued_sku = catalog.sku(SkuType::Usage);
    let valued = keyed(&f, eur, valued_sku, "region").await;
    price_from(
        &f,
        valued,
        1,
        day(2020, time::Month::January, 1),
        Some("eu"),
    )
    .await;
    price_from(
        &f,
        valued,
        2,
        day(2020, time::Month::January, 1),
        Some("us"),
    )
    .await;
    item(&f, rev, valued_sku, Some(valued), "paid").await;
    sale_date(&f, rev, None).await;
    let mixed = get(&f, &format!("/plan-revisions/{rev}")).await;
    assert!(
        entry_of(&mixed, valued)["price_on_sale_date"].is_null(),
        "{mixed}"
    );
    let checks = get(&f, &format!("/plan-revisions/{rev}/checks")).await;
    let uncovered = checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == "ITEM_UNCOVERED")
        .unwrap();
    assert_eq!(uncovered["ok"], true, "{checks}");
}

async fn keyed(f: &Fixture, book: Uuid, sku: Uuid, key: &str) -> Uuid {
    let now = OffsetDateTime::now_utc();
    price_book_entry_repo::insert(
        &f.db.conn().unwrap(),
        &scope(f),
        price_book_entry::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            book_id: book,
            sku_id: sku,
            charge_kind: "usage".into(),
            period: None,
            model: "per_unit".into(),
            usage_policy_id: None,
            usage_policy_version: None,
            usage_policy_digest: None,
            usage_sku_version: None,
            dimension_key: Some(key.into()),
            invoice_line_override: None,
            reservation_id: Uuid::new_v4(),
            reference_state: "confirmed".into(),
            version: 1,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap()
    .id
}

/// The money's policy, `price_book` read narrowed to `books`, or unavailable.
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

/// D-440's order on this read: plan read's 403, then the money's 503, then the 404.
#[tokio::test]
async fn the_money_policy_is_judged_before_the_revision_is_found() {
    let (f, _) = setup().await;
    let (s, b, _) = f
        .call_as(
            &holding(&f, "price:read"),
            "GET",
            &format!("/plan-revisions/{}", Uuid::new_v4()),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 403, "{b}");
    let app = money_app(&f, None, true);
    let (s, b, _) = request(
        &app,
        &f.ctx,
        "GET",
        &format!("/plan-revisions/{}", Uuid::new_v4()),
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(s, 503, "{b}");
}

/// D-480: `carried_sku_ids` is `[]` with nothing in effect, and the SKUs of a scheduled revision
/// that has come into force — not those of the stored-published predecessor. The due revision
/// itself, which reads published, carries null.
#[tokio::test]
async fn carried_sku_ids_name_the_revision_in_effect() {
    let (f, catalog) = setup().await;
    let (_, rev1, skus) = draft(&f, &catalog, "pro", 1).await;
    let bare = get(&f, &format!("/plan-revisions/{rev1}")).await;
    assert_eq!(bare["carried_sku_ids"], json!([]));
    policy(&f, "plan_revision", 0).await;
    let (s, receipt, _) = f
        .call(
            "POST",
            &format!("/plan-revisions/{rev1}/submit"),
            json!({}),
            None,
            Some("pub"),
        )
        .await;
    assert_eq!(s, 201, "{receipt}");
    let (s, copied, _) = f
        .call(
            "POST",
            &format!(
                "/plans/{}/revisions",
                receipt["revision"]["plan_id"].as_str().unwrap()
            ),
            json!({}),
            None,
            Some("copy"),
        )
        .await;
    assert_eq!(s, 201, "{copied}");
    let rev2 = id_of(&copied["id"]);
    assert_eq!(
        copied["carried_sku_ids"],
        Value::Null,
        "a write keeps the revision DTO"
    );
    assert!(copied.get("sale_date").is_none(), "{copied}");
    assert!(copied.get("entries").is_none(), "{copied}");
    assert_eq!(
        copied["reservations_settled"], false,
        "the copy is unreserved"
    );
    let extra = catalog.sku(SkuType::Usage);
    let eur = id_of(&get(&f, &format!("/plan-revisions/{rev2}")).await["book_id"]);
    let priced = policy_entry(&f, eur, extra, "usage", None).await;
    price_from(&f, priced, 1, day(2020, time::Month::January, 1), None).await;
    item(&f, rev2, extra, Some(priced), "paid").await;
    sale_date(&f, rev2, Some(days(-1))).await;
    let unit = plan_support::lock(&f, rev2).await;
    plan_revision_repo::schedule(
        &f.db.conn().unwrap(),
        &scope(&f),
        f.ctx.subject_tenant_id(),
        rev2,
        unit,
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let due = get(&f, &format!("/plan-revisions/{rev2}")).await;
    assert_eq!(due["state"], "published", "its date has come: {due}");
    assert!(due["carried_sku_ids"].is_null(), "{due}");
    let now = OffsetDateTime::now_utc();
    let rev3 = Uuid::now_v7();
    plan_revision_repo::insert(
        &f.db.conn().unwrap(),
        &scope(&f),
        plan_revision::Model {
            id: rev3,
            tenant_id: f.ctx.subject_tenant_id(),
            plan_id: id_of(&due["plan_id"]),
            rev_no: 3,
            book_id: eur,
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
    .unwrap();
    let beside = get(&f, &format!("/plan-revisions/{rev3}")).await;
    assert_eq!(beside["state"], "draft");
    let mut carried: Vec<String> = beside["carried_sku_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    carried.sort();
    let mut want = vec![skus[0].to_string(), extra.to_string()];
    want.sort();
    assert_eq!(carried, want, "the due revision, not rev 1 alone: {beside}");
}

/// D-480: settled is false while an item is unreserved, true once the reference ticker confirms
/// it, and still true when that reference is lost.
#[tokio::test]
async fn reservations_settle_through_the_ticker_and_lost_counts() {
    let (f, catalog) = setup().await;
    let (_, rev, _) = draft(&f, &catalog, "pro", 1).await;
    let conn = f.db.conn().unwrap();
    let row = plan_item_repo::for_revision(&conn, &scope(&f), f.ctx.subject_tenant_id(), rev)
        .await
        .unwrap()
        .pop()
        .unwrap();
    plan_item_repo::set_reference(
        &conn,
        &scope(&f),
        f.ctx.subject_tenant_id(),
        row.id,
        row.version,
        bss_pricing::domain::plan::ReferenceState::Unreserved,
        None,
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let open = get(&f, &format!("/plan-revisions/{rev}")).await;
    assert_eq!(open["reservations_settled"], false, "{open}");
    let door = get(&f, &format!("/plan-revisions/{rev}/reservations")).await;
    assert_eq!(door["settled"], false, "{door}");
    assert_eq!(door["items"][0]["reference_state"], "unreserved");
    let row = plan_item_repo::for_revision(&conn, &scope(&f), f.ctx.subject_tenant_id(), rev)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let mut op =
        reference_work::attach_op(&f.ctx, &row, Uuid::now_v7(), OffsetDateTime::now_utc()).unwrap();
    // A fresh attach waits out the in-flight grace; the ticker takes it once that instant has passed.
    op.next_attempt_at = OffsetDateTime::now_utc() - Duration::seconds(1);
    reference_op_repo::insert(&conn, &scope(&f), op)
        .await
        .unwrap();
    Ticker::new(f.state.clone(), Arc::new(WallClock), 10, 100)
        .tick()
        .await
        .unwrap();
    let settled = get(&f, &format!("/plan-revisions/{rev}")).await;
    assert_eq!(settled["reservations_settled"], true, "{settled}");
    assert_eq!(
        get(&f, &format!("/plan-revisions/{rev}/reservations")).await["settled"],
        true
    );
    let conn = f.db.conn().unwrap();
    let row = plan_item_repo::for_revision(&conn, &scope(&f), f.ctx.subject_tenant_id(), rev)
        .await
        .unwrap()
        .pop()
        .unwrap();
    plan_item_repo::set_reference(
        &conn,
        &scope(&f),
        f.ctx.subject_tenant_id(),
        row.id,
        row.version,
        bss_pricing::domain::plan::ReferenceState::Lost,
        row.reservation_id,
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let lost = get(&f, &format!("/plan-revisions/{rev}")).await;
    assert_eq!(
        lost["reservations_settled"], true,
        "lost counts as settled: {lost}"
    );
    assert_eq!(
        get(&f, &format!("/plan-revisions/{rev}/reservations")).await["items"][0]["reference_state"],
        "lost"
    );
}

/// D-480: the reservations door is plan read, 404, and two statements for 10 items and for 100.
#[tokio::test]
async fn the_reservations_door_reads_in_two_statements() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog.clone()).await;
    let (_, rev10, _) = draft(&f, &catalog, "ten", 10).await;
    recorder.clear();
    let body = get(&f, &format!("/plan-revisions/{rev10}/reservations")).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 10);
    assert_eq!(body["settled"], true);
    let ten = sql(&recorder);
    let (_, rev100, _) = draft(&f, &catalog, "hundred", 100).await;
    recorder.clear();
    let body = get(&f, &format!("/plan-revisions/{rev100}/reservations")).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 100);
    let hundred = sql(&recorder);
    assert_eq!(ten.len(), 2, "{ten:#?}");
    assert_eq!(ten, hundred, "the same statements, whatever the size");
    let (s, missing, _) = f
        .call(
            "GET",
            &format!("/plan-revisions/{}/reservations", Uuid::new_v4()),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 404, "{missing}");
    let (s, b, _) = f
        .call_as(
            &holding(&f, "plan:author"),
            "GET",
            &format!("/plan-revisions/{rev10}/reservations"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 403, "{b}");
}

/// D-480: absolute statement counts of the revision read, the same for 10 items and for 100.
/// Base after 9.5d-2, plus 3 (entries, admission, prices) and, on draft and pending, the
/// in-effect items.
#[tokio::test]
async fn the_revision_read_is_pinned_per_state_for_10_and_100_items() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog.clone()).await;
    let measure = async |rev: Uuid| {
        recorder.clear();
        let body = get(&f, &format!("/plan-revisions/{rev}")).await;
        (body, sql(&recorder))
    };
    // Draft: 3 + 3 + 1.
    let (_, small, _) = draft(&f, &catalog, "d10", 10).await;
    let (body, ten) = measure(small).await;
    assert_eq!(body["state"], "draft");
    assert_eq!(body["entries"].as_array().unwrap().len(), 10);
    for _ in 0..90 {
        let sku = catalog.sku(SkuType::Usage);
        let eur = id_of(&body["book_id"]);
        let priced = policy_entry(&f, eur, sku, "usage", None).await;
        price_from(&f, priced, 1, day(2020, time::Month::January, 1), None).await;
        item(&f, small, sku, Some(priced), "paid").await;
    }
    let (body, hundred) = measure(small).await;
    assert_eq!(body["entries"].as_array().unwrap().len(), 100);
    assert_eq!(ten.len(), 7, "{ten:#?}");
    assert_eq!(ten, hundred);
    // Pending: 5 + 3 + 1. Scheduled, published, superseded: 4 + 3.
    for (code, state, want) in [
        ("pend", "pending", 9usize),
        ("sched", "scheduled", 7usize),
        ("pub", "published", 7usize),
        ("old", "superseded", 7usize),
    ] {
        let (plan_id, rev, _) = draft_recurring(&f, &catalog, &format!("{code}10"), 10).await;
        prepare(&f, rev, state).await;
        recorder.clear();
        let body = get(&f, &format!("/plan-revisions/{rev}")).await;
        assert_eq!(body["state"], state, "{code}: {body}");
        let small_sql = sql(&recorder);
        let (_, rev, _) = draft_recurring(&f, &catalog, &format!("{code}100"), 100).await;
        prepare(&f, rev, state).await;
        recorder.clear();
        let _ = get(&f, &format!("/plan-revisions/{rev}")).await;
        let large_sql = sql(&recorder);
        assert_eq!(small_sql.len(), want, "{code}: {small_sql:#?}");
        assert_eq!(small_sql, large_sql, "{code}");
        let _ = plan_id;
    }
}
async fn submit_revision(f: &Fixture, revision: Uuid, key: &str) -> Value {
    let (status, body, _) = f
        .call(
            "POST",
            &format!("/plan-revisions/{revision}/submit"),
            json!({}),
            None,
            Some(key),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    body
}
async fn prepare(f: &Fixture, revision: Uuid, state: &str) {
    match state {
        "pending" => {
            policy(f, "plan_revision", 1).await;
            let body = submit_revision(f, revision, &format!("submit-{revision}")).await;
            assert_eq!(body["revision"]["state"], "pending", "{body}");
            assert!(body["revision"].get("sale_date").is_none(), "{body}");
        }
        "scheduled" => {
            sale_date(f, revision, Some(days(20))).await;
            policy(f, "plan_revision", 0).await;
            let body = submit_revision(f, revision, &format!("submit-{revision}")).await;
            assert_eq!(body["revision"]["state"], "scheduled", "{body}");
        }
        "published" | "superseded" => {
            policy(f, "plan_revision", 0).await;
            let body = submit_revision(f, revision, &format!("submit-{revision}")).await;
            assert_eq!(body["revision"]["state"], "published", "{body}");
            if state == "superseded" {
                supersede(f, id_of(&body["revision"]["plan_id"]), revision).await;
            }
        }
        _ => panic!("unknown state {state}"),
    }
}
async fn supersede(f: &Fixture, plan_id: Uuid, revision: Uuid) {
    let (status, copied, _) = f
        .call(
            "POST",
            &format!("/plans/{plan_id}/revisions"),
            json!({}),
            None,
            Some(&format!("copy-{revision}")),
        )
        .await;
    assert_eq!(status, 201, "{copied}");
    let rev2 = id_of(&copied["id"]);
    let body = submit_revision(f, rev2, &format!("submit2-{revision}")).await;
    assert_eq!(body["revision"]["state"], "published", "{body}");
}

/// D-481: the checks name the `plan_revision` quorum, and the effective-policy door answers each
/// kind under its own grant, in one statement.
#[tokio::test]
async fn the_quorum_is_on_the_checks_and_on_the_effective_policy() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog.clone()).await;
    policy(&f, "plan_revision", 2).await;
    policy(&f, "prices", 3).await;
    let (_, rev, _) = draft(&f, &catalog, "pro", 1).await;
    let checks = get(&f, &format!("/plan-revisions/{rev}/checks")).await;
    assert_eq!(checks["quorum_required"], 2, "{checks}");
    let approval = checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == "APPROVAL")
        .unwrap();
    assert!(
        approval["detail"].as_str().unwrap().contains('2'),
        "{approval}"
    );
    recorder.clear();
    let prices = get(&f, "/approval-policy/prices/effective").await;
    assert_eq!(prices, json!({"kind":"prices","quorum_required":3}));
    let one = sql(&recorder);
    recorder.clear();
    let plans = get(&f, "/approval-policy/plan_revision/effective").await;
    assert_eq!(plans, json!({"kind":"plan_revision","quorum_required":2}));
    assert_eq!(one.len(), 1, "{one:#?}");
    assert_eq!(one, sql(&recorder));
    let (s, b, _) = f
        .call_as(
            &holding(&f, "price_book_entry:read"),
            "GET",
            "/approval-policy/prices/effective",
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let (s, b, _) = f
        .call_as(
            &holding(&f, "price_book_entry:read"),
            "GET",
            "/approval-policy/plan_revision/effective",
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 403, "{b}");
    let (s, b, _) = f
        .call_as(
            &holding(&f, "plan:read"),
            "GET",
            "/approval-policy/plan_revision/effective",
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let (s, b, _) = f
        .call_as(
            &holding(&f, "plan:read"),
            "GET",
            "/approval-policy/prices/effective",
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 403, "{b}");
    let (s, b, _) = f
        .call_as(
            &holding(&f, "config:read"),
            "GET",
            "/approval-policy/prices/effective",
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 403, "the policy grant is not this door: {b}");
    let (s, b, _) = f
        .call(
            "GET",
            "/approval-policy/nope/effective",
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 400, "{b}");
    assert!(b.to_string().contains("QUERY_INVALID"), "{b}");
}

/// A grant that allows the tenant, and constrains `price_book_entry` read and `plan` read
/// to one resource id. That id is not a policy kind.
struct Narrow {
    tenant: Uuid,
    resource: Uuid,
}
#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverApi for Narrow {
    async fn evaluate(
        &self,
        _: toolkit_security::PlatformSecurityContext,
        request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, toolkit_canonical_errors::CanonicalError>
    {
        use authz_resolver_sdk::*;
        let constrained = request.action.name == "read"
            && matches!(
                request.resource.resource_type.as_str(),
                "gts.cf.bss.pricing.price_book_entry.v1~" | "gts.cf.bss.pricing.plan.v1~"
            );
        let mut predicates = vec![Predicate::In(InPredicate::new(
            toolkit_security::pep_properties::OWNER_TENANT_ID,
            vec![self.tenant],
        ))];
        if constrained {
            predicates.push(Predicate::In(InPredicate::new(
                toolkit_security::pep_properties::RESOURCE_ID,
                vec![self.resource],
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
fn narrow_app(f: &Fixture, resource: Uuid) -> axum::Router {
    entry_support::production(f.state.clone()).layer(axum::Extension(
        authz_resolver_sdk::PolicyEnforcer::new(Arc::new(Narrow {
            tenant: f.ctx.subject_tenant_id(),
            resource,
        })),
    ))
}

/// D-481: the effective quorum is the tenant's policy. A resource constraint on the grant
/// is the admission, not a filter on `kind`.
#[tokio::test]
async fn the_effective_quorum_is_the_tenants_under_a_resource_constraint() {
    let (f, _) = setup().await;
    policy(&f, "prices", 3).await;
    policy(&f, "plan_revision", 0).await;
    let app = narrow_app(&f, Uuid::new_v4());
    let (s, prices, _) = request(
        &app,
        &f.ctx,
        "GET",
        "/approval-policy/prices/effective",
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(s, 200, "{prices}");
    assert_eq!(prices, json!({"kind": "prices", "quorum_required": 3}));
    let (s, plans, _) = request(
        &app,
        &f.ctx,
        "GET",
        "/approval-policy/plan_revision/effective",
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(s, 200, "{plans}");
    assert_eq!(
        plans,
        json!({"kind": "plan_revision", "quorum_required": 0})
    );
}
