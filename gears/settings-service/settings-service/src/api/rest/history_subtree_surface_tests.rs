//! The history of a setting over a tenant's subtree, driven as a client
//! drives it: `GET /settings/{key}/history?descendants=true`.
//!
//! One list holds the records of the target and of every tenant below it that
//! administration from the target reaches, newest first. Nothing about what a
//! reader is shown changes with the mode: every record is rendered as the
//! single-scope history of its own tenant renders it, and the tests below
//! compare the two record by record.
//!
//! The tree is `root -> a -> b`, `root -> c`, `a -> s` (standalone); some
//! tests add `deep` below `b`.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};
use uuid::Uuid;

use super::history_actor_names_tests::{FakeAm, admin_of};
use crate::domain::access::TenantAccess;
use crate::domain::resolution::MASK_TOKEN;
use crate::test_support::RestHarness;

// ── Fixtures ─────────────────────────────────────────────────────────────────

fn encoded(h: &RestHarness, name: &str) -> String {
    h.inner.key(name).to_string().replace('~', "%7E")
}

/// The single-scope history of a setting at a tenant: the mode a request
/// without `descendants` reads.
fn scope_uri(h: &RestHarness, name: &str, tenant: Uuid) -> String {
    format!(
        "/settings-service/v1/settings/{}/history?tenant={tenant}",
        encoded(h, name)
    )
}

/// The history of a setting at a tenant and below it.
fn subtree_uri(h: &RestHarness, name: &str, tenant: Uuid) -> String {
    format!("{}&descendants=true", scope_uri(h, name, tenant))
}

/// Write a value at `tenant` as `caller`'s administrator: the first write at
/// the scope, or an overwrite presenting the tag the scope's row carries.
///
/// Then let the microsecond clock move on. Writes made back to back can be
/// recorded in the same microsecond, and the record's id, which is random,
/// then decides their order; a test about the order of the list must not
/// depend on that, so every write's record is on an instant of its own.
async fn put(h: &RestHarness, name: &str, tenant: Uuid, caller: Uuid, value: Value) {
    write(h, name, tenant, caller, value).await;
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
}

async fn write(h: &RestHarness, name: &str, tenant: Uuid, caller: Uuid, value: Value) {
    let uri = format!(
        "/settings-service/v1/settings/{}/value?tenant={tenant}",
        encoded(h, name)
    );
    let first = h
        .send(
            "PUT",
            &uri,
            Some(json!({ "value": value })),
            Some("absent"),
            caller,
        )
        .await;
    if first.status == 200 {
        return;
    }
    // The scope holds a row already: read its tag and overwrite.
    let read = h
        .send(
            "GET",
            &format!(
                "/settings-service/v1/settings/{}?tenant={tenant}",
                encoded(h, name)
            ),
            None,
            None,
            caller,
        )
        .await;
    assert_eq!(read.status, 200, "{}", read.body);
    let tag = read.body["etag"].as_str().expect("a tag").to_owned();
    let second = h
        .send(
            "PUT",
            &uri,
            Some(json!({ "value": value })),
            Some(&tag),
            caller,
        )
        .await;
    assert_eq!(second.status, 200, "{}", second.body);
}

/// `(tenant, operation)` of each record, in the order the page lists them.
fn shape(items: &[Value]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|i| {
            (
                i["tenant_id"].as_str().unwrap_or("definition").to_owned(),
                i["operation"].as_str().expect("an operation").to_owned(),
            )
        })
        .collect()
}

/// The records of a page by id.
fn by_id(items: &[Value]) -> BTreeMap<String, Value> {
    items
        .iter()
        .map(|i| (i["id"].as_str().expect("an id").to_owned(), i.clone()))
        .collect()
}

/// A tree with `deep` below `b`, so a record can sit two levels below a target.
fn with_deep(h: &RestHarness) -> Uuid {
    let deep = Uuid::new_v4();
    h.inner.hierarchy.add_tenant(deep, h.inner.tree.b);
    deep
}

