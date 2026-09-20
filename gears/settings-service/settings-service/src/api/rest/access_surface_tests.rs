// Created: 2026-09-17 by Virtuozzo International GmbH
//! The tenant-access surface driven as a client drives it.
//!
//! What a tenant may do with a setting is a sparse decision on a
//! `(setting, tenant)` pair, and every rule around it is a status code: only a
//! strict ancestor may record one, `overridable` is the absence of a row
//! rather than a value to store, the effective access is the strictest on the
//! chain, and a `hidden` setting is reported absent here exactly as it is
//! everywhere else.

use serde_json::{Value, json};
use uuid::Uuid;

use crate::test_support::RestHarness;

/// The permissions surface for one setting.
fn permissions(h: &RestHarness, name: &str) -> String {
    let key = h
        .inner
        .key(name)
        .to_string()
        .replace('~', "%7E")
        .replace('!', "%21");
    format!("/settings-service/v1/settings/{key}/permissions")
}

/// Read a pair and answer with the body and its state tag.
async fn read(h: &RestHarness, name: &str, tenant: Uuid, caller: Uuid) -> (Value, String) {
    let uri = format!("{}?tenant={tenant}", permissions(h, name));
    let answer = h.send("GET", &uri, None, None, caller).await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    let tag = answer.etag.expect("a state tag on the readout");
    (answer.body, tag)
}

/// Record a restriction the way an ancestor's administrator would, and answer
/// with the status.
async fn restrict(
    h: &RestHarness,
    name: &str,
    tenant: Uuid,
    access: &str,
    tag: &str,
    caller: Uuid,
) -> axum::http::StatusCode {
    let uri = format!("{}?tenant={tenant}", permissions(h, name));
    h.send(
        "PUT",
        &uri,
        Some(json!({ "access": access })),
        Some(tag),
        caller,
    )
    .await
    .status
}

// ── The default, and that reading does not materialise it ────────────────────

#[tokio::test]
async fn a_pair_with_no_row_reads_overridable_and_carries_a_tag_a_write_can_present() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;

    let (body, tag) = read(&h, "proxy", h.inner.tree.a, h.inner.tree.root).await;
    assert_eq!(body["effective"]["access"], json!("overridable"));
    assert!(
        body["effective"].get("supplied_by").is_none(),
        "nothing supplies the default: {body}"
    );
    assert!(
        body.get("stored").is_none(),
        "and reading created no row: {body}"
    );
    assert_eq!(tag, "absent");

    // That tag is what a first write presents, and it is accepted.
    assert_eq!(
        restrict(
            &h,
            "proxy",
            h.inner.tree.a,
            "read_only",
            &tag,
            h.inner.tree.root
        )
        .await,
        200
    );
}

// ── Who may record one ───────────────────────────────────────────────────────

#[tokio::test]
async fn only_a_strict_ancestor_may_restrict_a_tenant() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let a = h.inner.tree.a;

    // `a` targeting itself: a tenant never changes its own row.
    assert_eq!(
        restrict(&h, "proxy", a, "read_only", "absent", a).await,
        403,
        "its own row"
    );
    // `a` targeting its ancestor, and its sibling.
    for target in [h.inner.tree.root, h.inner.tree.c] {
        assert_eq!(
            restrict(&h, "proxy", target, "read_only", "absent", a).await,
            403,
            "{target}"
        );
    }
    // `a` targeting a standalone descendant.
    assert_eq!(
        restrict(&h, "proxy", h.inner.tree.s, "read_only", "absent", a).await,
        403,
        "a standalone descendant is opaque from above"
    );
    // `a` targeting its own strict descendant: allowed.
    assert_eq!(
        restrict(&h, "proxy", h.inner.tree.b, "read_only", "absent", a).await,
        200
    );
}

#[tokio::test]
async fn overridable_is_not_a_value_to_store() {
    // It is the absence of a row, and is expressed by clearing rather than by
    // writing it.
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    assert_eq!(
        restrict(
            &h,
            "proxy",
            h.inner.tree.a,
            "overridable",
            "absent",
            h.inner.tree.root
        )
        .await,
        400
    );
}

// ── The precondition ─────────────────────────────────────────────────────────

