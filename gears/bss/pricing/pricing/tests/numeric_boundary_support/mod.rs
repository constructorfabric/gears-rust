//! Pricing's numeric contract, walked end to end through the production router and the in-process
//! services on a database of either backend. `numeric_boundaries.rs` runs each walk on `SQLite`,
//! `postgres_numeric_boundaries.rs` on `PostgreSQL`. The rules are D-527 to D-533.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]
use crate::{acceptance_support::AcceptanceFixture, plan_support as p};
use bss_pricing::api::pricing_read::PricingReadProvider;
use bss_pricing::api::rest::authoring::AuthoringState;
use bss_pricing::domain::currency::CurrencyScales;
use bss_pricing::infra::storage::{
    RepoError,
    repo::{price_book_entry_repo, price_repo, usage_policy_repo},
};
use bss_pricing_sdk::acceptance::{CommandMeta, PricingAcceptanceV1, SellabilityV1};
use bss_pricing_sdk::read::{CatalogRef, ImmutablePrice, PriceModel, PriceQuery, PricingReadV1};
use bss_products_sdk::models::SkuType;
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, Database, Statement};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit_db::secure::AccessScope;
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

/// The largest rate: 28 digits, 12 of them fractional.
const LARGEST_RATE: &str = "9999999999999999.999999999999";
/// The smallest positive rate: one unit in the 12th place.
const SMALLEST_RATE: &str = "0.000000000001";
/// The largest whole value: 28 digits.
const LARGEST_WHOLE: &str = "9999999999999999999999999999";
/// The smallest positive quantity or threshold: one unit in the 28th place.
const SMALLEST_QUANTITY: &str = "0.0000000000000000000000000001";
/// The largest minimum fee at scale 2: 28 digits, 2 of them fractional.
const LARGEST_FEE_AT_2: &str = "99999999999999999999999999.99";

/// A fixture over `db` whose currency table adds `XTS` (ISO 4217 lists it without a scale) at 2.
async fn fixture(
    db: DBProvider<DbError>,
    dsn: p::entry_support::TestDsn,
) -> (p::Fixture, Arc<p::Catalog>) {
    let catalog = Arc::new(p::Catalog::default());
    let f = p::Fixture::on_with_currency_scales(
        db,
        Uuid::new_v4(),
        dsn,
        catalog.clone(),
        &[("XTS", 2)],
    )
    .await;
    (f, catalog)
}

/// A state and a router over the fixture's database whose currency table gives `XTS` the scale
/// `scale`: the deployment's table changed, the database did not.
async fn changed(f: &p::Fixture, scale: u8) -> (Arc<AuthoringState>, axum::Router) {
    let state = Arc::new(
        AuthoringState::new(f.db.clone(), f.state.hub.clone())
            .await
            .unwrap()
            .with_currency_scales(Arc::new(
                CurrencyScales::try_new([("XTS".to_owned(), scale)].into()).unwrap(),
            )),
    );
    let app = p::entry_support::app_for(state.clone(), f.ctx.subject_tenant_id());
    (state, app)
}

/// One reviewer approves a unit.
async fn quorum_one(f: &p::Fixture) {
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let (status, _, _) = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"quorum": 1}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(status, 200, "one reviewer approves a unit");
}

fn prices_path(entry: Uuid) -> String {
    format!("/price-book-entries/{entry}/prices")
}

fn draft(price: Value, min_fee: Option<&str>, from: &str) -> Value {
    let mut body = json!({"eligibility": "all", "effective_from": from});
    body["price"] = price;
    if let Some(fee) = min_fee {
        body["min_fee"] = json!(fee);
    }
    body
}

/// The code of a refusal: a conflict's reason, or a 400's first violation.
fn code_of(body: &Value) -> String {
    let context = &body["context"];
    context["reason"]
        .as_str()
        .or_else(|| context["field_violations"][0]["reason"].as_str())
        .unwrap_or_default()
        .to_owned()
}

/// A draft through `app`, as the fixture's user; the door's answer.
async fn create(
    f: &p::Fixture,
    app: &axum::Router,
    entry: Uuid,
    body: Value,
    key: &str,
) -> (u16, Value) {
    let (status, answer, _) = p::request(
        app,
        &f.ctx,
        "POST",
        &prices_path(entry),
        body,
        None,
        Some(key),
    )
    .await;
    (status, answer)
}

/// A draft through `app` that must be created; its first row.
async fn created(f: &p::Fixture, app: &axum::Router, entry: Uuid, body: Value, key: &str) -> Value {
    let (status, answer) = create(f, app, entry, body, key).await;
    assert_eq!(status, 201, "the draft is created");
    answer["items"][0].clone()
}

/// Submit one draft through `app`; its pending unit.
async fn submit(f: &p::Fixture, app: &axum::Router, price: &Value, key: &str) -> Value {
    let (status, receipt, _) = p::request(
        app,
        &f.ctx,
        "POST",
        &format!("/prices/{}/submit", price["id"].as_str().unwrap()),
        json!({}),
        None,
        Some(key),
    )
    .await;
    assert_eq!(status, 201, "the draft is submitted");
    receipt["unit"].clone()
}