// ── What the list holds ──────────────────────────────────────────────────────

#[tokio::test]
async fn the_subtree_history_lists_the_target_and_its_descendants_newest_first() {
    let h = RestHarness::new().await;
    let (root, a, b, c) = (
        h.inner.tree.root,
        h.inner.tree.a,
        h.inner.tree.b,
        h.inner.tree.c,
    );
    h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", a, root, json!(false)).await;
    put(&h, "proxy", b, root, json!(false)).await;
    put(&h, "proxy", c, root, json!(false)).await;
    put(&h, "proxy", a, root, json!(true)).await;

    let (status, body) = h.get(&subtree_uri(&h, "proxy", a), root).await;
    assert_eq!(status, 200, "{body}");
    let items = body["items"].as_array().expect("items");
    assert_eq!(
        shape(items),
        vec![
            (a.to_string(), "change".to_owned()),
            (b.to_string(), "create".to_owned()),
            (a.to_string(), "create".to_owned()),
        ],
        "the target's and the descendant's records in one list, newest first, the sibling's \
         absent: {body}"
    );
    let stamps: Vec<&str> = items
        .iter()
        .map(|i| i["occurred_at"].as_str().expect("a timestamp"))
        .collect();
    assert!(
        stamps.windows(2).all(|w| w[0] >= w[1]),
        "newest first across scopes: {stamps:?}"
    );
}

#[tokio::test]
async fn a_request_without_the_parameter_reads_one_scope_as_before() {
    // The default is the single-scope history: the same body whether the
    // parameter is absent or `false`, the target's records only, and the
    // total beside the cursors.
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", a, root, json!(false)).await;
    put(&h, "proxy", b, root, json!(false)).await;

    let absent = h
        .send("GET", &scope_uri(&h, "proxy", a), None, None, root)
        .await;
    assert_eq!(absent.status, 200, "{}", absent.body);
    assert_eq!(
        shape(absent.body["items"].as_array().expect("items")),
        vec![(a.to_string(), "create".to_owned())],
        "{}",
        absent.body
    );
    assert_eq!(
        absent.body["page_info"]["total_count"],
        json!(1),
        "{}",
        absent.body
    );

    let off = h
        .send(
            "GET",
            &format!("{}&descendants=false", scope_uri(&h, "proxy", a)),
            None,
            None,
            root,
        )
        .await;
    assert_eq!(off.status, 200, "{}", off.body);
    assert_eq!(off.raw, absent.raw, "`descendants=false` is the default");
}

#[tokio::test]
async fn a_value_that_is_not_a_boolean_is_refused() {
    let h = RestHarness::new().await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    for raw in ["yes", "1", "TRUE", ""] {
        let (status, body) = h
            .get(
                &format!("{}&descendants={raw}", scope_uri(&h, "proxy", a)),
                root,
            )
            .await;
        assert_eq!(status, 400, "`descendants={raw}`: {body}");
    }
}

#[tokio::test]
async fn the_subtree_history_reports_no_total_and_the_scope_history_keeps_it() {
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", a, root, json!(false)).await;
    put(&h, "proxy", b, root, json!(false)).await;

    let (status, subtree) = h.get(&subtree_uri(&h, "proxy", a), root).await;
    assert_eq!(status, 200, "{subtree}");
    assert!(
        subtree["page_info"].get("total_count").is_none(),
        "the field is absent, not null: {subtree}"
    );
    assert!(subtree["page_info"].get("limit").is_some(), "{subtree}");

    let (status, scope) = h.get(&scope_uri(&h, "proxy", a), root).await;
    assert_eq!(status, 200, "{scope}");
    assert_eq!(scope["page_info"]["total_count"], json!(1), "{scope}");
}

