// Created: 2026-09-17 by Virtuozzo International GmbH
//! The category surface driven as a client drives it.
//!
//! Categories are the tree settings hang from, so their rules are the ones a
//! client meets first: a name is unique, a key is immutable because every
//! setting is filed through it, a category with settings in it cannot be
//! removed, and every mutation is conditional on the state the caller read.
//! Each of those is a status code, and a status code is only true once a
//! request has produced it.

use serde_json::{Value, json};

use crate::test_support::RestHarness;

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
    // for ordering; the name and the key are.
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
    for field in ["name", "key"] {
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
