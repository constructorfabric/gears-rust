//! The revival report driven as a client drives it.
//!
//! Before an administrator re-declares a retired setting, the report says which
//! stored values would stop validating if it came back. It is advisory: it
//! stores nothing, asks for no step-up and leaves no audit record, and what it
//! lists is exactly what a revive would re-validate, so its answers follow the
//! create's own, in the create's order, until the point where the create would
//! ask for step-up.

use sea_orm::EntityTrait as _;
use serde_json::{Value, json};
use settings_service_sdk::SettingKey;
use toolkit_db::secure::SecureEntityExt as _;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::access::TenantAccess;
use crate::domain::declaration::{DeclarationDraft, DeclarationRepository as _};
use crate::domain::resolution::MASK_TOKEN;
use crate::domain::value::ValueRepository as _;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::test_support::{Answer, BOOL, RestHarness, SECRET, TEXT};

const REVIVAL: &str = "/settings-service/v1/declarations/revival-preview";
const DECLARATIONS: &str = "/settings-service/v1/declarations";

/// The body of a create at `acme.…network.<name>`: a boolean, cascading.
fn body(h: &RestHarness, name: &str) -> Value {
    json!({
        "value_type_id": BOOL,
        "vendor": "acme",
        "name": name,
        "category_id": h.inner.category_id(),
        "default_value": false,
        "scope_class": "cascading",
    })
}

/// The same body with some fields replaced.
fn body_with(h: &RestHarness, name: &str, changes: &Value) -> Value {
    let mut base = body(h, name);
    for (field, value) in changes.as_object().expect("an object") {
        base[field] = value.clone();
    }
    base
}

async fn report(h: &RestHarness, uri: &str, request: Value, caller: Uuid) -> Answer {
    h.send("POST", uri, Some(request), None, caller).await
}

async fn report_as_root(h: &RestHarness, request: Value) -> Answer {
    report(h, REVIVAL, request, h.inner.tree.root).await
}

/// What a declaration is stored with, apart from the identity under test.
struct Stored<'a> {
    vendor: &'a str,
    name: &'a str,
    value_type: &'a str,
    scope_class: &'a str,
    default: Value,
    classification: &'a str,
    source: &'a str,
    major: u32,
}

impl<'a> Stored<'a> {
    fn boolean(name: &'a str) -> Self {
        Self {
            vendor: "acme",
            name,
            value_type: BOOL,
            scope_class: "cascading",
            default: json!(false),
            classification: "public",
            source: "admin_authored",
            major: 1,
        }
    }
}

async fn insert(h: &RestHarness, stored: Stored<'_>) -> Uuid {
    // A module's declaration sits in the `settings` package an administrator's
    // key is composed with, so both land on the same version-stripped path.
    let key = if stored.source == "module_contributed" {
        SettingKey::contributed(
            stored.vendor,
            "settings",
            "network",
            stored.name,
            std::num::NonZeroU32::MIN,
        )
        .expect("key")
    } else {
        SettingKey::compose_at(
            stored.vendor,
            "network",
            stored.name,
            std::num::NonZeroU32::new(stored.major).expect("a major"),
        )
        .expect("key")
    };
    let conn = h.inner.db.conn().expect("connection");
    DeclarationRepo
        .insert(
            &conn,
            &AccessScope::allow_all(),
            DeclarationDraft {
                key: key.to_string(),
                leaf_slug: stored.name.to_owned(),
                value_type_id: stored.value_type.to_owned(),
                category_id: h.inner.category_id(),
                default_value: stored.default,
                scope_class: stored.scope_class.to_owned(),
                mode: "standard".to_owned(),
                requires_step_up: true,
                anonymous_exposable: false,
                domain_affinity: None,
                has_secret_trait: stored.classification == "secret",
                data_classification: stored.classification.to_owned(),
                source: stored.source.to_owned(),
                owner_module: (stored.source == "module_contributed")
                    .then(|| "module-x".to_owned()),
                licence_feature: None,
                display_name: None,
                description: None,
                created_by: "test".to_owned(),
            },
        )
        .await
        .expect("declaration")
        .id
}

/// A retired administrator-authored boolean at `acme.…network.<name>`.
async fn retired(h: &RestHarness, name: &str) -> Uuid {
    let id = insert(h, Stored::boolean(name)).await;
    h.inner.retire(id).await;
    id
}

async fn active_contributed(h: &RestHarness, name: &str) -> Uuid {
    insert(
        h,
        Stored {
            source: "module_contributed",
            ..Stored::boolean(name)
        },
    )
    .await
}