#[tokio::test]
async fn a_definition_record_is_listed_once_in_the_subtree_history() {
    // A definition record belongs to no tenant. It is in every scope's
    // history, and in a list over several scopes it is still one record.
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
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
    for tenant in [a, b] {
        let written = h
            .send(
                "PUT",
                &format!("/settings-service/v1/settings/{key}/value?tenant={tenant}"),
                Some(json!({ "value": false })),
                Some("absent"),
                root,
            )
            .await;
        assert_eq!(written.status, 200, "{}", written.body);
    }

    let uri = format!("/settings-service/v1/settings/{key}/history?tenant={a}&descendants=true");
    let items = h.items(&uri, root).await;
    let definitions = items
        .iter()
        .filter(|i| i.get("tenant_id").is_none())
        .count();
    assert_eq!(definitions, 1, "{items:?}");
    assert_eq!(items.len(), 3, "two values and the definition: {items:?}");

    // A tenant reader is shown it exactly as its own scope's history shows it:
    // the actor is on no path, so it is withheld.
    let tenant_view = h.items(&uri, a).await;
    let definition = tenant_view
        .iter()
        .find(|i| i.get("tenant_id").is_none())
        .unwrap_or_else(|| panic!("the definition record is listed: {tenant_view:?}"));
    assert_eq!(definition["actor"], json!(MASK_TOKEN), "{definition}");
    assert_eq!(definition["actor_withheld"], json!(true), "{definition}");
}

// ── The edge of the subtree ──────────────────────────────────────────────────

#[tokio::test]
async fn a_standalone_descendant_is_left_out_of_the_subtree_history() {
    // `s` is standalone below `a`: administration from above does not reach
    // it, so its records are in no ancestor's list — as its single-scope
    // history is refused to them.
    let h = RestHarness::new().await;
    let (root, a, b, s) = (
        h.inner.tree.root,
        h.inner.tree.a,
        h.inner.tree.b,
        h.inner.tree.s,
    );
    h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", s, s, json!(false)).await;
    put(&h, "proxy", b, a, json!(false)).await;

    for (target, reader) in [(a, a), (a, root), (root, root)] {
        let items = h.items(&subtree_uri(&h, "proxy", target), reader).await;
        assert_eq!(
            shape(&items),
            vec![(b.to_string(), "create".to_owned())],
            "no record of the standalone tenant for target {target} read by {reader}"
        );
    }
    let (status, _) = h.get(&scope_uri(&h, "proxy", s), a).await;
    assert_eq!(status, 403, "the single-scope read of it is refused too");

    // The standalone tenant still reads its own.
    let own = h.items(&subtree_uri(&h, "proxy", s), s).await;
    assert_eq!(shape(&own), vec![(s.to_string(), "create".to_owned())]);
}

#[tokio::test]
async fn a_record_outside_the_targets_subtree_is_never_listed() {
    // Records at an ancestor of the target, at a sibling and at the sibling's
    // side of the tree are not the target's subtree, whoever reads.
    let h = RestHarness::new().await;
    let (root, a, b, c) = (
        h.inner.tree.root,
        h.inner.tree.a,
        h.inner.tree.b,
        h.inner.tree.c,
    );
    let deep = with_deep(&h);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    for tenant in [root, a, b, c, deep] {
        put(&h, "proxy", tenant, root, json!(false)).await;
    }

    for reader in [root, a, b] {
        let items = h.items(&subtree_uri(&h, "proxy", b), reader).await;
        let mut tenants: Vec<String> = shape(&items).into_iter().map(|(t, _)| t).collect();
        tenants.sort();
        let mut expected = vec![b.to_string(), deep.to_string()];
        expected.sort();
        assert_eq!(tenants, expected, "target `b` read by {reader}");
    }
}

