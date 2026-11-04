//! The browse text narrowing `q`, driven through the router: which settings a
//! needle keeps, what it never matches, how it is bounded, and how a page and
//! a cursor behave under it.

use serde_json::{Value, json};
use uuid::Uuid;

use crate::domain::access::TenantAccess;
use crate::domain::resolution::MASK_TOKEN;
use crate::test_support::{RestHarness, SECRET, TEXT};

const BROWSE: &str = "/settings-service/v1/settings";

fn urlencoding(raw: &str) -> String {
    raw.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => c.to_string(),
            other => format!("%{:02X}", other as u32),
        })
        .collect()
}

fn keys_of(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|i| i["key"].as_str().expect("a key").to_owned())
        .collect()
}

/// Whether some item's key contains `needle`.
fn has(items: &[Value], needle: &str) -> bool {
    items
        .iter()
        .any(|i| i["key"].as_str().is_some_and(|k| k.contains(needle)))
}

/// Walk every page from `first`, as the root administrator.
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

/// Thirty settings, every fifth a `hit_NN`, the rest `miss_NN`.
async fn hit_and_miss_corpus(h: &RestHarness) -> Vec<String> {
    let mut hits = Vec::new();
    for i in 0..30 {
        let name = if i % 5 == 0 {
            format!("hit_{i:02}")
        } else {
            format!("miss_{i:02}")
        };
        h.inner.declare(&name, "cascading", json!(true)).await;
        if i % 5 == 0 {
            hits.push(h.inner.key(&name).to_string());
        }
    }
    hits
}

#[tokio::test]
async fn q_narrows_the_browse_to_the_settings_whose_text_or_shown_values_contain_it() {
    let h = RestHarness::new().await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);

    h.inner
        .declare("lantern_key", "cascading", json!("plain"))
        .await;
    h.inner
        .declare_labelled("by_label", "cascading", json!("plain"), "Lantern window")
        .await;
    h.inner
        .declare_described(
            "by_description",
            "cascading",
            json!("plain"),
            "Lights the lantern at dusk",
        )
        .await;
    let lanterns = h.inner.add_category("lanterns").await;
    h.inner
        .declare_in(
            lanterns,
            "by_category",
            "cascading",
            json!("plain"),
            crate::test_support::BOOL,
            "public",
        )
        .await;
    h.inner
        .declare("by_default", "cascading", json!("lantern-default"))
        .await;
    let by_value = h
        .inner
        .declare("by_value", "cascading", json!("plain"))
        .await;
    h.inner.set(by_value, root, json!("lantern-set")).await;
    let by_fallback = h
        .inner
        .declare("by_fallback", "cascading", json!("plain"))
        .await;
    h.inner.set(by_fallback, root, json!("lantern-up")).await;
    h.inner.set(by_fallback, a, json!("noon")).await;
    for name in ["other_one", "other_two", "other_three"] {
        let id = h.inner.declare(name, "cascading", json!("plain")).await;
        h.inner.set(id, root, json!("dark")).await;
    }

    let items = h.items(&format!("{BROWSE}?q=lantern"), a).await;
    let mut keys = keys_of(&items);
    keys.sort();
    let mut expected: Vec<String> = [
        "lantern_key",
        "by_label",
        "by_description",
        "by_category",
        "by_default",
        "by_value",
        "by_fallback",
    ]
    .iter()
    .map(|n| h.inner.key(n).to_string())
    .collect();
    expected.sort();
    assert_eq!(keys, expected, "{items:?}");
    for item in &items {
        assert_eq!(item["outcome"], json!("resolved"), "{item}");
        assert!(item["effective"].get("value").is_some(), "{item}");
        assert!(item.get("matched_field").is_none(), "{item}");
        assert!(item.get("matches").is_none(), "{item}");
    }
}

