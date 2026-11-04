//! The category surface driven as a client drives it.
//!
//! Categories are the tree settings hang from, so their rules are the ones a
//! client meets first: a name is unique, a key is immutable because every
//! setting is filed through it, a category with settings in it cannot be
//! removed, and every mutation is conditional on the state the caller read.
//! Each of those is a status code, and a status code is only true once a
//! request has produced it.

use serde_json::{Value, json};
use uuid::Uuid;

use crate::domain::access::TenantAccess;
use crate::test_support::{BOOL, RestHarness};

const CATEGORIES: &str = "/settings-service/v1/categories";

fn create_body(key: &str, name: &str) -> Value {
    json!({ "key": key, "name": name, "sort_order": 0 })
}

/// Create a category and answer with its id and its state tag.
async fn create(h: &RestHarness, key: &str, name: &str) -> (String, String) {
    let answer = h
        .send(
            "POST",
            CATEGORIES,
            Some(create_body(key, name)),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 201, "{}", answer.body);
    let id = answer.body["id"].as_str().expect("an id").to_owned();
    let etag = answer.etag.expect("a state tag on the create");
    (id, etag)
}

/// Create a category with an explicit ordering weight.
async fn create_weighted(h: &RestHarness, key: &str, name: &str, sort_order: i32) {
    let answer = h
        .send(
            "POST",
            CATEGORIES,
            Some(json!({ "key": key, "name": name, "sort_order": sort_order })),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 201, "{}", answer.body);
}

/// Three categories whose weight disagrees with their name: `beta` and `zeta`
/// weigh 1, `alpha` weighs 2, so ordering by weight is not ordering by name and
/// a listing that dropped the weight would be caught.
async fn create_weighted_trio(h: &RestHarness) {
    create_weighted(h, "alpha", "Alpha", 2).await;
    create_weighted(h, "beta", "Beta", 1).await;
    create_weighted(h, "zeta", "Zeta", 1).await;
}

/// The keys of a listing page, with the harness's own `network` fixture left out.
fn weighted_keys(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("a page")
        .iter()
        .filter_map(|c| c["key"].as_str())
        .filter(|key| *key != "network")
        .map(str::to_owned)
        .collect()
}

/// Follow a listing two rows at a time and answer every page's keys, in order.
///
/// The harness's `network` fixture is kept, because it decides where the page
/// boundary falls: callers assert that the `beta`/`zeta` tie straddles it, so
/// the cursor has to carry the tiebreak. A continuation sends the cursor alone:
/// the cursor carries the order it was minted under, and the platform refuses
/// `$orderby` beside it.
async fn pages_across(h: &RestHarness, query: &str) -> Vec<Vec<String>> {
    let mut pages = Vec::new();
    let mut uri = format!("{CATEGORIES}?limit=2{query}");
    for page in 0..6 {
        let (status, body) = h.get(&uri, h.inner.tree.root).await;
        assert_eq!(status, 200, "page {page}: {body}");
        pages.push(
            body["items"]
                .as_array()
                .expect("a page")
                .iter()
                .filter_map(|c| c["key"].as_str())
                .map(str::to_owned)
                .collect(),
        );
        match body["page_info"]["next_cursor"].as_str() {
            Some(cursor) => uri = format!("{CATEGORIES}?limit=2&cursor={cursor}"),
            None => return pages,
        }
    }
    panic!("the listing did not end within six pages: {pages:?}");
}

/// Assert the pages are exactly `expected`, so a tie split by a boundary is visible.
fn assert_pages(pages: &[Vec<String>], expected: &[&[&str]], why: &str) {
    let want: Vec<Vec<String>> = expected
        .iter()
        .map(|p| p.iter().map(|k| (*k).to_owned()).collect())
        .collect();
    assert_eq!(pages, want.as_slice(), "{why}: {pages:?}");
}

