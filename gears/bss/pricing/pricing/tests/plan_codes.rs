//! D-469 (owner, 2026-09-30, ask 39): a new plan's code follows a declared rule,
//! `^[A-Z0-9][A-Z0-9_-]{0,31}$`, on `POST /plans` and `POST /plans/{id}/clone`, judged as sent: no
//! trim and no case folding. The length cap (`FIELD_TOO_LONG`, 64, D-457) comes first, then a blank
//! code (`PLAN_CODE_REQUIRED`), then the rule (`PLAN_CODE_INVALID`), then the rest in D-456's order.
//! A code stored before the rule is grandfathered: it keeps reading and is never judged again.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
use bss_pricing::infra::storage::{entity::plan as plan_entity, repo::plan_repo};
use bss_products_sdk::models::SkuType;
use plan_support::{Fixture, book, entry, holding, id_of, item, plan, publish, scope, setup, text};
use serde_json::{Value, json};
use uuid::Uuid;

async fn create(f: &Fixture, body: Value, key: &str) -> (u16, Value) {
    let (s, b, _) = f.call("POST", "/plans", body, None, Some(key)).await;
    (s, b)
}
async fn clone(f: &Fixture, source: Uuid, body: Value, key: &str) -> (u16, Value) {
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/plans/{source}/clone"),
            body,
            None,
            Some(key),
        )
        .await;
    (s, b)
}
async fn plan_count(f: &Fixture) -> usize {
    let (s, b, _) = f.call("GET", "/plans", json!({}), None, None).await;
    assert_eq!(s, 200, "{b}");
    b["items"].as_array().unwrap().len()
}
/// The codes a new plan may not take, each with the code it is refused with.
fn refused_codes() -> Vec<(String, &'static str)> {
    vec![
        ("pro".to_owned(), "PLAN_CODE_INVALID"),
        ("Pro".to_owned(), "PLAN_CODE_INVALID"),
        ("PRO ".to_owned(), "PLAN_CODE_INVALID"),
        (" PRO".to_owned(), "PLAN_CODE_INVALID"),
        ("PR O".to_owned(), "PLAN_CODE_INVALID"),
        ("-PRO".to_owned(), "PLAN_CODE_INVALID"),
        ("_PRO".to_owned(), "PLAN_CODE_INVALID"),
        ("PRO.1".to_owned(), "PLAN_CODE_INVALID"),
        ("\u{c9}T\u{c9}".to_owned(), "PLAN_CODE_INVALID"),
        ("A".repeat(33), "PLAN_CODE_INVALID"),
        ("A".repeat(64), "PLAN_CODE_INVALID"),
        (String::new(), "PLAN_CODE_REQUIRED"),
        ("   ".to_owned(), "PLAN_CODE_REQUIRED"),
        ("A".repeat(65), "FIELD_TOO_LONG"),
    ]
}

#[tokio::test]
async fn a_new_plan_code_follows_the_rule_as_sent() {
    let (f, _) = setup().await;
    let eur = book(&f, "eur").await;
    for (n, code) in [
        "PRO",
        "A",
        "9",
        "PRO-2",
        "PRO_2026",
        "2026-Q4_EU",
        &"Z".repeat(32),
    ]
    .into_iter()
    .enumerate()
    {
        let (s, b) = create(
            &f,
            json!({"code":code,"name":code,"book_id":eur}),
            &format!("ok-{n}"),
        )
        .await;
        assert_eq!(s, 201, "{code}: {b}");
        assert_eq!(b["code"], code, "stored as sent");
    }
    let before = plan_count(&f).await;
    for (n, (code, reason)) in refused_codes().into_iter().enumerate() {
        let (s, b) = create(
            &f,
            json!({"code":code,"name":"n","book_id":eur}),
            &format!("refused-{n}"),
        )
        .await;
        assert_eq!(s, 400, "{code:?}: {b}");
        assert!(text(&b).contains(reason), "{code:?} is {reason}: {b}");
        assert!(text(&b).contains("\"field\":\"code\""), "{code:?}: {b}");
    }
    assert_eq!(plan_count(&f).await, before, "nothing written");
}

