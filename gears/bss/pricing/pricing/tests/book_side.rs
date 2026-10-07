//! The book side of plans (run 3.4, Task 3.4.4): an entry a plan item names — in a revision of
//! any state — cannot be deleted (409 `ENTRY_IN_USE`, D-408), and a `prices` unit shows the plan
//! revisions whose items name its entries, on its stored snapshot and on every live read (the
//! card, the queue and the publish-changes listing), with its entry SKUs' current descriptors kept
//! outside the fingerprinted `after`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
use bss_products_sdk::models::SkuType;
use plan_support::{
    Fixture, book, id_of, item, items, lock, plan, policy_entry as entry, publish, setup, text,
};
use serde_json::{Value, json};
use uuid::Uuid;

async fn delete_entry(f: &Fixture, entry: Uuid) -> (u16, Value) {
    let (s, b, _) = f
        .call(
            "DELETE",
            &format!("/price-book-entries/{entry}"),
            json!({}),
            None,
            None,
        )
        .await;
    (s, b)
}
async fn in_use(f: &Fixture, entry: Uuid, why: &str) {
    let (s, b) = delete_entry(f, entry).await;
    assert_eq!(s, 409, "{why}: {b}");
    assert!(text(&b).contains("ENTRY_IN_USE"), "{why}: {b}");
}

#[tokio::test]
async fn an_entry_a_plan_item_names_is_in_use_whatever_the_revisions_state() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    let named = entry(&f, eur, sku, "usage", None).await;
    let (pro, rev1) = plan(&f, "pro", eur).await;
    let plan_id = id_of(&pro["id"]);
    item(&f, rev1, sku, Some(named), "paid").await;
    in_use(&f, named, "a draft revision's item").await;
    publish(&f, plan_id, rev1).await;
    in_use(&f, named, "a published revision's item").await;
    let (s, copy, _) = f
        .call(
            "POST",
            &format!("/plans/{plan_id}/revisions"),
            json!({}),
            None,
            Some("copy"),
        )
        .await;
    assert_eq!(s, 201, "{copy}");
    let rev2 = id_of(&copy["id"]);
    let copied = items(&f, rev2).await.remove(0);
    let (s, b, _) = f
        .call(
            "DELETE",
            &format!("/plan-items/{}", copied.id),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 204, "{b}");
    publish(&f, plan_id, rev2).await;
    in_use(&f, named, "a superseded revision's item (D-414)").await;
    let other_sku = catalog.sku(SkuType::Usage);
    let other = entry(&f, eur, other_sku, "usage", None).await;
    let (_, basic_rev) = plan(&f, "basic", eur).await;
    item(&f, basic_rev, other_sku, Some(other), "paid").await;
    lock(&f, basic_rev).await;
    in_use(&f, other, "a pending revision's item").await;
    let free = entry(&f, eur, catalog.sku(SkuType::Usage), "usage", None).await;
    let (s, b) = delete_entry(&f, free).await;
    assert_eq!(s, 204, "an entry no item names is deleted: {b}");
}

struct Unit {
    snapshot: Value,
    id: Value,
}
/// A prices unit over one draft of `entry`, submitted under quorum 1.
async fn prices_unit(f: &Fixture, entry: Uuid, key: &str, from: &str) -> Unit {
    let (s, drafted, _) = f
        .call(
            "POST",
            &format!("/price-book-entries/{entry}/prices"),
            json!({"price":{"rate":"0.10"},"eligibility":"all","effective_from":from}),
            None,
            Some(&format!("draft-{key}")),
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
            Some(&format!("submit-{key}")),
        )
        .await;
    assert_eq!(s, 201, "{receipt}");
    Unit {
        snapshot: receipt["unit"]["snapshot"].clone(),
        id: receipt["unit"]["id"].clone(),
    }
}
async fn card(f: &Fixture, id: Value) -> Value {
    let path = format!("/approval-units/{}", id.as_str().unwrap());
    let (s, b, _) = f.call("GET", &path, json!({}), None, None).await;
    assert_eq!(s, 200, "{b}");
    b
}
fn plan_row(plan: &Value, revision: Uuid, rev_no: i32, state: &str) -> Value {
    json!({
        "plan_id": plan["id"],
        "code": plan["code"],
        "revision_id": revision,
        "rev_no": rev_no,
        "state": state,
    })
}