/// The rows of a declaration as stored: tenant, flag and detail.
async fn rows(h: &RestHarness, id: Uuid) -> Vec<(Uuid, Option<Value>, bool, Option<String>)> {
    let conn = h.inner.db.conn().expect("connection");
    let mut found: Vec<_> = ValueRepo
        .find_all(&conn, &AccessScope::allow_all(), id)
        .await
        .expect("rows")
        .into_iter()
        .map(|r| (r.tenant_id, r.value, r.needs_review, r.needs_review_detail))
        .collect();
    found.sort_by_key(|r| r.0);
    found
}

async fn status_of(h: &RestHarness, id: Uuid) -> String {
    let (status, body) = h
        .get(&format!("{DECLARATIONS}/{id}"), h.inner.tree.root)
        .await;
    assert_eq!(status, 200, "{body}");
    body["status"].as_str().expect("a status").to_owned()
}

async fn audit_rows(h: &RestHarness) -> usize {
    let conn = h.inner.db.conn().expect("connection");
    crate::infra::storage::entity::audit_record::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("audit rows")
        .len()
}

fn listed(answer: &Answer) -> Vec<Value> {
    answer.body["values"].as_array().expect("values").clone()
}

fn tenants(answer: &Answer) -> Vec<String> {
    listed(answer)
        .iter()
        .map(|v| v["tenant_id"].as_str().expect("a tenant").to_owned())
        .collect()
}

/// The stable problem code the not-found answer carries, wherever the problem
/// document puts it.
fn names_nothing_to_revive(answer: &Answer) -> bool {
    answer.body["context"]["resource_name"] == json!("nothing_to_revive")
}

fn detail_starts_with(answer: &Answer, code: &str) -> bool {
    answer.body["detail"]
        .as_str()
        .is_some_and(|d| d.starts_with(code))
}

// ── What comes back ──────────────────────────────────────────────────────────

