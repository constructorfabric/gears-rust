// Created: 2026-09-17 by Virtuozzo International GmbH
//! The read surface driven as a client drives it: a request through the router
//! the gear registers, and the status code the API promises.
//!
//! What only a request exercises is what only a request can break. The query
//! string is parsed by the same extractors, an option the resource does not
//! take is refused where a client would meet it, a domain refusal is rendered
//! by the error layer into its status, and a setting hidden from the caller
//! has to disappear from every one of these paths rather than from the one
//! that happened to have a unit test.

use serde_json::json;

use crate::domain::access::TenantAccess;
use crate::domain::resolution::MASK_TOKEN;
use crate::test_support::{BOOL, RestHarness, SECRET, TEXT};

/// The setting key as it travels in a path segment.
fn encoded(h: &RestHarness, name: &str) -> String {
    urlencoding(&h.inner.key(name).to_string())
}

/// Percent-encode the characters a GTS key carries that a path segment may not.
fn urlencoding(raw: &str) -> String {
    raw.chars()
        .map(|c| match c {
            '~' => "%7E".to_owned(),
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => c.to_string(),
            other => format!("%{:02X}", other as u32),
        })
        .collect()
}

// ── The two rules every read path shares ─────────────────────────────────────

#[tokio::test]
async fn a_hidden_setting_is_absent_from_the_browse_page() {
    let h = RestHarness::new().await;
    h.inner.declare("visible", "cascading", json!(true)).await;
    let concealed = h.inner.declare("concealed", "cascading", json!(true)).await;
    h.restrict(concealed, h.inner.tree.a, TenantAccess::Hidden)
        .await;

    let items = h
        .items("/settings-service/v1/settings", h.inner.tree.a)
        .await;
    let slugs: Vec<&str> = items
        .iter()
        .filter_map(|i| i["key"].as_str())
        .filter_map(|k| k.rsplit('.').nth(1))
        .collect();
    assert!(slugs.contains(&"visible"), "{slugs:?}");
    assert!(
        !slugs.contains(&"concealed"),
        "a hidden setting leaves the page silently: {slugs:?}"
    );
}

#[tokio::test]
async fn a_page_is_cut_after_the_hidden_exclusion_so_it_comes_back_full() {
    // Four settings, one hidden from `a`. A page of two must hold two of the
    // visible ones and point at the third — not come back short, which would
    // tell the caller something sits in the gap. By key: alpha, beta (hidden),
    // delta, gamma — all under one `px_` prefix the search can ask for.
    let h = RestHarness::new().await;
    for name in ["px_alpha", "px_gamma", "px_delta"] {
        h.inner.declare(name, "cascading", json!(true)).await;
    }
    let concealed = h.inner.declare("px_beta", "cascading", json!(true)).await;
    h.restrict(concealed, h.inner.tree.a, TenantAccess::Hidden)
        .await;

    let (status, body) = h
        .get("/settings-service/v1/settings?limit=2", h.inner.tree.a)
        .await;
    assert_eq!(status, 200, "{body}");
    let slugs: Vec<&str> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["key"].as_str())
        .filter_map(|k| k.rsplit('.').nth(1))
        .collect();
    assert_eq!(
        slugs,
        vec!["px_alpha", "px_delta"],
        "full page, hidden one skipped"
    );
    let cursor = body["page_info"]["next_cursor"]
        .as_str()
        .expect("the third visible setting is on the next page: {body}")
        .to_owned();

    let (status, body) = h
        .get(
            &format!("/settings-service/v1/settings?limit=2&cursor={cursor}"),
            h.inner.tree.a,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let slugs: Vec<&str> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["key"].as_str())
        .filter_map(|k| k.rsplit('.').nth(1))
        .collect();
    assert_eq!(
        slugs,
        vec!["px_gamma"],
        "the rest, hidden one still skipped"
    );

    // Search pages the same way.
    let (status, body) = h
        .get("/settings-service/v1/search?q=px_&limit=2", h.inner.tree.a)
        .await;
    assert_eq!(status, 200, "{body}");
    let hits: Vec<&str> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["leaf_slug"].as_str())
        .collect();
    assert_eq!(
        hits,
        vec!["px_alpha", "px_delta"],
        "full page of hits, hidden one skipped"
    );
}