#[tokio::test]
async fn a_prices_unit_names_the_plan_revisions_reading_its_entries_on_every_read() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    catalog.describe(sku, "4000");
    let e = entry(&f, eur, sku, "usage", None).await;
    let (basic, basic_rev) = plan(&f, "basic", eur).await;
    item(&f, basic_rev, sku, Some(e), "paid").await;
    publish(&f, id_of(&basic["id"]), basic_rev).await;
    let (pro, pro_rev) = plan(&f, "pro", eur).await;
    let pro_item = item(&f, pro_rev, sku, Some(e), "paid").await;
    // A plan on another entry of the same book is not impact.
    let (_, other_rev) = plan(&f, "other", eur).await;
    let other_sku = catalog.sku(SkuType::Usage);
    item(
        &f,
        other_rev,
        other_sku,
        Some(entry(&f, eur, other_sku, "usage", None).await),
        "paid",
    )
    .await;
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let (s, b, _) = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"kind":"prices","quorum":1}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let plans = json!([
        plan_row(&basic, basic_rev, 1, "published"),
        plan_row(&pro, pro_rev, 1, "draft"),
    ]);
    let unavailable = "unavailable until the Subscriptions integration";
    let impact = json!({"prices":1,"entries":1,"plans":plans,"subscriptions":unavailable});
    let unit = prices_unit(&f, e, "one", "2031-03-01").await;
    assert_eq!(unit.snapshot["impact"], impact, "{}", unit.snapshot);
    assert_eq!(
        unit.snapshot["descriptors"],
        json!([{"sku_id":sku,"gl_code":"4000","tax_category":null,"invoice_line_template":null,"billing_timing":null}]),
        "each entry SKU's current descriptors, beside after: {}",
        unit.snapshot
    );
    assert!(
        unit.snapshot["prices"][0]["after"]
            .get("descriptors")
            .is_none(),
        "{}",
        unit.snapshot
    );
    assert_eq!(card(&f, unit.id.clone()).await["impact"], impact);
    assert_eq!(f.all_units("kind=prices").await[0]["impact"], impact);
    let (s, drafted, _) = f
        .call(
            "POST",
            &format!("/price-book-entries/{e}/prices"),
            json!({"price":{"rate":"0.11"},"eligibility":"all","effective_from":"2031-06-01"}),
            None,
            Some("listed"),
        )
        .await;
    assert_eq!(s, 201, "{drafted}");
    let (s, listing, _) = f
        .call(
            "GET",
            &format!("/price-books/{eur}/publish-changes"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{listing}");
    assert_eq!(listing["impact"], impact, "the publish-changes listing");
    // The card is live: once the draft revision no longer names the entry, it drops out, while
    // the stored snapshot keeps what the reviewer was shown.
    let (s, b, _) = f
        .call(
            "DELETE",
            &format!("/plan-items/{}", pro_item.id),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 204, "{b}");
    let live = card(&f, unit.id.clone()).await;
    assert_eq!(
        live["impact"]["plans"],
        json!([plan_row(&basic, basic_rev, 1, "published")])
    );
    assert_eq!(live["snapshot"]["impact"], impact);
}

#[tokio::test]
async fn a_descriptor_change_never_refreshes_a_pending_prices_unit() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    catalog.describe(sku, "4000");
    let e = entry(&f, eur, sku, "usage", None).await;
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let (s, b, _) = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"kind":"prices","quorum":1}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let unit = prices_unit(&f, e, "one", "2031-03-01").await;
    catalog.describe(sku, "4100");
    let (s, b, _) = f
        .call_as(
            &f.user(),
            "POST",
            &format!("/approval-units/{}/approve", unit.id.as_str().unwrap()),
            json!({"generation":1}),
            None,
            Some("approve"),
        )
        .await;
    assert_eq!(s, 200, "a GL change is not content (D-408): {b}");
    assert_eq!(b["outcome"], "applied");
    assert_eq!(b["unit"]["generation"], 1);
    assert_eq!(
        b["unit"]["snapshot"]["descriptors"][0]["gl_code"], "4000",
        "the snapshot keeps what the reviewer was shown"
    );
}