#[tokio::test]
async fn the_target_is_gated_as_the_single_scope_history_gates_it() {
    // The mode does not widen what a caller may target: a sibling, an
    // ancestor and a standalone descendant are refused with or without it.
    let h = RestHarness::new().await;
    let (root, a, c, s) = (
        h.inner.tree.root,
        h.inner.tree.a,
        h.inner.tree.c,
        h.inner.tree.s,
    );
    h.inner.declare("proxy", "cascading", json!(true)).await;
    for target in [root, c, s] {
        let (scope, _) = h.get(&scope_uri(&h, "proxy", target), a).await;
        let (subtree, body) = h.get(&subtree_uri(&h, "proxy", target), a).await;
        assert_eq!(scope, 403);
        assert_eq!(subtree, 403, "{body}");
    }
}

#[tokio::test]
async fn a_subtree_past_the_budget_is_refused() {
    let h = RestHarness::new().await;
    let (root, a) = (h.inner.tree.root, h.inner.tree.a);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", a, root, json!(false)).await;
    h.inner
        .hierarchy
        .truncate_subtrees
        .store(true, Ordering::SeqCst);

    let (status, body) = h.get(&subtree_uri(&h, "proxy", a), root).await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body.to_string().contains("subtree_too_large"),
        "the refusal names its reason: {body}"
    );

    // The review listing refuses the same subtree the same way.
    let (review_status, review) = h
        .get(
            &format!("/settings-service/v1/settings/review?tenant={a}"),
            root,
        )
        .await;
    assert_eq!(review_status, 400, "{review}");
    assert_eq!(body["code"], review["code"], "{body} / {review}");
    assert_eq!(body["type"], review["type"], "{body} / {review}");

    // The single-scope history walks no subtree and is not affected.
    let (status, body) = h.get(&scope_uri(&h, "proxy", a), root).await;
    assert_eq!(status, 200, "{body}");
}

// ── Disclosure, per record ───────────────────────────────────────────────────

#[tokio::test]
async fn every_record_is_shown_as_its_own_scopes_history_shows_it() {
    // The boundary is applied per record, on the path from the reader tenant
    // down to that record's tenant. So the subtree list is exactly the union
    // of the single-scope histories of the tenants it covers, each record
    // byte-equal to how its own scope's history renders it for the same
    // reader: nothing more is shown, and nothing less.
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    let deep = with_deep(&h);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    // Actors above the reader `a`, at it, and below it, at each depth.
    put(&h, "proxy", a, root, json!(false)).await;
    put(&h, "proxy", a, a, json!(true)).await;
    put(&h, "proxy", b, root, json!(false)).await;
    put(&h, "proxy", b, a, json!(true)).await;
    put(&h, "proxy", b, b, json!(false)).await;
    put(&h, "proxy", deep, root, json!(false)).await;
    put(&h, "proxy", deep, a, json!(true)).await;
    put(&h, "proxy", deep, b, json!(false)).await;
    put(&h, "proxy", deep, deep, json!(true)).await;

    for (reader, target, covered) in [
        (a, a, vec![a, b, deep]),
        (a, b, vec![b, deep]),
        (b, b, vec![b, deep]),
        (root, a, vec![a, b, deep]),
        (root, root, vec![root, a, b, deep]),
    ] {
        let subtree = h.items(&subtree_uri(&h, "proxy", target), reader).await;
        let mut union = BTreeMap::new();
        for tenant in &covered {
            let (status, body) = h.get(&scope_uri(&h, "proxy", *tenant), reader).await;
            assert_eq!(status, 200, "{body}");
            union.extend(by_id(body["items"].as_array().expect("items")));
        }
        assert_eq!(
            by_id(&subtree),
            union,
            "reader {reader}, target {target}: the subtree list is the union of the \
             single-scope histories, record for record"
        );
        assert_eq!(subtree.len(), union.len(), "no record twice");
    }

    // And the cases by name, for the tenant reader `a` over its own subtree.
    let items = h.items(&subtree_uri(&h, "proxy", a), a).await;
    let withheld = |tenant: Uuid, operation: &str| -> bool {
        let record = items
            .iter()
            .find(|i| {
                i["tenant_id"] == json!(tenant.to_string()) && i["operation"] == json!(operation)
            })
            .unwrap_or_else(|| panic!("no `{operation}` at {tenant}: {items:?}"));
        let withheld = record
            .get("actor_withheld")
            .is_some_and(|w| w == &json!(true));
        assert_eq!(
            record["actor"] == json!(MASK_TOKEN),
            withheld,
            "the mask and the flag agree: {record}"
        );
        withheld
    };
    assert!(withheld(a, "create"), "written from above the reader");
    assert!(!withheld(a, "change"), "written from the reader tenant");
    assert!(withheld(b, "create"), "written from above the reader");
    assert!(withheld(deep, "create"), "written from above the reader");
    // The last write at `deep` is its own: its tenant is on the reader's path
    // to it only when the path runs past the target down to it.
    let newest_below = items
        .iter()
        .find(|i| i["tenant_id"] == json!(deep.to_string()))
        .expect("a record at the deep tenant");
    assert_ne!(newest_below["actor"], json!(MASK_TOKEN), "{newest_below}");
}