#[tokio::test]
async fn recording_and_clearing_are_both_conditional() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let uri = format!("{}?tenant={}", permissions(&h, "proxy"), h.inner.tree.a);
    let body = Some(json!({ "access": "read_only" }));

    let answer = h
        .send("PUT", &uri, body.clone(), None, h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 428, "no tag: {}", answer.body);

    let answer = h
        .send(
            "PUT",
            &uri,
            body.clone(),
            Some("a tag from another time"),
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 412, "a stale tag: {}", answer.body);

    let answer = h
        .send("PUT", &uri, body, Some("absent"), h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 200);
    let stored_tag = answer.etag.expect("the stored row's tag");
    assert_ne!(stored_tag, "absent", "a row exists now, so the tag moved");

    // The clear is conditional on the same tag.
    let answer = h.send("DELETE", &uri, None, None, h.inner.tree.root).await;
    assert_eq!(answer.status, 428);
    let answer = h
        .send("DELETE", &uri, None, Some("absent"), h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 412, "the absent-state tag is stale now");
    let answer = h
        .send("DELETE", &uri, None, Some(&stored_tag), h.inner.tree.root)
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(
        answer.body["effective"]["access"],
        json!("overridable"),
        "and the pair is back to the default"
    );
}

// ── The strictest value on the chain ─────────────────────────────────────────

#[tokio::test]
async fn the_effective_access_is_the_strictest_on_the_chain_and_names_who_supplies_it() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let (root, a, b, c) = (
        h.inner.tree.root,
        h.inner.tree.a,
        h.inner.tree.b,
        h.inner.tree.c,
    );

    assert_eq!(
        restrict(&h, "proxy", a, "hidden", "absent", root).await,
        200
    );
    assert_eq!(
        restrict(&h, "proxy", b, "read_only", "absent", root).await,
        200,
        "a narrower exception is recorded even under a stricter ancestor"
    );

    let (body, _) = read(&h, "proxy", b, root).await;
    assert_eq!(
        body["effective"]["access"],
        json!("hidden"),
        "a descendant cannot widen what an ancestor narrowed"
    );
    assert_eq!(
        body["effective"]["supplied_by"],
        json!(a.to_string()),
        "and the readout names the tenant whose row supplied it"
    );
    assert_eq!(
        body["stored"]["access"],
        json!("read_only"),
        "while the pair's own row is reported as it stands"
    );

    // A sibling outside the restricted branch is untouched.
    let (sibling, _) = read(&h, "proxy", c, root).await;
    assert_eq!(sibling["effective"]["access"], json!("overridable"));
}

#[tokio::test]
async fn clearing_an_ancestor_lets_the_row_below_it_take_effect() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    restrict(&h, "proxy", a, "hidden", "absent", root).await;
    restrict(&h, "proxy", b, "read_only", "absent", root).await;

    let (_, a_tag) = read(&h, "proxy", a, root).await;
    let cleared = h
        .send(
            "DELETE",
            &format!("{}?tenant={a}", permissions(&h, "proxy")),
            None,
            Some(&a_tag),
            root,
        )
        .await;
    assert_eq!(cleared.status, 200);

    let (body, _) = read(&h, "proxy", b, root).await;
    assert_eq!(
        body["effective"]["access"],
        json!("read_only"),
        "the row that was waiting is now the strictest on the chain"
    );
    assert_eq!(body["effective"]["supplied_by"], json!(b.to_string()));
}

