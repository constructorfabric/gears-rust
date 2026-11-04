//! The upward disclosure boundary, driven as a client drives it.
//!
//! A tenant administrator reads a resolved value, its history, the stored
//! restrictions and the review worklist of its own subtree, and none of those
//! may tell it what lies above its tenant: the scopes above it collapse into
//! one anonymous entry, and an identity is shown only when the tenant that
//! recorded it lies on the path from the reader down to the scope the identity
//! concerns. A platform administrator sees everything.
//!
//! The tree is `root -> a -> b`, `root -> c`, `a -> s` (standalone). A tenant
//! reader is `a`'s administrator, a platform administrator is `root`'s.
//!
//! Every assertion reads the JSON body, so the file compiles against the
//! shapes the surface has today and fails where the boundary is missing.

use serde_json::{Value, json};
use uuid::Uuid;

use crate::domain::resolution::MASK_TOKEN;
use crate::test_support::{RestHarness, SECRET};

// ── Fixtures ─────────────────────────────────────────────────────────────────

fn encoded(h: &RestHarness, name: &str) -> String {
    h.inner.key(name).to_string().replace('~', "%7E")
}

/// The single read of a setting at a tenant.
fn read_uri(h: &RestHarness, name: &str, tenant: Uuid) -> String {
    format!(
        "/settings-service/v1/settings/{}?tenant={tenant}",
        encoded(h, name)
    )
}

fn value_uri(h: &RestHarness, name: &str, tenant: Uuid) -> String {
    format!(
        "/settings-service/v1/settings/{}/value?tenant={tenant}",
        encoded(h, name)
    )
}

fn history_uri(h: &RestHarness, name: &str, tenant: Uuid) -> String {
    format!(
        "/settings-service/v1/settings/{}/history?tenant={tenant}",
        encoded(h, name)
    )
}

fn permissions_uri(h: &RestHarness, name: &str, tenant: Uuid) -> String {
    format!(
        "/settings-service/v1/settings/{}/permissions?tenant={tenant}",
        encoded(h, name)
    )
}

fn permissions_list_uri(h: &RestHarness, name: &str) -> String {
    format!(
        "/settings-service/v1/settings/{}/permissions/all",
        encoded(h, name)
    )
}

const REVIEW: &str = "/settings-service/v1/settings/review";

/// Read a setting at a tenant as `caller` and answer with the body.
async fn read(h: &RestHarness, name: &str, tenant: Uuid, caller: Uuid) -> Value {
    let (status, body) = h.get(&read_uri(h, name, tenant), caller).await;
    assert_eq!(status, 200, "{body}");
    body
}

