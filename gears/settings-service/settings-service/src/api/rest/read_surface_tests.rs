//! The read surface driven as a client drives it: a request through the router
//! the gear registers, and the status code the API promises.
//!
//! What only a request exercises is what only a request can break. The query
//! string is parsed by the same extractors, an option the resource does not
//! take is refused where a client would meet it, a domain refusal is rendered
//! by the error layer into its status, and a setting hidden from the caller
//! has to disappear from every one of these paths rather than from the one
//! that happened to have a unit test.

use serde_json::{Value, json};
use uuid::Uuid;

use crate::domain::access::TenantAccess;
use crate::domain::resolution::MASK_TOKEN;
use crate::test_support::{BOOL, RestHarness, SECRET, TEXT};

/// The review listing: the flagged overrides of the target's subtree.
const REVIEW: &str = "/settings-service/v1/settings/review";

/// The narrowed browse: the settings whose own override at the target is
/// flagged.
const NARROWED: &str = "/settings-service/v1/settings?$filter=needs_review%20eq%20true";

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
async fn a_subtree_past_the_budget_is_refused_by_search_and_the_review_listing() {
    // The corpus of a search and the rows of the review listing are the
    // target's subtree. A subtree the budget cuts would make the answer
    // silently incomplete, so those two refuse and name the bound instead.
    // The browse, narrowed or not, reads the target's own rows only and never
    // walks the subtree, so the budget is nothing it can trip over.
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

    let (status, body) = h.get(REVIEW, h.inner.tree.root).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        body["context"]["field_violations"][0]["reason"],
        json!("subtree_too_large"),
        "{body}"
    );

    // The plain browse never walks the subtree and is untouched, and neither
    // is the narrowed one: it asks about the target's own overrides.
    for uri in [
        "/settings-service/v1/settings",
        "/settings-service/v1/settings?$filter=needs_review%20eq%20true",
    ] {
        let (status, body) = h.get(uri, h.inner.tree.root).await;
        assert_eq!(status, 200, "{uri}: {body}");
    }
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
async fn search_takes_a_filter_and_refuses_orderby_and_select() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;

    for option in ["&$orderby=key", "&$select=key"] {
        let uri = format!("/settings-service/v1/search?q=proxy{option}");
        let (status, body) = h.get(&uri, h.inner.tree.root).await;
        assert_eq!(status, 400, "{option}: {body}");
    }
    // A filter in browse's grammar is served; outside it, refused with the
    // same refusals browse gives.
    let (status, body) = h
        .get(
            "/settings-service/v1/search?q=proxy&$filter=key%20eq%20%27x%27",
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    for (filter, said) in [
        ("tenant%20eq%20%27x%27", "tenant"),
        // The refusal names the expression and is reported against `$filter`;
        // the sentence around it is the handler's to word.
        ("needs_review%20eq%20false", "needs_review eq false"),
    ] {
        let uri = format!("/settings-service/v1/search?q=proxy&$filter={filter}");
        let (status, body) = h.get(&uri, h.inner.tree.root).await;
        assert_eq!(status, 400, "{filter}: {body}");
        assert!(body.to_string().contains(said), "{filter}: {body}");
        assert_eq!(
            body["context"]["field_violations"][0]["field"],
            json!("$filter"),
            "{filter}: {body}"
        );
        // The same expression, the same answer on browse.
        let (status, browse) = h
            .get(
                &format!("/settings-service/v1/settings?$filter={filter}"),
                h.inner.tree.root,
            )
            .await;
        assert_eq!(status, 400, "{filter}: {browse}");
        assert!(browse.to_string().contains(said), "{filter}: {browse}");
        assert_eq!(
            browse["context"]["field_violations"][0]["field"],
            json!("$filter"),
            "{filter}: {browse}"
        );
    }
}

/// Twelve settings whose keys all match `se` through the `settings` base
/// type: six in the harness's category, six in `other`; two of them, one in
/// each category, flagged for review at the root.
async fn corpus_for_filtering(h: &RestHarness) -> (Uuid, Vec<String>, Vec<String>) {
    let other = h.inner.add_category("other").await;
    let mut network = Vec::new();
    let mut in_other = Vec::new();
    for i in 0..6 {
        let name = format!("net_{i}");
        let id = h.inner.declare(&name, "cascading", json!(true)).await;
        if i == 1 {
            h.inner
                .set_flagged(id, h.inner.tree.root, json!("bad"))
                .await;
        }
        network.push(h.inner.key(&name).to_string());
        let name = format!("oth_{i}");
        let id = h
            .inner
            .declare_in(other, &name, "cascading", json!(true), TEXT, "public")
            .await;
        if i == 4 {
            h.inner
                .set_flagged(id, h.inner.tree.root, json!("bad"))
                .await;
        }
        in_other.push(h.inner.key(&name).to_string());
    }
    (other, network, in_other)
}

/// Follow a search to its end, one page at a time.
async fn every_page(h: &RestHarness, first: &str) -> (Vec<Value>, usize) {
    let mut items = Vec::new();
    let mut pages = 0;
    let mut uri = first.to_owned();
    loop {
        let (status, body) = h.get(&uri, h.inner.tree.root).await;
        assert_eq!(status, 200, "{uri}: {body}");
        pages += 1;
        items.extend(body["items"].as_array().expect("items").iter().cloned());
        match body["page_info"]["next_cursor"].as_str() {
            Some(cursor) => uri = format!("{first}&cursor={cursor}"),
            None => return (items, pages),
        }
    }
}

fn keys_of(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|i| i["key"].as_str().expect("a key").to_owned())
        .collect()
}

#[tokio::test]
async fn a_filtered_search_reaches_matches_beyond_the_first_page_and_pages_the_filtered_set() {
    // The defect: a client sieving the page it holds never sees a match past
    // it, and the cursor pages the unfiltered set. The filter is in the query:
    // every page holds only the filtered set, and the set is followed to its
    // end without a repeat or a gap.
    let h = RestHarness::new().await;
    let (other, _, in_other) = corpus_for_filtering(&h).await;

    let uri =
        format!("/settings-service/v1/search?q=se&limit=4&$filter=category_id%20eq%20{other}");
    let (items, pages) = every_page(&h, &uri).await;
    assert_eq!(pages, 2, "six matches over pages of four");
    let mut keys = keys_of(&items);
    let mut expected = in_other.clone();
    keys.sort();
    expected.sort();
    assert_eq!(
        keys, expected,
        "exactly the filtered set, no repeat, no gap"
    );
    assert!(
        items.iter().all(|i| i["category"]["id"] == json!(other)),
        "{items:?}"
    );

    // Without the filter the same search is twice the size.
    let (all, _) = every_page(&h, "/settings-service/v1/search?q=se&limit=4").await;
    assert_eq!(all.len(), 12);
}