#[tokio::test]
async fn q_matches_a_structured_value_on_its_leaves() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("conn", "cascading", json!(true)).await;
    h.inner
        .set(
            id,
            h.inner.tree.root,
            json!({"host": "db.internal", "port": 5432}),
        )
        .await;
    h.inner.declare("unrelated", "cascading", json!(true)).await;
    for (q, found) in [("db.internal", true), ("5432", true), ("zzz", false)] {
        let items = h.items(&format!("{BROWSE}?q={q}"), h.inner.tree.root).await;
        assert_eq!(has(&items, "conn"), found, "q={q}: {items:?}");
        assert!(!has(&items, "unrelated"), "q={q}: {items:?}");
    }
}

#[tokio::test]
async fn q_is_matched_without_regard_to_letter_case() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("dawn", "cascading", json!(true)).await;
    h.inner.set(id, h.inner.tree.root, json!("Sunrise")).await;
    h.inner.declare("dusk", "cascading", json!(true)).await;
    for q in ["SUNRISE", "sunr"] {
        let items = h.items(&format!("{BROWSE}?q={q}"), h.inner.tree.root).await;
        assert!(has(&items, "dawn"), "q={q}: {items:?}");
        assert!(!has(&items, "dusk"), "q={q}: {items:?}");
    }
}

#[tokio::test]
async fn q_outside_its_bounds_is_refused() {
    let h = RestHarness::new().await;
    h.inner.declare("anything", "cascading", json!(true)).await;
    let long = "a".repeat(201);
    for (q, status) in [
        ("a".to_owned(), 400),
        (long, 400),
        ("%20%20%20".to_owned(), 400),
        (String::new(), 400),
        ("ab".to_owned(), 200),
    ] {
        let (got, body) = h.get(&format!("{BROWSE}?q={q}"), h.inner.tree.root).await;
        assert_eq!(got, status, "q of {} characters: {body}", q.len());
        if status == 400 {
            assert_eq!(
                body["context"]["field_violations"][0]["field"],
                json!("q"),
                "the refusal names the parameter: {body}"
            );
        }
    }
}

#[tokio::test]
async fn q_never_matches_a_flagged_override_a_shadowed_ancestor_or_a_descendant() {
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    let chain = h.inner.declare("chain", "cascading", json!("plain")).await;
    h.inner.set(chain, root, json!("x-root")).await;
    h.inner.set(chain, a, json!("y-mid")).await;
    h.inner.set_flagged(chain, b, json!("bad-leaf")).await;
    let leaf = h
        .inner
        .declare("leafonly", "cascading", json!("plain"))
        .await;
    h.inner.set(leaf, b, json!("z-leaf")).await;

    let at = |tenant: Uuid, q: &str| format!("{BROWSE}?tenant={tenant}&q={q}");
    for (tenant, q, needle, present) in [
        (b, "x-root", "chain", false),
        (b, "bad-leaf", "chain", false),
        (b, "y-mid", "chain", true),
        (a, "x-root", "chain", true),
        (a, "y-mid", "chain", true),
        (a, "z-leaf", "leafonly", false),
        (b, "z-leaf", "leafonly", true),
    ] {
        let items = h.items(&at(tenant, q), root).await;
        assert_eq!(
            has(&items, needle),
            present,
            "tenant {tenant} q={q}: {items:?}"
        );
    }
}

#[tokio::test]
async fn q_never_matches_a_secret_and_a_masked_value_gives_no_sign() {
    let h = RestHarness::new().await;
    let id = h
        .inner
        .declare_typed("vault", "cascading", json!("x"), SECRET, "secret")
        .await;
    h.inner.set_secret(id, h.inner.tree.root, "ref").await;
    for q in [urlencoding(MASK_TOKEN), "ref".to_owned()] {
        let (status, body) = h.get(&format!("{BROWSE}?q={q}"), h.inner.tree.root).await;
        assert_eq!(status, 200, "{body}");
        let items = body["items"].as_array().cloned().unwrap_or_default();
        assert!(!has(&items, "vault"), "q={q}: {items:?}");
    }
}