/// Set a value through the write surface, so the row records who wrote it:
/// the first write at the scope, or an overwrite presenting the tag the
/// writer reads.
async fn put(h: &RestHarness, name: &str, tenant: Uuid, caller: Uuid, value: Value) {
    let tag = existing_tag(h, name, tenant, caller).await;
    let answer = h
        .send(
            "PUT",
            &value_uri(h, name, tenant),
            Some(json!({ "value": value })),
            Some(&tag),
            caller,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
}

/// `absent` where the scope holds no row of its own, the row's tag otherwise.
async fn existing_tag(h: &RestHarness, name: &str, tenant: Uuid, caller: Uuid) -> String {
    let body = read(h, name, tenant, caller).await;
    let trail = body["inheritance_trail"].as_array().expect("a trail");
    let holds_own_row = body["source"] == json!("own_override")
        || trail.iter().any(|e| {
            e["tenant_id"] == json!(tenant.to_string()) && e["has_override"] == json!(true)
        });
    if holds_own_row {
        body["etag"].as_str().expect("a tag").to_owned()
    } else {
        "absent".to_owned()
    }
}

fn trail(body: &Value) -> &Vec<Value> {
    body["inheritance_trail"].as_array().expect("a trail")
}

/// The trail entry of one tenant's scope.
fn entry_of(body: &Value, tenant: Uuid) -> &Value {
    trail(body)
        .iter()
        .find(|e| e["tenant_id"] == json!(tenant.to_string()))
        .unwrap_or_else(|| panic!("no entry for {tenant}: {body}"))
}

/// A setter shown to the reader: a name, not the mask, and no withheld flag.
fn assert_shown(entry: &Value, what: &str) {
    let who = entry["set_by"].as_str().unwrap_or("");
    assert!(
        !who.is_empty() && who != MASK_TOKEN,
        "{what}: the setter is shown: {entry}"
    );
    assert!(
        entry
            .get("set_by_withheld")
            .is_none_or(|w| w == &json!(false)),
        "{what}: a shown setter is not flagged withheld: {entry}"
    );
}

/// A setter withheld from the reader: the mask token and the flag that says
/// the mask is the boundary's, not the entitlement's.
fn assert_withheld(entry: &Value, what: &str) {
    assert_eq!(entry["set_by"], json!(MASK_TOKEN), "{what}: {entry}");
    assert_eq!(entry["set_by_withheld"], json!(true), "{what}: {entry}");
}

/// The entry the collapse leaves for everything above the reader: the verdict
/// and nothing that names a scope, a tenant, a setter or a time.
fn assert_collapsed(entry: &Value, provided_value: bool, has_override: bool) {
    assert_eq!(entry["above"], json!(true), "{entry}");
    assert_eq!(entry["provided_value"], json!(provided_value), "{entry}");
    assert_eq!(entry["has_override"], json!(has_override), "{entry}");
    assert_eq!(entry["needs_review"], json!(false), "{entry}");
    for named in ["scope", "tenant_id", "set_by", "last_change_at"] {
        assert!(
            entry.get(named).is_none(),
            "the collapsed entry carries no `{named}`: {entry}"
        );
    }
}

/// After a test moves a tenant, what an earlier read cached describes the old
/// tree; the boundary reads the tree as it stands now.
fn forget(h: &RestHarness, name: &str) {
    h.inner.cache.invalidate_key(h.inner.key(name).as_str());
}

/// A column of the stored rows as a tenant id. An error — a column that does
/// not exist yet — reads as no rows, so the assertion on it fails rather than
/// the test panicking.
async fn stored_tenants(h: &RestHarness, sql: &str) -> Vec<Option<Uuid>> {
    use sea_orm::TryGetable as _;
    let Ok(rows) = h.probe_query(sql).await else {
        return Vec::new();
    };
    rows.iter()
        .map(|row| {
            if let Ok(text) = Option::<String>::try_get(row, "", "v") {
                return text.and_then(|t| Uuid::parse_str(&t).ok());
            }
            Option::<Vec<u8>>::try_get(row, "", "v")
                .ok()
                .flatten()
                .and_then(|b| Uuid::from_slice(&b).ok())
        })
        .collect()
}

// ── The trail collapses ──────────────────────────────────────────────────────

#[tokio::test]
async fn a_tenant_readers_trail_collapses_what_is_above_its_tenant_and_keeps_the_rest_complete() {
    // §6 value-resolution: "A tenant reader's trail collapses the scopes
    // above its own tenant into one leading `above: true` entry ... and keeps
    // its own scope and those below it complete".
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set(id, root, json!(false)).await;
    put(&h, "proxy", a, a, json!(true)).await;

    // At the reader's own scope.
    let own = read(&h, "proxy", a, a).await;
    assert_eq!(
        trail(&own).len(),
        2,
        "one collapsed entry and the scope: {own}"
    );
    assert_collapsed(&trail(&own)[0], false, true);
    assert_eq!(trail(&own)[1]["scope"], json!(format!("/tenants/{a}")));
    assert_eq!(trail(&own)[1]["tenant_id"], json!(a.to_string()));
    assert_eq!(trail(&own)[1]["has_override"], json!(true));

    // At a descendant: the root collapsed, `a` and `b` complete.
    let below = read(&h, "proxy", b, a).await;
    assert_eq!(trail(&below).len(), 3, "{below}");
    assert_collapsed(&trail(&below)[0], false, true);
    assert_eq!(trail(&below)[1]["tenant_id"], json!(a.to_string()));
    assert_eq!(trail(&below)[2]["tenant_id"], json!(b.to_string()));
    assert_eq!(trail(&below)[2]["scope"], json!(format!("/tenants/{b}")));
    for entry in &trail(&below)[1..] {
        assert!(entry.get("above").is_none(), "{entry}");
    }
}

#[tokio::test]
async fn the_collapsed_entry_says_whether_the_winner_is_above_the_reader() {
    // The verdict the collapse keeps: `provided_value` when what the target
    // resolves to comes from above, `has_override` when a valid, unflagged
    // override sits there.
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set(id, root, json!(false)).await;

    let body = read(&h, "proxy", b, a).await;
    assert_collapsed(&trail(&body)[0], true, true);
    assert_eq!(body["value"], json!(false), "{body}");
}

#[tokio::test]
async fn a_platform_administrator_sees_the_whole_trail_and_every_setter() {
    // §6 value-resolution: "A platform administrator sees the whole trail and
    // every setter identity, unchanged".
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set(id, root, json!(false)).await;
    put(&h, "proxy", a, a, json!(true)).await;

    let body = read(&h, "proxy", b, root).await;
    assert_eq!(trail(&body).len(), 3, "{body}");
    for entry in trail(&body) {
        assert!(entry.get("above").is_none(), "nothing collapses: {entry}");
        assert!(entry["scope"].is_string(), "{entry}");
        assert!(entry["tenant_id"].is_string(), "{entry}");
    }
    assert_eq!(trail(&body)[0]["scope"], json!("/"));
    assert_shown(entry_of(&body, root), "the root's own row");
    assert_shown(entry_of(&body, a), "a tenant's row");
}

#[tokio::test]
async fn a_value_inherited_from_above_the_reader_names_no_source_scope() {
    // §6 value-resolution: "A value inherited from above the reader tenant
    // carries `source` `inherited` with no `source_scope`, and a fallback
    // inherited from above carries no `fallback_scope`".
    let h = RestHarness::new().await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set(id, root, json!(false)).await;

    let inherited = read(&h, "proxy", a, a).await;
    assert_eq!(inherited["source"], json!("inherited"), "{inherited}");
    assert!(
        inherited.get("source_scope").is_none(),
        "the root is above `a`: {inherited}"
    );

    // With an override of its own the value no longer comes from above, but
    // what it would fall back to does.
    put(&h, "proxy", a, a, json!(true)).await;
    let own = read(&h, "proxy", a, a).await;
    assert_eq!(own["fallback_source"], json!("inherited"), "{own}");
    assert!(
        own.get("fallback_scope").is_none(),
        "the fallback comes from the root, above `a`: {own}"
    );

    // The platform administrator is told where it comes from, as before.
    let platform = read(&h, "proxy", a, root).await;
    assert_eq!(platform["fallback_scope"], json!("/"), "{platform}");
}

#[tokio::test]
async fn a_flagged_row_above_the_reader_sets_neither_flag_on_the_collapsed_entry() {
    // §6 value-resolution: "A flagged ancestor row above the reader tenant
    // sets neither `has_override` nor `needs_review` on the collapsed entry".
    let h = RestHarness::new().await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set_flagged(id, root, json!(false)).await;

    let body = read(&h, "proxy", a, a).await;
    let first = &trail(&body)[0];
    assert_eq!(first["above"], json!(true), "{body}");
    assert_eq!(first["has_override"], json!(false), "{first}");
    assert_eq!(first["needs_review"], json!(false), "{first}");
}

// ── Who set it ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_platform_administrators_row_at_the_readers_scope_shows_its_value_and_withholds_its_setter()
 {
    // §6 value-resolution: "A platform administrator's row at the reader's own
    // scope shows its value and withholds `set_by` behind the mask token with
    // `set_by_withheld: true`, even for a caller with the PII entitlement".
    for h in [
        RestHarness::new().await,
        RestHarness::without_pii_entitlement().await,
    ] {
        let (root, a) = (h.inner.tree.root, h.inner.tree.a);
        h.inner.declare("proxy", "cascading", json!(true)).await;
        put(&h, "proxy", a, root, json!(false)).await;

        let body = read(&h, "proxy", a, a).await;
        assert_eq!(body["value"], json!(false), "the value is shown: {body}");
        assert_withheld(entry_of(&body, a), "the root's row at `a`");
    }
}

#[tokio::test]
async fn a_setter_at_the_readers_own_tenant_is_shown_and_still_pii_masked() {
    // The identity rule: shown when recorded on the path; a shown identity is
    // still masked for a caller without the PII entitlement.
    let entitled = RestHarness::new().await;
    let a = entitled.inner.tree.a;
    entitled
        .inner
        .declare("proxy", "cascading", json!(true))
        .await;
    put(&entitled, "proxy", a, a, json!(false)).await;
    let body = read(&entitled, "proxy", a, a).await;
    assert_shown(entry_of(&body, a), "own row, entitled");

    let masked = RestHarness::without_pii_entitlement().await;
    let a = masked.inner.tree.a;
    masked
        .inner
        .declare("proxy", "cascading", json!(true))
        .await;
    put(&masked, "proxy", a, a, json!(false)).await;
    let body = read(&masked, "proxy", a, a).await;
    let entry = entry_of(&body, a);
    assert_eq!(entry["set_by"], json!(MASK_TOKEN), "{entry}");
    assert!(
        entry
            .get("set_by_withheld")
            .is_none_or(|w| w == &json!(false)),
        "masked by the entitlement, not withheld by the boundary: {entry}"
    );
}

#[tokio::test]
async fn a_setter_recorded_below_the_reader_is_shown_on_the_trail() {
    // §6 value-resolution: "A setter recorded on the path below the reader
    // tenant is shown on the trail".
    let h = RestHarness::new().await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", b, b, json!(false)).await;

    let body = read(&h, "proxy", b, a).await;
    assert_shown(entry_of(&body, b), "`b`'s own row, read from `a`");
}

#[tokio::test]
async fn a_setter_whose_tenant_was_never_recorded_is_withheld_from_a_tenant_reader() {
    // §6 value-resolution: "A setter whose tenant was never recorded is
    // withheld from a tenant reader". A row the repository inserts without a
    // setter tenant is the row an older release left.
    let h = RestHarness::new().await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set(id, a, json!(false)).await;

    let tenant_reader = read(&h, "proxy", a, a).await;
    assert_withheld(entry_of(&tenant_reader, a), "an unrecorded setter");

    let platform = read(&h, "proxy", a, root).await;
    assert_shown(entry_of(&platform, a), "the platform administrator");
}

#[tokio::test]
async fn a_setter_moved_off_the_path_is_withheld_also_within_the_readers_subtree() {
    // §6 value-resolution: "A setter moved off the path from the reader tenant
    // to the row's scope is withheld, also when it was re-parented within the
    // reader's subtree". `d` sat between `a` and `b` when it wrote; `b` is
    // then moved to hang from `a` directly.
    let h = RestHarness::new().await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);
    let d = Uuid::new_v4();
    h.inner.hierarchy.add_tenant(d, a);
    h.inner.hierarchy.reparent(b, d);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", b, d, json!(false)).await;

    h.inner.hierarchy.reparent(b, a);
    forget(&h, "proxy");
    let body = read(&h, "proxy", b, a).await;
    assert_withheld(entry_of(&body, b), "a setter re-parented off the path");
}