#[tokio::test]
async fn the_report_lists_what_a_revive_would_flag_in_walk_order_and_changes_nothing() {
    // A refusing step-up harness: the report asks for none, so a 200 here is
    // also the proof it is never consulted.
    let h = RestHarness::stale_step_up().await;
    let id = retired(&h, "retry_policy").await;
    let t = &h.inner.tree;
    h.inner.set(id, t.root, json!(true)).await;
    // Never a boolean; stored while the setting was live and unflagged.
    h.inner.set(id, t.a, json!("aggressive")).await;
    // Flagged earlier, a boolean again.
    h.inner.set_flagged(id, t.b, json!(true)).await;
    let before = rows(&h, id).await;
    let audited = audit_rows(&h).await;

    let answer = report_as_root(&h, body(&h, "retry_policy")).await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.body["declaration_id"], json!(id.to_string()));
    assert!(answer.body["key"].is_string(), "{}", answer.body);
    assert_eq!(
        tenants(&answer),
        vec![t.root.to_string(), t.a.to_string(), t.b.to_string()],
        "the walk's order"
    );
    let values = listed(&answer);
    assert_eq!(values[0]["value"], json!(true));
    assert_eq!(values[0]["needs_review"], json!(false));
    assert_eq!(values[0]["masked"], json!(false));
    assert_eq!(values[1]["value"], json!("aggressive"));
    assert_eq!(values[1]["needs_review"], json!(true));
    assert!(
        values[1]["needs_review_detail"].is_string(),
        "the flag says why: {}",
        values[1]
    );
    assert_eq!(
        values[2]["needs_review"],
        json!(false),
        "a flagged value that validates again is reported clear"
    );
    assert!(values[2].get("needs_review_detail").is_none());
    assert_eq!(values[0]["scope"], json!("/"));
    assert_eq!(values[1]["scope"], json!(format!("/tenants/{}", t.a)));
    assert_eq!(values[2]["scope"], json!(format!("/tenants/{}", t.b)));
    assert_eq!(answer.body["total"], json!(3));
    assert_eq!(answer.body["total_needs_review"], json!(1));
    assert_eq!(answer.body["complete"], json!(true));
    // The root, `a`, `b` and the sibling `c`; the standalone `s` is left out.
    assert_eq!(answer.body["scanned"], json!(4), "{}", answer.body);

    // Advisory: nothing moved.
    assert_eq!(status_of(&h, id).await, "retired");
    assert_eq!(rows(&h, id).await, before, "rows and flags as they were");
    assert_eq!(audit_rows(&h).await, audited, "no audit record");
    assert!(h.published.events.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn a_standalone_subtree_is_neither_listed_nor_counted() {
    let h = RestHarness::new().await;
    let id = retired(&h, "retry_policy").await;
    let t = &h.inner.tree;
    let below = Uuid::new_v4();
    h.inner.hierarchy.add_tenant(below, t.s);
    h.inner.set(id, t.root, json!(true)).await;
    h.inner.set(id, t.s, json!("walled")).await;
    h.inner.set(id, below, json!("walled")).await;

    for caller in [t.root, t.a] {
        let answer = report(&h, REVIVAL, body(&h, "retry_policy"), caller).await;
        assert_eq!(answer.status, 200, "{}", answer.body);
        for tenant in [t.s, below] {
            assert!(
                !tenants(&answer).contains(&tenant.to_string()),
                "{caller}: {}",
                answer.body
            );
        }
        let expected = usize::from(caller == t.root);
        assert_eq!(answer.body["total"], json!(expected), "{}", answer.body);
        assert_eq!(answer.body["total_needs_review"], json!(0));
    }
}

#[tokio::test]
async fn a_value_is_masked_by_the_stricter_of_the_stored_and_the_derived_classification() {
    // A pii text that is no string any more: stored `pii`, body deriving public.
    for entitled in [true, false] {
        let h = if entitled {
            RestHarness::new().await
        } else {
            RestHarness::without_pii_entitlement().await
        };
        let id = insert(
            &h,
            Stored {
                value_type: TEXT,
                default: json!(""),
                classification: "pii",
                ..Stored::boolean("contact")
            },
        )
        .await;
        h.inner.retire(id).await;
        h.inner
            .set_classified(id, h.inner.tree.a, json!(42), "pii")
            .await;
        let request = body_with(
            &h,
            "contact",
            &json!({ "value_type_id": TEXT, "default_value": "" }),
        );
        let answer = report_as_root(&h, request).await;
        assert_eq!(answer.status, 200, "{}", answer.body);
        let value = &listed(&answer)[0];
        assert_eq!(value["needs_review"], json!(true), "{value}");
        if entitled {
            assert_eq!(value["value"], json!(42));
            assert_eq!(value["masked"], json!(false));
            assert!(value["needs_review_detail"].is_string(), "{value}");
        } else {
            assert_eq!(value["value"], json!(MASK_TOKEN));
            assert_eq!(value["masked"], json!(true));
            assert!(
                value.get("needs_review_detail").is_none(),
                "the detail would quote the value: {value}"
            );
        }
        assert_eq!(answer.body["total_needs_review"], json!(1));
    }
}

#[tokio::test]
async fn a_body_deriving_pii_masks_a_value_stored_as_public() {
    for entitled in [true, false] {
        let h = if entitled {
            RestHarness::new().await
        } else {
            RestHarness::without_pii_entitlement().await
        };
        let id = retired(&h, "contact").await;
        h.inner.set(id, h.inner.tree.a, json!("aggressive")).await;
        let answer = report_as_root(
            &h,
            body_with(&h, "contact", &json!({ "data_classification": "pii" })),
        )
        .await;
        assert_eq!(answer.status, 200, "{}", answer.body);
        let value = &listed(&answer)[0];
        assert_eq!(value["needs_review"], json!(true), "{value}");
        assert_eq!(value["masked"], json!(!entitled), "{value}");
        assert_eq!(
            value.get("needs_review_detail").is_some(),
            entitled,
            "{value}"
        );
        assert_eq!(
            value["value"],
            if entitled {
                json!("aggressive")
            } else {
                json!(MASK_TOKEN)
            }
        );
    }
}

#[tokio::test]
async fn a_secret_row_shows_the_mask_token_to_everyone_and_is_never_flagged() {
    for entitled in [true, false] {
        let h = if entitled {
            RestHarness::new().await
        } else {
            RestHarness::without_pii_entitlement().await
        };
        let id = insert(
            &h,
            Stored {
                value_type: SECRET,
                default: json!(""),
                classification: "secret",
                ..Stored::boolean("token")
            },
        )
        .await;
        h.inner.retire(id).await;
        h.inner.set_secret(id, h.inner.tree.a, "cred-ref-1").await;
        let request = body_with(
            &h,
            "token",
            &json!({ "value_type_id": SECRET, "default_value": "" }),
        );
        let answer = report_as_root(&h, request).await;
        assert_eq!(answer.status, 200, "{}", answer.body);
        let value = &listed(&answer)[0];
        assert_eq!(value["value"], json!(MASK_TOKEN), "{value}");
        assert_eq!(value["masked"], json!(true));
        assert_eq!(value["needs_review"], json!(false));
        assert!(value.get("needs_review_detail").is_none());
        assert_eq!(answer.body["total_needs_review"], json!(0));
    }
}

#[tokio::test]
async fn a_retired_declaration_with_no_values_in_the_callers_administration_has_an_empty_report() {
    let h = RestHarness::new().await;
    let id = retired(&h, "retry_policy").await;
    let t = &h.inner.tree;

    let none = report_as_root(&h, body(&h, "retry_policy")).await;
    assert_eq!(none.status, 200, "{}", none.body);
    assert_eq!(none.body["values"], json!([]));
    assert_eq!(none.body["total"], json!(0));
    assert_eq!(none.body["total_needs_review"], json!(0));
    assert_eq!(none.body["complete"], json!(true));

    // Rows exist, but at tenants `a` does not administer.
    h.inner.set(id, t.root, json!("x")).await;
    h.inner.set(id, t.c, json!("x")).await;
    let outside = report(&h, REVIVAL, body(&h, "retry_policy"), t.a).await;
    assert_eq!(outside.status, 200, "{}", outside.body);
    assert_eq!(outside.body["values"], json!([]));
    assert_eq!(outside.body["total"], json!(0));
    assert_eq!(outside.body["complete"], json!(true));
}

// ── Nothing to revive ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_key_with_nothing_to_revive_is_a_404_carrying_its_own_code() {
    let h = RestHarness::new().await;

    let fresh = report_as_root(&h, body(&h, "never_declared")).await;
    assert_eq!(fresh.status, 404, "{}", fresh.body);
    assert!(names_nothing_to_revive(&fresh), "{}", fresh.body);

    // An active declaration on the path: the create would evolve or duplicate.
    insert(&h, Stored::boolean("live_one")).await;
    let active = report_as_root(&h, body(&h, "live_one")).await;
    assert_eq!(active.status, 404, "{}", active.body);
    assert!(names_nothing_to_revive(&active), "{}", active.body);

    // A missing category is another 404, and does not say so.
    let mut request = body(&h, "never_declared");
    request["category_id"] = json!(Uuid::new_v4());
    let category = report_as_root(&h, request).await;
    assert_eq!(category.status, 404, "{}", category.body);
    assert!(!names_nothing_to_revive(&category), "{}", category.body);
}

