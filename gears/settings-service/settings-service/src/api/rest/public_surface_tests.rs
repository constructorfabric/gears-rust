//! The anonymous read surface driven as an unauthenticated client drives it.
//!
//! Every request here carries no principal at all. What it has to show: only
//! the flagged settings are served, resolved by the ordinary cascade; an
//! unknown, missing or malformed tenant is the same empty answer; and a real
//! failure is never mistaken for an empty one.

use serde_json::json;
use uuid::Uuid;

use crate::test_support::RestHarness;

const PATH: &str = "/settings-service/v1/public/settings";

fn at(tenant: Uuid) -> String {
    format!("{PATH}?tenant={tenant}")
}

fn item(h: &RestHarness, name: &str, value: &serde_json::Value) -> serde_json::Value {
    json!({ "key": h.inner.key(name).to_string(), "value": value })
}

#[tokio::test]
async fn an_exposable_value_is_served_without_any_authentication() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("banner", "cascading", json!(false)).await;
    h.inner.expose(id).await;
    h.inner.set(id, h.inner.tree.a, json!(true)).await;

    let answer = h.send_anonymous("GET", &at(h.inner.tree.a)).await;

    assert_eq!(answer.status, 200, "{}", answer.raw);
    assert_eq!(
        answer.body,
        json!({ "items": [item(&h, "banner", &json!(true))] })
    );
}

#[tokio::test]
async fn a_setting_that_is_not_flagged_is_absent_with_no_trace() {
    let h = RestHarness::new().await;
    let shown = h.inner.declare("banner", "cascading", json!(false)).await;
    h.inner.expose(shown).await;
    let hidden = h.inner.declare("internal", "cascading", json!(false)).await;
    h.inner.set(shown, h.inner.tree.a, json!(true)).await;
    h.inner.set(hidden, h.inner.tree.a, json!(true)).await;

    let answer = h.send_anonymous("GET", &at(h.inner.tree.a)).await;

    assert_eq!(answer.status, 200);
    assert_eq!(
        answer.body,
        json!({ "items": [item(&h, "banner", &json!(true))] })
    );
    assert!(
        !answer.raw.contains("internal"),
        "nothing names the other setting: {}",
        answer.raw
    );
}

#[tokio::test]
async fn a_child_tenant_is_served_the_value_it_inherits_from_its_parent() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("banner", "cascading", json!(false)).await;
    h.inner.expose(id).await;
    h.inner.set(id, h.inner.tree.a, json!(true)).await;

    // `b` is a child of `a` and holds no row of its own.
    let answer = h.send_anonymous("GET", &at(h.inner.tree.b)).await;

    assert_eq!(answer.status, 200);
    assert_eq!(
        answer.body,
        json!({ "items": [item(&h, "banner", &json!(true))] })
    );
}

#[tokio::test]
async fn the_scope_class_still_applies() {
    let h = RestHarness::new().await;
    let global = h.inner.declare("platform_wide", "global", json!(1)).await;
    h.inner.expose(global).await;
    h.inner.set(global, h.inner.tree.root, json!(2)).await;
    let local = h.inner.declare("per_tenant", "local", json!(false)).await;
    h.inner.expose(local).await;
    h.inner.set(local, h.inner.tree.a, json!(true)).await;

    let at_b = h.send_anonymous("GET", &at(h.inner.tree.b)).await;

    // A global setting is the platform's whatever the tenant; a local one is
    // never inherited, so `b` reads the Schema Default, not `a`'s row.
    assert_eq!(at_b.status, 200);
    assert_eq!(
        at_b.body,
        json!({ "items": [
            item(&h, "per_tenant", &json!(false)),
            item(&h, "platform_wide", &json!(2)),
        ] })
    );
}

#[tokio::test]
async fn a_retired_exposable_setting_is_not_served() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("banner", "cascading", json!(false)).await;
    h.inner.expose(id).await;
    h.inner.retire(id).await;

    let answer = h.send_anonymous("GET", &at(h.inner.tree.a)).await;

    assert_eq!(answer.status, 200);
    assert_eq!(answer.body, json!({ "items": [] }));
}