#[tokio::test]
async fn a_setter_whose_tenant_was_deleted_is_withheld_from_a_tenant_reader() {
    // §6 value-resolution: "A setter whose tenant was deleted is withheld from
    // a tenant reader".
    let h = RestHarness::new().await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);
    let d = Uuid::new_v4();
    h.inner.hierarchy.add_tenant(d, a);
    h.inner.hierarchy.reparent(b, d);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", b, d, json!(false)).await;

    h.inner.hierarchy.reparent(b, a);
    h.inner.hierarchy.remove_tenant(d);
    forget(&h, "proxy");
    let body = read(&h, "proxy", b, a).await;
    assert_withheld(entry_of(&body, b), "a setter whose tenant is gone");
}

#[tokio::test]
async fn a_reader_tenant_moved_to_another_chain_sees_the_collapse_follow_its_new_chain() {
    // §6 value-resolution: "A reader tenant re-parented to another chain sees
    // the collapse follow its new chain". `a` is moved under `c`, which holds
    // an override of its own: the collapsed entry now covers `c` too.
    let h = RestHarness::new().await;
    let (a, c) = (h.inner.tree.a, h.inner.tree.c);
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set(id, c, json!(false)).await;
    h.inner.set(id, a, json!(true)).await;

    h.inner.hierarchy.reparent(a, c);
    forget(&h, "proxy");
    let body = read(&h, "proxy", a, a).await;
    assert_eq!(trail(&body).len(), 2, "root and `c` collapsed: {body}");
    assert_collapsed(&trail(&body)[0], false, true);
    assert_eq!(trail(&body)[1]["tenant_id"], json!(a.to_string()));
}

#[tokio::test]
async fn an_overwrite_moves_the_setter_with_the_row_in_both_directions() {
    // §6 value-writes: "An overwrite by a platform administrator over a row a
    // tenant set withholds the new setter from the tenant reader, and an
    // overwrite by the tenant over a platform-set row shows it".
    let h = RestHarness::new().await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    h.inner.declare("proxy", "cascading", json!(true)).await;

    put(&h, "proxy", a, a, json!(false)).await;
    let shown = read(&h, "proxy", a, a).await;
    assert_shown(entry_of(&shown, a), "the tenant's own first write");

    put(&h, "proxy", a, root, json!(true)).await;
    let withheld = read(&h, "proxy", a, a).await;
    assert_withheld(entry_of(&withheld, a), "the platform's overwrite");

    put(&h, "proxy", a, a, json!(false)).await;
    let shown_again = read(&h, "proxy", a, a).await;
    assert_shown(entry_of(&shown_again, a), "the tenant's overwrite of it");
}

#[tokio::test]
async fn two_readers_of_one_cached_scope_each_get_their_own_redaction() {
    // §6 value-resolution: "Two readers of one cached `(key, scope)` each
    // receive the trail redacted for themselves, and neither sees what the
    // other was shown".
    let h = RestHarness::new().await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set(id, root, json!(false)).await;
    put(&h, "proxy", a, root, json!(true)).await;

    let first = read(&h, "proxy", a, root).await;
    assert_eq!(trail(&first).len(), 2, "{first}");
    assert_shown(entry_of(&first, a), "the platform administrator");

    let tenant = read(&h, "proxy", a, a).await;
    assert_eq!(trail(&tenant).len(), 2, "{tenant}");
    assert_collapsed(&trail(&tenant)[0], false, true);
    assert_withheld(entry_of(&tenant, a), "the tenant reader");

    let again = read(&h, "proxy", a, root).await;
    assert_eq!(
        trail(&again),
        trail(&first),
        "what the tenant reader was shown did not leak back: {again}"
    );
    assert!(trail(&again).iter().all(|e| e.get("above").is_none()));
}

// ── The review listing ───────────────────────────────────────────────────────

#[tokio::test]
async fn the_review_listing_shows_a_setter_only_on_the_path_to_the_rows_scope() {
    // §6 value-resolution: "The review listing applies the identity rule to
    // each row's `set_by`: a setter recorded on the path from the reader
    // tenant to the row's scope is shown, any other is withheld with
    // `set_by_withheld: true`". Rows: at `a` by `a`, at `b` by the platform,
    // at `deep` below `b` by `b` (the path runs through the walk's parent
    // links), and at `loose` by nobody recorded.
    let h = RestHarness::probed().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    let (deep, loose) = (Uuid::new_v4(), Uuid::new_v4());
    h.inner.hierarchy.add_tenant(deep, b);
    h.inner.hierarchy.add_tenant(loose, a);
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", a, a, json!(false)).await;
    put(&h, "proxy", b, root, json!(false)).await;
    put(&h, "proxy", deep, b, json!(false)).await;
    h.inner.set(id, loose, json!(false)).await;
    h.probe_exec("UPDATE setting_values SET needs_review = 1, needs_review_detail = 'x'")
        .await;

    let tenant = h.items(REVIEW, a).await;
    let row = |items: &[Value], tenant: Uuid| -> Value {
        items
            .iter()
            .find(|i| i["tenant_id"] == json!(tenant.to_string()))
            .unwrap_or_else(|| panic!("no row at {tenant}: {items:?}"))
            .clone()
    };
    assert_eq!(tenant.len(), 4, "{tenant:?}");
    assert_shown(&row(&tenant, a), "set by the reader's own tenant");
    assert_withheld(&row(&tenant, b), "set by the platform");
    assert_shown(
        &row(&tenant, deep),
        "set by `b`, on the path a -> b -> deep",
    );
    assert_withheld(&row(&tenant, loose), "never recorded");

    let platform = h.items(REVIEW, root).await;
    for tenant_id in [a, b, deep, loose] {
        assert_shown(&row(&platform, tenant_id), "the platform administrator");
    }
}