/// The rule is one of the body's refusals (D-456's order): after the length cap and the blank
/// code, before the date, the book's 404 and the grant on the book.
#[tokio::test]
async fn the_rule_comes_after_the_cap_and_before_the_date_and_the_book() {
    let (f, _) = setup().await;
    let eur = book(&f, "eur").await;
    for (body, reason) in [
        (
            json!({"code":"pro","name":"n","book_id":Uuid::new_v4()}),
            "PLAN_CODE_INVALID",
        ),
        (
            json!({"code":"pro","name":"n","book_id":eur,"available_from":"2031-13-01"}),
            "PLAN_CODE_INVALID",
        ),
        (
            json!({"code":"PRO","name":"n","book_id":eur,"available_from":"2031-13-01"}),
            "DATE_INVALID",
        ),
        (
            json!({"code":"pro","name":"x".repeat(201),"book_id":eur}),
            "FIELD_TOO_LONG",
        ),
    ] {
        let (s, b) = create(&f, body.clone(), &Uuid::new_v4().to_string()).await;
        assert_eq!(s, 400, "{body}: {b}");
        assert!(text(&b).contains(reason), "{body}: {reason}: {b}");
    }
}

/// The phase 9 review's R62: the order above is observable only under a grant that does not
/// admit the book. A caller holding `plan:author` alone, without `price_book` read, gets the code
/// rule's 400 for a refused code, on the create and on the clone, and the book's 403 only once the
/// code passes.
#[tokio::test]
async fn the_rule_comes_before_the_grant_on_the_book() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let (created, rev1) = plan(&f, "pro", eur).await;
    let source = id_of(&created["id"]);
    let sku = catalog.sku(SkuType::Usage);
    let e = entry(&f, eur, sku, "usage", None).await;
    item(&f, rev1, sku, Some(e), "paid").await;
    publish(&f, source, rev1).await;
    let author = holding(&f, "plan:author");
    let before = plan_count(&f).await;
    let clone_path = format!("/plans/{source}/clone");
    for (path, body, status, reason) in [
        (
            "/plans",
            json!({"code":"lower","name":"n","book_id":eur}),
            400,
            "PLAN_CODE_INVALID",
        ),
        (
            "/plans",
            json!({"code":"UPPER","name":"n","book_id":eur}),
            403,
            "PRICE_BOOK_READ_REQUIRED",
        ),
        (
            clone_path.as_str(),
            json!({"code":"lower","name":"n"}),
            400,
            "PLAN_CODE_INVALID",
        ),
        (
            clone_path.as_str(),
            json!({"code":"UPPER","name":"n"}),
            403,
            "PRICE_BOOK_READ_REQUIRED",
        ),
    ] {
        let (s, b, _) = f
            .call_as(
                &author,
                "POST",
                path,
                body.clone(),
                None,
                Some(&Uuid::new_v4().to_string()),
            )
            .await;
        assert_eq!(s, status, "{path} {body}: {b}");
        assert!(text(&b).contains(reason), "{path} {body}: {reason}: {b}");
    }
    assert_eq!(plan_count(&f).await, before, "nothing written");
}