/// Approve a unit through `app` as a reviewer who wrote none of its prices.
async fn approve(f: &p::Fixture, app: &axum::Router, unit: &Value, key: &str) -> (u16, Value) {
    let (status, body, _) = p::request(
        app,
        &f.user(),
        "POST",
        &format!("/approval-units/{}/approve", unit["id"].as_str().unwrap()),
        json!({"generation": 1}),
        None,
        Some(key),
    )
    .await;
    (status, body)
}

/// A draft submitted and approved through the fixture's router.
async fn approved(f: &p::Fixture, entry: Uuid, body: Value, key: &str) -> Value {
    let price = created(f, &f.app, entry, body, key).await;
    let unit = submit(f, &f.app, &price, &format!("{key}-submit")).await;
    let (status, _) = approve(f, &f.app, &unit, &format!("{key}-approve")).await;
    assert_eq!(status, 200, "the price is approved");
    price
}

/// A price row as the entry's price list reads it now.
async fn listed(f: &p::Fixture, entry: Uuid, price: &Value) -> Value {
    let (status, body, _) = f
        .call("GET", &prices_path(entry), json!({}), None, None)
        .await;
    assert_eq!(status, 200, "the entry's prices are listed");
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == price["id"])
        .expect("the price is listed")
        .clone()
}

/// The stored row's money as the database holds it: `(price_json, min_fee)`.
async fn stored(f: &p::Fixture, price: &Value) -> (Value, Option<String>) {
    let tenant = f.ctx.subject_tenant_id();
    let row = price_repo::find(
        &f.db.conn().unwrap(),
        &AccessScope::for_tenant(tenant),
        tenant,
        p::id_of(&price["id"]),
    )
    .await
    .unwrap()
    .expect("the price is stored");
    (row.price_json, row.min_fee)
}

/// The in-process typed read of an approved price over `state`.
async fn typed(
    f: &p::Fixture,
    state: Arc<AuthoringState>,
    price: &Value,
) -> Result<ImmutablePrice, toolkit_canonical_errors::CanonicalError> {
    let tenant = f.ctx.subject_tenant_id();
    PricingReadProvider::new(state, Arc::new(p::entry_support::enforcer_for(tenant)))
        .price(
            &f.ctx,
            PriceQuery {
                catalog: CatalogRef { tenant_id: tenant },
                price_id: p::id_of(&price["id"]),
            },
        )
        .await
}

/// A typed price's money as canonical text, in the stored shape: `(price, min_fee)`.
fn typed_money(price: &ImmutablePrice) -> (Value, Option<String>) {
    let text = |d: &Decimal| bss_money::canonical_decimal(*d);
    let tiers = |tiers: &[bss_pricing_sdk::read::Tier]| {
        tiers
            .iter()
            .map(|t| json!({"up_to": t.up_to.as_ref().map(text), "rate": text(&t.rate)}))
            .collect::<Vec<_>>()
    };
    let model = match &price.model {
        PriceModel::Flat { amount } => json!({"amount": text(amount)}),
        PriceModel::PerUnit { unit_amount } => json!({"rate": text(unit_amount)}),
        PriceModel::Package {
            package_size,
            package_price,
        } => json!({"package_size": text(package_size), "package_price": text(package_price)}),
        PriceModel::Volume { tiers: t } | PriceModel::Graduated { tiers: t } => {
            json!({"tiers": tiers(t)})
        }
    };
    (model, price.minimum_fee.as_ref().map(text))
}

/// A raw connection to the fixture's database, past every repository.
async fn raw(f: &p::Fixture) -> sea_orm::DatabaseConnection {
    Database::connect(&*f.dsn).await.unwrap()
}

/// Rows of every table a refused write could change: books, prices, approval units and their
/// votes, idempotency results, audit records and outbox events.
async fn footprint(f: &p::Fixture) -> Vec<i64> {
    let db = raw(f).await;
    let mut counts = Vec::new();
    for table in [
        "pricing_price_book",
        "pricing_price",
        "pricing_approval_unit",
        "pricing_approval_decision",
        "pricing_idempotency",
        "pricing_audit",
        "bss_pricing_outbox_body",
    ] {
        let row = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                format!("SELECT COUNT(*) AS n FROM {table}"),
            ))
            .await
            .unwrap()
            .unwrap();
        counts.push(row.try_get::<i64>("", "n").unwrap());
    }
    counts
}