// ── The embedded effective value of validate, revert and remove ──────────────

#[tokio::test]
async fn validate_revert_and_remove_render_their_effective_value_for_the_reader() {
    // §6 value-writes: "The effective value embedded in a validate, revert or
    // remove response is rendered for the reader: the scopes above its tenant
    // collapsed into one `above` entry, `source_scope` and `fallback_scope`
    // absent when above, and a setter off the path ... withheld".
    let h = RestHarness::new().await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    h.inner.set(id, root, json!(false)).await;
    put(&h, "proxy", a, root, json!(true)).await;

    // Validate, with the platform's row at `a` in the trail.
    let validated = h
        .send(
            "POST",
            &format!(
                "/settings-service/v1/settings/{}/validate?tenant={a}",
                encoded(&h, "proxy")
            ),
            Some(json!({ "value": false })),
            None,
            a,
        )
        .await;
    assert_eq!(validated.status, 200, "{}", validated.body);
    let effective = &validated.body["effective"];
    assert_collapsed(&trail(effective)[0], false, true);
    assert_withheld(entry_of(effective, a), "validate: the platform's row");
    assert!(
        effective["fallback_scope"].is_null(),
        "validate: the fallback comes from above: {effective}"
    );

    // Remove the row: the value falls back to the root, above the reader.
    let tag = existing_tag(&h, "proxy", a, a).await;
    let removed = h
        .send("DELETE", &value_uri(&h, "proxy", a), None, Some(&tag), a)
        .await;
    assert_eq!(removed.status, 200, "{}", removed.body);
    let effective = &removed.body["effective"];
    assert_eq!(effective["source"], json!("inherited"), "{effective}");
    assert!(
        effective.get("source_scope").is_none(),
        "remove: the source is above: {effective}"
    );
    assert_collapsed(&trail(effective)[0], true, true);

    // Revert: a row of the tenant's own, then reverted.
    put(&h, "proxy", a, a, json!(true)).await;
    let tag = existing_tag(&h, "proxy", a, a).await;
    let reverted = h
        .send(
            "POST",
            &format!(
                "/settings-service/v1/settings/{}/value/revert?tenant={a}",
                encoded(&h, "proxy")
            ),
            None,
            Some(&tag),
            a,
        )
        .await;
    assert_eq!(reverted.status, 200, "{}", reverted.body);
    let effective = &reverted.body["effective"];
    assert!(
        effective.get("source_scope").is_none(),
        "revert: the source is above: {effective}"
    );
    assert_collapsed(&trail(effective)[0], true, true);
}

// ── The permissions surface ──────────────────────────────────────────────────

/// Record a restriction as `caller`, with the tag the pair presents.
async fn restrict(h: &RestHarness, name: &str, tenant: Uuid, access: &str, caller: Uuid) -> Value {
    let read = h
        .send("GET", &permissions_uri(h, name, tenant), None, None, caller)
        .await;
    let tag = read.etag.unwrap_or_else(|| "absent".to_owned());
    let answer = h
        .send(
            "PUT",
            &permissions_uri(h, name, tenant),
            Some(json!({ "access": access })),
            Some(&tag),
            caller,
        )
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    answer.body
}

#[tokio::test]
async fn a_restriction_set_above_the_reader_shows_its_setter_withheld_on_the_read_and_the_list() {
    // §6 tenant-access: "A restriction set by an ancestor above the reader
    // tenant shows its `set_by` withheld, with `set_by_withheld: true`, on the
    // read and the list". Row at `a` by the platform, row at `b` by `a`.
    let h = RestHarness::new().await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    restrict(&h, "proxy", a, "read_only", h.inner.tree.root).await;
    restrict(&h, "proxy", b, "read_only", a).await;

    let read = h.get(&permissions_uri(&h, "proxy", a), a).await.1;
    assert_withheld(&read["stored"], "the read of the platform's restriction");

    let own = h.get(&permissions_uri(&h, "proxy", b), a).await.1;
    assert_shown(&own["stored"], "the read of the reader's own restriction");

    let listed = h.items(&permissions_list_uri(&h, "proxy"), a).await;
    let at = |tenant: Uuid| {
        listed
            .iter()
            .find(|r| r["tenant_id"] == json!(tenant.to_string()))
            .unwrap_or_else(|| panic!("no row at {tenant}: {listed:?}"))
            .clone()
    };
    assert_withheld(&at(a), "the list: the platform's restriction");
    assert_shown(&at(b), "the list: the reader's own restriction");
}

#[tokio::test]
async fn an_access_supplied_above_the_reader_is_read_with_the_supplier_collapsed() {
    // §6 tenant-access: "An access supplied by a tenant above the reader tenant
    // is read with `supplied_by` absent and `supplied_from_above: true`".
    let h = RestHarness::new().await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    restrict(&h, "proxy", a, "read_only", h.inner.tree.root).await;

    let from_b = h.get(&permissions_uri(&h, "proxy", b), b).await.1;
    assert_eq!(
        from_b["effective"]["access"],
        json!("read_only"),
        "{from_b}"
    );
    assert!(
        from_b["effective"].get("supplied_by").is_none(),
        "`a` is above `b`: {from_b}"
    );
    assert_eq!(
        from_b["effective"]["supplied_from_above"],
        json!(true),
        "{from_b}"
    );

    // A supplier that is the reader's own tenant is named, with no flag.
    let from_a = h.get(&permissions_uri(&h, "proxy", a), a).await.1;
    assert_eq!(
        from_a["effective"]["supplied_by"],
        json!(a.to_string()),
        "{from_a}"
    );
    assert!(from_a["effective"].get("supplied_from_above").is_none());
}

#[tokio::test]
async fn the_clear_response_collapses_a_supplier_above_the_reader() {
    // §6 tenant-access: "The set and clear responses collapse a supplying
    // tenant above the reader tenant". After `b` clears its own restriction at
    // `e`, the access it still sees there is `a`'s.
    let h = RestHarness::new().await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);
    let e = Uuid::new_v4();
    h.inner.hierarchy.add_tenant(e, b);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    restrict(&h, "proxy", a, "read_only", h.inner.tree.root).await;
    restrict(&h, "proxy", e, "read_only", b).await;

    let tag = h
        .send("GET", &permissions_uri(&h, "proxy", e), None, None, b)
        .await
        .etag
        .expect("a tag");
    let cleared = h
        .send(
            "DELETE",
            &permissions_uri(&h, "proxy", e),
            None,
            Some(&tag),
            b,
        )
        .await;
    assert_eq!(cleared.status, 200, "{}", cleared.body);
    assert_eq!(
        cleared.body["effective"]["access"],
        json!("read_only"),
        "{}",
        cleared.body
    );
    assert!(
        cleared.body["effective"].get("supplied_by").is_none(),
        "{}",
        cleared.body
    );
    assert_eq!(
        cleared.body["effective"]["supplied_from_above"],
        json!(true),
        "{}",
        cleared.body
    );
}