#[tokio::test]
async fn an_actor_from_a_sibling_branch_is_withheld_in_the_subtree_history() {
    // An actor whose tenant is below the reader but not on the path to the
    // record's tenant is withheld: the path is per record, not "anywhere in
    // the subtree". `e` and `b` are both children of `a`; a record at `b`
    // written from `e` (re-parented away afterwards) is not `a -> b`.
    let h = RestHarness::new().await;
    let (a, b) = (h.inner.tree.a, h.inner.tree.b);
    let e = Uuid::new_v4();
    h.inner.hierarchy.add_tenant(e, a);
    h.inner.hierarchy.reparent(b, e);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", b, e, json!(false)).await;
    h.inner.hierarchy.reparent(b, a);

    let items = h.items(&subtree_uri(&h, "proxy", a), a).await;
    let record = items
        .iter()
        .find(|i| i["tenant_id"] == json!(b.to_string()))
        .unwrap_or_else(|| panic!("the record at b: {items:?}"));
    assert_eq!(record["actor"], json!(MASK_TOKEN), "{record}");
    assert_eq!(record["actor_withheld"], json!(true), "{record}");
}

#[tokio::test]
async fn values_and_actors_are_masked_per_record_without_the_entitlement() {
    // The masking of values and of a `pii` actor is the single-scope one, on
    // every record of the list.
    let h = RestHarness::without_pii_entitlement().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    h.inner
        .declare_typed(
            "contact",
            "cascading",
            json!("nobody"),
            crate::test_support::TEXT,
            "pii",
        )
        .await;
    put(&h, "contact", a, root, json!("ada")).await;
    put(&h, "contact", b, root, json!("bob")).await;

    let items = h.items(&subtree_uri(&h, "contact", a), root).await;
    assert_eq!(items.len(), 2, "{items:?}");
    for item in &items {
        assert_eq!(item["values_masked"], json!(true), "{item}");
        assert_eq!(item["post_value"], json!(MASK_TOKEN), "{item}");
        assert_eq!(item["actor_masked"], json!(true), "{item}");
        assert_eq!(item["actor"], json!(MASK_TOKEN), "{item}");
        assert_eq!(item["actor_name_status"], json!("masked"), "{item}");
    }
}

// ── Hidden settings ──────────────────────────────────────────────────────────