/// The idempotency results stored under `key`, answered or claimed.
async fn results_under(f: &p::Fixture, key: &str) -> i64 {
    let db = raw(f).await;
    db.query_one_raw(Statement::from_string(
        db.get_database_backend(),
        format!("SELECT COUNT(*) AS n FROM pricing_idempotency WHERE client_key = '{key}'"),
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get::<i64>("", "n")
    .unwrap()
}

/// A usage entry of `book` for `sku` in `model`, written directly with its rating policy.
async fn usage_entry(f: &p::Fixture, book: Uuid, sku: Uuid, model: &str) -> Uuid {
    let content = serde_json::from_value(p::entry_support::policy_support::input()).unwrap();
    let policy = usage_policy_repo::intern(
        &f.db.conn().unwrap(),
        &p::scope(f),
        f.ctx.subject_tenant_id(),
        f.ctx.subject_id(),
        &content,
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    p::entry_with_policy(f, book, sku, "usage", None, model, Some(policy)).await
}

/// The entries of one book, one per model, each of its own SKU: one SKU's prices share a model.
struct Entries {
    per_unit: Uuid,
    flat: Uuid,
    package: Uuid,
    /// A second package entry: a usage chain keeps its package size.
    other_package: Uuid,
    volume: Uuid,
    graduated: Uuid,
}
impl Entries {
    async fn of(f: &p::Fixture, catalog: &p::Catalog, book: Uuid) -> Self {
        let usage = || catalog.sku(SkuType::Usage);
        let recurring = catalog.sku(SkuType::Recurring);
        Self {
            per_unit: usage_entry(f, book, usage(), "per_unit").await,
            flat: p::entry_in(f, book, recurring, "recurring", Some("month"), "flat").await,
            package: usage_entry(f, book, usage(), "package").await,
            other_package: usage_entry(f, book, usage(), "package").await,
            volume: usage_entry(f, book, usage(), "volume").await,
            graduated: usage_entry(f, book, usage(), "graduated").await,
        }
    }
}

/// One refused draft: its entry, price and `min_fee`, the code it is refused with, and a fragment
/// of the refusal's text.
type Refusal = (
    Uuid,
    Value,
    Option<&'static str>,
    &'static str,
    &'static str,
);

/// Rows 4 to 8 of the boundary table: one refused draft per cause and field.
#[expect(
    clippy::too_many_lines,
    reason = "one row per refused cause and field: a table, not logic"
)]
fn refusals(e: &Entries) -> Vec<Refusal> {
    let tiers = |up_to: Value, rate: Value| json!({"tiers": [{"up_to": up_to, "rate": "1"}, {"up_to": null, "rate": rate}]});
    vec![
        // A JSON number in any price or quantity field: send a string.
        (
            e.per_unit,
            json!({"rate": 0.047}),
            None,
            "AMOUNT_INVALID",
            "JSON strings",
        ),
        (
            e.flat,
            json!({"amount": 30}),
            None,
            "AMOUNT_INVALID",
            "JSON strings",
        ),
        (
            e.package,
            json!({"package_size": 10, "package_price": "5"}),
            None,
            "AMOUNT_INVALID",
            "JSON strings",
        ),
        (
            e.package,
            json!({"package_size": "10", "package_price": 5}),
            None,
            "AMOUNT_INVALID",
            "JSON strings",
        ),
        (
            e.volume,
            tiers(json!(10), json!("0.5")),
            None,
            "AMOUNT_INVALID",
            "JSON strings",
        ),
        (
            e.graduated,
            tiers(json!("10"), json!(0.5)),
            None,
            "AMOUNT_INVALID",
            "JSON strings",
        ),
        // One place past the rate bound.
        (
            e.per_unit,
            json!({"rate": "0.0000000000001"}),
            None,
            "AMOUNT_OUT_OF_RANGE",
            "12 fractional places",
        ),
        (
            e.flat,
            json!({"amount": "1.0000000000001"}),
            None,
            "AMOUNT_OUT_OF_RANGE",
            "12 fractional places",
        ),
        (
            e.package,
            json!({"package_size": "1", "package_price": "1.0000000000001"}),
            None,
            "AMOUNT_OUT_OF_RANGE",
            "12 fractional places",
        ),
        (
            e.graduated,
            tiers(json!("10"), json!("0.0000000000001")),
            None,
            "AMOUNT_OUT_OF_RANGE",
            "12 fractional places",
        ),
        // A 29th digit, or a 29th fractional place of a quantity or threshold.
        (
            e.per_unit,
            json!({"rate": "10000000000000000000000000000"}),
            None,
            "AMOUNT_OUT_OF_RANGE",
            "28 digits",
        ),
        (
            e.per_unit,
            json!({"rate": "99999999999999999.999999999999"}),
            None,
            "AMOUNT_OUT_OF_RANGE",
            "28 digits",
        ),
        (
            e.package,
            json!({"package_size": "10000000000000000000000000000", "package_price": "1"}),
            None,
            "AMOUNT_OUT_OF_RANGE",
            "28 digits",
        ),
        (
            e.package,
            json!({"package_size": "0.00000000000000000000000000001", "package_price": "1"}),
            None,
            "AMOUNT_OUT_OF_RANGE",
            "28 fractional places",
        ),
        (
            e.volume,
            tiers(json!("10000000000000000000000000000"), json!("1")),
            None,
            "AMOUNT_OUT_OF_RANGE",
            "28 digits",
        ),
        // Text that is not plain decimal text: exponent, sign, whitespace, separator, leading zero,
        // a missing digit.
        (
            e.per_unit,
            json!({"rate": "1e-3"}),
            None,
            "AMOUNT_INVALID",
            "plain decimal text",
        ),
        (
            e.per_unit,
            json!({"rate": "1E3"}),
            None,
            "AMOUNT_INVALID",
            "plain decimal text",
        ),
        (
            e.per_unit,
            json!({"rate": "+1"}),
            None,
            "AMOUNT_INVALID",
            "plain decimal text",
        ),
        (
            e.per_unit,
            json!({"rate": "-0"}),
            None,
            "AMOUNT_INVALID",
            "plain decimal text",
        ),
        (
            e.per_unit,
            json!({"rate": " 1"}),
            None,
            "AMOUNT_INVALID",
            "plain decimal text",
        ),
        (
            e.per_unit,
            json!({"rate": "1 "}),
            None,
            "AMOUNT_INVALID",
            "plain decimal text",
        ),
        (
            e.per_unit,
            json!({"rate": "1_000"}),
            None,
            "AMOUNT_INVALID",
            "plain decimal text",
        ),
        (
            e.per_unit,
            json!({"rate": "1,5"}),
            None,
            "AMOUNT_INVALID",
            "plain decimal text",
        ),
        (
            e.per_unit,
            json!({"rate": "01"}),
            None,
            "AMOUNT_INVALID",
            "plain decimal text",
        ),
        (
            e.per_unit,
            json!({"rate": "1."}),
            None,
            "AMOUNT_INVALID",
            "plain decimal text",
        ),
        (
            e.per_unit,
            json!({"rate": ".1"}),
            None,
            "AMOUNT_INVALID",
            "plain decimal text",
        ),
        (
            e.flat,
            json!({"amount": ""}),
            None,
            "AMOUNT_INVALID",
            "plain decimal text",
        ),
        (
            e.package,
            json!({"package_size": "1e2", "package_price": "1"}),
            None,
            "AMOUNT_INVALID",
            "plain decimal text",
        ),
        (
            e.volume,
            tiers(json!("010"), json!("1")),
            None,
            "AMOUNT_INVALID",
            "plain decimal text",
        ),
        // A sign is refused and named.
        (
            e.per_unit,
            json!({"rate": "-1"}),
            None,
            "AMOUNT_INVALID",
            "rate must not be negative",
        ),
        (
            e.package,
            json!({"package_size": "0", "package_price": "1"}),
            None,
            "AMOUNT_INVALID",
            "package_size must be above zero",
        ),
        // A minimum fee off the EUR increment is refused, never rounded.
        (
            e.per_unit,
            json!({"rate": "1"}),
            Some("12.341"),
            "MIN_FEE_INVALID",
            "a multiple of the increment",
        ),
        (
            e.per_unit,
            json!({"rate": "1"}),
            Some("-1"),
            "MIN_FEE_INVALID",
            "must not be negative",
        ),
        (
            e.per_unit,
            json!({"rate": "1"}),
            Some("1e1"),
            "MIN_FEE_INVALID",
            "plain decimal text",
        ),
        (
            e.per_unit,
            json!({"rate": "1"}),
            Some("10000000000000000000000000000"),
            "MIN_FEE_INVALID",
            "28 digits",
        ),
    ]
}

/// Every refused draft and book create, in order: `(case, status, code, the text says its cause)`,
/// and the answers each should give.
async fn refuse_all(
    f: &p::Fixture,
    e: &Entries,
) -> (
    Vec<(usize, u16, String, bool)>,
    Vec<(usize, u16, String, bool)>,
) {
    let mut answers = Vec::new();
    let mut expected = Vec::new();
    for (n, (entry, price, min_fee, code, says)) in refusals(e).into_iter().enumerate() {
        let (status, body) = create(
            f,
            &f.app,
            entry,
            draft(price, min_fee, "2031-01-01"),
            &format!("refused-{n}"),
        )
        .await;
        answers.push((n, status, code_of(&body), body.to_string().contains(says)));
        expected.push((n, 400, code.to_owned(), true));
    }
    // Unknown or malformed currencies at book create.
    for (n, currency) in ["ZZZ", "XTA", "eur", "EURO", "XAU"].into_iter().enumerate() {
        let (status, body, _) = f
            .call(
                "POST",
                "/price-books",
                json!({"code": format!("bad-{n}"), "name": "Bad", "currency": currency}),
                None,
                Some(&format!("bad-book-{n}")),
            )
            .await;
        answers.push((100 + n, status, code_of(&body), true));
        expected.push((100 + n, 400, "BOOK_CURRENCY_INVALID".to_owned(), true));
    }
    (answers, expected)
}

/// Rows 4 to 8 and 10 of the boundary table, and failure atomicity: every refused input answers
/// its designed code and cause, rounds nothing, and writes no book, price, approval unit,
/// idempotency result, audit record or outbox event. A key a refusal used is still free.
#[expect(
    clippy::cognitive_complexity,
    reason = "each refusal and its footprint check is one straight block"
)]
pub async fn refused_inputs_write_nothing(db: DBProvider<DbError>, dsn: p::entry_support::TestDsn) {
    let (f, catalog) = fixture(db, dsn).await;
    let book = p::book(&f, "eur").await;
    let e = Entries::of(&f, &catalog, book).await;
    let before = footprint(&f).await;
    let (answers, expected) = refuse_all(&f, &e).await;
    // A minimum fee sent as a JSON number is MIN_FEE_INVALID, told to send a string.
    let mut body = draft(json!({"rate": "1"}), None, "2031-01-01");
    body["min_fee"] = json!(12.34);
    let (status, body) = create(&f, &f.app, e.per_unit, body, "fee-number").await;
    assert_eq!(
        (
            status,
            code_of(&body),
            body.to_string().contains("JSON strings")
        ),
        (400, "MIN_FEE_INVALID".to_owned(), true),
        "a minimum fee sent as a JSON number is refused with its code"
    );
    assert_eq!(answers, expected);
    assert_eq!(
        footprint(&f).await,
        before,
        "a refused input writes no book, price, unit, idempotency result, audit record or event"
    );
    // The key a refusal used holds no completed result: the corrected request takes it.
    let (status, _) = create(
        &f,
        &f.app,
        e.per_unit,
        draft(json!({"rate": "1"}), Some("12.34"), "2031-01-01"),
        "refused-0",
    )
    .await;
    assert_eq!(status, 201, "a refused request leaves its key unused");
    // A PATCH with a JSON number is refused and leaves the draft as it was.
    let price = created(
        &f,
        &f.app,
        e.flat,
        draft(json!({"amount": "30"}), None, "2031-02-01"),
        "patched",
    )
    .await;
    let before = (listed(&f, e.flat, &price).await, footprint(&f).await);
    let (status, body, _) = f
        .call(
            "PATCH",
            &format!("/prices/{}", price["id"].as_str().unwrap()),
            json!({"price": {"amount": 31}}),
            Some(&format!("\"{}\"", price["version"])),
            None,
        )
        .await;
    assert_eq!(
        (status, code_of(&body)),
        (400, "AMOUNT_INVALID".to_owned()),
        "a JSON number in a PATCH is refused"
    );
    let (status, body, _) = f
        .call(
            "PATCH",
            &format!("/prices/{}", price["id"].as_str().unwrap()),
            json!({"min_fee": 5}),
            Some(&format!("\"{}\"", price["version"])),
            None,
        )
        .await;
    assert_eq!(
        (
            status,
            code_of(&body),
            body.to_string().contains("JSON strings")
        ),
        (400, "MIN_FEE_INVALID".to_owned(), true),
        "a minimum fee sent as a JSON number in a PATCH is refused with its code"
    );
    assert_eq!(
        (listed(&f, e.flat, &price).await, footprint(&f).await),
        before,
        "a refused PATCH writes nothing"
    );
}