#[tokio::test]
async fn the_set_response_withholds_nothing_the_setter_may_see_and_names_nothing_above_it() {
    // §6 tenant-access (the set response): the answer to a set is read as the
    // setter, so its own tenant is on the path and is named, while a row set
    // above it never leaks into the response.
    let h = RestHarness::new().await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    restrict(&h, "proxy", a, "read_only", h.inner.tree.root).await;

    let set = restrict(&h, "proxy", b, "hidden", a).await;
    assert_shown(&set["stored"], "the set response to its own setter");
    assert_eq!(
        set["effective"]["supplied_by"],
        json!(b.to_string()),
        "the stricter row is at the target: {set}"
    );
    assert!(set["effective"].get("supplied_from_above").is_none());
}

#[tokio::test]
async fn a_platform_administrator_sees_every_setter_and_supplier_on_the_permissions_surface() {
    // §6 tenant-access: "A platform administrator sees every setter and every
    // supplying tenant on the permissions surface".
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    restrict(&h, "proxy", a, "read_only", root).await;
    restrict(&h, "proxy", b, "read_only", a).await;

    let read = h.get(&permissions_uri(&h, "proxy", b), root).await.1;
    assert_shown(&read["stored"], "the stored row");
    assert!(read["effective"]["supplied_by"].is_string(), "{read}");
    assert!(read["effective"].get("supplied_from_above").is_none());

    let listed = h.items(&permissions_list_uri(&h, "proxy"), root).await;
    assert_eq!(listed.len(), 2, "{listed:?}");
    for row in &listed {
        assert_shown(row, "the list");
    }
}

#[tokio::test]
async fn an_upsert_over_an_existing_restriction_rewrites_the_setter_tenant_with_the_setter() {
    // §6 tenant-access: "An upsert over an existing restriction rewrites
    // `set_by_tenant` together with `set_by`": `a` overwrites what the
    // platform set at `b`.
    let h = RestHarness::probed().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    let sql = "SELECT set_by_tenant AS v FROM tenant_permissions";

    restrict(&h, "proxy", b, "read_only", root).await;
    assert_eq!(stored_tenants(&h, sql).await, vec![Some(root)]);

    restrict(&h, "proxy", b, "hidden", a).await;
    assert_eq!(stored_tenants(&h, sql).await, vec![Some(a)]);

    // And the boundary follows: `a` now shows the setter it is, `b` as the
    // restricted tenant sees an ancestor's row withheld.
    let read = h.get(&permissions_uri(&h, "proxy", b), a).await.1;
    assert_shown(&read["stored"], "`a`'s own overwrite, read by `a`");
}

// ── History ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_history_actor_is_shown_on_the_path_and_withheld_above_the_reader() {
    // §6 audit-store: "A history record whose actor's tenant lies on the path
    // from the reader tenant to the record's tenant shows that actor" and "A
    // record whose actor sits above the reader tenant ... is the mask token
    // with `actor_withheld: true`"; and "A platform administrator sees every
    // actor in the history".
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", b, a, json!(false)).await;
    put(&h, "proxy", b, root, json!(true)).await;

    let by_operation = |items: &[Value], operation: &str| -> Value {
        items
            .iter()
            .find(|i| i["operation"] == json!(operation) && i["tenant_id"] == json!(b.to_string()))
            .unwrap_or_else(|| panic!("no `{operation}` record: {items:?}"))
            .clone()
    };

    let tenant = h.items(&history_uri(&h, "proxy", b), a).await;
    let created = by_operation(&tenant, "create");
    assert_ne!(created["actor"], json!(MASK_TOKEN), "{created}");
    assert!(
        created
            .get("actor_withheld")
            .is_none_or(|w| w == &json!(false)),
        "{created}"
    );
    let changed = by_operation(&tenant, "change");
    assert_eq!(changed["actor"], json!(MASK_TOKEN), "{changed}");
    assert_eq!(changed["actor_withheld"], json!(true), "{changed}");
    // The contract for a client that only knows `actor_masked`: it is true
    // whenever `actor` is the mask token, `actor_withheld` says why.
    assert_eq!(changed["actor_masked"], json!(true), "{changed}");

    let platform = h.items(&history_uri(&h, "proxy", b), root).await;
    for record in &platform {
        assert_ne!(record["actor"], json!(MASK_TOKEN), "{record}");
        assert!(
            record
                .get("actor_withheld")
                .is_none_or(|w| w == &json!(false)),
            "{record}"
        );
    }
}

#[tokio::test]
async fn a_definition_records_actor_is_withheld_from_a_tenant_reader() {
    // §6 audit-store: "... a definition record's actor ... [is] the mask token
    // with `actor_withheld: true` for a tenant reader". A definition record
    // belongs to no tenant, so it is on no path.
    let h = RestHarness::new().await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    let created = h
        .send(
            "POST",
            "/settings-service/v1/declarations",
            Some(json!({
                "value_type_id": crate::test_support::BOOL,
                "vendor": "acme",
                "name": "proxy",
                "category_id": h.inner.category_id(),
                "default_value": true,
                "scope_class": "cascading",
            })),
            None,
            root,
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.body);
    let key = created.body["key"]
        .as_str()
        .expect("a key")
        .replace('~', "%7E");
    let uri = format!("/settings-service/v1/settings/{key}/history?tenant={a}");

    let tenant = h.items(&uri, a).await;
    let definition: Vec<&Value> = tenant.iter().filter(|i| i["tenant_id"].is_null()).collect();
    assert_eq!(definition.len(), 1, "{tenant:?}");
    assert_eq!(
        definition[0]["actor"],
        json!(MASK_TOKEN),
        "{}",
        definition[0]
    );
    assert_eq!(
        definition[0]["actor_withheld"],
        json!(true),
        "{}",
        definition[0]
    );

    let platform = h.items(&uri, root).await;
    let definition: Vec<&Value> = platform
        .iter()
        .filter(|i| i["tenant_id"].is_null())
        .collect();
    assert_ne!(
        definition[0]["actor"],
        json!(MASK_TOKEN),
        "{}",
        definition[0]
    );
}

#[tokio::test]
async fn a_record_written_before_the_actor_tenant_existed_has_its_actor_withheld() {
    // §6 audit-store: "A record written before `actor_tenant_id` existed
    // carries `NULL` and its actor is withheld from a tenant reader". The
    // legacy record is a copy of a real one with the new column left unset.
    let h = RestHarness::probed().await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", b, a, json!(false)).await;

    let columns = h
        .probe_query("SELECT name FROM pragma_table_info('audit_records')")
        .await
        .expect("the table is readable");
    let names: Vec<String> = columns
        .iter()
        .map(|c| c.try_get::<String>("", "name").expect("a name"))
        .filter(|n| n != "id" && n != "actor_tenant_id")
        .collect();
    let cols = names.join(", ");
    let legacy = Uuid::new_v4();
    h.probe_exec(&format!(
        "INSERT INTO audit_records (id, {cols}) \
         SELECT X'{}', {cols} FROM audit_records WHERE operation = 'create'",
        legacy.simple()
    ))
    .await;

    let items = h.items(&history_uri(&h, "proxy", b), a).await;
    let creates: Vec<&Value> = items
        .iter()
        .filter(|i| i["operation"] == json!("create"))
        .collect();
    assert_eq!(
        creates.len(),
        2,
        "the record and its legacy copy: {items:?}"
    );
    let legacy_record = creates
        .iter()
        .find(|r| r["id"] == json!(legacy.to_string()))
        .unwrap_or_else(|| panic!("the legacy copy is listed: {items:?}"));
    assert_eq!(legacy_record["actor"], json!(MASK_TOKEN), "{legacy_record}");
    assert_eq!(
        legacy_record["actor_withheld"],
        json!(true),
        "{legacy_record}"
    );
    let original = creates
        .iter()
        .find(|r| r["id"] != json!(legacy.to_string()))
        .expect("the original");
    assert_ne!(original["actor"], json!(MASK_TOKEN), "{original}");
}

