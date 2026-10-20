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

#[tokio::test]
async fn a_page_ordered_by_any_orderable_field_continues_without_a_gap_or_a_repeat() {
    // `mode`, `status` and `category_id` are the same for every row here: the
    // page boundary falls inside a tie, and only the `key` tiebreaker the
    // pagination appends keeps the next page from skipping or repeating one.
    let h = RestHarness::new().await;
    for name in ["alpha", "beta", "gamma"] {
        create(&h, name).await;
    }
    for order in ["category_id%20asc", "mode%20asc", "status%20desc"] {
        let (status, first) = h
            .get(
                &format!("{DECLARATIONS}?limit=2&$orderby={order}"),
                h.inner.tree.root,
            )
            .await;
        assert_eq!(status, 200, "{order}: {first}");
        let mut seen = slugs(&first);
        assert_eq!(seen.len(), 2, "{order}: a full first page");
        let cursor = first["page_info"]["next_cursor"]
            .as_str()
            .unwrap_or_else(|| panic!("{order}: a third row waits"))
            .to_owned();
        let (status, second) = h
            .get(
                &format!("{DECLARATIONS}?limit=2&cursor={cursor}"),
                h.inner.tree.root,
            )
            .await;
        assert_eq!(status, 200, "{order}: {second}");
        seen.extend(slugs(&second));
        seen.sort();
        assert_eq!(
            seen,
            vec!["alpha", "beta", "gamma"],
            "{order}: each row once"
        );
        assert!(
            second["page_info"]["next_cursor"].is_null(),
            "{order}: {second}"
        );
    }
}

#[tokio::test]
async fn a_default_that_does_not_survive_a_round_trip_is_refused_as_a_value_write_would() {
    // The same literal a value write refuses with `value_not_canonical` — a
    // decimal a double cannot hold exactly — must not slip in as a Schema
    // Default. Only the raw text can tell: once parsed, the literal is already
    // the nearest double, and no check on the parsed value can see the loss.
    let h = RestHarness::new().await;
    let body = format!(
        r#"{{"value_type_id": "{BOOL}", "vendor": "acme", "name": "precise",
            "category_id": "{}", "default_value": 0.1000000000000000055511151231257827,
            "scope_class": "cascading"}}"#,
        h.inner.category_id()
    );
    let answer = h
        .send_text("POST", DECLARATIONS, &body, None, h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 400, "{}", answer.body);
    assert_eq!(
        answer.body["context"]["field_violations"][0]["reason"],
        json!(crate::field::VALUE_NOT_CANONICAL),
        "{}",
        answer.body
    );
}

#[tokio::test]
async fn the_declaration_listing_reports_its_total_beside_the_page() {
    let h = RestHarness::new().await;
    for name in ["alpha", "beta", "gamma"] {
        create(&h, name).await;
    }

    let (status, body) = h
        .get(&format!("{DECLARATIONS}?limit=2"), h.inner.tree.root)
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(slugs(&body).len(), 2);
    assert_eq!(body["page_info"]["total_count"], json!(3), "{body}");

    // Under a filter, the total is of the filtered set: one more declaration
    // in another category leaves the harness category's total at three.
    let other = h.inner.add_category("other").await;
    h.inner
        .declare_in(other, "delta", "cascading", json!(true), BOOL, "public")
        .await;
    let (status, body) = h
        .get(
            &format!(
                "{DECLARATIONS}?$filter=category_id%20eq%20{}",
                h.inner.category_id()
            ),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["page_info"]["total_count"],
        json!(3),
        "under the filter: {body}"
    );
    let (status, body) = h.get(DECLARATIONS, h.inner.tree.root).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["page_info"]["total_count"],
        json!(4),
        "unfiltered: {body}"
    );
}

// ── The display name ─────────────────────────────────────────────────────────

/// Author a declaration with the given extra body fields and answer with the
/// whole response.
async fn create_with(h: &RestHarness, name: &str, extra: Value) -> crate::test_support::Answer {
    let mut body = create_body(h, name);
    for (field, value) in extra.as_object().expect("an object") {
        body[field] = value.clone();
    }
    h.send("POST", DECLARATIONS, Some(body), None, h.inner.tree.root)
        .await
}

/// The field a 400 problem document names, from its first violation.
fn violated_field(body: &Value) -> &Value {
    &body["context"]["field_violations"][0]["field"]
}

