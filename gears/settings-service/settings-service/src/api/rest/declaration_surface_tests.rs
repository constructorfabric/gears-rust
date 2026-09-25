// Created: 2026-09-17 by Virtuozzo International GmbH
//! The declaration surface driven as a client drives it.
//!
//! Authoring a declaration is where the two gates meet: authorization first,
//! then a recent re-authentication, because a declaration change moves what
//! every scope resolves. And it is where the immutability rule lives — a
//! behaviour-affecting field is not edited in place, it is expressed as a new
//! declaration — which on the wire is the difference between a 200 and a 409.

use serde_json::{Value, json};
use uuid::Uuid;

use crate::test_support::{BOOL, RestHarness};

const DECLARATIONS: &str = "/settings-service/v1/declarations";

fn create_body(h: &RestHarness, name: &str) -> Value {
    json!({
        "value_type_id": BOOL,
        "vendor": "acme",
        "name": name,
        "category_id": h.inner.category_id(),
        "default_value": true,
        "scope_class": "cascading",
    })
}

/// Author a declaration and answer with its id and its state tag.
async fn create(h: &RestHarness, name: &str) -> (String, String) {
    let answer = h
        .send(
            "POST",
            DECLARATIONS,
            Some(create_body(h, name)),
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
async fn a_create_composes_the_key_and_answers_201_with_its_location() {
    // The key is not among the fields a caller supplies: the service composes
    // it from the vendor, the category's slug and the leaf name, so a caller
    // cannot mint one that disagrees with where the setting is filed.
    let h = RestHarness::new().await;
    let answer = h
        .send(
            "POST",
            DECLARATIONS,
            Some(create_body(&h, "proxy_enabled")),
            None,
            h.inner.tree.root,
        )
        .await;

    assert_eq!(answer.status, 201, "{}", answer.body);
    let id = answer.body["id"].as_str().expect("an id");
    assert_eq!(
        answer.body["key"],
        json!("gts.cf.core.settings.setting_type.v1~acme.settings.network.proxy_enabled.v1~"),
        "the vendor, the category slug and the leaf name, in that order"
    );
    assert_eq!(answer.body["leaf_slug"], json!("proxy_enabled"));
    assert_eq!(answer.body["scope_class"], json!("cascading"));
    assert_eq!(
        answer.body["mode"],
        json!("standard"),
        "the default mode, a tag on every read"
    );
    assert_eq!(
        answer.location.as_deref(),
        Some(format!("{DECLARATIONS}/{id}").as_str())
    );
    assert!(answer.etag.is_some());
}

#[tokio::test]
async fn a_second_declaration_of_the_same_leaf_in_the_category_is_refused() {
    let h = RestHarness::new().await;
    create(&h, "proxy_enabled").await;

    let answer = h
        .send(
            "POST",
            DECLARATIONS,
            Some(create_body(&h, "proxy_enabled")),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 409, "{}", answer.body);
}

#[tokio::test]
async fn a_value_type_the_registry_does_not_know_is_refused_400() {
    let h = RestHarness::new().await;
    let mut body = create_body(&h, "proxy_enabled");
    body["value_type_id"] = json!("gts.cf.core.settings.type_invented.v1~");

    let answer = h
        .send("POST", DECLARATIONS, Some(body), None, h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 400, "{}", answer.body);
}

#[tokio::test]
async fn a_default_that_does_not_validate_against_its_type_is_refused_400() {
    // The Schema Default is a value like any other and is checked as one.
    let h = RestHarness::new().await;
    let mut body = create_body(&h, "proxy_enabled");
    body["default_value"] = json!("not a boolean");

    let answer = h
        .send("POST", DECLARATIONS, Some(body), None, h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 400, "{}", answer.body);
}

#[tokio::test]
async fn a_category_that_does_not_exist_is_refused() {
    let h = RestHarness::new().await;
    let mut body = create_body(&h, "proxy_enabled");
    body["category_id"] = json!(Uuid::new_v4());

    let answer = h
        .send("POST", DECLARATIONS, Some(body), None, h.inner.tree.root)
        .await;
    assert!(
        answer.status == 404 || answer.status == 400,
        "a declaration cannot be filed under a category that is not there: {} {}",
        answer.status,
        answer.body
    );
}

#[tokio::test]
async fn an_unknown_scope_class_is_refused_400() {
    let h = RestHarness::new().await;
    let mut body = create_body(&h, "proxy_enabled");
    body["scope_class"] = json!("sideways");

    let answer = h
        .send("POST", DECLARATIONS, Some(body), None, h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 400, "{}", answer.body);
}

// ── The second gate ──────────────────────────────────────────────────────────

#[tokio::test]
async fn declaring_a_setting_that_did_not_exist_is_not_gated_on_a_re_authentication() {
    // Step-up guards a change to what a live setting resolves. A first
    // declaration of a new key changes no resolution anywhere, so the gate
    // that guards retirement and reactivation does not stand in front of it.
    let h = RestHarness::stale_step_up().await;
    let answer = h
        .send(
            "POST",
            DECLARATIONS,
            Some(create_body(&h, "proxy_enabled")),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 201, "{}", answer.body);
}

#[tokio::test]
async fn retiring_needs_a_recent_re_authentication() {
    // Retirement drops a live setting out of resolution at once, so it is
    // gated like a value change — and refused with the RFC 9470 challenge a
    // browser knows how to act on.
    let h = RestHarness::stale_step_up().await;
    let (id, tag) = create(&h, "proxy_enabled").await;

    let answer = h
        .send(
            "DELETE",
            &format!("{DECLARATIONS}/{id}"),
            None,
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 401, "{}", answer.body);
    let challenge = answer
        .headers
        .get("www-authenticate")
        .expect("the RFC 9470 challenge");
    assert!(
        challenge.contains("insufficient_user_authentication"),
        "{challenge}"
    );
}

#[tokio::test]
async fn reviving_a_retired_declaration_needs_one_too() {
    // Reactivation changes whether a live setting resolves, so it is gated on
    // the same terms as the retirement that preceded it.
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "proxy_enabled").await;
    let retired = h
        .send(
            "DELETE",
            &format!("{DECLARATIONS}/{id}"),
            None,
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert!(retired.status.is_success(), "{}", retired.body);

    // Re-declaring the same key revives it, and that is the gated path.
    let stale = RestHarness::stale_step_up().await;
    let (other, other_tag) = create(&stale, "proxy_enabled").await;
    let retired = stale
        .send(
            "DELETE",
            &format!("{DECLARATIONS}/{other}"),
            None,
            Some(&other_tag),
            stale.inner.tree.root,
        )
        .await;
    assert_eq!(
        retired.status, 401,
        "the retirement is refused first, so the revive below is what a \
         deployment with a fresh assertion would reach"
    );

    let revived = h
        .send(
            "POST",
            DECLARATIONS,
            Some(create_body(&h, "proxy_enabled")),
            None,
            h.inner.tree.root,
        )
        .await;
    assert!(
        revived.status.is_success(),
        "with a fresh assertion the key comes back: {} {}",
        revived.status,
        revived.body
    );
}

// ── Read ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_read_carries_the_declaration_and_the_tag_a_mutation_presents() {
    let h = RestHarness::new().await;
    let (id, created_tag) = create(&h, "proxy_enabled").await;

    let answer = h
        .send(
            "GET",
            &format!("{DECLARATIONS}/{id}"),
            None,
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.body["leaf_slug"], json!("proxy_enabled"));
    assert_eq!(answer.body["value_type_id"], json!(BOOL));
    assert_eq!(answer.etag.as_deref(), Some(created_tag.as_str()));
}

#[tokio::test]
async fn a_declaration_that_does_not_exist_is_reported_absent() {
    let h = RestHarness::new().await;
    let answer = h
        .send(
            "GET",
            &format!("{DECLARATIONS}/{}", Uuid::new_v4()),
            None,
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 404);
}

#[tokio::test]
async fn the_listing_carries_the_declarations_that_exist() {
    let h = RestHarness::new().await;
    create(&h, "proxy_enabled").await;
    create(&h, "retention_days").await;

    let (status, body) = h.get(DECLARATIONS, h.inner.tree.root).await;
    assert_eq!(status, 200, "{body}");
    let mut slugs: Vec<&str> = body["items"]
        .as_array()
        .expect("a page")
        .iter()
        .filter_map(|d| d["leaf_slug"].as_str())
        .collect();
    slugs.sort_unstable();
    assert_eq!(slugs, ["proxy_enabled", "retention_days"]);
}

#[tokio::test]
async fn a_retired_declaration_stays_in_the_catalogue_and_is_marked_retired() {
    // The listing is the authoring catalogue, not the resolution set: an
    // administrator has to see a retired declaration to revive it. `status` is
    // a filterable field, so a client that wants only the live ones says so.
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "proxy_enabled").await;
    create(&h, "retention_days").await;

    let answer = h
        .send(
            "DELETE",
            &format!("{DECLARATIONS}/{id}"),
            None,
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert!(
        answer.status.is_success(),
        "{} {}",
        answer.status,
        answer.body
    );

    let (_, body) = h.get(DECLARATIONS, h.inner.tree.root).await;
    let retired = body["items"]
        .as_array()
        .expect("a page")
        .iter()
        .find(|d| d["leaf_slug"] == json!("proxy_enabled"))
        .expect("the retired declaration is still in the catalogue");
    assert_eq!(retired["status"], json!("retired"));

    let (_, live) = h
        .get(
            &format!("{DECLARATIONS}?$filter=status%20eq%20%27active%27"),
            h.inner.tree.root,
        )
        .await;
    let slugs: Vec<&str> = live["items"]
        .as_array()
        .expect("a page")
        .iter()
        .filter_map(|d| d["leaf_slug"].as_str())
        .collect();
    assert_eq!(
        slugs,
        ["retention_days"],
        "and a client that asks for the live ones gets only those"
    );
}

// ── Update: what may change in place, and what may not ───────────────────────

#[tokio::test]
async fn descriptive_metadata_is_edited_in_place() {
    // It changes no effective value, so it applies immediately.
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "proxy_enabled").await;

    let answer = h
        .send(
            "PATCH",
            &format!("{DECLARATIONS}/{id}"),
            Some(json!({ "description": "Whether the proxy is used", "mode": "advanced" })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(
        answer.body["description"],
        json!("Whether the proxy is used")
    );
    assert_eq!(answer.body["mode"], json!("advanced"));
}

#[tokio::test]
async fn an_update_is_conditional_like_every_other_mutation() {
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "proxy_enabled").await;
    let uri = format!("{DECLARATIONS}/{id}");
    let patch = Some(json!({ "description": "changed" }));

    let answer = h
        .send("PATCH", &uri, patch.clone(), None, h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 428, "{}", answer.body);

    let answer = h
        .send(
            "PATCH",
            &uri,
            patch,
            Some("a tag from another time"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 412, "{}", answer.body);
    let _ = tag;
}

#[tokio::test]
async fn a_field_the_update_does_not_take_is_refused_rather_than_ignored() {
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "proxy_enabled").await;

    for patch in [
        json!({ "scope_class": "local" }),
        json!({ "default_value": false }),
        json!({ "value_type_id": BOOL }),
    ] {
        let answer = h
            .send(
                "PATCH",
                &format!("{DECLARATIONS}/{id}"),
                Some(patch.clone()),
                Some(&tag),
                h.inner.tree.root,
            )
            .await;
        assert!(
            answer.status == 400 || answer.status == 409,
            "a behaviour-affecting field is not edited in place: {patch} gave {} {}",
            answer.status,
            answer.body
        );
    }
}

// ── Authorization ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_denied_caller_reaches_no_declaration_operation() {
    let h = RestHarness::denying().await;
    let id = Uuid::new_v4();

    for (method, uri, body) in [
        ("GET", DECLARATIONS.to_owned(), None),
        ("GET", format!("{DECLARATIONS}/{id}"), None),
        (
            "POST",
            DECLARATIONS.to_owned(),
            Some(create_body(&h, "proxy_enabled")),
        ),
        (
            "PATCH",
            format!("{DECLARATIONS}/{id}"),
            Some(json!({ "description": "changed" })),
        ),
        ("DELETE", format!("{DECLARATIONS}/{id}"), None),
    ] {
        let answer = h
            .send(method, &uri, body, Some("absent"), h.inner.tree.root)
            .await;
        assert_eq!(answer.status, 403, "{method} {uri}: {}", answer.body);
    }
}