#[tokio::test]
async fn an_actor_moved_off_the_path_is_withheld_from_the_history() {
    // §6 audit-store: "An actor re-parented off the path from the reader
    // tenant to the record's tenant is withheld".
    let h = RestHarness::new().await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);
    let d = Uuid::new_v4();
    h.inner.hierarchy.add_tenant(d, a);
    h.inner.hierarchy.reparent(b, d);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", b, d, json!(false)).await;

    h.inner.hierarchy.reparent(b, a);
    let items = h.items(&history_uri(&h, "proxy", b), a).await;
    let created = items
        .iter()
        .find(|i| i["operation"] == json!("create"))
        .unwrap_or_else(|| panic!("no create record: {items:?}"));
    assert_eq!(created["actor"], json!(MASK_TOKEN), "{created}");
    assert_eq!(created["actor_withheld"], json!(true), "{created}");
}

// ── Recording ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_value_write_path_records_the_setter_tenant_beside_the_setter() {
    // §6 value-writes: "A write records `set_by_tenant` with `set_by`, on the
    // first insert and on an overwrite" — and the batch and clone paths, which
    // insert rows of their own.
    let h = RestHarness::probed().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    for name in ["set", "batch", "clone"] {
        h.inner.declare(name, "cascading", json!(true)).await;
    }
    let tenants = |name: &'static str| {
        let sql = format!(
            "SELECT set_by_tenant AS v FROM setting_values \
             WHERE declaration_id = (SELECT id FROM setting_declarations WHERE leaf_slug = '{name}')"
        );
        let h = &h;
        async move { stored_tenants(h, &sql).await }
    };

    // Set: the first insert, then an overwrite by another tenant.
    put(&h, "set", b, a, json!(false)).await;
    assert_eq!(tenants("set").await, vec![Some(a)], "first insert");
    put(&h, "set", b, root, json!(true)).await;
    assert_eq!(tenants("set").await, vec![Some(root)], "overwrite");

    // Batch.
    let batch = h
        .send(
            "POST",
            "/settings-service/v1/settings/batch",
            Some(json!({ "changes": [
                { "key": h.inner.key("batch").to_string(), "value": false, "if_match": "absent" },
            ]})),
            None,
            a,
        )
        .await;
    assert_eq!(batch.status, 200, "{}", batch.body);
    assert_eq!(tenants("batch").await, vec![Some(a)], "batch");

    // Clone, written by `a` from `b`'s row.
    put(&h, "clone", b, root, json!(false)).await;
    let cloned = h
        .send(
            "POST",
            &format!(
                "/settings-service/v1/settings/{}/value/clone?tenant={a}",
                encoded(&h, "clone")
            ),
            Some(json!({ "from": b })),
            Some("absent"),
            a,
        )
        .await;
    assert_eq!(cloned.status, 200, "{}", cloned.body);
    let mut stored = tenants("clone").await;
    stored.sort();
    let mut expected = vec![Some(root), Some(a)];
    expected.sort();
    assert_eq!(stored, expected, "clone stamps the cloner, not the source");
}

#[tokio::test]
async fn every_audit_record_stamps_the_actor_tenant_from_the_security_context() {
    // §6 audit-store (recording): each record carries the tenant of the
    // security context that wrote it.
    let h = RestHarness::probed().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", b, a, json!(false)).await;
    put(&h, "proxy", b, root, json!(true)).await;

    let mut stored = stored_tenants(&h, "SELECT actor_tenant_id AS v FROM audit_records").await;
    stored.sort();
    let mut expected = vec![Some(a), Some(root)];
    expected.sort();
    assert_eq!(stored, expected, "one record per write, none unstamped");
}

// ── Evolve and module upgrade keep the setter ────────────────────────────────

#[tokio::test]
async fn an_evolution_copies_each_value_with_its_original_setter_and_tenant() {
    // §6 setting-declarations: "An evolution copies every value with its
    // original `set_by` and `set_by_tenant` unchanged" and "A tenant
    // administrator's own value stays shown to that tenant after evolve".
    let h = RestHarness::probed().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    let body = |default: &str| {
        json!({
            "value_type_id": crate::test_support::TEXT,
            "vendor": "acme",
            "name": "proxy",
            "category_id": h.inner.category_id(),
            "default_value": default,
            "scope_class": "cascading",
        })
    };
    let created = h
        .send(
            "POST",
            "/settings-service/v1/declarations",
            Some(body("on")),
            None,
            root,
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.body);
    let v1 = created.body["key"].as_str().expect("a key").to_owned();
    let put_as = |key: String, tenant: Uuid, caller: Uuid, value: &str| {
        let h = &h;
        let uri = format!(
            "/settings-service/v1/settings/{}/value?tenant={tenant}",
            key.replace('~', "%7E")
        );
        let payload = json!({ "value": value });
        async move {
            let answer = h
                .send("PUT", &uri, Some(payload), Some("absent"), caller)
                .await;
            assert_eq!(answer.status, 200, "{}", answer.body);
        }
    };
    put_as(v1.clone(), a, a, "tenant value").await;
    put_as(v1, b, root, "platform value").await;

    let evolved = h
        .send(
            "POST",
            "/settings-service/v1/declarations",
            Some(body("off")),
            None,
            root,
        )
        .await;
    assert_eq!(evolved.status, 200, "{}", evolved.body);
    assert_eq!(evolved.body["evolved"], json!(true), "{}", evolved.body);
    let v2 = evolved.body["key"]
        .as_str()
        .expect("a key")
        .replace('~', "%7E");

    let mut stored = stored_tenants(
        &h,
        &format!(
            "SELECT set_by_tenant AS v FROM setting_values \
             WHERE declaration_id = (SELECT id FROM setting_declarations WHERE key = '{}')",
            evolved.body["key"].as_str().expect("a key")
        ),
    )
    .await;
    stored.sort();
    let mut expected = vec![Some(a), Some(root)];
    expected.sort();
    assert_eq!(
        stored, expected,
        "the copies keep the original setter tenants"
    );

    let read_v2 = |tenant: Uuid, caller: Uuid| {
        let h = &h;
        let uri = format!("/settings-service/v1/settings/{v2}?tenant={tenant}");
        async move { h.get(&uri, caller).await.1 }
    };
    let own = read_v2(a, a).await;
    assert_shown(entry_of(&own, a), "the tenant's own value after evolve");
    let platform_row = read_v2(b, a).await;
    assert_withheld(
        entry_of(&platform_row, b),
        "the platform's value after evolve",
    );
}