// ── The create's answers, in the create's order ──────────────────────────────

#[tokio::test]
async fn the_report_answers_what_the_create_answers_before_it_looks_for_a_retired_declaration() {
    let h = RestHarness::new().await;
    let id = retired(&h, "retry_policy").await;

    let mut request = body(&h, "retry_policy");
    request["category_id"] = json!(Uuid::new_v4());
    let missing = report_as_root(&h, request).await;
    assert_eq!(missing.status, 404, "{}", missing.body);

    let segment = report_as_root(&h, body(&h, "Not A Token!")).await;
    assert_eq!(segment.status, 400, "{}", segment.body);

    let scope = report_as_root(
        &h,
        body_with(&h, "retry_policy", &json!({ "scope_class": "sideways" })),
    )
    .await;
    assert_eq!(scope.status, 400, "{}", scope.body);

    let name = report_as_root(
        &h,
        body_with(&h, "retry_policy", &json!({ "display_name": "   " })),
    )
    .await;
    assert_eq!(name.status, 400, "{}", name.body);

    // A module's active declaration on the path: the owner's refusal.
    active_contributed(&h, "owned").await;
    let owned = report_as_root(&h, body(&h, "owned")).await;
    assert_eq!(owned.status, 409, "{}", owned.body);
    assert!(
        detail_starts_with(&owned, "contributed_declaration_immutable"),
        "{}",
        owned.body
    );

    let default = report_as_root(
        &h,
        body_with(&h, "retry_policy", &json!({ "default_value": "nope" })),
    )
    .await;
    assert_eq!(default.status, 400, "{}", default.body);
    assert_eq!(status_of(&h, id).await, "retired");
}

#[tokio::test]
async fn a_revive_the_create_would_refuse_is_refused_with_the_creates_code() {
    let h = RestHarness::new().await;
    retired(&h, "retry_policy").await;

    let cases = [
        (
            "the secret boundary",
            json!({ "value_type_id": SECRET, "default_value": "" }),
            "secretness_changed",
        ),
        (
            "the scope class",
            json!({ "scope_class": "global" }),
            "scope_class_changed",
        ),
        (
            "the value type",
            json!({ "value_type_id": TEXT, "default_value": "" }),
            "value_type_changed",
        ),
    ];
    for (what, changes, code) in cases {
        let answer = report_as_root(&h, body_with(&h, "retry_policy", &changes)).await;
        assert_eq!(answer.status, 409, "{what}: {}", answer.body);
        assert!(detail_starts_with(&answer, code), "{what}: {}", answer.body);
    }
}