// ── Create ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_create_answers_201_with_the_location_and_the_state_tag() {
    let h = RestHarness::new().await;
    let answer = h
        .send(
            "POST",
            CATEGORIES,
            Some(create_body("billing", "Invoices")),
            None,
            h.inner.tree.root,
        )
        .await;

    assert_eq!(answer.status, 201, "{}", answer.body);
    let id = answer.body["id"].as_str().expect("an id");
    assert_eq!(answer.body["key"], json!("billing"));
    assert_eq!(answer.body["name"], json!("Invoices"));
    assert_eq!(
        answer.location.as_deref(),
        Some(format!("{CATEGORIES}/{id}").as_str()),
        "the created resource names where it now lives"
    );
    // And the tag the next write must present, as an HTTP entity tag:
    // quoted and strong, per RFC 9110.
    let header = answer.etag.expect("the tag the next write must present");
    let inner = header
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("a quoted entity tag");
    assert!(
        !inner.is_empty() && !inner.contains('"'),
        "a bare tag inside the quotes: {header}"
    );
}

#[tokio::test]
async fn a_duplicate_name_is_refused_409_rather_than_failing() {
    // The uniqueness is the database's, and the refusal has to arrive as a
    // conflict a client can act on, not as an internal error.
    let h = RestHarness::new().await;
    create(&h, "billing", "Invoices").await;

    let answer = h
        .send(
            "POST",
            CATEGORIES,
            Some(create_body("other", "Invoices")),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 409, "{}", answer.body);
}

#[tokio::test]
async fn a_duplicate_key_is_refused_409() {
    let h = RestHarness::new().await;
    create(&h, "billing", "Invoices").await;

    let answer = h
        .send(
            "POST",
            CATEGORIES,
            Some(create_body("billing", "Something else")),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 409, "{}", answer.body);
}

#[tokio::test]
async fn a_name_outside_its_bounds_is_refused_400() {
    let h = RestHarness::new().await;
    for name in [String::new(), "x".repeat(257)] {
        let answer = h
            .send(
                "POST",
                CATEGORIES,
                Some(create_body("billing", &name)),
                None,
                h.inner.tree.root,
            )
            .await;
        assert_eq!(answer.status, 400, "name of {} chars", name.len());
    }
}

#[tokio::test]
async fn a_key_carrying_a_separator_is_refused_400() {
    // A key with `/` would suggest nesting the grammar cannot express: a
    // setting key has exactly one category segment.
    let h = RestHarness::new().await;
    let answer = h
        .send(
            "POST",
            CATEGORIES,
            Some(create_body("billing/invoices", "Invoices")),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 400, "{}", answer.body);
}

// ── Read ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_key_no_setting_key_could_be_composed_under_is_refused_400() {
    // An uppercase key would be stored and then refuse every declaration
    // filed under it; it is refused here instead, where it can be corrected.
    let h = RestHarness::new().await;
    for key in ["Network", "net-work"] {
        let answer = h
            .send(
                "POST",
                CATEGORIES,
                Some(create_body(key, "Network")),
                None,
                h.inner.tree.root,
            )
            .await;
        assert_eq!(answer.status, 400, "`{key}`: {}", answer.body);
        assert_eq!(
            answer.body["context"]["field_violations"][0]["reason"],
            json!(crate::field::CATEGORY_KEY_GRAMMAR),
            "{}",
            answer.body
        );
    }
}

#[tokio::test]
async fn a_single_read_carries_the_tag_a_mutation_must_present() {
    let h = RestHarness::new().await;
    let (id, created_tag) = create(&h, "billing", "Invoices").await;

    let answer = h
        .send(
            "GET",
            &format!("{CATEGORIES}/{id}"),
            None,
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200);
    assert_eq!(answer.body["key"], json!("billing"));
    assert_eq!(
        answer.etag.as_deref(),
        Some(created_tag.as_str()),
        "the tag a create handed back is the tag a read hands back"
    );
}