#[tokio::test]
async fn a_module_upgrade_copies_each_value_with_its_original_setter_and_records_no_actor_tenant() {
    // §6 module-contributions: "An upgrade copies every value with its original
    // `set_by` and `set_by_tenant` unchanged" and "A tenant administrator's own
    // value stays shown to that tenant after a module upgrade"; and the
    // module's own audit records name no actor tenant, so a tenant reader's
    // history withholds their actor and a platform administrator's shows it.
    use settings_service_sdk::SettingKey;
    use settings_service_sdk::api::SettingsContributionClient as _;
    use settings_service_sdk::models::{ContributedDeclaration, ScopeClass};
    use std::num::NonZeroU32;
    use std::sync::Arc;

    let h = RestHarness::probed().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    let service = Arc::new(crate::domain::contribution::ContributionService::new(
        crate::infra::storage::declaration_repo::DeclarationRepo,
        crate::infra::storage::category_repo::CategoryRepo,
        crate::infra::storage::value_repo::ValueRepo,
        Arc::new(crate::infra::type_validator::GtsTypeValidator::new(
            crate::test_support::resolution_catalogue(),
        )),
        Arc::new(crate::test_support::RecordingRegistrar::default())
            as Arc<dyn crate::domain::contribution::SettingTypeRegistrar>,
        crate::infra::storage::audit_store::AuditStore,
    ));
    let client = crate::infra::contribution_client::ContributionClient::new(
        Arc::clone(&h.inner.db),
        service,
        Arc::clone(&h.inner.cache),
        Arc::clone(&h.published) as Arc<dyn crate::domain::ports::ChangePublisher>,
    );
    let key = |major: u32| {
        SettingKey::contributed(
            "cf",
            "settings_demo",
            "network",
            "listen",
            NonZeroU32::new(major).expect("a major"),
        )
        .expect("a key")
    };
    let declare = |major: u32, default: &str| {
        ContributedDeclaration::new(
            key(major),
            crate::test_support::TEXT.to_owned(),
            json!(default),
            ScopeClass::Cascading,
        )
    };
    let ctx = toolkit_security::SecurityContext::anonymous();
    client
        .register_declarations(&ctx, "settings-demo".to_owned(), vec![declare(1, "on")])
        .await
        .expect("v1 registers");

    let uri = |major: u32, tenant: Uuid| {
        format!(
            "/settings-service/v1/settings/{}/value?tenant={tenant}",
            key(major).to_string().replace('~', "%7E")
        )
    };
    for (tenant, caller, value) in [(a, a, "tenant value"), (b, root, "platform value")] {
        let answer = h
            .send(
                "PUT",
                &uri(1, tenant),
                Some(json!({ "value": value })),
                Some("absent"),
                caller,
            )
            .await;
        assert_eq!(answer.status, 200, "{}", answer.body);
    }

    let upgraded = client
        .register_declarations(&ctx, "settings-demo".to_owned(), vec![declare(2, "off")])
        .await
        .expect("v2 registers");
    assert_eq!(upgraded.errors, Vec::new(), "{upgraded:?}");

    let v2 = key(2).to_string().replace('~', "%7E");
    let read_v2 = |tenant: Uuid| {
        let h = &h;
        let uri = format!("/settings-service/v1/settings/{v2}?tenant={tenant}");
        async move { h.get(&uri, a).await.1 }
    };
    let own = read_v2(a).await;
    assert_shown(
        entry_of(&own, a),
        "the tenant's own value after the upgrade",
    );
    let platform_row = read_v2(b).await;
    assert_withheld(
        entry_of(&platform_row, b),
        "the platform's value after the upgrade",
    );

    // The module's records name no actor tenant.
    let stamped = stored_tenants(
        &h,
        "SELECT actor_tenant_id AS v FROM audit_records WHERE actor = 'settings-demo'",
    )
    .await;
    assert!(!stamped.is_empty(), "the module left records: {stamped:?}");
    assert!(
        stamped.iter().all(Option::is_none),
        "a machine record names no actor tenant: {stamped:?}"
    );

    // And the history shows a tenant reader none of them.
    let items = h
        .items(
            &format!("/settings-service/v1/settings/{v2}/history?tenant={a}"),
            a,
        )
        .await;
    let machine: Vec<&Value> = items.iter().filter(|i| i["tenant_id"].is_null()).collect();
    assert!(!machine.is_empty(), "{items:?}");
    for record in machine {
        assert_eq!(record["actor"], json!(MASK_TOKEN), "{record}");
        assert_eq!(record["actor_withheld"], json!(true), "{record}");
    }

    // A platform administrator is shown them.
    let platform = h
        .items(
            &format!("/settings-service/v1/settings/{v2}/history?tenant={a}"),
            root,
        )
        .await;
    let machine: Vec<&Value> = platform
        .iter()
        .filter(|i| i["tenant_id"].is_null())
        .collect();
    assert!(!machine.is_empty(), "{platform:?}");
    for record in machine {
        assert_ne!(record["actor"], json!(MASK_TOKEN), "{record}");
        assert!(
            record
                .get("actor_withheld")
                .is_none_or(|w| w == &json!(false)),
            "{record}"
        );
    }
}

// ── The trail of a setting whose class is not `cascading` ────────────────────

/// A trail entry reads as the collapsed one: a verdict and no name.
fn assert_anonymous(entry: &Value) {
    assert_eq!(entry["above"], json!(true), "{entry}");
    for named in ["scope", "tenant_id", "set_by", "last_change_at"] {
        assert!(entry.get(named).is_none(), "`{named}` leaked: {entry}");
    }
}

#[tokio::test]
async fn a_global_settings_platform_row_is_collapsed_for_a_tenant_reader() {
    // A `global` setting's trail is the root alone, and a tenant reader is not
    // on it: what is above the reader is decided from the target's chain, so
    // the root's scope, id and administrator still do not reach `a`.
    let h = RestHarness::new().await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    h.inner.declare("flag", "global", json!(true)).await;
    put(&h, "flag", root, root, json!(false)).await;

    let body = read(&h, "flag", a, a).await;
    assert_eq!(trail(&body).len(), 1, "{body}");
    assert_anonymous(&trail(&body)[0]);
    assert_eq!(trail(&body)[0]["provided_value"], json!(true), "{body}");
    assert_eq!(trail(&body)[0]["has_override"], json!(true), "{body}");
    assert_eq!(body["value"], json!(false), "the value is still shown");
    assert!(body.get("source_scope").is_none(), "{body}");
    assert!(body.get("fallback_scope").is_none(), "{body}");
    assert!(
        !body.to_string().contains(&root.to_string()),
        "the root's id appears nowhere in the answer: {body}"
    );

    // The platform administrator is told everything, as before.
    let platform = read(&h, "flag", root, root).await;
    assert_eq!(trail(&platform)[0]["scope"], json!("/"), "{platform}");
    assert_shown(&trail(&platform)[0], "the platform administrator");
}