#[tokio::test]
async fn a_clone_takes_a_code_under_the_same_rule() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let (created, rev1) = plan(&f, "pro", eur).await;
    let source = id_of(&created["id"]);
    let sku = catalog.sku(SkuType::Usage);
    let e = entry(&f, eur, sku, "usage", None).await;
    item(&f, rev1, sku, Some(e), "paid").await;
    publish(&f, source, rev1).await;
    let before = plan_count(&f).await;
    for (n, (code, reason)) in refused_codes().into_iter().enumerate() {
        let (s, b) = clone(
            &f,
            source,
            json!({"code":code,"name":"n"}),
            &format!("refused-{n}"),
        )
        .await;
        assert_eq!(s, 400, "{code:?}: {b}");
        assert!(text(&b).contains(reason), "{code:?} is {reason}: {b}");
    }
    let (s, b) = clone(
        &f,
        Uuid::new_v4(),
        json!({"code":"pro-2","name":"n"}),
        "unknown",
    )
    .await;
    assert_eq!(s, 400, "the rule before the source's 404: {b}");
    assert!(text(&b).contains("PLAN_CODE_INVALID"), "{b}");
    assert_eq!(plan_count(&f).await, before, "nothing written");
    let (s, b) = clone(&f, source, json!({"code":"PRO-2","name":"Pro 2"}), "ok").await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(b["code"], "PRO-2");
}

/// A code stored before the rule (the deployed database holds e2e plans such as `plan-6dfd4733`) is never
/// judged again: the plan reads, is renamed and clones to a code that follows the rule; the
/// uniqueness stays exact, so the upper-case code is another plan's.
#[tokio::test]
async fn a_grandfathered_code_keeps_reading_and_clones_to_a_valid_code() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    // Written through the repositories, as a plan of before D-469: its code is `plan-<uuid>`.
    let (stored, rev1) = plan_support::entry_support::plan_on(&f.state, &f.ctx, eur).await;
    let legacy = stored.code.clone();
    assert!(legacy.starts_with("plan-"), "{legacy}");
    let sku = catalog.sku(SkuType::Usage);
    let e = entry(&f, eur, sku, "usage", None).await;
    item(&f, rev1.id, sku, Some(e), "paid").await;
    publish(&f, stored.id, rev1.id).await;
    let (s, b, tag) = f
        .call(
            "GET",
            &format!("/plans/{}", stored.id),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["code"], legacy);
    let (s, b, _) = f.call("GET", "/plans", json!({}), None, None).await;
    assert_eq!(s, 200, "{b}");
    assert!(
        b["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == legacy),
        "{b}"
    );
    let (s, b, _) = f
        .call(
            "PATCH",
            &format!("/plans/{}", stored.id),
            json!({"name":"Renamed"}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "the name-only PATCH judges no code: {b}");
    assert_eq!(b["code"], legacy);
    let (s, b) = clone(
        &f,
        stored.id,
        json!({"code":&legacy,"name":"Again"}),
        "again",
    )
    .await;
    assert_eq!(s, 400, "a new plan never takes a code off the rule: {b}");
    assert!(text(&b).contains("PLAN_CODE_INVALID"), "{b}");
    let (s, b) = clone(
        &f,
        stored.id,
        json!({"code":"PLAN-CLONE","name":"Clone"}),
        "clone",
    )
    .await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(b["code"], "PLAN-CLONE");
    // The uniqueness stays exact: the deployed database's `plan-6dfd4733` and a new `PLAN-6DFD4733` are two
    // plans.
    let now = time::OffsetDateTime::now_utc();
    plan_repo::insert(
        &f.db.conn().unwrap(),
        &scope(&f),
        plan_entity::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            code: "plan-6dfd4733".into(),
            name: "E2E leftover".into(),
            published_rev: None,
            version: 1,
            created_by: f.ctx.subject_id(),
            created_at: now,
            updated_at: now,
            work_revision_id: None,
            work_state: None,
            scheduled_revision_id: None,
            scheduled_from: None,
            published_revision_id: None,
            current_book_id: None,
            current_currency: None,
            last_activity_at: now,
        },
    )
    .await
    .unwrap();
    let (s, b) = create(
        &f,
        json!({"code":"PLAN-6DFD4733","name":"New","book_id":eur}),
        "upper",
    )
    .await;
    assert_eq!(s, 201, "exact uniqueness: {b}");
}