#[tokio::test]
async fn each_filter_field_narrows_the_search_and_a_conjunction_narrows_further() {
    let h = RestHarness::new().await;
    let (other, network, in_other) = corpus_for_filtering(&h).await;
    let search =
        |filter: String| format!("/settings-service/v1/search?q=se&limit=1&$filter={filter}");

    // `needs_review eq true`: the flagged settings, in either category, found
    // past the first page of one.
    let (items, pages) = every_page(&h, &search("needs_review%20eq%20true".to_owned())).await;
    let mut keys = keys_of(&items);
    keys.sort();
    let mut flagged = vec![network[1].clone(), in_other[4].clone()];
    flagged.sort();
    assert_eq!((keys, pages), (flagged, 2));

    // `key eq` and `key in (...)`.
    let one = network[2].replace('~', "%7E");
    let (items, _) = every_page(&h, &search(format!("key%20eq%20%27{one}%27"))).await;
    assert_eq!(keys_of(&items), vec![network[2].clone()]);
    let two = in_other[0].replace('~', "%7E");
    let (items, _) = every_page(&h, &search(format!("key%20in%20(%27{one}%27,%27{two}%27)"))).await;
    let mut keys = keys_of(&items);
    keys.sort();
    let mut expected = vec![network[2].clone(), in_other[0].clone()];
    expected.sort();
    assert_eq!(keys, expected);

    // A conjunction: the flagged setting of one category alone.
    let (items, _) = every_page(
        &h,
        &search(format!(
            "category_id%20eq%20{other}%20and%20needs_review%20eq%20true"
        )),
    )
    .await;
    assert_eq!(keys_of(&items), vec![in_other[4].clone()]);
}