#[tokio::test]
async fn a_subtree_past_the_budget_is_refused_by_search_and_the_needs_review_browse() {
    // The corpus of overrides is the target's subtree. A subtree the budget
    // cuts would make the answer silently incomplete, so the surface refuses
    // and names the bound instead.
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner
        .hierarchy
        .truncate_subtrees
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let (status, body) = h
        .get("/settings-service/v1/search?q=proxy", h.inner.tree.root)
        .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        body["context"]["field_violations"][0]["reason"],
        json!("subtree_too_large"),
        "{body}"
    );

    let (status, body) = h
        .get(
            "/settings-service/v1/settings?$filter=needs_review%20eq%20true",
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        body["context"]["field_violations"][0]["reason"],
        json!("subtree_too_large"),
        "{body}"
    );

    // The plain browse never walks the subtree and is untouched.
    let (status, _) = h
        .get("/settings-service/v1/settings", h.inner.tree.root)
        .await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn a_hidden_setting_reads_as_absent_rather_than_forbidden() {
    // 404, never 403: a distinct denial would confirm that a setting the
    // caller may not see exists.
    let h = RestHarness::new().await;
    let id = h.inner.declare("concealed", "cascading", json!(true)).await;
    h.restrict(id, h.inner.tree.a, TenantAccess::Hidden).await;

    let uri = format!("/settings-service/v1/settings/{}", encoded(&h, "concealed"));
    let (status, _) = h.get(&uri, h.inner.tree.a).await;
    assert_eq!(status, 404);

    // The same setting, read by a tenant with no row, is served normally.
    let (status, _) = h.get(&uri, h.inner.tree.c).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn a_hidden_setting_is_absent_from_search_results() {
    let h = RestHarness::new().await;
    h.inner
        .declare("proxy_visible", "cascading", json!(true))
        .await;
    let hidden = h
        .inner
        .declare("proxy_hidden", "cascading", json!(true))
        .await;
    h.restrict(hidden, h.inner.tree.a, TenantAccess::Hidden)
        .await;

    let items = h
        .items("/settings-service/v1/search?q=proxy", h.inner.tree.a)
        .await;
    let slugs: Vec<&str> = items
        .iter()
        .filter_map(|i| i["leaf_slug"].as_str())
        .collect();
    assert_eq!(
        slugs,
        vec!["proxy_visible"],
        "a hidden setting is not discoverable through a search either"
    );
}

#[tokio::test]
async fn a_hidden_setting_keeps_its_history_from_the_caller() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("concealed", "cascading", json!(true)).await;
    h.restrict(id, h.inner.tree.a, TenantAccess::Hidden).await;

    let uri = format!(
        "/settings-service/v1/settings/{}/history",
        encoded(&h, "concealed")
    );
    let (status, _) = h.get(&uri, h.inner.tree.a).await;
    assert_eq!(status, 404);
}

// ── The options each resource does and does not take ─────────────────────────

#[tokio::test]
async fn search_refuses_the_odata_options_it_does_not_take() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;

    for option in [
        "&$filter=key%20eq%20%27x%27",
        "&$orderby=key",
        "&$select=key",
    ] {
        let uri = format!("/settings-service/v1/search?q=proxy{option}");
        let (status, body) = h.get(&uri, h.inner.tree.root).await;
        assert_eq!(status, 400, "{option}: {body}");
    }

    // Without them the same query is served.
    let (status, _) = h
        .get("/settings-service/v1/search?q=proxy", h.inner.tree.root)
        .await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn history_refuses_the_odata_options_it_does_not_take() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let key = encoded(&h, "proxy");

    for option in ["$filter=key%20eq%20%27x%27", "$orderby=key", "$select=key"] {
        let uri = format!("/settings-service/v1/settings/{key}/history?{option}");
        let (status, body) = h.get(&uri, h.inner.tree.root).await;
        assert_eq!(status, 400, "{option}: {body}");
    }
}