#[tokio::test]
async fn a_leaf_name_an_active_declaration_holds_in_the_category_is_a_409_and_the_default_is_judged_first()
 {
    let h = RestHarness::new().await;
    retired(&h, "retry_policy").await;
    // Another vendor's active declaration of the same leaf, off the path.
    insert(
        &h,
        Stored {
            vendor: "globex",
            ..Stored::boolean("retry_policy")
        },
    )
    .await;

    let taken = report_as_root(&h, body(&h, "retry_policy")).await;
    assert_eq!(taken.status, 409, "{}", taken.body);
    assert!(
        detail_starts_with(&taken, "leaf_name_taken"),
        "{}",
        taken.body
    );

    let default = report_as_root(
        &h,
        body_with(&h, "retry_policy", &json!({ "default_value": "nope" })),
    )
    .await;
    assert_eq!(default.status, 400, "{}", default.body);
}

#[tokio::test]
async fn when_the_scope_class_and_the_value_type_both_change_the_scope_class_is_named() {
    let h = RestHarness::new().await;
    retired(&h, "retry_policy").await;
    let answer = report_as_root(
        &h,
        body_with(
            &h,
            "retry_policy",
            &json!({ "scope_class": "global", "value_type_id": TEXT, "default_value": "" }),
        ),
    )
    .await;
    assert_eq!(answer.status, 409, "{}", answer.body);
    assert!(
        detail_starts_with(&answer, "scope_class_changed"),
        "{}",
        answer.body
    );
}

// ── Authorization and visibility ─────────────────────────────────────────────

#[tokio::test]
async fn a_denied_caller_is_refused_403_before_the_body_is_looked_at() {
    let h = RestHarness::denying().await;
    let with_nothing = report_as_root(&h, body(&h, "retry_policy")).await;
    assert_eq!(with_nothing.status, 403, "{}", with_nothing.body);

    retired(&h, "retry_policy").await;
    let with_retired = report_as_root(&h, body(&h, "retry_policy")).await;
    assert_eq!(with_retired.status, 403, "{}", with_retired.body);

    let malformed = report_as_root(&h, json!({ "nonsense": true })).await;
    assert_eq!(malformed.status, 403, "{}", malformed.body);
}

#[tokio::test]
async fn a_retired_declaration_hidden_from_the_caller_is_nothing_to_revive() {
    let h = RestHarness::new().await;
    let id = retired(&h, "retry_policy").await;
    let t = &h.inner.tree;
    h.inner.set(id, t.a, json!("aggressive")).await;
    h.restrict(id, t.a, TenantAccess::Hidden).await;

    let hidden = report(&h, REVIVAL, body(&h, "retry_policy"), t.a).await;
    assert_eq!(hidden.status, 404, "{}", hidden.body);
    assert!(names_nothing_to_revive(&hidden), "{}", hidden.body);

    // The administrator above it, to whom it is not hidden, gets the report.
    let above = report_as_root(&h, body(&h, "retry_policy")).await;
    assert_eq!(above.status, 200, "{}", above.body);
}

#[tokio::test]
async fn an_active_contributed_declaration_the_caller_cannot_read_is_the_same_409_as_the_create() {
    let h = RestHarness::new().await;
    let id = active_contributed(&h, "owned").await;
    h.restrict(id, h.inner.tree.a, TenantAccess::Hidden).await;
    let caller = h.inner.tree.a;

    let create = h
        .send("POST", DECLARATIONS, Some(body(&h, "owned")), None, caller)
        .await;
    let answer = report(&h, REVIVAL, body(&h, "owned"), caller).await;
    assert_eq!(create.status, 409, "{}", create.body);
    assert_eq!(answer.status, 409, "{}", answer.body);
    assert_eq!(answer.body["detail"], create.body["detail"]);
}