/// One accepted draft: its entry, the price and `min_fee` sent, and the price and `min_fee` stored.
type Accepted = (
    Uuid,
    Value,
    Option<&'static str>,
    Value,
    Option<&'static str>,
);

/// Rows 1 to 3 and 9 of the boundary table: accepted values keep every digit, trailing zeros are
/// dropped once, and the largest and smallest values of each field read back exactly from the
/// door, the list, the stored row, the pinned read and the typed read.
#[expect(
    clippy::cognitive_complexity,
    reason = "each read of every accepted value is one straight block of asserts"
)]
pub async fn accepted_values_round_trip_exactly(
    db: DBProvider<DbError>,
    dsn: p::entry_support::TestDsn,
) {
    let (f, catalog) = fixture(db, dsn).await;
    let book = p::book(&f, "eur").await;
    quorum_one(&f).await;
    let e = Entries::of(&f, &catalog, book).await;
    // (entry, price sent, min_fee sent, price stored, min_fee stored)
    let cases: Vec<Accepted> = vec![
        // A rate finer than the cent is accepted unchanged.
        (
            e.per_unit,
            json!({"rate": "0.047"}),
            None,
            json!({"rate": "0.047"}),
            None,
        ),
        // Trailing zeros are dropped: one spelling of each value.
        (
            e.per_unit,
            json!({"rate": "0.10"}),
            Some("12.340"),
            json!({"rate": "0.1"}),
            Some("12.34"),
        ),
        (
            e.per_unit,
            json!({"rate": LARGEST_RATE}),
            Some(LARGEST_FEE_AT_2),
            json!({"rate": LARGEST_RATE}),
            Some(LARGEST_FEE_AT_2),
        ),
        (
            e.per_unit,
            json!({"rate": SMALLEST_RATE}),
            Some("0.01"),
            json!({"rate": SMALLEST_RATE}),
            Some("0.01"),
        ),
        (
            e.per_unit,
            json!({"rate": LARGEST_WHOLE}),
            Some("0.00"),
            json!({"rate": LARGEST_WHOLE}),
            Some("0"),
        ),
        (
            e.flat,
            json!({"amount": LARGEST_RATE}),
            None,
            json!({"amount": LARGEST_RATE}),
            None,
        ),
        (
            e.flat,
            json!({"amount": "0.00"}),
            None,
            json!({"amount": "0"}),
            None,
        ),
        (
            e.package,
            json!({"package_size": SMALLEST_QUANTITY, "package_price": LARGEST_RATE}),
            None,
            json!({"package_size": SMALLEST_QUANTITY, "package_price": LARGEST_RATE}),
            None,
        ),
        (
            e.other_package,
            json!({"package_size": LARGEST_WHOLE, "package_price": SMALLEST_RATE}),
            None,
            json!({"package_size": LARGEST_WHOLE, "package_price": SMALLEST_RATE}),
            None,
        ),
        (
            e.graduated,
            json!({"tiers": [
                {"up_to": SMALLEST_QUANTITY, "rate": SMALLEST_RATE},
                {"up_to": LARGEST_WHOLE, "rate": LARGEST_RATE},
                {"up_to": null, "rate": "0"}
            ]}),
            None,
            json!({"tiers": [
                {"up_to": SMALLEST_QUANTITY, "rate": SMALLEST_RATE},
                {"up_to": LARGEST_WHOLE, "rate": LARGEST_RATE},
                {"up_to": null, "rate": "0"}
            ]}),
            None,
        ),
        (
            e.volume,
            json!({"tiers": [{"up_to": "10.000", "rate": "1.50"}, {"up_to": null, "rate": "0.0470"}]}),
            Some("5.0"),
            json!({"tiers": [{"up_to": "10", "rate": "1.5"}, {"up_to": null, "rate": "0.047"}]}),
            Some("5"),
        ),
    ];
    let mut drafts = Vec::new();
    for (n, (entry, price, min_fee, want, want_fee)) in cases.into_iter().enumerate() {
        // Each price of a chain starts on its own day.
        let from = format!("2031-01-{:02}", n + 1);
        let row = created(
            &f,
            &f.app,
            entry,
            draft(price, min_fee, &from),
            &format!("ok-{n}"),
        )
        .await;
        drafts.push((entry, row, want, want_fee.map(str::to_owned)));
    }
    let answered: Vec<(Value, Option<String>)> = drafts
        .iter()
        .map(|(_, row, _, _)| {
            (
                row["price_json"].clone(),
                row["min_fee"].as_str().map(str::to_owned),
            )
        })
        .collect();
    let want: Vec<(Value, Option<String>)> = drafts
        .iter()
        .map(|(_, _, price, fee)| (price.clone(), fee.clone()))
        .collect();
    assert_eq!(answered, want, "the door answers canonical text");
    let mut stored_rows = Vec::new();
    let mut listed_rows = Vec::new();
    for (entry, row, _, _) in &drafts {
        stored_rows.push(stored(&f, row).await);
        let read = listed(&f, *entry, row).await;
        listed_rows.push((
            read["price_json"].clone(),
            read["min_fee"].as_str().map(str::to_owned),
        ));
    }
    assert_eq!(stored_rows, want, "storage holds canonical text");
    assert_eq!(listed_rows, want, "the price list reads the stored text");
    // Approved in one unit, every value reads back exactly from the pinned and the typed read.
    let (status, receipt, _) = f
        .call(
            "POST",
            &format!("/price-books/{book}/publish-changes"),
            json!({}),
            None,
            Some("publish"),
        )
        .await;
    assert_eq!(status, 201, "the book's drafts are submitted in one unit");
    let (status, _) = approve(&f, &f.app, &receipt["unit"], "approve").await;
    assert_eq!(status, 200, "the unit is approved");
    let mut pinned = Vec::new();
    let mut typed_reads = Vec::new();
    for (_, row, _, _) in &drafts {
        let (status, read, _) = f
            .call(
                "GET",
                &format!("/prices/{}", row["id"].as_str().unwrap()),
                json!({}),
                None,
                None,
            )
            .await;
        assert_eq!(status, 200, "the approved price is read");
        assert_eq!(
            (&read["currency"], &read["currency_scale"]),
            (&json!("EUR"), &json!(2)),
            "the pinned read carries the stored spec"
        );
        pinned.push((
            read["price"].clone(),
            read["min_fee"].as_str().map(str::to_owned),
        ));
        let price = typed(&f, f.state.clone(), row).await.unwrap();
        assert_eq!(
            (price.currency.code(), price.currency.scale()),
            ("EUR", 2),
            "the typed read carries the stored spec"
        );
        typed_reads.push(typed_money(&price));
    }
    assert_eq!(pinned, want, "the pinned read serves the exact stored text");
    assert_eq!(
        typed_reads, want,
        "the typed read serves the exact stored values"
    );
}