#[tokio::test]
async fn a_search_query_shorter_than_two_characters_is_refused() {
    let h = RestHarness::new().await;
    for q in ["", "a", "%20a%20"] {
        let uri = format!("/settings-service/v1/search?q={q}");
        let (status, _) = h.get(&uri, h.inner.tree.root).await;
        assert_eq!(status, 400, "q={q:?}");
    }
    let long = "x".repeat(201);
    let (status, _) = h
        .get(
            &format!("/settings-service/v1/search?q={long}"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 400, "a query past the upper bound");
}

#[tokio::test]
async fn browse_refuses_a_filter_on_a_field_it_does_not_map() {
    let h = RestHarness::new().await;
    let uri = "/settings-service/v1/settings?$filter=tenant%20eq%20%27x%27";
    let (status, _) = h.get(uri, h.inner.tree.root).await;
    assert_eq!(
        status, 400,
        "an unmapped field is refused, never silently ignored"
    );
}

#[tokio::test]
async fn browse_refuses_the_filter_operators_it_does_not_take_and_serves_the_ones_it_does() {
    // The generated `$filter` parameter text lists every operator the parser
    // knows for a field's kind; this handler takes a narrower, deliberate set
    // and says so in its description. What it does not take is a `400` with
    // a problem document, never a silently ignored clause.
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let key = h.inner.key("proxy");
    // A UUID literal is bare in OData; a quoted one is a type mismatch, which
    // is a different refusal from the operator's and not what is pinned here.
    let category = h.inner.category_id();

    for filter in [
        format!("key ne '{key}'"),
        "key contains 'proxy'".to_owned(),
        "key startswith 'gts'".to_owned(),
        "key endswith 'v1~'".to_owned(),
        format!("category_id ne {category}"),
        format!("category_id in ({category})"),
        "needs_review ne true".to_owned(),
    ] {
        let uri = format!(
            "/settings-service/v1/settings?$filter={}",
            urlencoding(&filter)
        );
        let (status, body) = h.get(&uri, h.inner.tree.root).await;
        assert_eq!(status, 400, "`{filter}` is not taken: {body}");
        assert_eq!(body["status"], json!(400), "a problem document: {body}");
    }

    // The forms the handler takes, served.
    for filter in [
        format!("key eq '{key}'"),
        format!("key in ('{key}')"),
        format!("category_id eq {category}"),
        "needs_review eq true".to_owned(),
    ] {
        let uri = format!(
            "/settings-service/v1/settings?$filter={}",
            urlencoding(&filter)
        );
        let (status, body) = h.get(&uri, h.inner.tree.root).await;
        assert_eq!(status, 200, "`{filter}` is taken: {body}");
    }
}

// ── The target check ─────────────────────────────────────────────────────────

#[tokio::test]
async fn a_target_outside_the_callers_subtree_is_refused_on_every_read() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let key = encoded(&h, "proxy");
    // `c` is the caller's sibling, not its descendant.
    let outside = h.inner.tree.c;

    for uri in [
        format!("/settings-service/v1/settings?tenant={outside}"),
        format!("/settings-service/v1/search?q=proxy&tenant={outside}"),
        format!("/settings-service/v1/settings/{key}?tenant={outside}"),
        format!("/settings-service/v1/settings/{key}/history?tenant={outside}"),
    ] {
        let (status, _) = h.get(&uri, h.inner.tree.a).await;
        assert_eq!(status, 403, "{uri}");
    }
}

#[tokio::test]
async fn a_standalone_descendant_is_refused_from_above() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    // `s` is a descendant of `a`, marked standalone: inheritance flows in,
    // nothing of its own state is readable from above.
    let uri = format!("/settings-service/v1/settings?tenant={}", h.inner.tree.s);
    let (status, _) = h.get(&uri, h.inner.tree.a).await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn a_malformed_tenant_is_refused_before_anything_else() {
    let h = RestHarness::new().await;
    for uri in [
        "/settings-service/v1/settings?tenant=not-a-uuid",
        "/settings-service/v1/search?q=proxy&tenant=not-a-uuid",
    ] {
        let (status, _) = h.get(uri, h.inner.tree.root).await;
        assert_eq!(status, 400, "{uri}");
    }
}

// ── Authorization ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_denied_caller_is_refused_on_every_read_and_learns_nothing() {
    let h = RestHarness::denying().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let key = encoded(&h, "proxy");

    for uri in [
        "/settings-service/v1/settings".to_owned(),
        "/settings-service/v1/search?q=proxy".to_owned(),
        format!("/settings-service/v1/settings/{key}"),
        format!("/settings-service/v1/settings/{key}/history"),
    ] {
        let (status, body) = h.get(&uri, h.inner.tree.root).await;
        assert_eq!(status, 403, "{uri}");
        assert!(
            body.get("items").is_none(),
            "a refusal carries no page: {uri}"
        );
    }
}

// ── Masking, on the way out ──────────────────────────────────────────────────

#[tokio::test]
async fn a_secret_is_masked_on_the_browse_page_and_in_a_single_read() {
    let h = RestHarness::new().await;
    let id = h
        .inner
        .declare_typed("api_token", "cascading", json!(""), BOOL, "secret")
        .await;
    h.inner
        .set_secret(id, h.inner.tree.root, "settings/abc/def")
        .await;

    let items = h
        .items("/settings-service/v1/settings", h.inner.tree.root)
        .await;
    let entry = items
        .iter()
        .find(|i| i["key"].as_str().is_some_and(|k| k.contains("api_token")))
        .expect("the secret setting is on the page");
    assert_eq!(entry["effective"]["value"], json!(MASK_TOKEN));
    assert_eq!(entry["effective"]["masked"], json!(true));

    let uri = format!("/settings-service/v1/settings/{}", encoded(&h, "api_token"));
    let (status, body) = h.get(&uri, h.inner.tree.root).await;
    assert_eq!(status, 200);
    assert_eq!(body["value"], json!(MASK_TOKEN));
    let wire = body.to_string();
    assert!(
        !wire.contains("settings/abc/def"),
        "the store reference never reaches a client: {wire}"
    );
}

// ── What a read hands back for the write that follows ────────────────────────

