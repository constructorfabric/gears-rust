// Created: 2026-09-17 by Constructor Tech
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
use crate::test_support::{BOOL, RestHarness};

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