/// Rows 11, 12, 14 and 15 of the boundary table. `XTS` at 2, then a state over the same database
/// with `XTS` at 3: an approved price keeps its stored scale and value; a pending draft written
/// at 2 is refused at approval and left as it was. The same value sent with trailing zeros is the
/// same stored text and the same money digest; the same value at another stored scale is another
/// money digest.
#[expect(
    clippy::cognitive_complexity,
    reason = "the table change is one ordered scenario: before, change, after"
)]
pub async fn a_currency_table_change_keeps_history(
    db: DBProvider<DbError>,
    dsn: p::entry_support::TestDsn,
) {
    let (f, catalog) = fixture(db, dsn).await;
    let book = p::book_in(&f, "xts", "XTS").await;
    quorum_one(&f).await;
    let entry = usage_entry(&f, book, catalog.sku(SkuType::Usage), "per_unit").await;
    let first = approved(
        &f,
        entry,
        draft(json!({"rate": "0.10"}), Some("0.120"), "2031-01-01"),
        "first",
    )
    .await;
    let same = approved(
        &f,
        entry,
        draft(json!({"rate": "0.1000"}), Some("0.12"), "2032-01-01"),
        "same",
    )
    .await;
    assert_eq!(
        (stored(&f, &first).await, stored(&f, &same).await),
        (
            (json!({"rate": "0.1"}), Some("0.12".to_owned())),
            (json!({"rate": "0.1"}), Some("0.12".to_owned()))
        ),
        "one value, one stored text"
    );
    let first_read = typed(&f, f.state.clone(), &first).await.unwrap();
    let same_read = typed(&f, f.state.clone(), &same).await.unwrap();
    assert_eq!(
        first_read.money_digest, same_read.money_digest,
        "one value, one money digest"
    );
    let pending = created(
        &f,
        &f.app,
        entry,
        draft(json!({"rate": "0.1"}), None, "2033-01-01"),
        "pending",
    )
    .await;
    let unit = submit(&f, &f.app, &pending, "pending-submit").await;

    // The table changes: XTS at 3.
    let (state, app) = changed(&f, 3).await;
    let (status, read, _) = p::request(
        &app,
        &f.ctx,
        "GET",
        &format!("/prices/{}", first["id"].as_str().unwrap()),
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "the approved price is read");
    assert_eq!(
        (
            &read["currency"],
            &read["currency_scale"],
            &read["price"],
            &read["min_fee"]
        ),
        (
            &json!("XTS"),
            &json!(2),
            &json!({"rate": "0.1"}),
            &json!("0.12")
        ),
        "an approved price reads its stored scale and value"
    );
    let reread = typed(&f, state.clone(), &first).await.unwrap();
    assert_eq!(
        (reread.currency.scale(), reread.money_digest),
        (2, first_read.money_digest),
        "the typed read keeps the stored scale and money digest"
    );
    let before = (listed(&f, entry, &pending).await, footprint(&f).await);
    let (status, body) = approve(&f, &app, &unit, "pending-approve").await;
    assert_eq!(
        (status, code_of(&body)),
        (409, "CURRENCY_SCALE_CHANGED".to_owned()),
        "a draft written at the old scale is not approved"
    );
    assert_eq!(
        (listed(&f, entry, &pending).await, footprint(&f).await),
        before,
        "the refused approval writes no price, unit, vote, audit record, event or result"
    );
    assert_eq!(
        results_under(&f, "pending-approve").await,
        0,
        "the refused approval stores no result under its key"
    );
    // The same value at the new scale is other money: another money digest.
    let (status, _, _) = p::request(
        &app,
        &f.ctx,
        "POST",
        &format!("/approval-units/{}/withdraw", unit["id"].as_str().unwrap()),
        json!({}),
        None,
        Some("pending-withdraw"),
    )
    .await;
    assert_eq!(status, 200, "the stale unit is withdrawn");
    let later = created(
        &f,
        &app,
        entry,
        draft(json!({"rate": "0.1"}), Some("0.12"), "2034-01-01"),
        "later",
    )
    .await;
    assert_eq!(
        later["currency_scale"], 3,
        "new content takes the table's scale"
    );
    let unit = submit(&f, &app, &later, "later-submit").await;
    let (status, _) = approve(&f, &app, &unit, "later-approve").await;
    assert_eq!(status, 200, "the price at the new scale is approved");
    let later_read = typed(&f, state, &later).await.unwrap();
    assert_eq!(
        (typed_money(&later_read), later_read.currency.scale()),
        (typed_money(&first_read), 3),
        "the same money at another scale"
    );
    assert_ne!(
        later_read.money_digest, first_read.money_digest,
        "another stored scale is another money digest"
    );
}