#[tokio::test]
async fn a_search_cursor_minted_under_one_filter_is_refused_under_another() {
    let h = RestHarness::new().await;
    let (other, _, _) = corpus_for_filtering(&h).await;
    let (_, first) = h
        .get(
            &format!("/settings-service/v1/search?q=se&limit=4&$filter=category_id%20eq%20{other}"),
            h.inner.tree.root,
        )
        .await;
    let cursor = first["page_info"]["next_cursor"]
        .as_str()
        .expect("a second page")
        .to_owned();

    for filter in ["", "&$filter=needs_review%20eq%20true"] {
        let (status, body) = h
            .get(
                &format!("/settings-service/v1/search?q=se&limit=4{filter}&cursor={cursor}"),
                h.inner.tree.root,
            )
            .await;
        assert_eq!(status, 400, "another filter, another search: {body}");
    }
    // The same filter, spelt with different spacing, continues.
    let (status, _) = h
        .get(
            &format!(
                "/settings-service/v1/search?q=se&limit=4&$filter=category_id%20%20eq%20{other}&cursor={cursor}"
            ),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn a_filtered_search_hides_what_an_unfiltered_one_hides() {
    let h = RestHarness::new().await;
    let (other, _, in_other) = corpus_for_filtering(&h).await;
    let hidden_key = in_other[0].clone();
    let hidden = h
        .inner
        .resolver
        .find_declaration(
            &h.inner.db.conn().expect("connection"),
            &settings_service_sdk::SettingKey::parse(&hidden_key).expect("key"),
        )
        .await
        .expect("lookup")
        .expect("declared")
        .id;
    h.restrict(hidden, h.inner.tree.a, TenantAccess::Hidden)
        .await;

    let items = h
        .items(
            &format!("/settings-service/v1/search?q=se&$filter=category_id%20eq%20{other}"),
            h.inner.tree.a,
        )
        .await;
    let keys = keys_of(&items);
    assert_eq!(keys.len(), 5);
    assert!(!keys.contains(&hidden_key), "{keys:?}");
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
async fn browse_orders_by_the_fields_it_advertises_and_refuses_the_rest_up_front() {
    // The browse pages declarations, so an order it accepts must be a
    // declaration column that is never empty: `needs_review` is not one, and
    // `mode`, `status` or `domain_affinity` are not what the page advertises —
    // the last would also break the page's cursor on its first empty value.
    let h = RestHarness::new().await;
    h.inner.declare("alpha", "cascading", json!(true)).await;
    for field in ["needs_review", "mode", "domain_affinity", "owner_module"] {
        let (status, body) = h
            .get(
                &format!("/settings-service/v1/settings?$orderby={field}%20asc"),
                h.inner.tree.root,
            )
            .await;
        assert_eq!(status, 400, "{field}: {body}");
        assert_eq!(
            body["context"]["field_violations"][0]["reason"],
            json!(crate::field::ODATA_UNSORTABLE_FIELD),
            "{field}: {body}"
        );
    }
    for field in ["key", "category_id"] {
        let (status, body) = h
            .get(
                &format!("/settings-service/v1/settings?$orderby={field}%20desc"),
                h.inner.tree.root,
            )
            .await;
        assert_eq!(status, 200, "{field}: {body}");
    }
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
        format!("{REVIEW}?tenant={outside}"),
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
    for path in ["/settings-service/v1/settings", REVIEW] {
        let uri = format!("{path}?tenant={}", h.inner.tree.s);
        let (status, _) = h.get(&uri, h.inner.tree.a).await;
        assert_eq!(status, 403, "{uri}");
    }
}

#[tokio::test]
async fn a_malformed_tenant_is_refused_before_anything_else() {
    let h = RestHarness::new().await;
    for uri in [
        "/settings-service/v1/settings?tenant=not-a-uuid",
        "/settings-service/v1/settings/review?tenant=not-a-uuid",
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
        REVIEW.to_owned(),
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

// ── The needs-review narrowing of the browse ─────────────────────────────────

#[tokio::test]
async fn the_narrowed_browse_keeps_the_settings_flagged_at_the_target_in_the_resolved_shape() {
    // `needs_review eq true` on the browse is a narrowing, not a second
    // listing: the page is the ordinary one, cut to the settings whose own
    // override at the target is flagged. The flagged value is never served —
    // the item carries the fallthrough with the flag beside it — and a flag
    // below the target does not mark the target's row.
    let h = RestHarness::new().await;
    let root = h.inner.tree.root;
    let here = h
        .inner
        .declare_labelled(
            "flagged_here",
            "cascading",
            json!(true),
            "Minimum TLS version",
        )
        .await;
    let below = h
        .inner
        .declare("flagged_below", "cascading", json!(true))
        .await;
    let sound = h.inner.declare("sound", "cascading", json!(true)).await;
    h.inner.set_flagged(here, root, json!(false)).await;
    h.inner
        .set_flagged(below, h.inner.tree.a, json!(false))
        .await;
    h.inner.set(sound, root, json!(false)).await;

    let items = h.items(NARROWED, root).await;
    assert_eq!(
        items.len(),
        1,
        "only the setting flagged at the target: {items:?}"
    );
    let item = &items[0];
    assert_eq!(item["key"], json!(h.inner.key("flagged_here").to_string()));
    assert_eq!(item["outcome"], json!("resolved"), "{item}");
    assert!(
        item.get("flagged").is_none(),
        "the flagged-row item is retired from the page: {item}"
    );
    assert_eq!(item["mode"], json!("standard"), "{item}");
    assert_eq!(item["display_name"], json!("Minimum TLS version"), "{item}");
    let effective = &item["effective"];
    assert_eq!(effective["needs_review"], json!(true), "{item}");
    assert_eq!(
        effective["needs_review_detail"],
        json!("no longer validates"),
        "{item}"
    );
    assert_eq!(
        effective["value"],
        json!(true),
        "the fallthrough is served, never the flagged value: {item}"
    );
    assert_eq!(effective["fallback"], json!(true), "{item}");
    assert!(effective["fallback_source"].is_string(), "{item}");
    assert_eq!(
        h.inner.hierarchy.subtree_calls(),
        0,
        "the narrowing reads the target's own rows and walks no subtree"
    );

    // The narrowing changes which settings are on the page, never the shape of
    // an item: the same setting on the unfiltered page has the same keys.
    let unfiltered = h.items("/settings-service/v1/settings", root).await;
    let same = unfiltered
        .iter()
        .find(|i| i["key"] == item["key"])
        .expect("the setting on the unfiltered page");
    let shape = |v: &Value| -> Vec<String> {
        let mut keys: Vec<String> = v.as_object().expect("an object").keys().cloned().collect();
        keys.sort();
        keys
    };
    assert_eq!(shape(item), shape(same), "{item} against {same}");
}

#[tokio::test]
async fn the_narrowed_browse_does_not_see_a_flag_that_sits_only_below_the_target() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("flagged", "cascading", json!(true)).await;
    h.inner.set_flagged(id, h.inner.tree.b, json!(false)).await;

    // `b` is below `a`: the setting is flagged in `a`'s subtree, not at `a`.
    let from_above = h
        .items(
            &format!(
                "/settings-service/v1/settings?tenant={}&$filter=needs_review%20eq%20true",
                h.inner.tree.a
            ),
            h.inner.tree.root,
        )
        .await;
    assert!(
        from_above.is_empty(),
        "a flag below the target is not the target's own: {from_above:?}"
    );

    let at_the_row = h
        .items(
            &format!(
                "/settings-service/v1/settings?tenant={}&$filter=needs_review%20eq%20true",
                h.inner.tree.b
            ),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(
        at_the_row.len(),
        1,
        "but the tenant holding it sees it: {at_the_row:?}"
    );
}

#[tokio::test]
async fn the_narrowed_browse_refuses_needs_review_eq_false() {
    let h = RestHarness::new().await;
    h.inner.declare("alpha", "cascading", json!(true)).await;

    let (status, body) = h
        .get(
            "/settings-service/v1/settings?$filter=needs_review%20eq%20false",
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        body["context"]["field_violations"][0]["field"],
        json!("$filter"),
        "{body}"
    );
    assert!(
        body.to_string().contains("needs_review eq false"),
        "the refusal names the expression: {body}"
    );
}

#[tokio::test]
async fn the_narrowed_browse_pages_without_a_repeat_or_a_gap() {
    // The narrowing is part of the page query, so every page is full of
    // flagged settings and the cursor walks the narrowed set: a narrowing
    // applied to the page after it was cut would return short pages and lose
    // the flagged ones that sit past them.
    let h = RestHarness::new().await;
    let root = h.inner.tree.root;
    let mut flagged = Vec::new();
    for i in 0..5 {
        let name = format!("flag_{i}");
        let id = h.inner.declare(&name, "cascading", json!(true)).await;
        h.inner.set_flagged(id, root, json!(false)).await;
        flagged.push(h.inner.key(&name).to_string());
        let name = format!("clean_{i}");
        let id = h.inner.declare(&name, "cascading", json!(true)).await;
        h.inner.set(id, root, json!(false)).await;
    }

    let (items, pages) = every_page(&h, &format!("{NARROWED}&limit=2")).await;
    assert_eq!(pages, 3, "five flagged settings over pages of two");
    let mut keys = keys_of(&items);
    keys.sort();
    flagged.sort();
    assert_eq!(keys, flagged, "exactly the flagged set, no repeat, no gap");
}

#[tokio::test]
async fn a_named_key_that_is_not_flagged_is_absent_from_the_narrowed_browse_not_not_found() {
    // The narrowing asks which settings are flagged, so a visible key whose
    // own override is sound is simply not an answer. A key that does not exist
    // is a different matter: it keeps the entry every named key gets.
    let h = RestHarness::new().await;
    let root = h.inner.tree.root;
    let flagged = h.inner.declare("flagged", "cascading", json!(true)).await;
    let sound = h.inner.declare("sound", "cascading", json!(true)).await;
    h.inner.declare("untouched", "cascading", json!(true)).await;
    h.inner.set_flagged(flagged, root, json!(false)).await;
    h.inner.set(sound, root, json!(false)).await;

    let filter = urlencoding(&format!(
        "key in ('{}','{}','{}','{}') and needs_review eq true",
        h.inner.key("flagged"),
        h.inner.key("sound"),
        h.inner.key("untouched"),
        h.inner.key("never_declared")
    ));
    let items = h
        .items(
            &format!("/settings-service/v1/settings?$filter={filter}"),
            root,
        )
        .await;
    let by_key = |needle: &str| {
        items
            .iter()
            .find(|i| i["key"].as_str().is_some_and(|k| k.contains(needle)))
    };
    assert_eq!(
        by_key("flagged.").map(|i| i["outcome"].clone()),
        Some(json!("resolved")),
        "{items:?}"
    );
    assert!(
        by_key("sound.").is_none() && by_key("untouched.").is_none(),
        "a visible key that is not flagged is absent, not reported not_found: {items:?}"
    );
    assert_eq!(
        by_key("never_declared").map(|i| i["outcome"].clone()),
        Some(json!("not_found")),
        "a key that does not exist still gets its entry: {items:?}"
    );
    assert_eq!(items.len(), 2, "{items:?}");
}

#[tokio::test]
async fn under_the_narrowing_a_named_key_hidden_from_the_caller_is_not_found() {
    // The narrowing must not turn the per-key entry into an existence oracle:
    // a key hidden from the caller is answered as one that does not exist,
    // flagged or not, while a visible flagged one is served. (A key in another
    // administrative domain is the same query predicate, pinned on the
    // repository: this surface's resource declares no domain property, so no
    // decision can carry one to it.)
    let h = RestHarness::new().await;
    let a = h.inner.tree.a;
    let concealed = h.inner.declare("concealed", "cascading", json!(true)).await;
    let open = h.inner.declare("open", "cascading", json!(true)).await;
    h.restrict(concealed, a, TenantAccess::Hidden).await;
    h.inner.set_flagged(concealed, a, json!(false)).await;
    h.inner.set_flagged(open, a, json!(false)).await;

    let filter = urlencoding(&format!(
        "key in ('{}','{}') and needs_review eq true",
        h.inner.key("concealed"),
        h.inner.key("open"),
    ));
    let items = h
        .items(
            &format!("/settings-service/v1/settings?$filter={filter}"),
            a,
        )
        .await;
    let outcome_of = |needle: &str| {
        items
            .iter()
            .find(|i| i["key"].as_str().is_some_and(|k| k.contains(needle)))
            .map(|i| i["outcome"].clone())
    };
    assert_eq!(
        outcome_of("concealed."),
        Some(json!("not_found")),
        "hidden: {items:?}"
    );
    assert_eq!(
        outcome_of("open."),
        Some(json!("resolved")),
        "visible and flagged: {items:?}"
    );
    assert_eq!(items.len(), 2, "{items:?}");
}

// ── The review listing ───────────────────────────────────────────────────────

/// A tenant added below `parent` to the harness's tree.
fn grow(h: &RestHarness, parent: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    h.inner.hierarchy.add_tenant(id, parent);
    id
}

/// Five tenants a listing from the root reaches: the root, `a`, `b` below it,
/// `c` and a fresh one below `c`. The standalone `s` is not one of them.
fn five_tenants(h: &RestHarness) -> Vec<Uuid> {
    let tree = &h.inner.tree;
    let d = grow(h, tree.c);
    vec![tree.root, tree.a, tree.b, tree.c, d]
}

fn tenants_of(items: &[Value]) -> Vec<Uuid> {
    items
        .iter()
        .map(|i| {
            i["tenant_id"]
                .as_str()
                .and_then(|t| t.parse().ok())
                .expect("a tenant id")
        })
        .collect()
}

#[tokio::test]
async fn the_review_listing_returns_one_row_per_flagged_override_of_the_subtree() {
    // The listing answers rows — each at the scope that holds it, where it
    // must be fixed — and not one effective value per setting. A standalone
    // descendant's rows are its own administrators' and are not here.
    let h = RestHarness::new().await;
    let tree = &h.inner.tree;
    let x = h.inner.declare("x", "cascading", json!(true)).await;
    let y = h.inner.declare("y", "cascading", json!(true)).await;
    let sound = h.inner.declare("sound", "cascading", json!(true)).await;
    for tenant in [tree.root, tree.a, tree.s] {
        h.inner.set_flagged(x, tenant, json!(false)).await;
    }
    h.inner.set_flagged(y, tree.b, json!(false)).await;
    h.inner.set(sound, tree.a, json!(false)).await;

    let (status, body) = h.get(REVIEW, tree.root).await;
    assert_eq!(status, 200, "{body}");
    let items = body["items"].as_array().expect("items");
    let mut found: Vec<(String, Uuid)> = items
        .iter()
        .map(|i| {
            (
                i["key"].as_str().expect("key").to_owned(),
                tenants_of(std::slice::from_ref(i))[0],
            )
        })
        .collect();
    found.sort();
    let mut expected = vec![
        (h.inner.key("x").to_string(), tree.root),
        (h.inner.key("x").to_string(), tree.a),
        (h.inner.key("y").to_string(), tree.b),
    ];
    expected.sort();
    assert_eq!(
        found, expected,
        "flagged rows of the root and its non-standalone descendants, nothing else: {items:?}"
    );
}

#[tokio::test]
async fn a_review_item_carries_the_row_and_none_of_the_resolved_shape() {
    let h = RestHarness::new().await;
    let tree = &h.inner.tree;
    let plain = h.inner.declare("plain", "cascading", json!(true)).await;
    let labelled = h
        .inner
        .declare_labelled("labelled", "cascading", json!(true), "Minimum TLS version")
        .await;
    h.inner.set_flagged(plain, tree.a, json!(false)).await;
    h.inner.set_flagged(labelled, tree.root, json!(false)).await;

    let items = h.items(REVIEW, tree.root).await;
    assert_eq!(items.len(), 2, "{items:?}");
    let own = |needle: &str| {
        items
            .iter()
            .find(|i| i["key"].as_str().is_some_and(|k| k.contains(needle)))
            .expect("an item for the setting")
    };

    let at_child = own("plain.");
    assert_eq!(at_child["mode"], json!("standard"), "{at_child}");
    assert_eq!(at_child["tenant_id"], json!(tree.a.to_string()));
    assert_eq!(
        at_child["scope"],
        json!(format!("/tenants/{}", tree.a)),
        "the scope path, in the form `source_scope` uses"
    );
    assert_eq!(
        at_child["value"],
        json!(false),
        "the stored value, as stored"
    );
    assert_eq!(at_child["masked"], json!(false));
    assert_eq!(
        at_child["needs_review_detail"],
        json!("no longer validates")
    );
    assert!(at_child["last_change_at"].is_string(), "{at_child}");
    assert_eq!(at_child["set_by"], json!(format!("admin-of-{}", tree.a)));
    assert!(at_child["etag"].is_string(), "{at_child}");
    assert!(
        at_child.get("display_name").is_none(),
        "a setting with no display name omits it, never derives one: {at_child}"
    );
    for retired in ["outcome", "effective", "flagged", "fallback"] {
        assert!(
            at_child.get(retired).is_none(),
            "a row is not a resolved value; `{retired}` has no place on it: {at_child}"
        );
    }

    let at_root = own("labelled.");
    assert_eq!(
        at_root["scope"],
        json!("/"),
        "platform scope is `/`: {at_root}"
    );
    assert_eq!(
        at_root["display_name"],
        json!("Minimum TLS version"),
        "{at_root}"
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
            &format!("{REVIEW}?tenant={}", h.inner.tree.a),
            h.inner.tree.root,
        )
        .await;
    assert!(
        from_the_sibling.is_empty(),
        "a sibling's flagged row is not on this page: {from_the_sibling:?}"
    );

    let from_the_root = h.items(REVIEW, h.inner.tree.root).await;
    assert_eq!(
        from_the_root.len(),
        1,
        "but the root sees it: {from_the_root:?}"
    );
}

#[tokio::test]
async fn one_setting_flagged_across_more_tenants_than_a_page_continues_on_the_next_page() {
    // There is no row bound: a setting flagged at thousands of tenants spans
    // pages. The page is cut over the rows, so the cursor walks one setting's
    // rows and picks up exactly where the page ended.
    let h = RestHarness::new().await;
    let tenants = five_tenants(&h);
    let id = h.inner.declare("spread", "cascading", json!(true)).await;
    for tenant in &tenants {
        h.inner.set_flagged(id, *tenant, json!(false)).await;
    }

    let (items, pages) = every_page(&h, &format!("{REVIEW}?limit=2")).await;
    assert_eq!(pages, 3, "five rows over pages of two");
    let found = tenants_of(&items);
    let mut expected = tenants.clone();
    expected.sort();
    assert_eq!(
        found, expected,
        "all five rows, in tenant order, with no repeat and no gap"
    );
}

#[tokio::test]
async fn the_review_listing_is_ordered_by_declaration_then_tenant() {
    // The order is fixed and total — `uq_value_scope` makes the pair unique
    // for subject-less rows — which is what a keyset cursor needs.
    let h = RestHarness::new().await;
    let tenants = five_tenants(&h);
    let first = h.inner.declare("first", "cascading", json!(true)).await;
    let second = h.inner.declare("second", "cascading", json!(true)).await;
    let mut expected = Vec::new();
    for (declaration, rows) in [(first, 5), (second, 3)] {
        for tenant in tenants.iter().take(rows) {
            h.inner
                .set_flagged(declaration, *tenant, json!(false))
                .await;
            expected.push((declaration, *tenant));
        }
    }
    expected.sort();

    let (items, pages) = every_page(&h, &format!("{REVIEW}?limit=3")).await;
    assert_eq!(pages, 3, "eight rows over pages of three");
    let declaration_of = |key: &str| {
        if key.contains("first.") {
            first
        } else {
            second
        }
    };
    let found: Vec<(Uuid, Uuid)> = items
        .iter()
        .map(|i| {
            (
                declaration_of(i["key"].as_str().expect("key")),
                tenants_of(std::slice::from_ref(i))[0],
            )
        })
        .collect();
    assert_eq!(
        found, expected,
        "rows sorted by (declaration id, tenant id)"
    );
}

#[tokio::test]
async fn the_review_listing_is_narrowed_by_category_and_by_key() {
    let h = RestHarness::new().await;
    let root = h.inner.tree.root;
    let other = h.inner.add_category("other").await;
    let here = h.inner.declare("here", "cascading", json!(true)).await;
    let there = h
        .inner
        .declare_in(other, "there", "cascading", json!(true), TEXT, "public")
        .await;
    let sound = h.inner.declare("sound", "cascading", json!(true)).await;
    h.inner.set_flagged(here, root, json!(false)).await;
    h.inner.set_flagged(there, root, json!("bad")).await;
    h.inner.set(sound, root, json!(false)).await;

    let items = h
        .items(
            &format!("{REVIEW}?$filter=category_id%20eq%20{other}"),
            root,
        )
        .await;
    assert_eq!(
        keys_of(&items),
        vec![h.inner.key("there").to_string()],
        "the one category's rows"
    );

    // A named key with no flagged row is simply absent: no entry, no outcome.
    let filter = urlencoding(&format!(
        "key in ('{}','{}','{}')",
        h.inner.key("here"),
        h.inner.key("sound"),
        h.inner.key("never_declared")
    ));
    let items = h.items(&format!("{REVIEW}?$filter={filter}"), root).await;
    assert_eq!(
        keys_of(&items),
        vec![h.inner.key("here").to_string()],
        "the unflagged and the undeclared key have no item: {items:?}"
    );
    assert!(items[0].get("outcome").is_none(), "{items:?}");
}

#[tokio::test]
async fn the_review_listing_refuses_what_it_does_not_take() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("flagged", "cascading", json!(true)).await;
    h.inner
        .set_flagged(id, h.inner.tree.root, json!(false))
        .await;

    // `needs_review` is the listing itself, so naming it is refused; so is a
    // field the filter does not map.
    for filter in ["needs_review%20eq%20true", "tenant%20eq%20%27x%27"] {
        let (status, body) = h
            .get(&format!("{REVIEW}?$filter={filter}"), h.inner.tree.root)
            .await;
        assert_eq!(status, 400, "{filter}: {body}");
        assert_eq!(
            body["context"]["field_violations"][0]["field"],
            json!("$filter"),
            "{filter}: {body}"
        );
    }

    // The order is fixed and a projection would vary the shape; both refused
    // as unsupported options, not silently ignored.
    for option in ["$orderby=key%20asc", "$select=key"] {
        let (status, body) = h
            .get(&format!("{REVIEW}?{option}"), h.inner.tree.root)
            .await;
        assert_eq!(status, 400, "{option}: {body}");
        assert_eq!(
            body["context"]["field_violations"][0]["reason"],
            json!(crate::field::ODATA_UNSUPPORTED_OPTION),
            "{option}: {body}"
        );
    }
}

#[tokio::test]
async fn a_review_cursor_is_bound_to_its_filter_and_its_target() {
    let h = RestHarness::new().await;
    let tenants = five_tenants(&h);
    let id = h.inner.declare("spread", "cascading", json!(true)).await;
    for tenant in &tenants {
        h.inner.set_flagged(id, *tenant, json!(false)).await;
    }
    let category = h.inner.category_id();
    let other = h.inner.add_category("other").await;
    let (status, first) = h
        .get(
            &format!("{REVIEW}?limit=2&$filter=category_id%20eq%20{category}"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{first}");
    let cursor = first["page_info"]["next_cursor"]
        .as_str()
        .expect("a second page")
        .to_owned();

    // The same filter and target continue it.
    let (status, body) = h
        .get(
            &format!("{REVIEW}?limit=2&$filter=category_id%20eq%20{category}&cursor={cursor}"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{body}");

    // Another filter, or none, is another listing; so is another target.
    for (what, uri) in [
        (
            "another filter",
            format!("{REVIEW}?limit=2&$filter=category_id%20eq%20{other}&cursor={cursor}"),
        ),
        ("no filter", format!("{REVIEW}?limit=2&cursor={cursor}")),
        (
            "another tenant",
            format!(
                "{REVIEW}?limit=2&tenant={}&$filter=category_id%20eq%20{category}&cursor={cursor}",
                h.inner.tree.a
            ),
        ),
    ] {
        let (status, body) = h.get(&uri, h.inner.tree.root).await;
        assert_eq!(
            status, 400,
            "{what} is refused before any row is read: {body}"
        );
    }
}

#[tokio::test]
async fn a_hidden_setting_has_no_rows_on_the_review_listing() {
    let h = RestHarness::new().await;
    let tree = &h.inner.tree;
    let visible = h.inner.declare("visible", "cascading", json!(true)).await;
    let concealed = h.inner.declare("concealed", "cascading", json!(true)).await;
    h.inner.set_flagged(visible, tree.b, json!(false)).await;
    h.inner.set_flagged(concealed, tree.b, json!(false)).await;
    h.restrict(concealed, tree.a, TenantAccess::Hidden).await;

    let items = h.items(REVIEW, tree.a).await;
    assert_eq!(
        keys_of(&items),
        vec![h.inner.key("visible").to_string()],
        "a setting hidden from the caller leaves the listing silently: {items:?}"
    );
}

#[tokio::test]
async fn the_review_listing_refuses_a_target_the_caller_may_not_administer() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("flagged", "cascading", json!(true)).await;
    h.inner.set_flagged(id, h.inner.tree.c, json!(false)).await;
    h.inner.set_flagged(id, h.inner.tree.s, json!(false)).await;

    // `c` is the caller's sibling; `s` is a standalone descendant of `a`.
    for target in [h.inner.tree.c, h.inner.tree.s] {
        let (status, body) = h
            .get(&format!("{REVIEW}?tenant={target}"), h.inner.tree.a)
            .await;
        assert_eq!(status, 403, "{target}: {body}");
        assert!(
            body.get("items").is_none(),
            "a refusal carries no rows: {body}"
        );
    }
}

#[tokio::test]
async fn the_review_listing_of_a_secret_shows_neither_the_value_nor_its_reference() {
    // A secret override is a row holding a store reference. The listing exists
    // so an administrator can find it and correct it, which needs the key and
    // the scope — not the value, and not the handle to the value either; with
    // the PII entitlement or without it, which is about `pii`, not `secret`.
    for h in [
        RestHarness::new().await,
        RestHarness::without_pii_entitlement().await,
    ] {
        let id = h
            .inner
            .declare_typed("api_token", "cascading", json!(""), SECRET, "secret")
            .await;
        h.inner
            .set_flagged_secret(id, h.inner.tree.a, "credstore-ref-1")
            .await;

        let items = h.items(REVIEW, h.inner.tree.root).await;
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0]["value"], json!(MASK_TOKEN));
        assert_eq!(items[0]["masked"], json!(true), "{}", items[0]);
        let wire = items[0].to_string();
        assert!(!wire.contains("credstore-ref-1"), "{wire}");
    }
}

#[tokio::test]
async fn a_pii_row_is_masked_on_the_review_listing_without_the_entitlement_and_shown_with_it() {
    // The value and the setter's identity are both personal data: masked for
    // a reader who may not see `pii` unmasked, shown to one who may.
    let entitled = RestHarness::new().await;
    let masked = RestHarness::without_pii_entitlement().await;
    for h in [&entitled, &masked] {
        let id = h
            .inner
            .declare_typed("support_email", "cascading", json!("x"), TEXT, "pii")
            .await;
        h.inner
            .set_flagged_classified(id, h.inner.tree.a, json!("ops@example.test"), "pii")
            .await;
    }

    let items = entitled.items(REVIEW, entitled.inner.tree.root).await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["value"], json!("ops@example.test"), "{}", items[0]);
    assert_eq!(items[0]["masked"], json!(false), "{}", items[0]);
    assert_eq!(
        items[0]["set_by"],
        json!(format!("admin-of-{}", entitled.inner.tree.a)),
        "{}",
        items[0]
    );

    let items = masked.items(REVIEW, masked.inner.tree.root).await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["value"], json!(MASK_TOKEN), "{}", items[0]);
    assert_eq!(items[0]["masked"], json!(true), "{}", items[0]);
    assert_eq!(items[0]["set_by"], json!(MASK_TOKEN), "{}", items[0]);
}

#[tokio::test]
async fn a_search_hit_for_a_flagged_override_is_not_a_match_on_its_value() {
    // A flagged override is not served — resolution falls through it — so the
    // search does not match on what it holds either: a needle found only in
    // the flagged value finds nothing, with the narrowing or without it.
    let h = RestHarness::new().await;
    let id = h
        .inner
        .declare("motto", "cascading", json!("nothing"))
        .await;
    h.inner
        .set_flagged(id, h.inner.tree.root, json!("zzneedle"))
        .await;

    for filter in ["", "&$filter=needs_review%20eq%20true"] {
        let items = h
            .items(
                &format!("/settings-service/v1/search?q=zzneedle{filter}"),
                h.inner.tree.root,
            )
            .await;
        assert!(
            items.is_empty(),
            "the value of a flagged row is not in the override match{filter}: {items:?}"
        );
    }
}

#[tokio::test]
async fn a_narrowed_search_keeps_the_settings_flagged_at_the_target_and_not_those_flagged_below() {
    let h = RestHarness::new().await;
    let tree = &h.inner.tree;
    let here = h
        .inner
        .declare("proxy_here", "cascading", json!(true))
        .await;
    let below = h
        .inner
        .declare("proxy_below", "cascading", json!(true))
        .await;
    let sound = h
        .inner
        .declare("proxy_sound", "cascading", json!(true))
        .await;
    h.inner.set_flagged(here, tree.root, json!(false)).await;
    h.inner.set_flagged(below, tree.a, json!(false)).await;
    h.inner.set(sound, tree.root, json!(false)).await;

    let slugs = |items: &[Value]| -> Vec<String> {
        let mut slugs: Vec<String> = items
            .iter()
            .filter_map(|i| i["leaf_slug"].as_str().map(str::to_owned))
            .collect();
        slugs.sort();
        slugs.dedup();
        slugs
    };
    let items = h
        .items(
            "/settings-service/v1/search?q=proxy&$filter=needs_review%20eq%20true",
            tree.root,
        )
        .await;
    assert_eq!(
        slugs(&items),
        vec!["proxy_here"],
        "only the setting whose own override at the target is flagged: {items:?}"
    );

    // From `a`, its own flagged override qualifies and the root's does not.
    let items = h
        .items(
            &format!(
                "/settings-service/v1/search?q=proxy&tenant={}&$filter=needs_review%20eq%20true",
                tree.a
            ),
            tree.root,
        )
        .await;
    assert_eq!(slugs(&items), vec!["proxy_below"], "{items:?}");
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
async fn a_pii_value_stays_masked_in_the_history_after_the_setting_is_declassified() {
    // An administrator may downgrade `pii` to `public` with step-up. The
    // values recorded while the setting was `pii` stay personal data: the
    // record keeps their class, so the history masks them for a reader
    // without the entitlement whatever the declaration says now, and shows
    // them to one with it.
    for (h, entitled) in [
        (RestHarness::new().await, true),
        (RestHarness::without_pii_entitlement().await, false),
    ] {
        let id = h
            .inner
            .declare_typed("contact", "cascading", json!("nobody"), TEXT, "pii")
            .await;
        let value = format!(
            "/settings-service/v1/settings/{}/value",
            encoded(&h, "contact")
        );
        let written = h
            .send(
                "PUT",
                &value,
                Some(json!({ "value": "alice@example.com" })),
                Some("absent"),
                h.inner.tree.root,
            )
            .await;
        assert_eq!(written.status, 200, "{}", written.body);
        h.inner.reclassify(id, "public").await;

        let items = h
            .items(
                &format!(
                    "/settings-service/v1/settings/{}/history",
                    encoded(&h, "contact")
                ),
                h.inner.tree.root,
            )
            .await;
        assert_eq!(items.len(), 1, "{items:?}");
        if entitled {
            assert_eq!(
                items[0]["post_value"],
                json!("alice@example.com"),
                "{}",
                items[0]
            );
            assert_eq!(items[0]["values_masked"], json!(false), "{}", items[0]);
        } else {
            assert_eq!(items[0]["post_value"], json!(MASK_TOKEN), "{}", items[0]);
            assert_eq!(items[0]["values_masked"], json!(true), "{}", items[0]);
        }
    }
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
async fn the_tag_a_review_row_is_listed_with_is_the_one_a_correcting_write_presents() {
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

    let items = h.items(REVIEW, root).await;
    assert_eq!(items.len(), 1, "{items:?}");
    let tag = items[0]["etag"]
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

#[tokio::test]
async fn setter_identity_is_masked_for_a_reader_without_the_pii_entitlement() {
    // Who set an ancestor's override is an administrator's identity: the trail
    // and the review listing show it to a reader who may read unmasked and
    // mask it for one who may not, as the audit history masks its actor.
    let entitled = RestHarness::new().await;
    let d = entitled
        .inner
        .declare("proxy", "cascading", json!(true))
        .await;
    entitled
        .inner
        .set(d, entitled.inner.tree.root, json!(false))
        .await;
    entitled
        .inner
        .set_flagged(d, entitled.inner.tree.a, json!("aggressive"))
        .await;
    let key = entitled.inner.key("proxy").to_string().replace('~', "%7E");
    let one = format!(
        "/settings-service/v1/settings/{key}?tenant={}",
        entitled.inner.tree.a
    );
    let flagged = REVIEW;

    let (status, body) = entitled.get(&one, entitled.inner.tree.root).await;
    assert_eq!(status, 200, "{body}");
    let trail = body["inheritance_trail"].as_array().expect("trail");
    assert!(
        trail.iter().any(|e| e["set_by"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s != MASK_TOKEN)),
        "an entitled reader sees who set it: {body}"
    );

    let masked = RestHarness::without_pii_entitlement().await;
    let d = masked
        .inner
        .declare("proxy", "cascading", json!(true))
        .await;
    masked
        .inner
        .set(d, masked.inner.tree.root, json!(false))
        .await;
    masked
        .inner
        .set_flagged(d, masked.inner.tree.a, json!("aggressive"))
        .await;
    let one = format!(
        "/settings-service/v1/settings/{key}?tenant={}",
        masked.inner.tree.a
    );
    let (status, body) = masked.get(&one, masked.inner.tree.root).await;
    assert_eq!(status, 200, "{body}");
    for entry in body["inheritance_trail"].as_array().expect("trail") {
        if entry["has_override"] == json!(true) {
            assert_eq!(
                entry["set_by"],
                json!(MASK_TOKEN),
                "masked on the trail: {entry}"
            );
        }
    }
    let (status, body) = entitled.get(flagged, entitled.inner.tree.root).await;
    assert_eq!(status, 200, "{body}");
    let items = body["items"].as_array().expect("items");
    assert!(!items.is_empty(), "{body}");
    for item in items {
        assert!(
            item["set_by"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && s != MASK_TOKEN),
            "an entitled reader sees who set the flagged row: {item}"
        );
    }
    let (status, body) = masked.get(flagged, masked.inner.tree.root).await;
    assert_eq!(status, 200, "{body}");
    let items = body["items"].as_array().expect("items");
    assert!(!items.is_empty(), "{body}");
    for item in items {
        assert_eq!(
            item["set_by"],
            json!(MASK_TOKEN),
            "masked on the listing: {item}"
        );
    }
}

// ── The total beside the cursors ─────────────────────────────────────────────

#[tokio::test]
async fn the_browse_total_counts_the_settings_after_the_hidden_exclusion() {
    // The same four settings as the page-cut test, one hidden from `a`. The
    // total is of what the walk holds for the caller — three for `a`, four
    // for the root — counted under the filter and never under the cursor, so
    // a short page and a later page report the same number.
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
    assert_eq!(body["items"].as_array().map(Vec::len), Some(2));
    assert_eq!(body["page_info"]["total_count"], json!(3), "{body}");
    let cursor = body["page_info"]["next_cursor"]
        .as_str()
        .expect("a third page")
        .to_owned();
    let (status, body) = h
        .get(
            &format!("/settings-service/v1/settings?limit=2&cursor={cursor}"),
            h.inner.tree.a,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["page_info"]["total_count"],
        json!(3),
        "same walk, same total"
    );

    let (status, body) = h
        .get("/settings-service/v1/settings?limit=2", h.inner.tree.root)
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["page_info"]["total_count"],
        json!(4),
        "nothing hidden from the root"
    );

    // By category, the total is the category's count.
    let (status, body) = h
        .get(
            &format!(
                "/settings-service/v1/settings?$filter=category_id%20eq%20{}",
                h.inner.category_id()
            ),
            h.inner.tree.a,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["page_info"]["total_count"], json!(3), "{body}");
}

#[tokio::test]
async fn the_narrowed_browse_counts_the_narrowed_set_and_the_review_listing_claims_no_total() {
    // Under `needs_review eq true` the browse pages the settings whose own
    // override at the target is flagged — narrowed in the page query — so its
    // total is of that set. The review listing pages flagged rows across
    // tenants, one setting to many rows, and claims no total rather than a
    // wrong one.
    let h = RestHarness::new().await;
    let flagged = h.inner.declare("flagged", "cascading", json!(true)).await;
    h.inner.declare("clean", "cascading", json!(true)).await;
    h.inner
        .set_flagged(flagged, h.inner.tree.a, json!(false))
        .await;
    h.inner
        .set_flagged(flagged, h.inner.tree.b, json!(false))
        .await;

    let (status, body) = h
        .get(
            "/settings-service/v1/settings?$filter=needs_review%20eq%20true",
            h.inner.tree.a,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        body["page_info"]["total_count"],
        json!(1),
        "the total is of the narrowed set: {body}"
    );

    let (status, body) = h
        .get(
            "/settings-service/v1/settings/review?limit=1",
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["items"].as_array().map(Vec::len), Some(1));
    assert!(
        body["page_info"].get("total_count").is_none(),
        "no total on the review listing: {body}"
    );
    assert!(body["page_info"]["limit"].is_number(), "{body}");
}

#[tokio::test]
async fn search_reports_how_many_settings_match_beyond_the_page() {
    let h = RestHarness::new().await;
    for name in ["px_alpha", "px_gamma", "px_delta"] {
        h.inner.declare(name, "cascading", json!(true)).await;
    }
    let concealed = h.inner.declare("px_beta", "cascading", json!(true)).await;
    h.restrict(concealed, h.inner.tree.a, TenantAccess::Hidden)
        .await;

    let (status, body) = h
        .get("/settings-service/v1/search?q=px_&limit=2", h.inner.tree.a)
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["items"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        body["page_info"]["total_count"],
        json!(3),
        "the hidden setting matches nothing for this caller: {body}"
    );
}

#[tokio::test]
async fn history_reports_how_many_records_the_pair_holds() {
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

    let (status, body) = h
        .get(
            &format!(
                "/settings-service/v1/settings/{}/history?limit=1",
                encoded(&h, "proxy")
            ),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["page_info"]["total_count"], json!(2), "{body}");
}

// ── The display name ─────────────────────────────────────────────────────────

#[tokio::test]
async fn a_browse_item_carries_the_display_name_when_set_and_omits_it_otherwise() {
    // The label rides beside `mode` on every browse item so a console renders
    // one page without a second read; an unset one is absent, not derived.
    let h = RestHarness::new().await;
    h.inner
        .declare_labelled("labelled", "cascading", json!(true), "Minimum TLS version")
        .await;
    h.inner.declare("bare", "cascading", json!(true)).await;

    let items = h
        .items("/settings-service/v1/settings", h.inner.tree.root)
        .await;
    assert_eq!(items.len(), 2, "{items:?}");
    for item in &items {
        let key = item["key"].as_str().expect("a key");
        if key.contains(".labelled.") {
            assert_eq!(item["display_name"], json!("Minimum TLS version"), "{item}");
        } else {
            assert!(
                item.get("display_name").is_none(),
                "an unset label is omitted, never derived from the slug: {item}"
            );
        }
    }
}

#[tokio::test]
async fn the_single_effective_read_never_carries_the_display_name() {
    // Value-only by design: a detail view composes the declaration read with
    // this one, as it does for the description.
    let h = RestHarness::new().await;
    h.inner
        .declare_labelled("labelled", "cascading", json!(true), "Minimum TLS version")
        .await;

    let uri = format!("/settings-service/v1/settings/{}", encoded(&h, "labelled"));
    let (status, body) = h.get(&uri, h.inner.tree.root).await;
    assert_eq!(status, 200, "{body}");
    assert!(
        body.get("display_name").is_none(),
        "the effective value read carries no label: {body}"
    );
}

#[tokio::test]
async fn a_search_hit_carries_the_display_name_when_set_and_omits_it_otherwise() {
    let h = RestHarness::new().await;
    h.inner
        .declare_labelled("proxy_host", "cascading", json!("direct"), "Proxy host")
        .await;
    h.inner
        .declare("proxy_port", "cascading", json!("direct"))
        .await;

    let items = h
        .items("/settings-service/v1/search?q=proxy", h.inner.tree.root)
        .await;
    assert_eq!(items.len(), 2, "{items:?}");
    for hit in &items {
        if hit["leaf_slug"] == json!("proxy_host") {
            assert_eq!(hit["display_name"], json!("Proxy host"), "{hit}");
        } else {
            assert!(hit.get("display_name").is_none(), "{hit}");
        }
    }
}

#[tokio::test]
async fn a_hit_found_only_in_the_display_name_names_it_and_carries_no_scope() {
    // What a person types is the label they see, not the key. The hit says the
    // label matched, and since the label belongs to the declaration rather
    // than to a tenant it carries no scope.
    let h = RestHarness::new().await;
    h.inner
        .declare_labelled("relay", "cascading", json!("direct"), "Minimum TLS version")
        .await;

    let items = h
        .items("/settings-service/v1/search?q=tls", h.inner.tree.root)
        .await;
    assert_eq!(items.len(), 1, "{items:?}");
    let hit = &items[0];
    assert_eq!(hit["matched_field"], json!("display_name"), "{hit}");
    assert_eq!(hit["display_name"], json!("Minimum TLS version"));
    assert!(
        hit.get("scope").is_none(),
        "a declaration-level hit carries no scope: {hit}"
    );
}

#[tokio::test]
async fn an_override_hit_carries_the_display_name_of_its_declaration() {
    let h = RestHarness::new().await;
    let id = h
        .inner
        .declare_labelled("motto", "cascading", json!("nothing"), "Company motto")
        .await;
    h.inner.set(id, h.inner.tree.a, json!("alpha")).await;

    let items = h
        .items("/settings-service/v1/search?q=alpha", h.inner.tree.a)
        .await;
    assert_eq!(items.len(), 1, "{items:?}");
    let hit = &items[0];
    assert_eq!(hit["matched_field"], json!("value"));
    assert_eq!(
        hit["display_name"],
        json!("Company motto"),
        "an override hit names its declaration's label too: {hit}"
    );
}

#[tokio::test]
async fn a_hidden_setting_is_not_found_through_its_display_name() {
    // The label is ordinary declaration text under the same visibility filter
    // as the key and the description: hiding a setting hides it from this path.
    let h = RestHarness::new().await;
    h.inner
        .declare_labelled("visible", "cascading", json!(true), "Secretive label")
        .await;
    let hidden = h
        .inner
        .declare_labelled("concealed", "cascading", json!(true), "Secretive label")
        .await;
    h.restrict(hidden, h.inner.tree.a, TenantAccess::Hidden)
        .await;

    let items = h
        .items("/settings-service/v1/search?q=secretive", h.inner.tree.a)
        .await;
    let slugs: Vec<&str> = items
        .iter()
        .filter_map(|i| i["leaf_slug"].as_str())
        .collect();
    assert_eq!(
        slugs,
        vec!["visible"],
        "a hidden setting is not discoverable through its label either"
    );
}

#[tokio::test]
async fn the_narrowed_browse_item_and_the_review_item_carry_the_display_name() {
    // A console renders the label of a flagged setting exactly as it does on
    // the resolved page, whether it came through the narrowed browse or the
    // review listing.
    let h = RestHarness::new().await;
    let flagged = h
        .inner
        .declare_labelled("flagged", "cascading", json!(true), "Minimum TLS version")
        .await;
    h.inner
        .set_flagged(flagged, h.inner.tree.a, json!(false))
        .await;

    let narrowed = h
        .items(
            &format!(
                "/settings-service/v1/settings?tenant={}&$filter=needs_review%20eq%20true",
                h.inner.tree.a
            ),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(narrowed.len(), 1, "{narrowed:?}");
    assert_eq!(narrowed[0]["outcome"], json!("resolved"));
    assert_eq!(
        narrowed[0]["display_name"],
        json!("Minimum TLS version"),
        "{}",
        narrowed[0]
    );

    let review = h.items(REVIEW, h.inner.tree.root).await;
    assert_eq!(review.len(), 1, "{review:?}");
    assert_eq!(
        review[0]["display_name"],
        json!("Minimum TLS version"),
        "{}",
        review[0]
    );
}

#[tokio::test]
async fn a_failed_entry_carries_the_display_name_and_a_not_found_one_has_none() {
    // A retired setting fails as its own entry yet still has a declaration to
    // label it; a requested key with no declaration has nothing to report.
    let h = RestHarness::new().await;
    let retired = h
        .inner
        .declare_labelled("retired", "cascading", json!(true), "Old label")
        .await;
    h.inner.retire(retired).await;

    let items = h
        .items("/settings-service/v1/settings", h.inner.tree.root)
        .await;
    let gone = items
        .iter()
        .find(|i| i["key"].as_str().is_some_and(|k| k.contains("retired")))
        .expect("the retired setting is listed");
    assert_eq!(gone["outcome"], json!("retired"));
    assert_eq!(gone["display_name"], json!("Old label"), "{gone}");

    let filter = urlencoding(&format!("key in ('{}')", h.inner.key("never_declared")));
    let items = h
        .items(
            &format!("/settings-service/v1/settings?$filter={filter}"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["outcome"], json!("not_found"));
    assert!(
        items[0].get("display_name").is_none(),
        "no declaration, no label: {}",
        items[0]
    );
}