#[tokio::test]
async fn q_matches_a_pii_value_only_for_a_caller_entitled_to_read_it_unmasked() {
    let entitled = RestHarness::new().await;
    let masked = RestHarness::without_pii_entitlement().await;
    for h in [&entitled, &masked] {
        let root = h.inner.tree.root;
        let id = h
            .inner
            .declare_typed("contact", "cascading", json!("x"), TEXT, "pii")
            .await;
        h.inner
            .set_classified(id, root, json!("ops@example.test"), "pii")
            .await;
        let open = h.inner.declare("notice", "cascading", json!("x")).await;
        h.inner.set(open, root, json!("ops@example.test")).await;
    }
    let uri = format!("{BROWSE}?q={}", urlencoding("ops@example"));
    let items = entitled.items(&uri, entitled.inner.tree.root).await;
    assert!(has(&items, "contact") && has(&items, "notice"), "{items:?}");
    let items = masked.items(&uri, masked.inner.tree.root).await;
    assert!(!has(&items, "contact"), "{items:?}");
    assert!(has(&items, "notice"), "{items:?}");
}

#[tokio::test]
async fn q_combines_with_filter_by_conjunction() {
    let h = RestHarness::new().await;
    let cat_a = h.inner.add_category("cat_a").await;
    let cat_b = h.inner.add_category("cat_b").await;
    for (cat, names) in [
        (cat_a, ["a_match", "a_other"]),
        (cat_b, ["b_match", "b_other"]),
    ] {
        for name in names {
            h.inner
                .declare_in(
                    cat,
                    name,
                    "cascading",
                    json!(true),
                    crate::test_support::BOOL,
                    "public",
                )
                .await;
        }
    }
    let filter = urlencoding(&format!("category_id eq {cat_a}"));
    let items = h
        .items(
            &format!("{BROWSE}?q=_match&$filter={filter}"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(
        keys_of(&items),
        vec![h.inner.key("a_match").to_string()],
        "{items:?}"
    );
}

#[tokio::test]
async fn under_q_a_named_key_the_text_does_not_match_is_absent_while_hidden_and_undeclared_keys_keep_not_found()
 {
    let h = RestHarness::new().await;
    let a = h.inner.tree.a;
    h.inner.declare("match", "cascading", json!("needle")).await;
    h.inner.declare("nomatch", "cascading", json!("hay")).await;
    let concealed = h
        .inner
        .declare("concealed", "cascading", json!("needle"))
        .await;
    h.restrict(concealed, a, TenantAccess::Hidden).await;

    let filter = urlencoding(&format!(
        "key in ('{}','{}','{}','{}')",
        h.inner.key("match"),
        h.inner.key("nomatch"),
        h.inner.key("concealed"),
        h.inner.key("never_declared")
    ));
    let items = h
        .items(&format!("{BROWSE}?q=needle&$filter={filter}"), a)
        .await;
    let outcome = |needle: &str| {
        items
            .iter()
            .find(|i| i["key"].as_str().is_some_and(|k| k.contains(needle)))
            .map(|i| i["outcome"].clone())
    };
    assert_eq!(outcome("match."), Some(json!("resolved")), "{items:?}");
    assert_eq!(outcome("nomatch"), None, "{items:?}");
    assert_eq!(outcome("concealed"), Some(json!("not_found")), "{items:?}");
    assert_eq!(
        outcome("never_declared"),
        Some(json!("not_found")),
        "{items:?}"
    );
    assert_eq!(items.len(), 3, "{items:?}");
}

#[tokio::test]
async fn a_setting_that_did_not_resolve_qualifies_by_its_text_alone_under_q() {
    let h = RestHarness::new().await;
    let first = h
        .inner
        .declare("gone_beacon", "cascading", json!(true))
        .await;
    let second = h
        .inner
        .declare("gone_quiet", "cascading", json!(true))
        .await;
    h.inner.retire(first).await;
    h.inner.retire(second).await;
    let items = h
        .items(&format!("{BROWSE}?q=beacon"), h.inner.tree.root)
        .await;
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(has(&items, "gone_beacon"), "{items:?}");
    assert_eq!(items[0]["outcome"], json!("retired"), "{}", items[0]);
}

#[tokio::test]
async fn a_narrowed_page_is_full_and_its_cursor_continues_without_a_repeat_or_a_gap() {
    let h = RestHarness::new().await;
    let mut hits = hit_and_miss_corpus(&h).await;
    let (items, pages) = every_page(&h, &format!("{BROWSE}?q=hit_&limit=2")).await;
    assert_eq!(pages, 3, "six hits over pages of two");
    assert!(items.len() == 6, "{items:?}");
    assert!(
        items.chunks(2).all(|c| c.len() == 2),
        "every page is full: {items:?}"
    );
    let mut keys = keys_of(&items);
    keys.sort();
    hits.sort();
    assert_eq!(keys, hits, "exactly the hits, no repeat, no gap");
}

#[tokio::test]
async fn a_q_page_can_be_walked_back_with_its_prev_cursor() {
    let h = RestHarness::new().await;
    hit_and_miss_corpus(&h).await;
    // Misses that sort ahead of every hit, so an unnarrowed page 1 differs.
    for i in 0..4 {
        h.inner
            .declare(&format!("ahead_{i:02}"), "cascading", json!(true))
            .await;
    }
    let root = h.inner.tree.root;
    let first = format!("{BROWSE}?q=hit_&limit=2");
    let (_, page1) = h.get(&first, root).await;
    let next = page1["page_info"]["next_cursor"].as_str().expect("next");
    let (_, page2) = h.get(&format!("{first}&cursor={next}"), root).await;
    let prev = page2["page_info"]["prev_cursor"].as_str().expect("prev");
    let (status, back) = h.get(&format!("{first}&cursor={prev}"), root).await;
    assert_eq!(status, 200, "{back}");
    assert!(
        keys_of(back["items"].as_array().expect("items"))
            .iter()
            .all(|k| k.contains("hit_")),
        "the walked-back page holds hits only: {back}"
    );
    assert_eq!(
        keys_of(back["items"].as_array().expect("items")),
        keys_of(page1["items"].as_array().expect("items")),
        "{back}"
    );
}

#[tokio::test]
async fn a_q_matching_nothing_yields_an_empty_page_without_a_cursor_over_a_catalogue_wider_than_a_batch()
 {
    let h = RestHarness::new().await;
    for i in 0..250 {
        h.inner
            .declare(&format!("wide_{i:03}"), "cascading", json!(true))
            .await;
    }
    let (status, body) = h
        .get(
            &format!("{BROWSE}?q=nothing-here&limit=5"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["items"], json!([]), "{body}");
    assert!(body["page_info"]["next_cursor"].is_null(), "{body}");
}

#[tokio::test]
async fn a_browse_cursor_is_bound_to_its_q() {
    let h = RestHarness::new().await;
    hit_and_miss_corpus(&h).await;
    let root = h.inner.tree.root;
    let cursor_of = |body: &Value| {
        body["page_info"]["next_cursor"]
            .as_str()
            .expect("a second page")
            .to_owned()
    };
    let (_, with_q) = h.get(&format!("{BROWSE}?q=hit_&limit=2"), root).await;
    let with_q = cursor_of(&with_q);
    for other in ["q=miss_&limit=2", "limit=2"] {
        let (status, body) = h
            .get(&format!("{BROWSE}?{other}&cursor={with_q}"), root)
            .await;
        assert_eq!(status, 400, "{other}: {body}");
    }
    let (_, without_q) = h.get(&format!("{BROWSE}?limit=2"), root).await;
    let without_q = cursor_of(&without_q);
    let (status, body) = h
        .get(&format!("{BROWSE}?q=hit_&limit=2&cursor={without_q}"), root)
        .await;
    assert_eq!(status, 400, "{body}");
    let (status, body) = h
        .get(&format!("{BROWSE}?q=hit_&limit=2&cursor={with_q}"), root)
        .await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn a_browse_cursor_is_bound_to_its_target() {
    let h = RestHarness::new().await;
    hit_and_miss_corpus(&h).await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    let cursor_of = |body: &Value| {
        body["page_info"]["next_cursor"]
            .as_str()
            .expect("a second page")
            .to_owned()
    };
    for listing in ["limit=2", "q=hit_&limit=2"] {
        // Minted at the caller's own tenant, named or implied: one listing.
        let (_, first) = h.get(&format!("{BROWSE}?{listing}"), root).await;
        let own = cursor_of(&first);
        let (status, body) = h
            .get(
                &format!("{BROWSE}?tenant={root}&{listing}&cursor={own}"),
                root,
            )
            .await;
        assert_eq!(status, 200, "{listing}: {body}");
        // Replayed at another tenant: another listing, refused before any row.
        let (status, body) = h
            .get(&format!("{BROWSE}?tenant={a}&{listing}&cursor={own}"), root)
            .await;
        assert_eq!(status, 400, "{listing}: {body}");
        assert_eq!(
            body["context"]["field_violations"][0]["field"], "cursor",
            "{listing}: {body}"
        );
        // Minted at one descendant and replayed at another, or at the same.
        let (_, at_a) = h.get(&format!("{BROWSE}?tenant={a}&{listing}"), root).await;
        let at_a = cursor_of(&at_a);
        let (status, body) = h
            .get(
                &format!("{BROWSE}?tenant={b}&{listing}&cursor={at_a}"),
                root,
            )
            .await;
        assert_eq!(status, 400, "{listing}: {body}");
        let (status, body) = h
            .get(
                &format!("{BROWSE}?tenant={a}&{listing}&cursor={at_a}"),
                root,
            )
            .await;
        assert_eq!(status, 200, "{listing}: {body}");
    }
}

#[tokio::test]
async fn q_with_needs_review_keeps_only_the_flagged_settings_the_text_matches() {
    let h = RestHarness::new().await;
    let root = h.inner.tree.root;
    for (name, flagged) in [
        ("lantern_flagged", true),
        ("lantern_plain", false),
        ("other_flagged", true),
    ] {
        let id = h.inner.declare(name, "cascading", json!("plain")).await;
        if flagged {
            h.inner.set_flagged(id, root, json!("bad")).await;
        } else {
            h.inner.set(id, root, json!("fine")).await;
        }
    }
    let filter = urlencoding("needs_review eq true");
    let items = h
        .items(&format!("{BROWSE}?q=lantern&$filter={filter}"), root)
        .await;
    assert_eq!(
        keys_of(&items),
        vec![h.inner.key("lantern_flagged").to_string()],
        "{items:?}"
    );
}

#[tokio::test]
async fn a_json_null_default_is_not_matched_by_the_word_null() {
    let h = RestHarness::new().await;
    h.inner.declare("empty_one", "cascading", json!(null)).await;
    h.inner
        .declare("full_one", "cascading", json!("null-ish"))
        .await;
    for q in ["null", "nul"] {
        let items = h.items(&format!("{BROWSE}?q={q}"), h.inner.tree.root).await;
        assert!(!has(&items, "empty_one"), "q={q}: {items:?}");
        assert!(has(&items, "full_one"), "q={q}: {items:?}");
    }
}

#[tokio::test]
async fn an_outage_that_could_hide_a_match_refuses_the_narrowed_page() {
    let h = RestHarness::new().await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    h.inner.declare("shown_one", "cascading", json!("x")).await;
    // The batch's chain fetch fails, and with it the value read.
    h.inner.hierarchy.fail_next_chains(1);
    let (status, body) = h.get(&format!("{BROWSE}?tenant={a}&q=zzz"), root).await;
    assert_eq!(status, 503, "{body}");
    // The refusal is not a standing state: the same request answers once the
    // dependency does.
    let (status, body) = h.get(&format!("{BROWSE}?tenant={a}&q=zzz"), root).await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn an_outage_over_values_that_could_never_match_does_not_refuse_the_page() {
    let h = RestHarness::new().await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    h.inner
        .declare_typed("vault_one", "cascading", json!("x"), SECRET, "secret")
        .await;
    h.inner.hierarchy.fail_next_chains(1);
    let (status, body) = h.get(&format!("{BROWSE}?tenant={a}&q=zzz"), root).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["items"], json!([]), "{body}");
}

#[tokio::test]
async fn an_outage_over_pii_is_refused_only_for_a_caller_who_could_have_matched_it() {
    let entitled = RestHarness::new().await;
    let masked = RestHarness::without_pii_entitlement().await;
    for (h, status) in [(&entitled, 503), (&masked, 200)] {
        let (root, a) = (h.inner.tree.root, h.inner.tree.a);
        h.inner
            .declare_typed("contact_one", "cascading", json!("x"), TEXT, "pii")
            .await;
        h.inner.hierarchy.fail_next_chains(1);
        let (got, body) = h.get(&format!("{BROWSE}?tenant={a}&q=zzz"), root).await;
        assert_eq!(got, status, "{body}");
    }
}

#[tokio::test]
async fn the_page_size_matches_what_the_unnarrowed_browse_reports() {
    use crate::api::rest::browse_narrowing::page_size;
    let h = RestHarness::new().await;
    h.inner.declare("anything", "cascading", json!(true)).await;
    let root = h.inner.tree.root;
    for limit in [None, Some(1000_u64), Some(7), Some(1)] {
        let query = limit.map_or(String::new(), |l| format!("&limit={l}"));
        let (_, plain) = h
            .get(&format!("{BROWSE}?{}", query.trim_start_matches('&')), root)
            .await;
        let (_, narrowed) = h.get(&format!("{BROWSE}?q=any{query}"), root).await;
        assert_eq!(
            plain["page_info"]["limit"],
            json!(page_size(limit)),
            "{limit:?}"
        );
        assert_eq!(
            narrowed["page_info"]["limit"], plain["page_info"]["limit"],
            "{limit:?}"
        );
    }
}

/// `n000`, `n001`, … in key order, those at `hits` carrying `_hit`.
async fn indexed_corpus(h: &RestHarness, count: usize, hits: &[usize]) -> Vec<String> {
    let mut hit_keys = Vec::new();
    for i in 0..count {
        let name = if hits.contains(&i) {
            format!("n{i:03}_hit")
        } else {
            format!("n{i:03}")
        };
        h.inner.declare(&name, "cascading", json!(true)).await;
        if hits.contains(&i) {
            hit_keys.push(h.inner.key(&name).to_string());
        }
    }
    hit_keys
}

#[tokio::test]
async fn a_walk_across_several_candidate_batches_pages_forward_and_back_without_repeat_or_gap() {
    let h = RestHarness::new().await;
    let hits = indexed_corpus(&h, 120, &[0, 40, 80]).await;
    let root = h.inner.tree.root;
    let first = format!("{BROWSE}?q=_hit&limit=2");
    let (_, page1) = h.get(&first, root).await;
    let items1 = keys_of(page1["items"].as_array().expect("items"));
    assert_eq!(items1, hits[..2], "{page1}");
    let next = page1["page_info"]["next_cursor"]
        .as_str()
        .expect("a cursor");
    let (_, page2) = h.get(&format!("{first}&cursor={next}"), root).await;
    assert_eq!(
        keys_of(page2["items"].as_array().expect("items")),
        hits[2..],
        "{page2}"
    );
    assert!(page2["page_info"]["next_cursor"].is_null(), "{page2}");
    let prev = page2["page_info"]["prev_cursor"].as_str().expect("prev");
    let (status, back) = h.get(&format!("{first}&cursor={prev}"), root).await;
    assert_eq!(status, 200, "{back}");
    assert_eq!(
        keys_of(back["items"].as_array().expect("items")),
        items1,
        "{back}"
    );
    assert!(
        back["page_info"]["prev_cursor"].is_null(),
        "nothing earlier: {back}"
    );
    assert!(back["page_info"]["next_cursor"].is_string(), "{back}");
}

#[tokio::test]
async fn a_page_ending_exactly_at_a_batch_boundary_continues_with_the_next_hit() {
    let h = RestHarness::new().await;
    let hits = indexed_corpus(&h, 60, &[50, 51]).await;
    let root = h.inner.tree.root;
    let first = format!("{BROWSE}?q=_hit&limit=1");
    let (_, page1) = h.get(&first, root).await;
    assert_eq!(
        keys_of(page1["items"].as_array().expect("items")),
        hits[..1],
        "{page1}"
    );
    let next = page1["page_info"]["next_cursor"]
        .as_str()
        .expect("a cursor");
    let (_, page2) = h.get(&format!("{first}&cursor={next}"), root).await;
    assert_eq!(
        keys_of(page2["items"].as_array().expect("items")),
        hits[1..],
        "{page2}"
    );
    assert!(page2["page_info"]["next_cursor"].is_null(), "{page2}");
}

#[tokio::test]
async fn orderby_category_holds_across_pages_and_back_under_q() {
    let h = RestHarness::new().await;
    let root = h.inner.tree.root;
    let cats = [
        h.inner.add_category("cat_a").await,
        h.inner.add_category("cat_b").await,
    ];
    for (c, cat) in cats.iter().enumerate() {
        for i in 0..4 {
            let name = if i % 2 == 0 {
                format!("c{c}_{i}_hit")
            } else {
                format!("c{c}_{i}")
            };
            h.inner
                .declare_in(
                    *cat,
                    &name,
                    "cascading",
                    json!(true),
                    crate::test_support::BOOL,
                    "public",
                )
                .await;
        }
    }
    let order = urlencoding("category_id desc");
    let (_, all) = h
        .get(&format!("{BROWSE}?$orderby={order}&limit=200"), root)
        .await;
    let expected: Vec<String> = keys_of(all["items"].as_array().expect("items"))
        .into_iter()
        .filter(|k| k.contains("_hit"))
        .collect();
    assert_eq!(expected.len(), 4);
    let first = format!("{BROWSE}?q=_hit&$orderby={order}&limit=3");
    // The cursor carries the order: a continuation names none.
    let rest = format!("{BROWSE}?q=_hit&limit=3");
    let (_, page1) = h.get(&first, root).await;
    let next = page1["page_info"]["next_cursor"]
        .as_str()
        .expect("a cursor");
    let (_, page2) = h.get(&format!("{rest}&cursor={next}"), root).await;
    assert!(page2.get("items").is_some(), "{page2}");
    let mut walked = keys_of(page1["items"].as_array().expect("items"));
    walked.extend(keys_of(page2["items"].as_array().expect("items")));
    assert_eq!(walked, expected, "{page1} {page2}");
    assert!(page2["page_info"]["next_cursor"].is_null(), "{page2}");
    let prev = page2["page_info"]["prev_cursor"].as_str().expect("prev");
    let (_, back) = h.get(&format!("{rest}&cursor={prev}"), root).await;
    assert_eq!(
        keys_of(back["items"].as_array().expect("items")),
        keys_of(page1["items"].as_array().expect("items")),
        "{back}"
    );
}

#[tokio::test]
async fn an_outage_on_the_row_read_past_a_full_page_owes_a_cursor_instead_of_refusing_it() {
    let h = RestHarness::new().await;
    let hits = indexed_corpus(&h, 60, &[0, 55]).await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    // The first batch's chain is fetched; the hierarchy then goes down, so the
    // second batch — read only to look past the full page — cannot resolve.
    let hierarchy = std::sync::Arc::clone(&h.inner.hierarchy);
    *h.inner.hierarchy.on_chain.lock().expect("lock") =
        Some(Box::new(move || hierarchy.set_unavailable(true)));
    let first = format!("{BROWSE}?tenant={a}&q=_hit&limit=1");
    let (status, page1) = h.get(&first, root).await;
    assert_eq!(status, 200, "{page1}");
    assert_eq!(
        keys_of(page1["items"].as_array().expect("items")),
        hits[..1]
    );
    let next = page1["page_info"]["next_cursor"]
        .as_str()
        .expect("a cursor is owed")
        .to_owned();

    h.inner.hierarchy.set_unavailable(false);
    let (status, page2) = h.get(&format!("{first}&cursor={next}"), root).await;
    assert_eq!(status, 200, "{page2}");
    assert_eq!(
        keys_of(page2["items"].as_array().expect("items")),
        hits[1..]
    );
    assert!(page2["page_info"]["next_cursor"].is_null(), "{page2}");
}
