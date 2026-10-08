//! D-514: a plan-revision unit pending before the policy digest moved finds its content changed
//! once. The first approve answers 400 `UNIT_STALE` and records no vote. The approve of that
//! generation applies. Modelled on `plan_items_legacy`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;

use bss_pricing::infra::storage::repo::{price_book_entry_repo, price_repo, usage_policy_repo};
use bss_pricing::infra::usage_policy_wire::{self, digest_text};
use bss_pricing_sdk::digest::policy_digest;
use bss_pricing_sdk::terms::{
    AggregationScope, Fold, PartialWindow, RatingWindow, Reset, UsageRatingPolicyInput,
};
use bss_products_sdk::models::SkuType;
use plan_support::{book, id_of, item, plan, policy_entry, scope, setup, text};
use serde_json::{Value, json};
use uuid::Uuid;

fn blob(id: Uuid) -> String {
    format!("X'{}'", id.simple())
}

#[tokio::test]
async fn a_pending_unit_whose_policy_digest_moved_refreshes_once_and_then_applies() {
    let (f, catalog) = setup().await;
    let book = book(&f, "RESHAPE").await;
    let sku = catalog.sku(SkuType::Usage);
    let entry = policy_entry(&f, book, sku, "usage", None).await;
    plan_support::raw(
        &f,
        &format!(
            "UPDATE pricing_price_book_entry SET usage_sku_version = 1 WHERE id = {}",
            blob(entry)
        ),
    )
    .await;
    let conn = f.db.conn().unwrap();
    let stored = price_book_entry_repo::find(&conn, &scope(&f), f.ctx.subject_tenant_id(), entry)
        .await
        .unwrap()
        .unwrap();
    let mut price = plan_support::entry_support::price(&stored);
    price.state = "approved".into();
    price.effective_from = time::macros::date!(2020 - 01 - 01);
    price.min_fee = None;
    price_repo::insert(&conn, &scope(&f), price).await.unwrap();
    let (_created, rev) = plan(&f, "reshape", book).await;
    item(&f, rev, sku, Some(entry), "paid").await;
    let (status, body, _) = f
        .call(
            "POST",
            &format!("/plan-revisions/{rev}/submit"),
            json!({}),
            None,
            Some("submit"),
        )
        .await;
    assert_eq!(status, 201, "{body}");
    let unit = id_of(&body["unit"]["id"]);
    let rules = body["unit"]["snapshot"]["meter_evidence"][0]["rules"].clone();
    assert_eq!(rules["fold"], "SUM", "{body}");
    assert!(rules.get("quantity_semantics").is_none(), "{rules}");
    assert_eq!(
        body["unit"]["snapshot"]["after"]["items"][0]["usage_sku_version"],
        1
    );
    let alt = UsageRatingPolicyInput {
        rating_window: RatingWindow::BillingCycle,
        aggregation_scope: AggregationScope::Resource,
        reset: Reset::RatingWindowStart,
        partial_window: PartialWindow::ActualQuantityFullThresholds,
        fold: Fold::Sum,
    };
    let interned = usage_policy_repo::intern(
        &conn,
        &scope(&f),
        f.ctx.subject_tenant_id(),
        f.ctx.subject_id(),
        &usage_policy_wire::UsageRatingPolicyInput::from(&alt),
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let digest = digest_text(policy_digest(&alt));
    assert_eq!(interned.digest, digest);
    plan_support::raw(
        &f,
        &format!(
            "UPDATE pricing_price_book_entry SET usage_policy_id = {}, usage_policy_version = {}, usage_policy_digest = '{digest}' WHERE id = {}",
            blob(interned.policy_id),
            interned.version,
            blob(entry)
        ),
    )
    .await;
    let (status, body) = vote(&f, unit, 1, "first").await;
    assert_eq!(status, 400, "{body}");
    assert!(text(&body).contains("UNIT_STALE"), "{body}");
    assert_eq!(body["context"]["generation"], 2);
    let card = get(&f, &format!("/approval-units/{unit}")).await;
    assert_eq!(card["decisions"], json!([]));
    let (status, body) = vote(&f, unit, 2, "second").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["outcome"], "applied");
}

async fn vote(f: &plan_support::Fixture, unit: Uuid, generation: i64, key: &str) -> (u16, Value) {
    let (status, body, _) = f
        .call_as(
            &f.user(),
            "POST",
            &format!("/approval-units/{unit}/approve"),
            json!({"generation": generation}),
            None,
            Some(key),
        )
        .await;
    (status, body)
}

async fn get(f: &plan_support::Fixture, path: &str) -> Value {
    let (status, body, _) = f.call("GET", path, json!({}), None, None).await;
    assert_eq!(status, 200, "{body}");
    body
}