// ── Bounded ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_listing_is_cut_at_the_limit_and_the_total_stays_exact() {
    let h = RestHarness::new().await;
    let id = retired(&h, "retry_policy").await;
    let t = &h.inner.tree;
    for tenant in [t.root, t.a, t.b] {
        h.inner.set(id, tenant, json!(true)).await;
    }

    let two = report(
        &h,
        &format!("{REVIVAL}?limit=2"),
        body(&h, "retry_policy"),
        t.root,
    )
    .await;
    assert_eq!(two.status, 200, "{}", two.body);
    assert_eq!(tenants(&two), vec![t.root.to_string(), t.a.to_string()]);
    assert_eq!(two.body["total"], json!(3));

    let over = report(
        &h,
        &format!("{REVIVAL}?limit=1000"),
        body(&h, "retry_policy"),
        t.root,
    )
    .await;
    assert_eq!(over.status, 200, "{}", over.body);
    assert!(listed(&over).len() <= 500);

    let zero = report(
        &h,
        &format!("{REVIVAL}?limit=0"),
        body(&h, "retry_policy"),
        t.root,
    )
    .await;
    assert_eq!(zero.status, 400, "{}", zero.body);
    assert!(zero.body.to_string().contains("limit"), "{}", zero.body);
}

#[tokio::test]
async fn a_walk_the_budget_cut_is_reported_incomplete() {
    let h = RestHarness::new().await;
    let id = retired(&h, "retry_policy").await;
    h.inner.set(id, h.inner.tree.root, json!(true)).await;
    h.inner
        .hierarchy
        .truncate_subtrees
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let answer = report_as_root(&h, body(&h, "retry_policy")).await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.body["complete"], json!(false));
    assert!(
        answer.body["total"].as_u64().is_some_and(|n| n >= 1),
        "what was found is a lower bound: {}",
        answer.body
    );
}

// ── The validator ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_validator_that_is_unavailable_fails_the_whole_request_503() {
    let h = RestHarness::with_unavailable_registry().await;
    let id = retired(&h, "retry_policy").await;
    h.inner.set(id, h.inner.tree.root, json!(true)).await;

    let answer = report_as_root(&h, body(&h, "retry_policy")).await;
    assert_eq!(answer.status, 503, "{}", answer.body);
}

// ── Advisory ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_revive_needs_no_report_and_a_report_changes_nothing_about_it() {
    let mut outcomes = Vec::new();
    for with_report in [false, true] {
        let h = RestHarness::new().await;
        let id = retired(&h, "retry_policy").await;
        let t = &h.inner.tree;
        h.inner.set(id, t.a, json!("aggressive")).await;
        h.inner.set_flagged(id, t.b, json!(true)).await;
        if with_report {
            let answer = report_as_root(&h, body(&h, "retry_policy")).await;
            assert_eq!(answer.status, 200, "{}", answer.body);
        }
        let revived = h
            .send(
                "POST",
                DECLARATIONS,
                Some(body(&h, "retry_policy")),
                None,
                t.root,
            )
            .await;
        assert!(revived.status.is_success(), "{}", revived.body);
        let mut stored: Vec<_> = rows(&h, id)
            .await
            .into_iter()
            .map(|(tenant, value, flagged, _)| (tenant == t.a, value, flagged))
            .collect();
        // The tenants' ids differ between the two harnesses: order by the
        // role they play instead.
        stored.sort_by_key(|row| row.0);
        outcomes.push((
            revived.status,
            revived.body["status"].clone(),
            status_of(&h, id).await,
            stored,
        ));
    }
    assert_eq!(outcomes[0], outcomes[1], "the revive's outcome is the same");
}

#[tokio::test]
async fn a_retired_declaration_outside_the_callers_domain_fails_the_read_gate() {
    // The REST harness's policy never constrains a domain (no resource here
    // declares a PEP property), so the domain arm of the gate is driven at
    // the repository read the handler's gate ends in, with the visibility a
    // domain-constrained scope derives: a key it does not return is answered
    // `nothing_to_revive`.
    use crate::domain::category::visibility::DomainVisibility;

    let h = RestHarness::new().await;
    let id = retired(&h, "retry_policy").await;
    h.inner.bind_domain(id, "commercial").await;
    let key = SettingKey::compose("acme", "network", "retry_policy")
        .expect("key")
        .to_string();
    let chain = vec![h.inner.tree.root];
    let conn = h.inner.db.conn().expect("connection");
    let visible = |visibility: DomainVisibility| {
        let (conn, chain, key) = (&conn, &chain, &key);
        async move {
            DeclarationRepo
                .visible_keys(
                    conn,
                    &AccessScope::allow_all(),
                    &visibility,
                    chain,
                    std::slice::from_ref(key),
                )
                .await
                .expect("visible keys")
        }
    };

    assert!(
        visible(DomainVisibility::Restricted(vec![
            "infrastructure".to_owned()
        ]))
        .await
        .is_empty(),
        "another domain's administrator cannot read it"
    );
    assert_eq!(
        visible(DomainVisibility::Restricted(vec!["commercial".to_owned()])).await,
        vec![key.clone()],
        "its own domain's can"
    );
    assert_eq!(visible(DomainVisibility::Unrestricted).await, vec![key]);
}