#[tokio::test]
async fn a_single_read_carries_the_state_tag_a_write_must_present() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let uri = format!("/settings-service/v1/settings/{}", encoded(&h, "proxy"));
    let (status, body) = h.get(&uri, h.inner.tree.root).await;
    assert_eq!(status, 200);
    assert_eq!(
        body["etag"],
        json!("absent"),
        "no row at this scope yet, and the absent state has a tag of its own"
    );
}

#[tokio::test]
async fn a_key_that_is_not_a_key_is_refused_before_the_database() {
    let h = RestHarness::new().await;
    let (status, _) = h
        .get("/settings-service/v1/settings/not-a-key", h.inner.tree.root)
        .await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn an_absent_setting_is_reported_absent() {
    let h = RestHarness::new().await;
    let uri = format!(
        "/settings-service/v1/settings/{}",
        encoded(&h, "never_declared")
    );
    let (status, _) = h.get(&uri, h.inner.tree.root).await;
    assert_eq!(status, 404);
}

// ── The page a client actually renders ───────────────────────────────────────

#[tokio::test]
async fn a_browse_item_carries_its_mode_as_a_tag_and_nothing_is_withheld_by_it() {
    let h = RestHarness::new().await;
    h.inner
        .declare("standard_one", "cascading", json!(true))
        .await;
    h.inner.declare("standard_two", "local", json!(false)).await;

    let items = h
        .items("/settings-service/v1/settings", h.inner.tree.root)
        .await;
    assert_eq!(items.len(), 2, "no read filters by mode");
    for item in &items {
        assert_eq!(
            item["mode"],
            json!("standard"),
            "every item names its declaration's mode"
        );
    }
}

#[tokio::test]
async fn a_search_hit_names_the_field_that_matched_and_its_breadcrumb() {
    let h = RestHarness::new().await;
    h.inner
        .declare("proxy_host", "cascading", json!("direct"))
        .await;

    let items = h
        .items("/settings-service/v1/search?q=proxy", h.inner.tree.root)
        .await;
    assert_eq!(items.len(), 1);
    let hit = &items[0];
    assert_eq!(hit["leaf_slug"], json!("proxy_host"));
    assert_eq!(hit["matched_field"], json!("key"));
    assert_eq!(hit["category"]["key"], json!("network"));
    assert_eq!(hit["mode"], json!("standard"));
    assert!(
        hit.get("scope").is_none(),
        "a declaration-level hit carries no scope: {hit}"
    );
}

#[tokio::test]
async fn an_override_hit_names_the_scope_it_is_set_at() {
    let h = RestHarness::new().await;
    let id = h
        .inner
        .declare("motto", "cascading", json!("nothing"))
        .await;
    h.inner.set(id, h.inner.tree.a, json!("alpha")).await;

    let items = h
        .items("/settings-service/v1/search?q=alpha", h.inner.tree.a)
        .await;
    assert_eq!(items.len(), 1, "{items:?}");
    let hit = &items[0];
    assert_eq!(hit["matched_field"], json!("value"));
    assert_eq!(hit["tenant_id"], json!(h.inner.tree.a.to_string()));
    assert_eq!(hit["value"], json!("alpha"));
    assert_eq!(hit["scope"], json!(format!("/tenants/{}", h.inner.tree.a)));
}

// ── The review listing ───────────────────────────────────────────────────────

#[tokio::test]
async fn the_review_listing_returns_the_flagged_rows_of_the_subtree_not_resolved_values() {
    // `needs_review eq true` is a different question from browsing: it asks
    // which stored overrides stopped validating, so it answers rows — each at
    // the scope that holds it — rather than one effective value per setting.
    let h = RestHarness::new().await;
    let flagged = h.inner.declare("flagged", "cascading", json!(true)).await;
    let sound = h.inner.declare("sound", "cascading", json!(true)).await;
    h.inner
        .set_flagged(flagged, h.inner.tree.a, json!(false))
        .await;
    h.inner.set(sound, h.inner.tree.a, json!(false)).await;

    let items = h
        .items(
            "/settings-service/v1/settings?$filter=needs_review%20eq%20true",
            h.inner.tree.root,
        )
        .await;
    assert_eq!(items.len(), 1, "one flagged row: {items:?}");
    let entry = &items[0];
    assert_eq!(entry["outcome"], json!("needs_review"));
    assert_eq!(
        entry["flagged"]["tenant_id"],
        json!(h.inner.tree.a.to_string()),
        "the row names the scope that holds it"
    );
    assert_eq!(entry["flagged"]["value"], json!(false));
    assert!(
        entry["flagged"]["etag"].is_string(),
        "and carries the tag a correcting write must present: {entry}"
    );
    assert_eq!(entry["mode"], json!("standard"));
    assert!(
        entry.get("effective").is_none(),
        "a flagged row is not a resolved value: {entry}"
    );
}

#[tokio::test]
async fn the_review_listing_stops_at_the_targets_own_subtree() {
    // A flagged row under a sibling is another administrator's to fix.
    let h = RestHarness::new().await;
    let id = h.inner.declare("flagged", "cascading", json!(true)).await;
    h.inner.set_flagged(id, h.inner.tree.c, json!(false)).await;

    let from_the_sibling = h
        .items(
            &format!(
                "/settings-service/v1/settings?tenant={}&$filter=needs_review%20eq%20true",
                h.inner.tree.a
            ),
            h.inner.tree.root,
        )
        .await;
    assert!(
        from_the_sibling.is_empty(),
        "a sibling's flagged row is not on this page: {from_the_sibling:?}"
    );

    let from_the_root = h
        .items(
            "/settings-service/v1/settings?$filter=needs_review%20eq%20true",
            h.inner.tree.root,
        )
        .await;
    assert_eq!(
        from_the_root.len(),
        1,
        "but the root sees it: {from_the_root:?}"
    );
}

#[tokio::test]
async fn the_review_listing_of_a_secret_shows_neither_the_value_nor_its_reference() {
    // A secret override is a row holding a store reference. The listing exists
    // so an administrator can find it and correct it, which needs the key and
    // the scope — not the value, and not the handle to the value either.
    let h = RestHarness::new().await;
    let id = h
        .inner
        .declare_typed("api_token", "cascading", json!(""), SECRET, "secret")
        .await;
    h.inner
        .set_flagged_secret(id, h.inner.tree.a, "credstore-ref-1")
        .await;

    let items = h
        .items(
            "/settings-service/v1/settings?$filter=needs_review%20eq%20true",
            h.inner.tree.root,
        )
        .await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["flagged"]["value"], json!(MASK_TOKEN));
    let wire = items[0].to_string();
    assert!(!wire.contains("credstore-ref-1"), "{wire}");
}