#[tokio::test]
async fn an_unknown_missing_or_malformed_tenant_is_the_same_empty_answer() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("banner", "cascading", json!(false)).await;
    h.inner.expose(id).await;

    let unknown = h.send_anonymous("GET", &at(Uuid::new_v4())).await;
    let missing = h.send_anonymous("GET", PATH).await;
    let empty = h.send_anonymous("GET", &format!("{PATH}?tenant=")).await;
    let malformed = h
        .send_anonymous("GET", &format!("{PATH}?tenant=not-a-tenant"))
        .await;

    let random = Uuid::new_v4();
    let repeated = h
        .send_anonymous(
            "GET",
            &format!("{PATH}?tenant={}&tenant={}", h.inner.tree.a, h.inner.tree.b),
        )
        .await;
    let bad_encoding = h.send_anonymous("GET", &format!("{PATH}?tenant=%ff")).await;
    let bad_pair = h
        .send_anonymous("GET", &format!("{PATH}?tenant=%zz&x=%ff&tenant={random}"))
        .await;

    for answer in [
        &unknown,
        &missing,
        &empty,
        &malformed,
        &repeated,
        &bad_encoding,
        &bad_pair,
    ] {
        assert_eq!(answer.status, 200, "{}", answer.raw);
        assert_eq!(answer.body, json!({ "items": [] }));
        assert_eq!(answer.raw, unknown.raw, "byte-identical answers");
    }
}

#[tokio::test]
async fn an_unknown_tenant_cannot_be_told_from_a_known_one_with_nothing_exposable() {
    // Nothing is flagged, so a tenant that exists has nothing to show.
    let h = RestHarness::new().await;
    h.inner.declare("internal", "cascading", json!(false)).await;

    let known = h.send_anonymous("GET", &at(h.inner.tree.a)).await;
    let unknown = h.send_anonymous("GET", &at(Uuid::new_v4())).await;

    assert_eq!(known.status, 200);
    assert_eq!(unknown.status, 200);
    assert_eq!(known.raw, unknown.raw);
}

#[tokio::test]
async fn an_unreachable_tenant_resolver_is_a_503_and_never_an_empty_answer() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("banner", "cascading", json!(false)).await;
    h.inner.expose(id).await;
    h.inner.hierarchy.set_unavailable(true);

    let answer = h.send_anonymous("GET", &at(h.inner.tree.a)).await;

    assert_eq!(answer.status, 503, "{}", answer.raw);
    assert!(
        answer.body.get("items").is_none(),
        "an outage is not an answer: {}",
        answer.raw
    );
}

#[tokio::test]
async fn a_database_failure_is_a_503_and_never_an_empty_answer() {
    let h = RestHarness::probed().await;
    let id = h.inner.declare("banner", "cascading", json!(false)).await;
    h.inner.expose(id).await;
    h.probe_exec("DROP TABLE setting_declarations").await;

    let answer = h.send_anonymous("GET", &at(h.inner.tree.a)).await;

    assert_eq!(answer.status, 503, "{}", answer.raw);
    assert!(
        answer.body.get("items").is_none(),
        "an outage is not an answer: {}",
        answer.raw
    );
}

#[tokio::test]
async fn only_get_is_registered_on_the_path() {
    let h = RestHarness::new().await;

    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let answer = h.send_anonymous(method, &at(h.inner.tree.a)).await;
        assert_eq!(answer.status, 405, "{method}");
    }
}

#[tokio::test]
async fn an_unknown_extra_parameter_beside_a_good_tenant_does_not_change_the_answer() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("banner", "cascading", json!(false)).await;
    h.inner.expose(id).await;
    h.inner.set(id, h.inner.tree.a, json!(true)).await;

    let plain = h.send_anonymous("GET", &at(h.inner.tree.a)).await;
    let extra = h
        .send_anonymous("GET", &format!("{}&utm=x&flag", at(h.inner.tree.a)))
        .await;

    assert_eq!(extra.status, 200);
    assert_eq!(extra.raw, plain.raw);
}

