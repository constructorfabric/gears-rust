//! D-516: a book named by id also carries `{ id, code, name, currency }`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
use bss_pricing::infra::storage::repo::{price_book_entry_repo, price_repo};
use bss_products_sdk::models::SkuType;
use plan_support::{Fixture, book, id_of, item, plan, policy_entry as entry, scope, setup};
use serde_json::{Value, json};
use time::Date;
use uuid::Uuid;

async fn get(f: &Fixture, path: &str) -> Value {
    let (s, b, _) = f.call("GET", path, json!({}), None, None).await;
    assert_eq!(s, 200, "{path}: {b}");
    b
}
async fn policy(f: &Fixture, quorum: u32) {
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let (s, b, _) = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"kind":"plan_revision","quorum":quorum}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
}
async fn approved(f: &Fixture, entry: Uuid) {
    let tenant = f.ctx.subject_tenant_id();
    let conn = f.db.conn().unwrap();
    let e = price_book_entry_repo::find(&conn, &scope(f), tenant, entry)
        .await
        .unwrap()
        .unwrap();
    let mut p = plan_support::entry_support::price(&e);
    p.state = "approved".into();
    p.effective_from = Date::from_calendar_date(2020, time::Month::January, 1).unwrap();
    price_repo::insert(&conn, &scope(f), p).await.unwrap();
}
async fn submit(f: &Fixture, revision: Uuid, key: &str) -> Value {
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/plan-revisions/{revision}/submit"),
            json!({}),
            None,
            Some(key),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    b
}
fn book_identity(id: Uuid, code: &str) -> Value {
    json!({"id": id, "code": code, "name": code, "currency": "EUR"})
}

/// Each revision header carries `book` beside `book_id`, including a header that is not current.
/// A plan revision unit's snapshot does the same beside `after.book_id`, on the receipt, the card
/// and the list.
#[tokio::test]
async fn a_revision_header_and_its_unit_name_the_book_beside_book_id() {
    let (f, catalog) = setup().await;
    let alpha = book(&f, "alpha").await;
    let beta = book(&f, "beta").await;
    let (created, rev1) = plan(&f, "heads", alpha).await;
    let plan_id = id_of(&created["id"]);
    assert_eq!(created["revisions"][0]["book_id"], alpha.to_string());
    assert_eq!(
        created["revisions"][0]["book"],
        book_identity(alpha, "alpha")
    );
    let sku = catalog.sku(SkuType::Usage);
    let priced = entry(&f, alpha, sku, "usage", None).await;
    approved(&f, priced).await;
    item(&f, rev1, sku, Some(priced), "paid").await;
    policy(&f, 0).await;
    let receipt = submit(&f, rev1, "heads-rev1").await;
    let after = &receipt["unit"]["snapshot"]["after"];
    assert_eq!(after["book_id"], alpha.to_string());
    assert_eq!(after["book"], book_identity(alpha, "alpha"));
    assert!(receipt["unit"]["snapshot"]["before"].is_null());
    let (s, copied, _) = f
        .call(
            "POST",
            &format!("/plans/{plan_id}/revisions"),
            json!({}),
            None,
            Some("heads-copy"),
        )
        .await;
    assert_eq!(s, 201, "{copied}");
    let rev2 = id_of(&copied["id"]);
    let path = format!("/plan-revisions/{rev2}");
    let (_, _, tag) = f.call("GET", &path, json!({}), None, None).await;
    let (s, patched, _) = f
        .call("PATCH", &path, json!({"book_id": beta}), Some(&tag), None)
        .await;
    assert_eq!(s, 200, "{patched}");
    let listed = get(&f, &format!("/plans/{plan_id}")).await;
    let header = |id: Uuid| {
        listed["revisions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id.to_string())
            .unwrap()
            .clone()
    };
    assert_eq!(header(rev1)["book"], book_identity(alpha, "alpha"));
    assert_eq!(header(rev1)["book_id"], alpha.to_string());
    assert_eq!(header(rev2)["book"], book_identity(beta, "beta"));
    assert_eq!(header(rev2)["book_id"], beta.to_string());
    let card = get(
        &f,
        &format!(
            "/approval-units/{}",
            receipt["unit"]["id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(
        card["snapshot"]["after"]["book"],
        book_identity(alpha, "alpha")
    );
    let page = f.all_units("kind=plan_revision").await;
    let listed_unit = page
        .iter()
        .find(|u| u["id"] == receipt["unit"]["id"])
        .unwrap();
    assert_eq!(
        listed_unit["snapshot"]["after"]["book"],
        book_identity(alpha, "alpha")
    );
}

/// A prices unit's snapshot names its book beside `book_id`, on the receipt and the card.
#[tokio::test]
async fn a_prices_snapshot_names_its_book_beside_book_id() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    let priced = entry(&f, eur, sku, "usage", None).await;
    let (s, drafted, _) = f
        .call(
            "POST",
            &format!("/price-book-entries/{priced}/prices"),
            json!({"price":{"rate":"0.10"},"eligibility":"all","effective_from":"2031-03-01"}),
            None,
            Some("draft-book"),
        )
        .await;
    assert_eq!(s, 201, "{drafted}");
    let price = drafted["items"][0]["id"].as_str().unwrap();
    let (s, receipt, _) = f
        .call(
            "POST",
            &format!("/prices/{price}/submit"),
            json!({}),
            None,
            Some("submit-book"),
        )
        .await;
    assert_eq!(s, 201, "{receipt}");
    let snapshot = &receipt["unit"]["snapshot"];
    assert_eq!(snapshot["book_id"], eur.to_string());
    assert_eq!(snapshot["book"], book_identity(eur, "eur"));
    let card = get(
        &f,
        &format!(
            "/approval-units/{}",
            receipt["unit"]["id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(card["snapshot"]["book"], book_identity(eur, "eur"));
}