// ── Naming keys ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_named_key_with_no_declaration_gets_its_own_entry() {
    // The console asks about the keys one screen needs. A key that does not
    // exist is that key's outcome, not a failure of the request: the other
    // keys on the screen still have answers.
    let h = RestHarness::new().await;
    h.inner.declare("declared", "cascading", json!(true)).await;
    let filter = urlencoding(&format!(
        "key in ('{}','{}')",
        h.inner.key("declared"),
        h.inner.key("never_declared")
    ));

    let items = h
        .items(
            &format!("/settings-service/v1/settings?$filter={filter}"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(items.len(), 2, "both keys are answered: {items:?}");
    let absent = items
        .iter()
        .find(|i| {
            i["key"]
                .as_str()
                .is_some_and(|k| k.contains("never_declared"))
        })
        .expect("an entry for the key that does not exist");
    assert_eq!(absent["outcome"], json!("not_found"));
    assert!(
        absent.get("mode").is_none(),
        "a key with no declaration has no mode to report: {absent}"
    );
    let present = items
        .iter()
        .find(|i| i["key"].as_str().is_some_and(|k| k.contains("declared.")))
        .expect("an entry for the declared key");
    assert_eq!(present["outcome"], json!("resolved"));
}

#[tokio::test]
async fn a_named_key_hidden_from_the_caller_is_reported_absent_like_any_other() {
    // Hidden is 404 everywhere, and the browse page's per-key entry is no
    // exception: the console must not be able to tell the two apart.
    let h = RestHarness::new().await;
    let concealed = h.inner.declare("concealed", "cascading", json!(true)).await;
    h.restrict(concealed, h.inner.tree.a, TenantAccess::Hidden)
        .await;
    let filter = urlencoding(&format!("key eq '{}'", h.inner.key("concealed")));

    let items = h
        .items(
            &format!("/settings-service/v1/settings?$filter={filter}"),
            h.inner.tree.a,
        )
        .await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["outcome"], json!("not_found"));
}