#[tokio::test]
async fn other_spellings_of_a_valid_uuid_resolve_and_encoded_or_bare_forms_do_not() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("banner", "cascading", json!(false)).await;
    h.inner.expose(id).await;
    h.inner.set(id, h.inner.tree.a, json!(true)).await;
    let a = h.inner.tree.a;
    let served = json!({ "items": [item(&h, "banner", &json!(true))] });

    // The forms the UUID parser accepts name the tenant.
    for spelled in [
        a.hyphenated().to_string().to_uppercase(),
        a.simple().to_string(),
        a.urn().to_string(),
    ] {
        let answer = h
            .send_anonymous("GET", &format!("{PATH}?tenant={spelled}"))
            .await;
        assert_eq!(answer.status, 200, "{spelled}: {}", answer.raw);
        assert_eq!(answer.body, served, "{spelled}");
    }

    // Percent-encoded hyphens, a plus, another key's case and a bare `tenant`
    // beside a good one are all the one empty answer.
    let hyphens = a.to_string().replace('-', "%2D");
    let plus = format!("+{a}");
    for query in [
        format!("tenant={hyphens}"),
        format!("tenant={plus}"),
        format!("TENANT={a}"),
        "TENANT=".to_owned(),
        format!("tenant&tenant={a}"),
        format!("tenant={a}&tenant"),
        "tenant".to_owned(),
    ] {
        let answer = h.send_anonymous("GET", &format!("{PATH}?{query}")).await;
        assert_eq!(answer.status, 200, "{query}: {}", answer.raw);
        assert_eq!(answer.body, json!({ "items": [] }), "{query}");
    }
}

// ── The cached selection follows declaration changes on the replica ──────────

const DECLARATIONS: &str = "/settings-service/v1/declarations";

fn declaration_body(h: &RestHarness, name: &str) -> serde_json::Value {
    json!({
        "value_type_id": crate::test_support::BOOL,
        "vendor": "acme",
        "name": name,
        "category_id": h.inner.category_id(),
        "default_value": true,
        "scope_class": "cascading",
        "anonymous_exposable": true,
    })
}

fn served_leaves(answer: &crate::test_support::Answer) -> Vec<String> {
    let mut leaves: Vec<String> = answer.body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["key"].as_str().expect("key").to_owned())
        .collect();
    leaves.sort();
    leaves
}

#[tokio::test]
async fn clearing_the_flag_after_a_cached_read_is_visible_to_the_next_read() {
    let h = RestHarness::new().await;
    let created = h
        .send(
            "POST",
            DECLARATIONS,
            Some(declaration_body(&h, "banner")),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.raw);
    let id = created.body["id"].as_str().expect("an id").to_owned();
    let tag = created.etag.expect("a state tag");

    let before = h.send_anonymous("GET", &at(h.inner.tree.a)).await;
    assert_eq!(served_leaves(&before).len(), 1, "{}", before.raw);

    let patched = h
        .send(
            "PATCH",
            &format!("{DECLARATIONS}/{id}"),
            Some(json!({ "anonymous_exposable": false })),
            Some(&tag),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(patched.status, 200, "{}", patched.raw);

    let after = h.send_anonymous("GET", &at(h.inner.tree.a)).await;
    assert_eq!(after.status, 200);
    assert_eq!(after.body, json!({ "items": [] }));
}

#[tokio::test]
async fn creating_an_exposable_declaration_after_a_cached_read_is_visible_to_the_next_read() {
    let h = RestHarness::new().await;
    let first = h
        .send(
            "POST",
            DECLARATIONS,
            Some(declaration_body(&h, "banner")),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(first.status, 201, "{}", first.raw);

    let before = h.send_anonymous("GET", &at(h.inner.tree.a)).await;
    assert_eq!(served_leaves(&before).len(), 1, "{}", before.raw);

    let second = h
        .send(
            "POST",
            DECLARATIONS,
            Some(declaration_body(&h, "notice")),
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(second.status, 201, "{}", second.raw);

    let after = h.send_anonymous("GET", &at(h.inner.tree.a)).await;
    assert_eq!(after.status, 200);
    assert_eq!(served_leaves(&after).len(), 2, "{}", after.raw);
}