// ── The create's own answers for the body ────────────────────────────────────

#[tokio::test]
async fn a_body_the_extractor_refuses_is_answered_exactly_as_the_create_answers_it() {
    let h = RestHarness::new().await;
    let t = h.inner.tree.root;
    let valid = body(&h, "retry_policy").to_string();
    let mut unknown = body(&h, "retry_policy");
    unknown["surprise"] = json!(1);
    let mut missing = body(&h, "retry_policy");
    missing
        .as_object_mut()
        .expect("an object")
        .remove("default_value");
    let cases: Vec<(&str, String, Option<&str>)> = vec![
        (
            "a syntax error",
            "{ not json".to_owned(),
            Some("application/json"),
        ),
        (
            "an unknown field",
            unknown.to_string(),
            Some("application/json"),
        ),
        (
            "a missing field",
            missing.to_string(),
            Some("application/json"),
        ),
        (
            "a wrong type",
            r#"{"value_type_id": 7}"#.to_owned(),
            Some("application/json"),
        ),
        ("another content type", valid.clone(), Some("text/plain")),
        ("no content type", valid, None),
    ];
    for (what, text, content_type) in cases {
        let create = h
            .send_with_content_type("POST", DECLARATIONS, &text, content_type, t)
            .await;
        let report = h
            .send_with_content_type("POST", REVIVAL, &text, content_type, t)
            .await;
        assert!(
            create.status.is_client_error(),
            "{what}: the create refuses it: {} {}",
            create.status,
            create.body
        );
        assert_eq!(report.status, create.status, "{what}: {}", report.body);
        assert!(
            !report.raw.is_empty(),
            "{what}: there is an answer to compare"
        );
        assert_eq!(report.raw, create.raw, "{what}: the response text");
        assert_eq!(
            report.headers.get("content-type"),
            create.headers.get("content-type"),
            "{what}: the content type"
        );
    }
}

#[tokio::test]
async fn each_of_the_two_decisions_is_asked_before_anything_else() {
    for action in ["create", "read"] {
        let h = RestHarness::denying_action(action).await;
        let id = retired(&h, "retry_policy").await;
        h.inner.set(id, h.inner.tree.root, json!("x")).await;
        let t = h.inner.tree.root;
        let malformed = h
            .send_with_content_type("POST", REVIVAL, "{ not json", Some("application/json"), t)
            .await;
        assert_eq!(malformed.status, 403, "{action}: {}", malformed.body);
        let at_retired = report_as_root(&h, body(&h, "retry_policy")).await;
        assert_eq!(at_retired.status, 403, "{action}: {}", at_retired.body);
        let limit_one = report(&h, &format!("{REVIVAL}?limit=1"), body(&h, "x"), t).await;
        assert_eq!(limit_one.status, 403, "{action}: {}", limit_one.body);
        // The `limit` is judged before the decisions, which do not depend on
        // what exists: a bad one is a 400 naming it whatever is denied.
        let bad_limit = report(&h, &format!("{REVIVAL}?limit=0"), body(&h, "x"), t).await;
        assert_eq!(bad_limit.status, 400, "{action}: {}", bad_limit.body);
        assert!(
            bad_limit.body.to_string().contains("limit"),
            "{}",
            bad_limit.body
        );
    }
}

#[tokio::test]
async fn a_limit_too_large_for_any_integer_is_clamped_and_only_nonsense_is_refused() {
    let h = RestHarness::new().await;
    let id = retired(&h, "retry_policy").await;
    h.inner.set(id, h.inner.tree.root, json!(true)).await;
    let huge = "9".repeat(40);
    for (limit, status) in [
        (huge.as_str(), 200),
        ("9223372036854775808", 200),
        ("500", 200),
        ("0", 400),
        ("-1", 400),
        ("abc", 400),
        ("1.5", 400),
    ] {
        let answer = report(
            &h,
            &format!("{REVIVAL}?limit={limit}"),
            body(&h, "retry_policy"),
            h.inner.tree.root,
        )
        .await;
        assert_eq!(answer.status, status, "limit={limit}: {}", answer.body);
    }
}