/// Rows 13 and 16 of the boundary table. A resolve binding carries the price's currency and its
/// scale as a JSON integer. A receipt issued under `XTS` at 2 replays, checks and holds unchanged
/// after the table says 3: the same snapshot and the same digests.
pub async fn a_receipt_keeps_its_snapshot(db: DBProvider<DbError>, dsn: p::entry_support::TestDsn) {
    let (fixture, catalog) = fixture(db, dsn).await;
    let mut f = AcceptanceFixture::in_currency(fixture, catalog, "XTS").await;
    f.query.start_at += time::Duration::hours(10);
    f.clock.advance(time::Duration::hours(10));
    let resolve = format!(
        "/resolve?plan_revision_id={}&date={}",
        f.query.plan_revision_id,
        f.query.start_at.date()
    );
    let binding = |body: &Value| body["items"][0]["chains"][0]["binding"].clone();
    let (status, body, _) = f.fixture.call("GET", &resolve, json!({}), None, None).await;
    assert_eq!(status, 200, "the revision resolves");
    let bound = binding(&body);
    assert_eq!(
        (
            &bound["currency"],
            &bound["currency_scale"],
            &bound["price"]
        ),
        (&json!("XTS"), &json!(2), &json!({"rate": "10"})),
        "the binding carries the price's currency and scale"
    );
    assert!(
        bound["currency_scale"].is_u64(),
        "the scale is a JSON integer"
    );
    let issued = f
        .sellability
        .check(&f.ctx, f.query.clone(), f.meta.clone())
        .await
        .unwrap();
    let price = issued.bindings[0].price.clone();
    assert_eq!(
        (price.currency.code(), price.currency.scale()),
        ("XTS", 2),
        "the receipt snapshots the stored spec"
    );

    // The table changes: XTS at 3.
    let (state, app) = changed(&f.fixture, 3).await;
    let (status, body, _) = p::request(&app, &f.ctx, "GET", &resolve, json!({}), None, None).await;
    assert_eq!(status, 200, "the revision resolves");
    assert_eq!(binding(&body), bound, "the binding keeps the stored spec");
    let enforcer = Arc::new(p::entry_support::enforcer_for(f.ctx.subject_tenant_id()));
    let service = Arc::new(
        bss_pricing::infra::commercial_terms::CommercialTermsService::new(
            state,
            enforcer,
            f.clock.clone(),
            bss_pricing::config::SellerHoldPolicy::default(),
        ),
    );
    let sellability = bss_pricing::api::sellability::SellabilityProvider::new(service.clone());
    let acceptance = bss_pricing::api::pricing_acceptance::PricingAcceptanceProvider::new(service);
    assert_eq!(
        sellability
            .check(&f.ctx, f.query.clone(), f.meta.clone())
            .await
            .unwrap(),
        issued,
        "the acceptance replays as issued"
    );
    let q = AcceptanceFixture::fulfilment_query(&issued);
    assert!(
        sellability
            .check_fulfilment(&f.ctx, q.clone())
            .await
            .is_ok(),
        "the fulfilment check recomputes the money at the stored scale"
    );
    let held = acceptance
        .hold(
            &f.ctx,
            q,
            CommandMeta {
                idempotency_key: "held".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (&held.bindings, held.terms_digest),
        (&issued.bindings, issued.terms_digest),
        "the hold freezes the issued snapshot and digests"
    );
    assert_eq!(
        held.bindings[0].price.money_digest, price.money_digest,
        "the money digest is the issued one"
    );
}

/// Row 17 of the boundary table. A stored decimal out of the contract (a minimum fee off its stored
/// scale's increment included), or a price whose stored currency is not its book's, is a corrupt
/// row: every read refuses it and none rewrites it. The database itself refuses a non-canonical
/// fee and a scale out of range.
#[expect(
    clippy::cognitive_complexity,
    reason = "each corruption is one straight block: corrupt, read, check, repair"
)]
pub async fn a_corrupt_row_is_refused_and_never_repaired(
    db: DBProvider<DbError>,
    dsn: p::entry_support::TestDsn,
) {
    let (f, catalog) = fixture(db, dsn).await;
    let book = p::book(&f, "eur").await;
    quorum_one(&f).await;
    let entry = usage_entry(&f, book, catalog.sku(SkuType::Usage), "per_unit").await;
    let price = approved(
        &f,
        entry,
        draft(json!({"rate": "0.5"}), Some("1.5"), "2031-01-01"),
        "price",
    )
    .await;
    let tenant = f.ctx.subject_tenant_id();
    let reads = || async {
        let (pinned, _, _) = f
            .call(
                "GET",
                &format!("/prices/{}", price["id"].as_str().unwrap()),
                json!({}),
                None,
                None,
            )
            .await;
        let (list, _, _) = f
            .call("GET", &prices_path(entry), json!({}), None, None)
            .await;
        let typed_read = match typed(&f, f.state.clone(), &price).await {
            Ok(_) => 200,
            Err(e) => e.status_code(),
        };
        (pinned, list, typed_read)
    };
    assert_eq!(reads().await, (200, 200, 200), "the stored price is served");
    let db = raw(&f).await;
    let backend = db.get_database_backend();
    let repair = r#"UPDATE pricing_price SET price_json = '{"rate":"0.5"}', currency = 'EUR', min_fee = '1.5'"#;
    let fee = |text: &str| Some(text.to_owned());
    for (corruption, money, cause) in [
        (
            r#"UPDATE pricing_price SET price_json = '{"rate":"0.50"}'"#,
            (json!({"rate": "0.50"}), fee("1.5")),
            "price_json: rate is not canonical decimal text",
        ),
        (
            r#"UPDATE pricing_price SET price_json = '{"rate":0.5}'"#,
            (json!({"rate": 0.5}), fee("1.5")),
            "price_json",
        ),
        (
            r#"UPDATE pricing_price SET price_json = '{"rate":"0.0000000000001"}'"#,
            (json!({"rate": "0.0000000000001"}), fee("1.5")),
            "price_json",
        ),
        (
            "UPDATE pricing_price SET min_fee = '1.555'",
            (json!({"rate": "0.5"}), fee("1.555")),
            "min_fee: 1.555 is off the increment of currency_scale 2",
        ),
        (
            "UPDATE pricing_price SET currency = 'USD'",
            (json!({"rate": "0.5"}), fee("1.5")),
            "currency: USD is not its book's EUR",
        ),
    ] {
        db.execute_raw(Statement::from_string(backend, corruption))
            .await
            .unwrap();
        assert_eq!(
            reads().await,
            (500, 500, 500),
            "no read serves the corrupt row"
        );
        let conn = f.db.conn().unwrap();
        let scope = AccessScope::for_tenant(tenant);
        let row = price_repo::find(&conn, &scope, tenant, p::id_of(&price["id"]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (row.price_json.clone(), row.min_fee.clone()),
            money,
            "no read rewrote the row"
        );
        let stored_entry = price_book_entry_repo::find(&conn, &scope, tenant, entry)
            .await
            .unwrap()
            .unwrap();
        let model = price_book_entry_repo::model_of(&stored_entry).unwrap();
        let Err(RepoError::CorruptRow(what)) = price_repo::in_book(&row, model, "EUR") else {
            panic!("the shared reader refuses the corrupt row");
        };
        assert!(
            what.contains(cause),
            "the integrity error names the column and why"
        );
        db.execute_raw(Statement::from_string(backend, repair))
            .await
            .unwrap();
        assert_eq!(
            reads().await,
            (200, 200, 200),
            "the repaired row is served again"
        );
    }
    for refused in [
        "UPDATE pricing_price SET min_fee = '1.50'",
        "UPDATE pricing_price SET min_fee = '01.5'",
        "UPDATE pricing_price SET currency_scale = 29",
        "UPDATE pricing_price SET currency = 'eur'",
    ] {
        assert!(
            db.execute_raw(Statement::from_string(backend, refused))
                .await
                .is_err(),
            "the database refuses a value out of its column's contract"
        );
    }
    assert_eq!(
        stored(&f, &price).await,
        (json!({"rate": "0.5"}), Some("1.5".to_owned())),
        "the row is as it was approved"
    );
}