#[tokio::test]
async fn a_setting_hidden_at_a_descendant_keeps_its_records_in_the_ancestors_list() {
    // Hiding acts on the tenant it names and below: that tenant's own read is
    // `404`, while an ancestor — the one that hides it — still reads the
    // records of the scope, alone or in the subtree list.
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", a, a, json!(false)).await;
    put(&h, "proxy", b, a, json!(false)).await;
    h.restrict(id, b, TenantAccess::Hidden).await;

    for reader in [a, root] {
        let items = h.items(&subtree_uri(&h, "proxy", a), reader).await;
        let at_b = items
            .iter()
            .filter(|i| i["tenant_id"] == json!(b.to_string()))
            .count();
        assert!(
            at_b >= 1,
            "the hidden descendant's records, read by {reader}: {items:?}"
        );
        // Exactly what the single-scope read of that scope shows the same
        // reader.
        let (status, scope) = h.get(&scope_uri(&h, "proxy", b), reader).await;
        assert_eq!(status, 200, "{scope}");
        let scope = by_id(scope["items"].as_array().expect("items"));
        for (id, record) in by_id(&items) {
            if record["tenant_id"] == json!(b.to_string()) {
                assert_eq!(Some(&record), scope.get(&id));
            }
        }
    }

    // The tenant it is hidden from reads neither.
    for uri in [scope_uri(&h, "proxy", b), subtree_uri(&h, "proxy", b)] {
        let (status, body) = h.get(&uri, b).await;
        assert_eq!(status, 404, "{body}");
    }
}

#[tokio::test]
async fn a_setting_hidden_from_the_caller_has_no_subtree_history() {
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    let id = h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", b, root, json!(false)).await;
    h.restrict(id, a, TenantAccess::Hidden).await;

    let (status, body) = h.get(&subtree_uri(&h, "proxy", a), a).await;
    assert_eq!(status, 404, "{body}");
    // Hidden at `a` is hidden below it: `b` reads none either.
    let (status, body) = h.get(&subtree_uri(&h, "proxy", b), b).await;
    assert_eq!(status, 404, "{body}");
}

// ── Cursors ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_subtree_history_pages_through_every_record_once() {
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    let deep = with_deep(&h);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    for (tenant, value) in [
        (a, false),
        (b, false),
        (deep, false),
        (a, true),
        (b, true),
        (deep, true),
        (a, false),
    ] {
        put(&h, "proxy", tenant, root, json!(value)).await;
    }

    let whole = h.items(&subtree_uri(&h, "proxy", a), root).await;
    assert_eq!(whole.len(), 7, "{whole:?}");
    let expected: Vec<&str> = whole
        .iter()
        .map(|i| i["id"].as_str().expect("id"))
        .collect();

    let mut walked: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let uri = match &cursor {
            Some(token) => format!("{}&limit=3&cursor={token}", subtree_uri(&h, "proxy", a)),
            None => format!("{}&limit=3", subtree_uri(&h, "proxy", a)),
        };
        let (status, body) = h.get(&uri, root).await;
        assert_eq!(status, 200, "{body}");
        pages += 1;
        for item in body["items"].as_array().expect("items") {
            walked.push(item["id"].as_str().expect("id").to_owned());
        }
        assert!(
            body["page_info"].get("total_count").is_none(),
            "no page of the walk reports a total: {body}"
        );
        match body["page_info"]["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
        assert!(pages < 10, "the walk ends");
    }
    assert_eq!(pages, 3, "seven records in pages of three");
    assert_eq!(
        walked, expected,
        "the pages are the unpaged list, in order, no record twice"
    );
}