#[tokio::test]
async fn a_category_that_does_not_exist_is_reported_absent() {
    let h = RestHarness::new().await;
    let answer = h
        .send(
            "GET",
            &format!("{CATEGORIES}/{}", uuid::Uuid::new_v4()),
            None,
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 404);
}

#[tokio::test]
async fn the_listing_refuses_the_odata_option_it_does_not_implement() {
    let h = RestHarness::new().await;
    let answer = h
        .send(
            "GET",
            &format!("{CATEGORIES}?$select=key"),
            None,
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(
        answer.status, 400,
        "`$select` is refused rather than ignored: {}",
        answer.body
    );
}

#[tokio::test]
async fn the_listing_refuses_to_order_by_a_column_that_may_be_empty() {
    // `domain_affinity` is optional, and a page cursor has no spelling for an
    // empty sort value: the second page would be refused. It is not offered
    // for ordering; the name, the key and the weight are.
    let h = RestHarness::new().await;
    let answer = h
        .send(
            "GET",
            &format!("{CATEGORIES}?$orderby=domain_affinity%20asc"),
            None,
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 400, "{}", answer.body);
    assert_eq!(
        answer.body["context"]["field_violations"][0]["reason"],
        json!(crate::field::ODATA_UNSORTABLE_FIELD),
        "{}",
        answer.body
    );
    for field in ["name", "key", "sort_order"] {
        let answer = h
            .send(
                "GET",
                &format!("{CATEGORIES}?$orderby={field}%20asc"),
                None,
                None,
                h.inner.tree.root,
            )
            .await;
        assert_eq!(answer.status, 200, "{field}: {}", answer.body);
    }
}

#[tokio::test]
async fn the_listing_carries_the_categories_that_exist() {
    let h = RestHarness::new().await;
    create(&h, "billing", "Invoices").await;
    create(&h, "logging", "Logging").await;

    let (status, body) = h.get(CATEGORIES, h.inner.tree.root).await;
    assert_eq!(status, 200);
    let keys: Vec<&str> = body["items"]
        .as_array()
        .expect("a page")
        .iter()
        .filter_map(|c| c["key"].as_str())
        .collect();
    // `network` is the harness's own fixture category.
    assert!(keys.contains(&"billing"), "{keys:?}");
    assert!(keys.contains(&"logging"), "{keys:?}");
    assert!(keys.contains(&"network"), "{keys:?}");
}

#[tokio::test]
async fn the_listing_defaults_to_weight_then_name() {
    // Weight is the order an administrator curated; name only breaks a tie.
    // `alpha` sorts first by name but weighs most, so it must come last here.
    let h = RestHarness::new().await;
    create_weighted_trio(&h).await;

    let (status, body) = h.get(CATEGORIES, h.inner.tree.root).await;
    assert_eq!(status, 200, "{body}");
    let keys = weighted_keys(&body);
    assert_eq!(
        keys,
        vec!["beta", "zeta", "alpha"],
        "weight first, name within a weight: {keys:?}"
    );
}

#[tokio::test]
async fn ordering_by_sort_order_ascending_is_the_default_order() {
    let h = RestHarness::new().await;
    create_weighted_trio(&h).await;

    let (status, default) = h.get(CATEGORIES, h.inner.tree.root).await;
    assert_eq!(status, 200, "{default}");
    let (status, explicit) = h
        .get(
            &format!("{CATEGORIES}?$orderby=sort_order%20asc"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{explicit}");
    assert_eq!(
        weighted_keys(&explicit),
        weighted_keys(&default),
        "an explicit ascending weight is what the listing does anyway"
    );
    assert_eq!(weighted_keys(&explicit), vec!["beta", "zeta", "alpha"]);
}

#[tokio::test]
async fn ordering_by_sort_order_descending_reverses_the_weights_but_not_the_ties() {
    // Only the weight flips; equal weights stay in name order so the listing is
    // total and a cursor can resume inside a tie.
    let h = RestHarness::new().await;
    create_weighted_trio(&h).await;

    let (status, body) = h
        .get(
            &format!("{CATEGORIES}?$orderby=sort_order%20desc"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(weighted_keys(&body), vec!["alpha", "beta", "zeta"]);
}

#[tokio::test]
async fn a_descending_weight_order_holds_across_pages_when_a_tie_straddles_the_boundary() {
    // With `limit=2` the pages are `alpha, beta` and `zeta, network`: `beta` and
    // `zeta` weigh the same and the boundary falls between them, so the cursor
    // must resume inside the tie rather than skip or repeat it.
    let h = RestHarness::new().await;
    create_weighted_trio(&h).await;

    let pages = pages_across(&h, "&$orderby=sort_order%20desc").await;
    assert_pages(
        &pages,
        &[&["alpha", "beta"], &["zeta", "network"]],
        "the tie straddles the boundary and no row is lost or repeated",
    );
}

#[tokio::test]
async fn the_default_order_holds_across_pages_when_a_tie_straddles_the_boundary() {
    // The harness's `network` fixture weighs 0 and is served first, so the pages
    // are `network, beta` and `zeta, alpha`: the `beta`/`zeta` tie straddles the
    // boundary.
    let h = RestHarness::new().await;
    create_weighted_trio(&h).await;

    let pages = pages_across(&h, "").await;
    assert_pages(
        &pages,
        &[&["network", "beta"], &["zeta", "alpha"]],
        "the tie straddles the boundary and no row is lost or repeated",
    );
}

#[tokio::test]
async fn a_filter_on_sort_order_narrows_to_that_weight() {
    let h = RestHarness::new().await;
    create_weighted_trio(&h).await;

    let (status, body) = h
        .get(
            &format!("{CATEGORIES}?$filter=sort_order%20eq%201"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(weighted_keys(&body), vec!["beta", "zeta"]);
}

#[tokio::test]
async fn a_fractional_weight_in_a_filter_is_truncated_by_the_shared_odata_library() {
    // This is toolkit-db behaviour inherited through the declared I64 kind: the
    // library truncates `1.9` to `1`. It is recorded here so a change in the
    // library is noticed, not because the gear endorses the behaviour.
    let h = RestHarness::new().await;
    create_weighted_trio(&h).await;

    let (status, body) = h
        .get(
            &format!("{CATEGORIES}?$filter=sort_order%20eq%201.9"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        weighted_keys(&body),
        vec!["beta", "zeta"],
        "`eq 1.9` answers the weight-1 page, as `eq 1` does"
    );
}

#[tokio::test]
async fn naming_the_tiebreaker_already_is_accepted_without_breaking_the_cursor() {
    // `name` is already an order key here, so the service must not append it a
    // second time: a duplicated key makes the cursor ambiguous.
    let h = RestHarness::new().await;
    create_weighted_trio(&h).await;
    let query = "&$orderby=sort_order%20desc,name%20asc";

    let (status, body) = h
        .get(
            &format!("{CATEGORIES}?$orderby=sort_order%20desc,name%20asc"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(weighted_keys(&body), vec!["alpha", "beta", "zeta"]);

    let pages = pages_across(&h, query).await;
    assert_pages(
        &pages,
        &[&["alpha", "beta"], &["zeta", "network"]],
        "one boundary followed, the tie straddling it",
    );
}

// ── Update ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_update_needs_the_tag_and_takes_effect_with_it() {
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "billing", "Invoices").await;
    let uri = format!("{CATEGORIES}/{id}");
    let renamed = json!({ "name": "Invoices & Payments", "sort_order": 3 });

    let answer = h
        .send(
            "PATCH",
            &uri,
            Some(renamed.clone()),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(
        answer.status, 428,
        "a mutation with no tag never opted into the check: {}",
        answer.body
    );

    let answer = h
        .send(
            "PATCH",
            &uri,
            Some(renamed.clone()),
            Some("a tag from another time"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 412, "a stale tag: {}", answer.body);

    let answer = h
        .send("PATCH", &uri, Some(renamed), Some(&tag), h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.body["name"], json!("Invoices & Payments"));
    assert_eq!(answer.body["sort_order"], json!(3));
    assert_ne!(
        answer.etag.as_deref(),
        Some(tag.as_str()),
        "the row moved, so its tag moved with it"
    );
}

#[tokio::test]
async fn a_patch_touches_only_the_fields_it_carries_and_an_explicit_null_clears_one() {
    // The documented contract: any of the updatable fields, the rest left as
    // they are. Omitting a field is not the same as sending `null` — the first
    // leaves it, the second clears it — and `name` need not be resent.
    let h = RestHarness::new().await;
    let answer = h
        .send(
            "POST",
            CATEGORIES,
            Some(json!({
                "key": "billing", "name": "Invoices", "sort_order": 1,
                "description": "Money in", "domain_affinity": "finance", "icon": "coins"
            })),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 201, "{}", answer.body);
    let id = answer.body["id"].as_str().expect("an id").to_owned();
    let tag = answer.etag.expect("a state tag");
    let uri = format!("{CATEGORIES}/{id}");

    let answer = h
        .send(
            "PATCH",
            &uri,
            Some(json!({ "sort_order": 7 })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.body["sort_order"], json!(7));
    assert_eq!(
        answer.body["name"],
        json!("Invoices"),
        "not resent, not touched"
    );
    assert_eq!(answer.body["description"], json!("Money in"));
    assert_eq!(answer.body["domain_affinity"], json!("finance"));
    assert_eq!(answer.body["icon"], json!("coins"));
    let tag = answer.etag.expect("a refreshed tag");

    let answer = h
        .send(
            "PATCH",
            &uri,
            Some(json!({ "description": null, "name": "Billing" })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.body["name"], json!("Billing"));
    assert!(
        answer.body["description"].is_null(),
        "an explicit null clears: {}",
        answer.body
    );
    assert_eq!(answer.body["icon"], json!("coins"), "still untouched");
    assert_eq!(answer.body["sort_order"], json!(7));
}

#[tokio::test]
async fn an_update_carrying_a_key_is_refused_even_when_the_key_is_unchanged() {
    // The rule is that an update does not carry one. Accepting an echo would
    // make the wire contract depend on a value the caller cannot change.
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "billing", "Invoices").await;

    let answer = h
        .send(
            "PATCH",
            &format!("{CATEGORIES}/{id}"),
            Some(json!({ "key": "billing", "name": "Invoices", "sort_order": 0 })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 400, "{}", answer.body);
}

#[tokio::test]
async fn an_update_to_a_name_another_category_holds_is_refused_409() {
    let h = RestHarness::new().await;
    create(&h, "billing", "Invoices").await;
    let (id, tag) = create(&h, "logging", "Logging").await;

    let answer = h
        .send(
            "PATCH",
            &format!("{CATEGORIES}/{id}"),
            Some(json!({ "name": "Invoices", "sort_order": 0 })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 409, "{}", answer.body);
}

// ── Delete ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_empty_category_is_deleted_and_is_then_absent() {
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "billing", "Invoices").await;
    let uri = format!("{CATEGORIES}/{id}");

    let answer = h
        .send("DELETE", &uri, None, Some(&tag), h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 204, "{}", answer.body);

    let answer = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    assert_eq!(answer.status, 404);
}

#[tokio::test]
async fn a_category_that_still_holds_a_setting_is_not_removed() {
    // The no-orphan rule: a setting is filed through its category's slug, so
    // removing the category under it would leave a key pointing nowhere.
    let h = RestHarness::new().await;
    let uri = format!("{CATEGORIES}/{}", h.inner.category_id());
    let read = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    let tag = read.etag.expect("the fixture category's tag");
    h.inner.declare("proxy", "cascading", json!(true)).await;

    let answer = h
        .send("DELETE", &uri, None, Some(&tag), h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 409, "{}", answer.body);

    let answer = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    assert_eq!(answer.status, 200, "and it is still there");
}

#[tokio::test]
async fn a_category_whose_only_declaration_is_retired_is_not_removed() {
    // A retired declaration keeps its `category_id`, so removing the category
    // under it would leave a dangling reference nobody is looking at.
    let h = RestHarness::new().await;
    let uri = format!("{CATEGORIES}/{}", h.inner.category_id());
    let read = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    let tag = read.etag.expect("the fixture category's tag");
    let declaration = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.retire(declaration).await;

    let answer = h
        .send("DELETE", &uri, None, Some(&tag), h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 409, "{}", answer.body);

    let answer = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    assert_eq!(answer.status, 200, "and it is still there");
}

#[tokio::test]
async fn the_refusal_says_how_many_declarations_are_in_the_way_retired_ones_included() {
    // The fixture category starts empty, so the count is exactly what this
    // test declares: one active and one retired.
    let h = RestHarness::new().await;
    let uri = format!("{CATEGORIES}/{}", h.inner.category_id());
    let read = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    let tag = read.etag.expect("the fixture category's tag");
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let retired = h.inner.declare("mtu", "cascading", json!(true)).await;
    h.inner.retire(retired).await;

    let answer = h
        .send("DELETE", &uri, None, Some(&tag), h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 409, "{}", answer.body);
    let detail = answer.body["detail"].as_str().unwrap_or_default();
    assert!(detail.contains("2 declaration"), "{}", answer.body);
}

#[tokio::test]
async fn a_delete_without_a_tag_is_refused_428() {
    let h = RestHarness::new().await;
    let (id, _) = create(&h, "billing", "Invoices").await;

    let answer = h
        .send(
            "DELETE",
            &format!("{CATEGORIES}/{id}"),
            None,
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 428, "{}", answer.body);
}

#[tokio::test]
async fn a_delete_with_a_stale_tag_is_refused_412_and_the_row_survives() {
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "billing", "Invoices").await;
    let uri = format!("{CATEGORIES}/{id}");
    // Another administrator renames it first, moving the tag.
    h.send(
        "PATCH",
        &uri,
        Some(json!({ "name": "Renamed", "sort_order": 0 })),
        Some(&tag),
        h.inner.tree.root,
    )
    .await;

    let answer = h
        .send("DELETE", &uri, None, Some(&tag), h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 412, "{}", answer.body);

    let answer = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    assert_eq!(answer.status, 200, "the newer state stands");
    assert_eq!(answer.body["name"], json!("Renamed"));
}

// ── Authorization ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_denied_caller_reaches_no_category_operation() {
    let h = RestHarness::denying().await;
    let id = h.inner.category_id();

    for (method, uri, body) in [
        ("GET", CATEGORIES.to_owned(), None),
        ("GET", format!("{CATEGORIES}/{id}"), None),
        (
            "POST",
            CATEGORIES.to_owned(),
            Some(create_body("billing", "Invoices")),
        ),
        (
            "PATCH",
            format!("{CATEGORIES}/{id}"),
            Some(json!({ "name": "Renamed", "sort_order": 0 })),
        ),
        ("DELETE", format!("{CATEGORIES}/{id}"), None),
    ] {
        let answer = h
            .send(method, &uri, body, Some("absent"), h.inner.tree.root)
            .await;
        assert_eq!(answer.status, 403, "{method} {uri}");
    }
}

#[tokio::test]
async fn a_quoted_or_padded_tag_matches_here_as_on_every_other_surface() {
    // RFC 7232 clients quote the validator they send back, and some pad it.
    // The framing is the header's, not the tag's: one helper strips it on
    // every mutation handler, so a spelling that matches on a declaration or
    // a value matches on a category too, never a 412 for a state that agrees.
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "billing", "Invoices").await;
    let uri = format!("{CATEGORIES}/{id}");

    let quoted = format!("\"{tag}\"");
    let answer = h
        .send(
            "PATCH",
            &uri,
            Some(json!({ "name": "Invoices (quoted)" })),
            Some(quoted.as_str()),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "a quoted tag matches: {}", answer.body);
    let tag = answer.etag.expect("a refreshed tag");

    let padded = format!("  {tag} ");
    let answer = h
        .send(
            "PATCH",
            &uri,
            Some(json!({ "name": "Invoices (padded)" })),
            Some(padded.as_str()),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "a padded tag matches: {}", answer.body);
    let tag = answer.etag.expect("a refreshed tag");

    // A weak validator is not this tag: strong comparison, as If-Match asks.
    let weak = format!("W/\"{tag}\"");
    let answer = h
        .send(
            "PATCH",
            &uri,
            Some(json!({ "name": "Invoices (weak)" })),
            Some(weak.as_str()),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(
        answer.status, 412,
        "a weak validator never matches: {}",
        answer.body
    );
}

#[tokio::test]
async fn an_update_naming_no_field_is_refused_and_moves_nothing() {
    // Nothing to apply: accepting `{}` would still move the tag and record a
    // change that is none.
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "billing", "Invoices").await;
    let uri = format!("{CATEGORIES}/{id}");
    let answer = h
        .send(
            "PATCH",
            &uri,
            Some(json!({})),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 400, "{}", answer.body);
    assert_eq!(
        answer.body["context"]["field_violations"][0]["reason"],
        json!(crate::field::CATEGORY_UPDATE_EMPTY),
        "{}",
        answer.body
    );
    let read = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    assert_eq!(
        read.etag.as_deref(),
        Some(tag.as_str()),
        "the tag did not move"
    );
}

// ── Counts ───────────────────────────────────────────────────────────────────

/// The category with `key` on a listing body.
fn category<'a>(body: &'a Value, key: &str) -> &'a Value {
    body["items"]
        .as_array()
        .expect("a page")
        .iter()
        .find(|c| c["key"] == json!(key))
        .unwrap_or_else(|| panic!("category `{key}` on the page: {body}"))
}

/// A billing category holding four settings: a standard one, an advanced
/// one, a retired one, and one hidden from tenant `a`.
async fn billing_with_four(h: &RestHarness) -> Uuid {
    let billing = h.inner.add_category("billing").await;
    for name in ["plain", "deep", "old", "concealed"] {
        let id = h
            .inner
            .declare_in(billing, name, "cascading", json!(true), BOOL, "public")
            .await;
        match name {
            "deep" => h.inner.tag_advanced(id).await,
            "old" => h.inner.retire(id).await,
            "concealed" => {
                h.restrict(id, h.inner.tree.a, TenantAccess::Hidden).await;
            }
            _ => {}
        }
    }
    billing
}

#[tokio::test]
async fn the_listing_counts_each_categorys_settings_as_the_caller_sees_them() {
    // The rail shows a number beside each category, and the number has to
    // agree with the table under it: every setting the caller's browse of
    // the category lists, whatever its status, split into the advanced ones
    // a console folds away and the retired ones it marks.
    let h = RestHarness::new().await;
    let billing = billing_with_four(&h).await;

    let (status, body) = h.get(CATEGORIES, h.inner.tree.root).await;
    assert_eq!(status, 200, "{body}");
    let row = category(&body, "billing");
    assert_eq!(row["setting_count"], json!(4), "{row}");
    assert_eq!(row["advanced_count"], json!(1), "{row}");
    assert_eq!(row["retired_count"], json!(1), "{row}");
    // The harness's own category holds nothing: zeros, not an absent count.
    let empty = category(&body, "network");
    assert_eq!(empty["setting_count"], json!(0), "{empty}");
    assert_eq!(empty["advanced_count"], json!(0), "{empty}");
    assert_eq!(empty["retired_count"], json!(0), "{empty}");

    // A setting hidden from tenant `a` is absent from its count as it is
    // absent from its browse — and the two agree.
    let (status, body) = h.get(CATEGORIES, h.inner.tree.a).await;
    assert_eq!(status, 200, "{body}");
    let row = category(&body, "billing");
    assert_eq!(
        row["setting_count"],
        json!(3),
        "hidden one not counted: {row}"
    );
    assert_eq!(row["advanced_count"], json!(1), "{row}");
    assert_eq!(row["retired_count"], json!(1), "{row}");
    let browse = h
        .items(
            &format!("/settings-service/v1/settings?$filter=category_id%20eq%20{billing}"),
            h.inner.tree.a,
        )
        .await;
    assert_eq!(browse.len(), 3, "the table under the rail: {browse:?}");
}

#[tokio::test]
async fn a_single_read_carries_the_same_counts_as_the_listing() {
    let h = RestHarness::new().await;
    let billing = billing_with_four(&h).await;

    let (status, body) = h
        .get(&format!("{CATEGORIES}/{billing}"), h.inner.tree.a)
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["setting_count"], json!(3), "{body}");
    assert_eq!(body["advanced_count"], json!(1), "{body}");
    assert_eq!(body["retired_count"], json!(1), "{body}");
}

#[tokio::test]
async fn a_caller_without_the_value_read_gets_the_categories_and_no_counts() {
    // A count is an aggregate over settings and is gated as they are: a
    // caller that may not open any table under the rail is given the rail
    // without numbers, not numbers for what it cannot open.
    let h = RestHarness::without_value_read().await;
    let billing = billing_with_four(&h).await;

    let (status, body) = h.get(CATEGORIES, h.inner.tree.root).await;
    assert_eq!(status, 200, "{body}");
    let row = category(&body, "billing");
    for field in ["setting_count", "advanced_count", "retired_count"] {
        assert!(
            row.get(field).is_none(),
            "no `{field}` for this caller: {row}"
        );
    }
    // The total of categories is not a count of settings and stays.
    assert_eq!(body["page_info"]["total_count"], json!(2), "{body}");

    let (status, body) = h
        .get(&format!("{CATEGORIES}/{billing}"), h.inner.tree.root)
        .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.get("setting_count").is_none(), "{body}");

    // And the table itself is refused, as it was.
    let (status, _) = h
        .get(
            &format!("/settings-service/v1/settings?$filter=category_id%20eq%20{billing}"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn the_listing_total_is_the_whole_set_not_the_page() {
    // A cursor says there is more; the total says how much. It is counted
    // under the page's own filter and never under its cursor, so every page
    // of one walk reports the same number.
    let h = RestHarness::new().await;
    create(&h, "billing", "Invoices").await;
    create(&h, "logging", "Logging").await;
    create(&h, "storage", "Storage").await;

    let (status, first) = h
        .get(&format!("{CATEGORIES}?limit=2"), h.inner.tree.root)
        .await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["items"].as_array().map(Vec::len), Some(2));
    // Three created plus the harness's `network`.
    assert_eq!(first["page_info"]["total_count"], json!(4), "{first}");
    let cursor = first["page_info"]["next_cursor"]
        .as_str()
        .expect("more pages")
        .to_owned();

    let (status, second) = h
        .get(
            &format!("{CATEGORIES}?limit=2&cursor={cursor}"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{second}");
    assert_eq!(second["page_info"]["total_count"], json!(4), "{second}");

    let (status, filtered) = h
        .get(
            &format!("{CATEGORIES}?$filter=key%20eq%20'billing'"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{filtered}");
    assert_eq!(filtered["page_info"]["total_count"], json!(1), "{filtered}");
}