// ── The listing ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_listing_carries_the_rows_recorded_for_the_setting() {
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let root = h.inner.tree.root;
    restrict(&h, "proxy", h.inner.tree.a, "hidden", "absent", root).await;
    restrict(&h, "proxy", h.inner.tree.c, "read_only", "absent", root).await;

    let (status, body) = h
        .get(&format!("{}/all", permissions(&h, "proxy")), root)
        .await;
    assert_eq!(status, 200, "{body}");
    let mut rows: Vec<(String, String)> = body["items"]
        .as_array()
        .expect("a page")
        .iter()
        .map(|r| {
            (
                r["tenant_id"].as_str().unwrap_or_default().to_owned(),
                r["access"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    rows.sort();
    let mut expected = vec![
        (h.inner.tree.a.to_string(), "hidden".to_owned()),
        (h.inner.tree.c.to_string(), "read_only".to_owned()),
    ];
    expected.sort();
    assert_eq!(rows, expected);
}

// ── What the surface refuses ─────────────────────────────────────────────────

#[tokio::test]
async fn the_target_tenant_is_required_here() {
    // On the value surface an absent `tenant` means "my own scope". A
    // restriction is always about another tenant, so defaulting it would
    // restrict the caller instead of the tenant they meant.
    let h = RestHarness::new().await;
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let answer = h
        .send(
            "GET",
            &permissions(&h, "proxy"),
            None,
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 400, "{}", answer.body);
}

#[tokio::test]
async fn a_setting_that_does_not_exist_is_reported_absent_here_too() {
    let h = RestHarness::new().await;
    let uri = format!(
        "{}?tenant={}",
        permissions(&h, "never_declared"),
        h.inner.tree.a
    );
    let answer = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    assert_eq!(answer.status, 404);
}

#[tokio::test]
async fn a_hidden_setting_is_absent_from_the_permissions_surface_as_well() {
    // 404 rather than 403, exactly as on every other administrative read: a
    // distinct denial would confirm the setting exists.
    let h = RestHarness::new().await;
    h.inner.declare("concealed", "cascading", json!(true)).await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    restrict(&h, "concealed", a, "hidden", "absent", root).await;

    let uri = format!("{}?tenant={b}", permissions(&h, "concealed"));
    let answer = h.send("GET", &uri, None, None, a).await;
    assert_eq!(
        answer.status, 404,
        "hidden from `a`, so absent on its permissions reads: {}",
        answer.body
    );

    // The administrator above still sees what it restricted.
    let answer = h.send("GET", &uri, None, None, root).await;
    assert_eq!(answer.status, 200);
}

#[tokio::test]
async fn a_malformed_key_or_tenant_is_refused_400() {
    let h = RestHarness::new().await;
    let answer = h
        .send(
            "GET",
            "/settings-service/v1/settings/not-a-key/permissions?tenant=00000000-0000-0000-0000-000000000001",
            None,
            None,
            h.inner.tree.root,
        )
        .await;
    assert_eq!(answer.status, 400);

    let uri = format!("{}?tenant=not-a-uuid", permissions(&h, "proxy"));
    let answer = h.send("GET", &uri, None, None, h.inner.tree.root).await;
    assert_eq!(answer.status, 400);
}

#[tokio::test]
async fn a_denied_caller_reaches_no_permissions_operation() {
    let h = RestHarness::denying().await;
    let uri = format!("{}?tenant={}", permissions(&h, "proxy"), h.inner.tree.a);

    for (method, uri, body) in [
        ("GET", uri.clone(), None),
        ("PUT", uri.clone(), Some(json!({ "access": "read_only" }))),
        ("DELETE", uri, None),
        ("GET", format!("{}/all", permissions(&h, "proxy")), None),
    ] {
        let answer = h
            .send(method, &uri, body, Some("absent"), h.inner.tree.root)
            .await;
        assert_eq!(answer.status, 403, "{method} {uri}");
    }
}

// ── What a restriction does and does not touch ───────────────────────────────

#[tokio::test]
async fn a_restriction_gates_the_caller_and_not_the_stored_value() {
    // An override set before the tenant became `read_only` still resolves and
    // is still inherited: access gates who may act, not what is stored.
    let h = RestHarness::new().await;
    let id = h
        .inner
        .declare("proxy", "cascading", json!("default"))
        .await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    h.inner
        .set(id, a, json!("set before the restriction"))
        .await;
    restrict(&h, "proxy", a, "read_only", "absent", root).await;

    let key = h.inner.key("proxy").to_string().replace('~', "%7E");
    let (status, body) = h
        .get(
            &format!("/settings-service/v1/settings/{key}?tenant={b}"),
            root,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["value"],
        json!("set before the restriction"),
        "the value still resolves and is still inherited"
    );
}

#[tokio::test]
async fn a_restriction_survives_the_declaration_being_retired_and_revived() {
    let h = RestHarness::new().await;
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    restrict(&h, "proxy", a, "read_only", "absent", root).await;

    h.inner.retire(id).await;
    let (body, _) = read(&h, "proxy", a, root).await;
    assert_eq!(
        body["stored"]["access"],
        json!("read_only"),
        "the row is retained across a retire: {body}"
    );
}