#[tokio::test]
async fn a_limit_above_the_bound_lists_exactly_the_bound_and_counts_every_row() {
    let h = RestHarness::new().await;
    let id = retired(&h, "retry_policy").await;
    let root = h.inner.tree.root;
    for _ in 0..501 {
        let tenant = Uuid::new_v4();
        h.inner.hierarchy.add_tenant(tenant, root);
        h.inner.set(id, tenant, json!("aggressive")).await;
    }
    let answer = report(
        &h,
        &format!("{REVIVAL}?limit=1000"),
        body(&h, "retry_policy"),
        root,
    )
    .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(listed(&answer).len(), 500);
    assert_eq!(answer.body["total"], json!(501));
    assert_eq!(answer.body["total_needs_review"], json!(501));
    assert_eq!(answer.body["complete"], json!(true));
}

// ── Read-only, and the order of refusals ─────────────────────────────────────

#[tokio::test]
async fn a_report_registers_no_type_and_touches_no_cache() {
    let h = RestHarness::new().await;
    let id = retired(&h, "retry_policy").await;
    let t = &h.inner.tree;
    h.inner.set(id, t.a, json!("aggressive")).await;
    let key = SettingKey::compose("acme", "network", "retry_policy")
        .expect("key")
        .to_string();
    let registered = h.registrar.registered.lock().expect("lock").len();
    let before = (
        h.inner.cache.generation(&key, t.root),
        h.inner.cache.generation(&key, t.a),
    );

    let answer = report_as_root(&h, body(&h, "retry_policy")).await;
    assert_eq!(answer.status, 200, "{}", answer.body);

    assert_eq!(
        h.registrar.registered.lock().expect("lock").len(),
        registered
    );
    assert_eq!(
        (
            h.inner.cache.generation(&key, t.root),
            h.inner.cache.generation(&key, t.a)
        ),
        before,
        "no eviction of the key"
    );
}

#[tokio::test]
async fn a_refused_body_is_answered_before_the_walk_is_reached() {
    let h = RestHarness::new().await;
    let id = retired(&h, "retry_policy").await;
    h.inner.set(id, h.inner.tree.root, json!(true)).await;

    let refused = report_as_root(
        &h,
        body_with(&h, "retry_policy", &json!({ "scope_class": "global" })),
    )
    .await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert_eq!(
        h.inner.hierarchy.subtree_calls(),
        0,
        "no walk was asked for"
    );

    let ok = report_as_root(&h, body(&h, "retry_policy")).await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert_eq!(h.inner.hierarchy.subtree_calls(), 1);
}

#[tokio::test]
async fn an_earlier_major_retired_beside_an_active_later_one_is_nothing_to_revive() {
    // The create would evolve the active `v2~`, not revive the retired `v1~`.
    let h = RestHarness::new().await;
    let v1 = insert(&h, Stored::boolean("retry_policy")).await;
    h.inner.retire(v1).await;
    insert(
        &h,
        Stored {
            major: 2,
            ..Stored::boolean("retry_policy")
        },
    )
    .await;
    let answer = report_as_root(&h, body(&h, "retry_policy")).await;
    assert_eq!(answer.status, 404, "{}", answer.body);
    assert!(names_nothing_to_revive(&answer), "{}", answer.body);
}

#[tokio::test]
async fn a_body_with_two_defects_is_answered_for_the_first_in_the_creates_order() {
    let h = RestHarness::new().await;
    retired(&h, "retry_policy").await;
    active_contributed(&h, "owned").await;

    // Missing category before a bad segment.
    let mut both = body(&h, "Not A Token!");
    both["category_id"] = json!(Uuid::new_v4());
    let answer = report_as_root(&h, both).await;
    assert_eq!(answer.status, 404, "{}", answer.body);
    assert!(!names_nothing_to_revive(&answer));

    // A bad segment before an invalid Schema Default.
    let answer = report_as_root(
        &h,
        body_with(&h, "Not A Token!", &json!({ "default_value": "nope" })),
    )
    .await;
    assert_eq!(answer.status, 400, "{}", answer.body);
    assert_eq!(
        violated_field(&answer.body),
        &json!("key"),
        "{}",
        answer.body
    );

    // An active contributed declaration on the path before an invalid default.
    let answer = report_as_root(
        &h,
        body_with(&h, "owned", &json!({ "default_value": "nope" })),
    )
    .await;
    assert_eq!(answer.status, 409, "{}", answer.body);
    assert!(
        detail_starts_with(&answer, "contributed_declaration_immutable"),
        "{}",
        answer.body
    );
}

/// The field a 400 problem document names, from its first violation.
fn violated_field(body: &Value) -> &Value {
    &body["context"]["field_violations"][0]["field"]
}