// ── History ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn history_returns_the_records_the_writes_left_newest_first() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let value = format!(
        "/settings-service/v1/settings/{}/value",
        encoded(&h, "proxy")
    );
    let first = h
        .send(
            "PUT",
            &value,
            Some(json!({ "value": false })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    let tag = first.body["etag"].as_str().expect("a tag").to_owned();
    h.send(
        "PUT",
        &value,
        Some(json!({ "value": true })),
        Some(&tag),
        h.inner.tree.root,
    )
    .await;

    let items = h
        .items(
            &format!(
                "/settings-service/v1/settings/{}/history",
                encoded(&h, "proxy")
            ),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(items.len(), 2, "one record per write: {items:?}");
    assert_eq!(items[0]["operation"], json!("change"), "newest first");
    assert_eq!(items[0]["pre_value"], json!(false));
    assert_eq!(items[0]["post_value"], json!(true));
    assert_eq!(items[0]["outcome"], json!("success"));
    assert!(items[0]["change_set_id"].is_string());
    assert!(!items[0]["values_masked"].as_bool().unwrap_or(true));
    assert!(!items[0]["actor_masked"].as_bool().unwrap_or(true));
    assert_eq!(items[1]["operation"], json!("create"));
    assert!(
        items[1].get("pre_value").is_none(),
        "a first write has no image before it: {}",
        items[1]
    );
}

#[tokio::test]
async fn history_of_a_secret_shows_the_mask_token_it_was_recorded_with() {
    // A secret is never recorded in plaintext, so there is no entitlement that
    // would unmask it here — the record itself holds the token.
    let h = RestHarness::new().await;
    h.inner
        .declare_typed("api_token", "cascading", json!(""), SECRET, "secret")
        .await;
    h.send(
        "PUT",
        &format!(
            "/settings-service/v1/settings/{}/value",
            encoded(&h, "api_token")
        ),
        Some(json!({ "value": "hunter2" })),
        Some("absent"),
        h.inner.tree.root,
    )
    .await;

    let items = h
        .items(
            &format!(
                "/settings-service/v1/settings/{}/history",
                encoded(&h, "api_token")
            ),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["post_value"], json!(MASK_TOKEN));
    let wire = items[0].to_string();
    assert!(!wire.contains("hunter2"), "{wire}");
}

#[tokio::test]
async fn history_is_read_per_scope() {
    // Each scope's trail is its own: a change at one tenant is not in another
    // tenant's history, or an administrator would read changes they cannot see
    // the values of.
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    h.send(
        "PUT",
        &format!(
            "/settings-service/v1/settings/{}/value?tenant={}",
            encoded(&h, "proxy"),
            h.inner.tree.a
        ),
        Some(json!({ "value": false })),
        Some("absent"),
        h.inner.tree.root,
    )
    .await;
    let history = |tenant| {
        format!(
            "/settings-service/v1/settings/{}/history?tenant={tenant}",
            encoded(&h, "proxy")
        )
    };

    assert_eq!(
        h.items(&history(h.inner.tree.a), h.inner.tree.root)
            .await
            .len(),
        1
    );
    assert!(
        h.items(&history(h.inner.tree.b), h.inner.tree.root)
            .await
            .is_empty(),
        "a descendant that was never written has no trail of its own"
    );
}

#[tokio::test]
async fn history_of_a_retired_declaration_is_still_readable() {
    // Retiring stops new values; it does not erase what was done. The trail is
    // what an audit reads afterwards.
    let h = RestHarness::new().await;
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.send(
        "PUT",
        &format!(
            "/settings-service/v1/settings/{}/value",
            encoded(&h, "proxy")
        ),
        Some(json!({ "value": false })),
        Some("absent"),
        h.inner.tree.root,
    )
    .await;
    h.inner.retire(id).await;

    let (status, body) = h
        .get(
            &format!(
                "/settings-service/v1/settings/{}/history",
                encoded(&h, "proxy")
            ),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["items"].as_array().map(Vec::len),
        Some(1),
        "the record survives the retirement: {body}"
    );
}

#[tokio::test]
async fn history_of_a_setting_that_was_never_declared_is_absent() {
    let h = RestHarness::new().await;
    let (status, _) = h
        .get(
            &format!(
                "/settings-service/v1/settings/{}/history",
                encoded(&h, "never_declared")
            ),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 404);
}

// ── The unmask entitlement ───────────────────────────────────────────────────

#[tokio::test]
async fn a_pii_value_is_masked_from_a_caller_without_the_entitlement() {
    // Reading that a setting is configured and reading the personal data in it
    // are two decisions. A caller holding the first and not the second sees the
    // setting, its source and its tag — everything but the value.
    let h = RestHarness::without_pii_entitlement().await;
    let id = h
        .inner
        .declare_typed("contact_email", "cascading", json!(""), TEXT, "pii")
        .await;
    h.inner
        .set(id, h.inner.tree.root, json!("someone@example.test"))
        .await;

    let (status, body) = h
        .get(
            &format!(
                "/settings-service/v1/settings/{}",
                encoded(&h, "contact_email")
            ),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "the read itself is allowed: {body}");
    assert_eq!(body["value"], json!(MASK_TOKEN));
    assert_eq!(
        body["source"],
        json!("own_override"),
        "the source still shows"
    );
    assert!(body["etag"].is_string());
    assert!(!body.to_string().contains("someone@example.test"));

    let items = h
        .items("/settings-service/v1/settings", h.inner.tree.root)
        .await;
    let entry = items
        .iter()
        .find(|i| {
            i["key"]
                .as_str()
                .is_some_and(|k| k.contains("contact_email"))
        })
        .expect("the setting is on the page");
    assert_eq!(entry["effective"]["value"], json!(MASK_TOKEN));
}

#[tokio::test]
async fn the_entitlement_unmasks_the_same_value() {
    // The counterpart, so the masking above is the entitlement's doing and not
    // the classification's alone.
    let h = RestHarness::new().await;
    let id = h
        .inner
        .declare_typed("contact_email", "cascading", json!(""), TEXT, "pii")
        .await;
    h.inner
        .set(id, h.inner.tree.root, json!("someone@example.test"))
        .await;

    let (_, body) = h
        .get(
            &format!(
                "/settings-service/v1/settings/{}",
                encoded(&h, "contact_email")
            ),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(body["value"], json!("someone@example.test"));
}

#[tokio::test]
async fn history_of_a_pii_setting_masks_both_images_without_the_entitlement() {
    // The trail records what the value was. Without the entitlement it reports
    // that a change happened, who made it and when, and not what the value was.
    let h = RestHarness::without_pii_entitlement().await;
    h.inner
        .declare_typed("contact_email", "cascading", json!(""), TEXT, "pii")
        .await;
    let uri = format!(
        "/settings-service/v1/settings/{}/value",
        encoded(&h, "contact_email")
    );
    let first = h
        .send(
            "PUT",
            &uri,
            Some(json!({ "value": "first@example.test" })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    let tag = first.body["etag"].as_str().expect("a tag").to_owned();
    h.send(
        "PUT",
        &uri,
        Some(json!({ "value": "second@example.test" })),
        Some(&tag),
        h.inner.tree.root,
    )
    .await;

    let items = h
        .items(
            &format!(
                "/settings-service/v1/settings/{}/history",
                encoded(&h, "contact_email")
            ),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(items.len(), 2, "{items:?}");
    let latest = &items[0];
    assert!(latest["values_masked"].as_bool().unwrap_or(false));
    assert_eq!(latest["pre_value"], json!(MASK_TOKEN));
    assert_eq!(latest["post_value"], json!(MASK_TOKEN));
    assert_eq!(
        latest["operation"],
        json!("change"),
        "the change still shows"
    );
    assert!(latest["occurred_at"].is_string());
    let wire = items.iter().map(ToString::to_string).collect::<String>();
    assert!(!wire.contains("@example.test"), "{wire}");
}

#[tokio::test]
async fn a_setting_that_cannot_be_resolved_carries_its_own_outcome_on_the_page() {
    // One entry failing is that entry's outcome, never the page's: a retired
    // setting beside a live one must not cost the client the live one's value.
    let h = RestHarness::new().await;
    let retired = h.inner.declare("retired", "cascading", json!(true)).await;
    h.inner.declare("live", "cascading", json!(true)).await;
    h.inner.retire(retired).await;

    let items = h
        .items("/settings-service/v1/settings", h.inner.tree.root)
        .await;
    assert_eq!(items.len(), 2, "both are on the page: {items:?}");
    let gone = items
        .iter()
        .find(|i| i["key"].as_str().is_some_and(|k| k.contains("retired")))
        .expect("the retired setting is listed");
    assert_eq!(gone["outcome"], json!("retired"));
    assert!(
        gone.get("effective").is_none(),
        "with no value to report: {gone}"
    );
    assert!(gone["detail"].is_string(), "and a reason: {gone}");
    assert_eq!(
        gone["mode"],
        json!("standard"),
        "the tag survives the failure"
    );

    let live = items
        .iter()
        .find(|i| i["key"].as_str().is_some_and(|k| k.contains("live")))
        .expect("the live setting is listed");
    assert_eq!(live["outcome"], json!("resolved"));
}

#[tokio::test]
async fn the_search_corpus_follows_the_callers_entitlement_through_the_enforcer() {
    // The handler decides the corpus from the authorization decision on
    // `read_unmasked`, before the query runs. Driven through the real
    // enforcer wiring: a needle present only in the overrides — one `pii`,
    // one `public`, one `secret` — is found where the caller may look, and is
    // neither matched nor counted where they may not.
    for (h, entitled) in [
        (RestHarness::without_pii_entitlement().await, false),
        (RestHarness::new().await, true),
    ] {
        let root = h.inner.tree.root;
        let pii = h
            .inner
            .declare_typed("contact_email", "cascading", json!(""), TEXT, "pii")
            .await;
        let public = h
            .inner
            .declare_typed("support_email", "cascading", json!(""), TEXT, "public")
            .await;
        let secret = h
            .inner
            .declare_typed(
                "api_token",
                "cascading",
                json!(""),
                crate::test_support::SECRET,
                "secret",
            )
            .await;
        // The rows as the writer leaves them: the classification denormalized
        // from the declaration, which is the column the corpus reads.
        h.inner
            .set_classified(pii, root, json!("ops@needle.example"), "pii")
            .await;
        h.inner
            .set(public, root, json!("help@needle.example"))
            .await;
        h.inner.set_secret(secret, root, "needle-reference").await;

        let items = h.items("/settings-service/v1/search?q=needle", root).await;
        let mut slugs: Vec<&str> = items
            .iter()
            .filter_map(|i| i["leaf_slug"].as_str())
            .collect();
        slugs.sort_unstable();
        if entitled {
            assert_eq!(
                slugs,
                vec!["contact_email", "support_email"],
                "with `read_unmasked`, pii is in the corpus; the secret never is"
            );
        } else {
            assert_eq!(
                slugs,
                vec!["support_email"],
                "without it, the pii override is neither matched nor counted"
            );
        }
    }
}

#[tokio::test]
async fn the_tag_a_flagged_row_is_listed_with_is_the_one_a_correcting_write_presents() {
    // The review listing exists so an administrator can fix what stopped
    // validating. The tag it hands out must be the value state tag the write
    // compares — the row's `last_change_at` — not its `updated_at`, which a
    // flag moves on its own: otherwise the listed tag is stale the moment the
    // row was flagged and the correction is refused 412 forever.
    use crate::domain::value::ValueRepository;
    use crate::infra::storage::value_repo::ValueRepo;
    use toolkit_security::AccessScope;

    let h = RestHarness::new().await;
    let id = h.inner.declare("port_like", "cascading", json!(true)).await;
    let root = h.inner.tree.root;
    h.inner.set(id, root, json!(false)).await;
    // Flag it the way a revalidation does: `updated_at` moves, the value and
    // its `last_change_at` do not.
    {
        let conn = h.inner.db.conn().expect("connection");
        let scope = AccessScope::allow_all();
        let row = ValueRepo
            .find_all(&conn, &scope, id)
            .await
            .expect("rows")
            .remove(0);
        ValueRepo
            .flag(
                &conn,
                &scope,
                row.id,
                Some("no longer validates".to_owned()),
            )
            .await
            .expect("flagged");
    }

    let items = h
        .items(
            "/settings-service/v1/settings?$filter=needs_review%20eq%20true",
            root,
        )
        .await;
    let tag = items[0]["flagged"]["etag"]
        .as_str()
        .expect("the listed tag")
        .to_owned();

    let uri = format!(
        "/settings-service/v1/settings/{}/value",
        h.inner.key("port_like").to_string().replace('~', "%7E")
    );
    let answer = h
        .send(
            "PUT",
            &uri,
            Some(json!({ "value": true })),
            Some(&tag),
            root,
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "the listed tag lets the correction through: {}",
        answer.body
    );
}

#[tokio::test]
async fn history_shows_the_settings_definition_records_beside_the_scopes_and_tells_them_apart() {
    // A scope's history is its own records plus the setting's definition
    // records — the declaration created, changed, retired — which belong to no
    // tenant: "the platform changed the default" is part of why a scope's
    // effective value moved. Each item says which it is, so a caller can
    // separate them.
    let h = RestHarness::new().await;
    let created = h
        .send(
            "POST",
            "/settings-service/v1/declarations",
            Some(json!({
                "value_type_id": BOOL,
                "vendor": "acme",
                "name": "proxy",
                "category_id": h.inner.category_id(),
                "default_value": true,
                "scope_class": "cascading",
            })),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.body);
    let key = created.body["key"]
        .as_str()
        .expect("a key")
        .replace('~', "%7E");
    let a = h.inner.tree.a;
    let written = h
        .send(
            "PUT",
            &format!("/settings-service/v1/settings/{key}/value?tenant={a}"),
            Some(json!({ "value": false })),
            Some("absent"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(written.status, 200, "{}", written.body);

    let items = h
        .items(
            &format!("/settings-service/v1/settings/{key}/history?tenant={a}"),
            h.inner.tree.root,
        )
        .await;
    let scoped = items
        .iter()
        .filter(|i| i["tenant_id"] == json!(a.to_string()))
        .count();
    let definition: Vec<_> = items.iter().filter(|i| i["tenant_id"].is_null()).collect();
    assert_eq!(scoped, 1, "the value write at `a`: {items:?}");
    assert_eq!(definition.len(), 1, "the declaration's creation: {items:?}");
    assert_eq!(definition[0]["operation"], json!("create"));

    // Another scope sees the same definition record and none of `a`'s.
    let b = h.inner.tree.b;
    let items = h
        .items(
            &format!("/settings-service/v1/settings/{key}/history?tenant={b}"),
            h.inner.tree.root,
        )
        .await;
    assert!(items.iter().all(|i| i["tenant_id"].is_null()), "{items:?}");
    assert_eq!(items.len(), 1);
}