#[tokio::test]
async fn a_create_with_a_display_name_answers_201_with_it_and_every_read_carries_it() {
    // The label is what a console shows; it has to survive the create and be
    // served by the single read and the listing alike.
    let h = RestHarness::new().await;
    let answer = create_with(
        &h,
        "min_tls",
        json!({ "display_name": "Minimum TLS version" }),
    )
    .await;
    assert_eq!(answer.status, 201, "{}", answer.body);
    assert_eq!(answer.body["display_name"], json!("Minimum TLS version"));
    let id = answer.body["id"].as_str().expect("an id").to_owned();

    let read = h
        .send(
            "GET",
            &format!("{DECLARATIONS}/{id}"),
            None,
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(read.body["display_name"], json!("Minimum TLS version"));

    let (_, listing) = h.get(DECLARATIONS, h.inner.tree.root).await;
    assert_eq!(
        listing["items"][0]["display_name"],
        json!("Minimum TLS version"),
        "{listing}"
    );
}

#[tokio::test]
async fn a_create_without_a_display_name_has_no_such_key_anywhere() {
    // Omitted, never `null` and never derived from the leaf slug: the client
    // owns the fallback label.
    let h = RestHarness::new().await;
    let answer = create_with(&h, "min_tls", json!({})).await;
    assert_eq!(answer.status, 201, "{}", answer.body);
    assert!(
        answer.body.get("display_name").is_none(),
        "the create body: {}",
        answer.body
    );
    let id = answer.body["id"].as_str().expect("an id").to_owned();

    let read = h
        .send(
            "GET",
            &format!("{DECLARATIONS}/{id}"),
            None,
            None,
            h.inner.tree.root,
        )
        .await;
    assert!(read.body.get("display_name").is_none(), "{}", read.body);
    let (_, listing) = h.get(DECLARATIONS, h.inner.tree.root).await;
    assert!(
        listing["items"][0].get("display_name").is_none(),
        "{listing}"
    );
}

#[tokio::test]
async fn a_create_with_a_null_display_name_behaves_as_if_it_were_absent() {
    let h = RestHarness::new().await;
    let answer = create_with(&h, "min_tls", json!({ "display_name": null })).await;
    assert_eq!(answer.status, 201, "{}", answer.body);
    assert!(answer.body.get("display_name").is_none(), "{}", answer.body);
}

#[tokio::test]
async fn an_empty_blank_or_over_long_display_name_is_refused_400_naming_the_field() {
    let h = RestHarness::new().await;
    for (n, bad) in ["", "   ", &"x".repeat(257)].into_iter().enumerate() {
        let answer = create_with(&h, &format!("refused_{n}"), json!({ "display_name": bad })).await;
        assert_eq!(answer.status, 400, "{n}: {}", answer.body);
        assert_eq!(
            violated_field(&answer.body),
            &json!("display_name"),
            "{n}: {}",
            answer.body
        );
    }
}

#[tokio::test]
async fn the_length_bound_counts_characters_after_trimming_not_bytes() {
    // 256 `ё` are 512 bytes but 256 characters, which is what `varchar(256)`
    // counts; surrounding whitespace is not part of the bound and is not kept.
    let h = RestHarness::new().await;
    let multi_byte = "\u{451}".repeat(256);
    let answer = create_with(&h, "multi_byte", json!({ "display_name": multi_byte })).await;
    assert_eq!(answer.status, 201, "{}", answer.body);
    assert_eq!(answer.body["display_name"], json!("\u{451}".repeat(256)));

    let core = "y".repeat(256);
    let answer = create_with(
        &h,
        "padded",
        json!({ "display_name": format!("  {core}\t ") }),
    )
    .await;
    assert_eq!(answer.status, 201, "{}", answer.body);
    assert_eq!(
        answer.body["display_name"],
        json!(core),
        "the trimmed value is what is stored and returned"
    );
}

#[tokio::test]
async fn a_patch_sets_clears_and_leaves_the_display_name() {
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "min_tls").await;
    let uri = format!("{DECLARATIONS}/{id}");

    let set = h
        .send(
            "PATCH",
            &uri,
            Some(json!({ "display_name": "x" })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(set.status, 200, "{}", set.body);
    assert_eq!(set.body["display_name"], json!("x"));
    let tag = set.etag.expect("a fresh tag");

    let other = h
        .send(
            "PATCH",
            &uri,
            Some(json!({ "description": "unrelated" })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(other.status, 200, "{}", other.body);
    assert_eq!(
        other.body["display_name"],
        json!("x"),
        "a patch without the field leaves it"
    );
    let tag = other.etag.expect("a fresh tag");

    let cleared = h
        .send(
            "PATCH",
            &uri,
            Some(json!({ "display_name": null })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(cleared.status, 200, "{}", cleared.body);
    assert!(
        cleared.body.get("display_name").is_none(),
        "an explicit null clears it and the key goes: {}",
        cleared.body
    );
}

#[tokio::test]
async fn a_display_name_edit_moves_the_tag_and_not_last_change_at() {
    // The label is descriptive: `last_change_at` is what value resolution is
    // stamped with, and it must not move. A `description` edit is the existing
    // behaviour this one is held to.
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "min_tls").await;
    let uri = format!("{DECLARATIONS}/{id}");
    let read = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    let stamp = read.body["last_change_at"].clone();

    let by_description = h
        .send(
            "PATCH",
            &uri,
            Some(json!({ "description": "d" })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(by_description.body["last_change_at"], stamp);
    let tag_after_description = by_description.etag.expect("a tag");
    assert_ne!(tag_after_description, tag);

    let by_label = h
        .send(
            "PATCH",
            &uri,
            Some(json!({ "display_name": "Label" })),
            Some(&tag_after_description),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(by_label.status, 200, "{}", by_label.body);
    assert_ne!(
        by_label.etag.as_deref(),
        Some(tag_after_description.as_str()),
        "the tag moves with a label edit"
    );
    assert_eq!(
        by_label.body["last_change_at"], stamp,
        "and the resolution stamp does not"
    );
}

#[tokio::test]
async fn a_patch_with_an_empty_display_name_is_refused_400_naming_the_field() {
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "min_tls").await;
    let answer = h
        .send(
            "PATCH",
            &format!("{DECLARATIONS}/{id}"),
            Some(json!({ "display_name": "" })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 400, "{}", answer.body);
    assert_eq!(
        violated_field(&answer.body),
        &json!("display_name"),
        "{}",
        answer.body
    );
}

#[tokio::test]
async fn a_display_name_patch_on_a_contributed_declaration_is_a_409() {
    // The owning gear is the only author of a contributed declaration's
    // metadata; the label arrives with its re-registration.
    let h = RestHarness::new().await;
    let id = h
        .inner
        .declare_labelled("contributed", "cascading", json!(true), "Module label")
        .await;
    let uri = format!("{DECLARATIONS}/{id}");
    let read = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    assert_eq!(read.status, 200, "{}", read.body);
    assert_eq!(read.body["display_name"], json!("Module label"));
    let tag = read.etag.expect("a tag");

    let answer = h
        .send(
            "PATCH",
            &uri,
            Some(json!({ "display_name": "mine now" })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 409, "{}", answer.body);
    assert!(
        answer.body["detail"]
            .as_str()
            .is_some_and(|d| d.starts_with("contributed_declaration_immutable")),
        "the conflict names its reason first: {}",
        answer.body
    );
}

#[tokio::test]
async fn a_display_name_with_a_control_character_is_refused_400_on_create() {
    // NUL and a line break are not text a console can show as a label.
    let h = RestHarness::new().await;
    for (n, bad) in ["a\u{0}b", "a\nb"].into_iter().enumerate() {
        let answer = create_with(&h, &format!("control_{n}"), json!({ "display_name": bad })).await;
        assert_eq!(answer.status, 400, "{n}: {}", answer.body);
        assert_eq!(
            violated_field(&answer.body),
            &json!("display_name"),
            "{n}: {}",
            answer.body
        );
    }
}

#[tokio::test]
async fn a_patch_with_a_control_character_in_the_display_name_is_refused_400() {
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "min_tls").await;
    let answer = h
        .send(
            "PATCH",
            &format!("{DECLARATIONS}/{id}"),
            Some(json!({ "display_name": "a\tb" })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 400, "{}", answer.body);
    assert_eq!(
        violated_field(&answer.body),
        &json!("display_name"),
        "{}",
        answer.body
    );
}

#[tokio::test]
async fn a_patch_accepts_the_boundary_display_name_and_stores_it_trimmed() {
    // The create path already proves the bound; the same bound must hold on a
    // PATCH, where the padding and the tab are trimmed before counting.
    let h = RestHarness::new().await;
    let (id, tag) = create(&h, "min_tls").await;
    let uri = format!("{DECLARATIONS}/{id}");
    let core = "\u{451}".repeat(256);

    let padded = h
        .send(
            "PATCH",
            &uri,
            Some(json!({ "display_name": format!("  {core}\t ") })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(padded.status, 200, "{}", padded.body);
    assert_eq!(
        padded.body["display_name"],
        json!(core),
        "the response carries the trimmed value"
    );
    let read = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    assert_eq!(
        read.body["display_name"],
        json!(core),
        "a following read carries it too"
    );

    let tag = padded.etag.expect("a fresh tag");
    let one = h
        .send(
            "PATCH",
            &uri,
            Some(json!({ "display_name": "x" })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(
        one.status, 200,
        "a one-character label is accepted: {}",
        one.body
    );
    assert_eq!(one.body["display_name"], json!("x"));
}

#[tokio::test]
async fn surrounding_whitespace_is_trimmed_before_the_control_check_and_a_format_character_stays() {
    // Trimming runs first, so a trailing newline is dropped rather than
    // refused; a zero-width space is a format character, not a control one,
    // and stays: the rule is about what a console cannot render, not about
    // what looks odd in it.
    let h = RestHarness::new().await;
    let answer = create_with(&h, "trailing_newline", json!({ "display_name": "Label\n" })).await;
    assert_eq!(answer.status, 201, "{}", answer.body);
    assert_eq!(
        answer.body["display_name"],
        json!("Label"),
        "the newline was trimmed, not refused"
    );

    let answer = create_with(&h, "zero_width", json!({ "display_name": "a\u{200B}b" })).await;
    assert_eq!(answer.status, 201, "{}", answer.body);
    assert_eq!(
        answer.body["display_name"],
        json!("a\u{200B}b"),
        "a format character is not a control character"
    );
}