#[tokio::test]
async fn a_cursor_of_one_mode_is_refused_by_the_other() {
    // A cursor is a boundary in one result set. The two modes of one
    // (setting, tenant) are two result sets, so each refuses the other's
    // cursor — with the refusal a cursor of another scope's history gets.
    let h = RestHarness::new().await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    for value in [false, true, false] {
        put(&h, "proxy", a, root, json!(value)).await;
        put(&h, "proxy", b, root, json!(value)).await;
    }
    let next = |body: &Value| -> String {
        body["page_info"]["next_cursor"]
            .as_str()
            .unwrap_or_else(|| panic!("a second page: {body}"))
            .to_owned()
    };
    let scope = format!("{}&limit=1", scope_uri(&h, "proxy", a));
    let subtree = format!("{}&limit=1", subtree_uri(&h, "proxy", a));
    let (_, first_scope) = h.get(&scope, root).await;
    let (_, first_subtree) = h.get(&subtree, root).await;
    let (scope_cursor, subtree_cursor) = (next(&first_scope), next(&first_subtree));

    // Each continues its own.
    let (status, body) = h.get(&format!("{scope}&cursor={scope_cursor}"), root).await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = h
        .get(&format!("{subtree}&cursor={subtree_cursor}"), root)
        .await;
    assert_eq!(status, 200, "{body}");

    // The refusal a cursor of another scope's history gets today.
    let (status, foreign) = h
        .get(
            &format!(
                "{}&limit=1&cursor={scope_cursor}",
                scope_uri(&h, "proxy", b)
            ),
            root,
        )
        .await;
    assert_eq!(status, 400, "{foreign}");

    for (uri, cursor, what) in [
        (
            &subtree,
            &scope_cursor,
            "a single-scope cursor in subtree mode",
        ),
        (
            &scope,
            &subtree_cursor,
            "a subtree cursor in single-scope mode",
        ),
    ] {
        let (status, body) = h.get(&format!("{uri}&cursor={cursor}"), root).await;
        assert_eq!(status, 400, "{what}: {body}");
        assert_eq!(body["code"], foreign["code"], "{what}: {body} / {foreign}");
        assert_eq!(body["type"], foreign["type"], "{what}: {body} / {foreign}");
    }

    // And a subtree cursor of one target is refused on another target.
    let (status, body) = h
        .get(
            &format!(
                "{}&limit=1&cursor={subtree_cursor}",
                subtree_uri(&h, "proxy", b)
            ),
            root,
        )
        .await;
    assert_eq!(status, 400, "{body}");
}

// ── Actor names ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn actor_names_follow_the_mask_on_every_record_of_the_subtree_list() {
    // The name lookup is the single-scope one over the page. Over a directory
    // that would name anyone it is asked about, the shown actor is named and
    // the withheld one is not — and was never asked about, which is what
    // keeps a name the reader may not see out of the answer.
    let am = FakeAm::naming_everyone("Ada Admin");
    let h = RestHarness::with_directory(am.clone()).await;
    let (root, a, b) = (h.inner.tree.root, h.inner.tree.a, h.inner.tree.b);
    h.inner.declare("proxy", "cascading", json!(true)).await;
    put(&h, "proxy", a, a, json!(false)).await;
    put(&h, "proxy", b, root, json!(false)).await;

    let items = h.items(&subtree_uri(&h, "proxy", a), a).await;
    assert_eq!(items.len(), 2, "{items:?}");
    let (withheld, shown): (Vec<&Value>, Vec<&Value>) = items
        .iter()
        .partition(|item| item["actor_masked"] == json!(true));
    let ([withheld], [shown]) = (withheld.as_slice(), shown.as_slice()) else {
        panic!("one withheld and one shown: {items:?}");
    };

    // The reader's own write at its own scope.
    assert_eq!(shown["tenant_id"], json!(a), "{shown}");
    assert_eq!(shown["actor"], json!(admin_of(a)), "{shown}");
    assert_eq!(shown["actor_name_status"], json!("resolved"), "{shown}");
    assert_eq!(shown["actor_name"], json!("Ada Admin"), "{shown}");

    // The write made from above the reader, at the tenant below it.
    assert_eq!(withheld["tenant_id"], json!(b), "{withheld}");
    assert_eq!(withheld["actor"], json!(MASK_TOKEN), "{withheld}");
    assert_eq!(withheld["actor_name_status"], json!("masked"), "{withheld}");
    assert!(withheld.get("actor_name").is_none(), "{withheld}");

    assert_eq!(
        am.calls(),
        [(a, vec![admin_of(a)])],
        "the directory is asked for the shown actor, in its tenant, and for nobody else"
    );
}