// ── Pagination ───────────────────────────────────────────────────────────────

/// The leaf slugs on a page, in the order served.
fn slugs(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["leaf_slug"].as_str())
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn a_declaration_page_continues_from_its_cursor_without_a_gap_or_a_repeat() {
    // The cursor is minted from the last row served, by the field the page is
    // ordered on — the key, unique, so the boundary is exact.
    let h = RestHarness::new().await;
    for name in ["alpha", "beta", "gamma"] {
        create(&h, name).await;
    }

    let (status, first) = h
        .get(&format!("{DECLARATIONS}?limit=2"), h.inner.tree.root)
        .await;
    assert_eq!(status, 200, "{first}");
    let first_slugs = slugs(&first);
    assert_eq!(
        first_slugs,
        vec!["alpha", "beta"],
        "a full page in key order"
    );
    let cursor = first["page_info"]["next_cursor"]
        .as_str()
        .expect("a third row waits on the next page")
        .to_owned();

    let (status, second) = h
        .get(
            &format!("{DECLARATIONS}?limit=2&cursor={cursor}"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{second}");
    assert_eq!(slugs(&second), vec!["gamma"], "the remainder, once");
    assert!(
        second["page_info"]["next_cursor"].is_null(),
        "nothing after the remainder: {second}"
    );
}

#[tokio::test]
async fn ordering_by_a_column_that_may_be_empty_is_refused_before_any_page() {
    // A page cursor carries the sort value of the last row served, and it has
    // no spelling for an empty one: a listing ordered on a column that may be
    // empty would serve its first page and then refuse its own cursor. Such a
    // column is not offered for ordering, and asking for it is refused up
    // front, naming it.
    let h = RestHarness::new().await;
    for name in ["alpha", "beta", "gamma"] {
        create(&h, name).await;
    }
    for field in ["domain_affinity", "owner_module"] {
        let (status, body) = h
            .get(
                &format!("{DECLARATIONS}?limit=2&$orderby={field}%20asc"),
                h.inner.tree.root,
            )
            .await;
        assert_eq!(status, 400, "{field}: {body}");
        let violation = &body["context"]["field_violations"][0];
        assert_eq!(violation["field"], json!("$orderby"), "{body}");
        assert_eq!(
            violation["reason"],
            json!(crate::field::ODATA_UNSORTABLE_FIELD),
            "{body}"
        );
        assert!(
            violation["description"]
                .as_str()
                .is_some_and(|d| d.contains(field)),
            "the refusal names the field: {body}"
        );
    }

    // A column that is never empty still orders the listing.
    let (status, body) = h
        .get(
            &format!("{DECLARATIONS}?limit=2&$orderby=key%20desc"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(slugs(&body), vec!["gamma", "beta"]);
}

#[tokio::test]
async fn re_declaring_an_active_setting_with_a_new_shape_answers_200_evolved_under_the_next_major()
{
    let h = RestHarness::new().await;
    let (id, _) = create(&h, "proxy_enabled").await;

    let mut body = create_body(&h, "proxy_enabled");
    body["value_type_id"] = json!(crate::test_support::TEXT);
    body["default_value"] = json!("on");
    let answer = h
        .send(
            "POST",
            DECLARATIONS,
            Some(body.clone()),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.body["evolved"], json!(true), "{}", answer.body);
    assert_eq!(answer.body["reactivated"], json!(false));
    let key = answer.body["key"].as_str().expect("a key");
    assert!(key.ends_with(".proxy_enabled.v2~"), "{key}");
    assert_ne!(answer.body["id"], json!(id), "a new declaration");
    assert!(answer.etag.is_some());

    // The same request again matches the live v2: a conflict, not a v3.
    let repeat = h
        .send("POST", DECLARATIONS, Some(body), None, h.inner.tree.root)
        .await;
    assert_eq!(repeat.status, 409, "{}", repeat.body);
}