#[tokio::test]
async fn the_browse_collapses_a_platform_row_for_a_tenant_reader() {
    let h = RestHarness::new().await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    h.inner.declare("flag", "global", json!(true)).await;
    h.inner.declare("inherited", "cascading", json!(true)).await;
    put(&h, "flag", root, root, json!(false)).await;
    put(&h, "inherited", root, root, json!(false)).await;

    let (status, body) = h
        .get(&format!("/settings-service/v1/settings?tenant={a}"), a)
        .await;
    assert_eq!(status, 200, "{body}");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 2, "{body}");
    for item in items {
        let effective = &item["effective"];
        let first = &trail(effective)[0];
        assert_anonymous(first);
        assert_eq!(first["provided_value"], json!(true), "{item}");
        assert!(effective.get("source_scope").is_none(), "{item}");
        assert!(
            !item.to_string().contains(&root.to_string()),
            "the root's id appears nowhere in the item: {item}"
        );
    }

    let (_, platform) = h
        .get(
            &format!("/settings-service/v1/settings?tenant={root}"),
            root,
        )
        .await;
    for item in platform["items"].as_array().expect("items") {
        assert_eq!(trail(&item["effective"])[0]["scope"], json!("/"), "{item}");
    }
}

#[tokio::test]
async fn a_local_settings_row_at_a_descendant_is_shown_to_the_reader_above_it() {
    // A `local` trail is the target alone; the reader tenant is above it, not
    // on it, and must still see the target's own entry complete.
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    h.inner.declare("flag", "local", json!(true)).await;
    put(&h, "flag", b, a, json!(false)).await;

    let tenant = read(&h, "flag", b, a).await;
    assert_eq!(trail(&tenant).len(), 1, "{tenant}");
    assert!(trail(&tenant)[0].get("above").is_none(), "{tenant}");
    assert_eq!(trail(&tenant)[0]["tenant_id"], json!(b.to_string()));
    assert_shown(&trail(&tenant)[0], "set by `a`, on the path a -> b");

    let platform = read(&h, "flag", b, root).await;
    assert_eq!(trail(&platform)[0]["tenant_id"], json!(b.to_string()));
    assert_shown(&trail(&platform)[0], "the platform administrator");
}

// ── Responses that carry no provenance ───────────────────────────────────────

#[tokio::test]
async fn set_batch_and_clone_responses_carry_no_provenance() {
    // §6 value-writes: "Set, batch and clone responses carry no provenance and
    // are unchanged": no trail, no scope above, no setter, no supplier.
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    for name in ["set", "batch", "clone"] {
        let id = h.inner.declare(name, "cascading", json!(true)).await;
        h.inner.set(id, root, json!(false)).await;
    }
    let provenance = |what: &str, body: &Value| {
        let wire = body.to_string();
        for named in [
            "inheritance_trail",
            "source_scope",
            "fallback_scope",
            "set_by",
            "supplied_by",
            "above",
        ] {
            assert!(
                !wire.contains(&format!("\"{named}\"")),
                "{what} carries `{named}`: {wire}"
            );
        }
        assert!(
            !wire.contains(&root.to_string()),
            "{what} names the root: {wire}"
        );
    };

    let set = h
        .send(
            "PUT",
            &value_uri(&h, "set", a),
            Some(json!({ "value": true })),
            Some("absent"),
            a,
        )
        .await;
    assert_eq!(set.status, 200, "{}", set.body);
    provenance("set", &set.body);

    let batch = h
        .send(
            "POST",
            "/settings-service/v1/settings/batch",
            Some(json!({ "changes": [
                { "key": h.inner.key("batch").to_string(), "value": true, "if_match": "absent" },
            ]})),
            None,
            a,
        )
        .await;
    assert_eq!(batch.status, 200, "{}", batch.body);
    provenance("batch", &batch.body);

    put(&h, "clone", b, a, json!(true)).await;
    let cloned = h
        .send(
            "POST",
            &format!(
                "/settings-service/v1/settings/{}/value/clone?tenant={a}",
                encoded(&h, "clone")
            ),
            Some(json!({ "from": b })),
            Some("absent"),
            a,
        )
        .await;
    assert_eq!(cloned.status, 200, "{}", cloned.body);
    provenance("clone", &cloned.body);
}

// ── The actor tenant is stamped from the security context ────────────────────

#[tokio::test]
async fn category_declaration_restriction_and_secret_records_name_the_callers_tenant() {
    let h = RestHarness::probed().await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);
    let tenants = |operation: &'static str, key: &'static str| {
        let sql = format!(
            "SELECT actor_tenant_id AS v FROM audit_records \
             WHERE operation = '{operation}' AND declaration_key LIKE '%{key}%'"
        );
        let h = &h;
        async move { stored_tenants(h, &sql).await }
    };

    // A category change, by `a`.
    let created = h
        .send(
            "POST",
            "/settings-service/v1/categories",
            Some(json!({ "key": "storage", "name": "Storage", "sort_order": 0 })),
            None,
            a,
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.body);
    assert_eq!(
        tenants("create", "storage").await,
        vec![Some(a)],
        "category"
    );

    // A declaration create, by `a`.
    let declared = h
        .send(
            "POST",
            "/settings-service/v1/declarations",
            Some(json!({
                "value_type_id": crate::test_support::BOOL,
                "vendor": "acme",
                "name": "proxy",
                "category_id": h.inner.category_id(),
                "default_value": true,
                "scope_class": "cascading",
            })),
            None,
            a,
        )
        .await;
    assert_eq!(declared.status, 201, "{}", declared.body);
    assert_eq!(
        tenants("create", "proxy").await,
        vec![Some(a)],
        "declaration"
    );

    // A restriction set and cleared, by `a` over `b`.
    h.inner.declare("limit", "cascading", json!(true)).await;
    restrict(&h, "limit", b, "read_only", a).await;
    let tag = h
        .send("GET", &permissions_uri(&h, "limit", b), None, None, a)
        .await
        .etag
        .expect("a tag");
    let cleared = h
        .send(
            "DELETE",
            &permissions_uri(&h, "limit", b),
            None,
            Some(&tag),
            a,
        )
        .await;
    assert_eq!(cleared.status, 200, "{}", cleared.body);
    assert_eq!(tenants("create", "limit").await, vec![Some(a)], "set");
    assert_eq!(tenants("remove", "limit").await, vec![Some(a)], "clear");

    // A secret staged, by `a`.
    h.inner
        .declare_typed("token", "cascading", json!(""), SECRET, "secret")
        .await;
    let staged = h
        .send(
            "POST",
            &format!(
                "/settings-service/v1/settings/{}/secret-stage?tenant={a}",
                encoded(&h, "token")
            ),
            Some(json!({ "value": "hunter2" })),
            None,
            a,
        )
        .await;
    assert_eq!(staged.status, 200, "{}", staged.body);
    assert_eq!(tenants("stage", "token").await, vec![Some(a)], "stage");
}
