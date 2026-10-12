//! Price book entry authoring protocol branches through the production router.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod entry_support;
use entry_support::policy_support;
use entry_support::{Fixture, KINDS, Kind, Script, Target};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;
async fn setup(mode: usize) -> (Fixture, Arc<Script>, String, Value) {
    let script = Arc::new(Script::default());
    script.set(mode);
    let f = Fixture::new(script.clone()).await;
    let (book, _) = f.book().await;
    let path = format!("/price-books/{}/entries", book["id"].as_str().unwrap());
    (
        f,
        script,
        path,
        // D-427: an entry is created in a model; `per_unit` is one every charge kind allows.
        if mode == 11 {
            json!({"sku_id":Uuid::new_v4(),"period":"month","model":"per_unit"})
        } else if mode == 21 {
            json!({"sku_id":Uuid::new_v4(),"model":"per_unit"})
        } else {
            json!({"usage_rating_policy":policy_support::input(),"sku_id":Uuid::new_v4(),"model":"per_unit"})
        },
    )
}
#[tokio::test]
async fn create_replay_current_kind_unique_key_and_delete() {
    let (f, script, path, input) = setup(11).await;
    assert_eq!(
        f.call("POST", &path, input.clone(), None, None).await.0,
        400
    );
    let first = f
        .call("POST", &path, input.clone(), None, Some("one"))
        .await;
    assert_eq!(first.0, 201, "{first:?}");
    assert_eq!(first.1["reference_state"], "confirmed");
    assert_eq!(first.1["charge_kind"], "recurring");
    assert_eq!(
        f.call("POST", &path, input.clone(), None, Some("one"))
            .await,
        first
    );
    assert_eq!(Script::count(&script.reserve_calls), 1);
    let dup = f.call("POST", &path, input, None, Some("two")).await;
    assert_eq!(dup.0, 409, "{dup:?}");
    assert!(dup.1.to_string().contains("ENTRY_KEY_TAKEN"));
    let id = first.1["id"].as_str().unwrap();
    // The read answers the entry the create answered, and its usage (D-428): nothing uses it yet.
    let mut read = f
        .call(
            "GET",
            &format!("/price-book-entries/{id}"),
            json!({}),
            None,
            None,
        )
        .await
        .1;
    assert_eq!(
        read.as_object_mut().unwrap().remove("usage"),
        Some(json!({
            "prices": {
                "approved": 0,
                "pending": 0,
                "draft": 0,
                "scheduled": 0,
                "active": 0,
                "superseded": 0,
            },
            "plans": 0,
            "plans_superseded_only": 0,
        }))
    );
    // No price is in force (D-440), and none is next (D-472).
    for headline in ["current_price", "next_price"] {
        assert_eq!(
            read.as_object_mut().unwrap().remove(headline),
            Some(Value::Null),
            "{headline}"
        );
    }
    assert_eq!(read, first.1);
    assert_eq!(
        f.call(
            "DELETE",
            &format!("/price-book-entries/{id}"),
            json!({}),
            None,
            None
        )
        .await
        .0,
        204
    );
    assert_eq!(Script::count(&script.releases), 2);
}
#[tokio::test]
async fn refusal_branches_answer_key_and_release_only_after_reservation() {
    for (mode, code, releases) in [
        (4, "SKU_FENCED", 0),
        (8, "BUNDLE_SKU_NOT_PRICEABLE", 1),
        (9, "SKU_DEPRECATED", 1),
        (10, "SKU_DRAFT", 1),
    ] {
        let (f, script, path, input) = setup(mode).await;
        let refused = f
            .call("POST", &path, input.clone(), None, Some("one"))
            .await;
        assert_eq!(refused.0, 409, "{refused:?}");
        assert!(refused.1.to_string().contains(code), "{refused:?}");
        script.set(0);
        assert_eq!(
            f.call("POST", &path, input, None, Some("one")).await,
            refused
        );
        assert_eq!(Script::count(&script.releases), releases);
    }
}
/// A fresh fixture for one reference kind, its target and a create body.
async fn setup_kind(mode: usize, kind: Kind) -> (Fixture, Arc<Script>, Target, Value) {
    let script = Arc::new(Script::default());
    script.set(mode);
    let f = Fixture::new(script.clone()).await;
    let t = f.target(kind).await;
    let input = t.input(&f.caller()).await;
    (f, script, t, input)
}
#[tokio::test]
async fn a_503_before_the_write_frees_the_key_and_a_confirm_timeout_keeps_it() {
    for kind in KINDS {
        // Mode 5: the reserve answer is lost. Nothing was written, so the same key runs afresh.
        let (f, script, t, input) = setup_kind(5, kind).await;
        let c = f.caller();
        let result = t.create(&c, input.clone(), "one").await;
        assert_eq!(result.0, 503, "{kind:?}: {result:?}");
        let retry = t.create(&c, input, "one").await;
        assert_eq!(retry.0, 201, "{kind:?}: {retry:?}");
        assert_eq!(Script::count(&script.reserve_calls), 2);
        // Mode 6: the reference is written and its confirm timed out; the key stays in flight.
        let (f, script, t, input) = setup_kind(6, kind).await;
        let c = f.caller();
        let result = t.create(&c, input.clone(), "one").await;
        assert_eq!(result.0, 503, "{kind:?}: {result:?}");
        let retry = t.create(&c, input, "one").await;
        assert_eq!(retry.0, 409);
        assert!(
            retry.1.to_string().contains("IDEMPOTENCY_KEY_IN_FLIGHT"),
            "{kind:?}: {retry:?}"
        );
        assert_eq!(Script::count(&script.releases), 0);
    }
}
#[tokio::test]
async fn released_on_confirm_is_answered_pending_never_lost() {
    for kind in KINDS {
        let (f, script, t, input) = setup_kind(7, kind).await;
        let c = f.caller();
        let first = t.create(&c, input.clone(), "one").await;
        assert_eq!(first.0, 201, "{kind:?}: {first:?}");
        assert_eq!(first.1["reference_state"], "confirmation_pending");
        assert_eq!(t.create(&c, input, "one").await, first);
        assert_eq!(Script::count(&script.releases), 0);
    }
}
#[tokio::test]
async fn patch_template_and_dimension_preconditions() {
    let (f, _, path, input) = setup(0).await;
    let first = f.call("POST", &path, input, None, Some("one")).await;
    assert_eq!(first.0, 201, "{first:?}");
    let path = format!("/price-book-entries/{}", first.1["id"].as_str().unwrap());
    assert_eq!(f.call("PATCH", &path, json!({}), None, None).await.0, 400);
    assert_eq!(
        f.call(
            "PATCH",
            &path,
            json!({"invoice_line_override":"{phase}"}),
            Some(&first.2),
            None
        )
        .await
        .0,
        400
    );
    let changed = f
        .call(
            "PATCH",
            &path,
            json!({"invoice_line_override":"{sku} {unit}"}),
            Some(&first.2),
            None,
        )
        .await;
    assert_eq!(changed.0, 200, "{changed:?}");
    assert_eq!(
        f.call("PATCH", &path, json!({}), Some(&first.2), None)
            .await
            .0,
        409
    );
    assert_eq!(
        f.call(
            "PATCH",
            &path,
            json!({"invoice_line_override":null}),
            Some(&changed.2),
            None
        )
        .await
        .1["invoice_line_override"],
        Value::Null
    );
}

#[tokio::test]
async fn translated_dimension_key_change_rejected_and_approved_delete_refused() {
    use bss_pricing::infra::storage::repo::{price_book_entry_repo, price_repo};
    let (f, _, path, input) = setup(0).await;
    let (_, _, tag) = f
        .call("GET", "/dimension-keys", json!({}), None, None)
        .await;
    assert_eq!(
        f.call(
            "PUT",
            "/dimension-keys",
            json!({"items":[{"key":"region","values":["eu","us"]}]}),
            Some(&tag),
            None
        )
        .await
        .0,
        200
    );
    let first = f.call("POST", &path, input, None, Some("one")).await;
    let id = first.1["id"].as_str().unwrap().parse().unwrap();
    let path = format!("/price-book-entries/{id}");
    let changed = f
        .call(
            "PATCH",
            &path,
            json!({"dimension_key":"region"}),
            Some(&first.2),
            None,
        )
        .await;
    assert_eq!(changed.0, 200, "{changed:?}");
    let scope = toolkit_db::secure::AccessScope::for_tenant(f.ctx.subject_tenant_id());
    let conn = f.db.conn().unwrap();
    let p = price_book_entry_repo::find(&conn, &scope, f.ctx.subject_tenant_id(), id)
        .await
        .unwrap()
        .unwrap();
    let mut price = entry_support::price(&p);
    price.dim_value = Some("eu".into());
    price.state = "approved".into();
    price_repo::insert(&conn, &scope, price).await.unwrap();
    let refusal = f
        .call(
            "PATCH",
            &path,
            json!({"dimension_key":null}),
            Some(&changed.2),
            None,
        )
        .await;
    assert_eq!(refusal.0, 409);
    assert!(refusal.1.to_string().contains("DIMENSION_KEY_IN_USE"));
    // D-426: the entry now carries approved money, so its invoice line is locked.
    let locked = f
        .call(
            "PATCH",
            &path,
            json!({"invoice_line_override":"{sku}"}),
            Some(&changed.2),
            None,
        )
        .await;
    assert_eq!(locked.0, 409, "{locked:?}");
    assert!(
        locked.1.to_string().contains("INVOICE_LINE_LOCKED"),
        "{locked:?}"
    );
    assert_eq!(f.call("DELETE", &path, json!({}), None, None).await.0, 409);
}

// D-426 (owner 2026-09-26): an entry's `invoice_line_override` reaches consumers through resolve
// (D-421: the entry wins over the SKU's template), so once the entry carries money — an approved or
// a pending price — a change of it is refused 409 `INVOICE_LINE_LOCKED`; a draft price does not lock
// it, and sending the same value is no change.
#[tokio::test]
async fn an_entry_invoice_line_locks_once_the_entry_carries_money() {
    use bss_pricing::infra::storage::repo::{price_book_entry_repo, price_repo};
    for state in ["draft", "pending", "approved"] {
        let (f, _, path, input) = setup(0).await;
        let first = f.call("POST", &path, input, None, Some("one")).await;
        assert_eq!(first.0, 201, "{first:?}");
        let id: Uuid = first.1["id"].as_str().unwrap().parse().unwrap();
        let path = format!("/price-book-entries/{id}");
        let set = f
            .call(
                "PATCH",
                &path,
                json!({"invoice_line_override":"{sku} {unit}"}),
                Some(&first.2),
                None,
            )
            .await;
        assert_eq!(set.0, 200, "{state}: no price yet: {set:?}");
        let scope = toolkit_db::secure::AccessScope::for_tenant(f.ctx.subject_tenant_id());
        let conn = f.db.conn().unwrap();
        let p = price_book_entry_repo::find(&conn, &scope, f.ctx.subject_tenant_id(), id)
            .await
            .unwrap()
            .unwrap();
        let mut price = entry_support::price(&p);
        price.state = state.into();
        price_repo::insert(&conn, &scope, price).await.unwrap();
        let same = f
            .call(
                "PATCH",
                &path,
                json!({"invoice_line_override":"{sku} {unit}"}),
                Some(&set.2),
                None,
            )
            .await;
        assert_eq!(
            same.0, 200,
            "{state}: the same value is no change: {same:?}"
        );
        let other = f
            .call(
                "PATCH",
                &path,
                json!({"invoice_line_override":"{sku}"}),
                Some(&same.2),
                None,
            )
            .await;
        if state == "draft" {
            assert_eq!(
                other.0, 200,
                "a draft price does not lock the line: {other:?}"
            );
        } else {
            assert_eq!(other.0, 409, "{state}: {other:?}");
            assert!(
                other.1.to_string().contains("INVOICE_LINE_LOCKED"),
                "{state}: {other:?}"
            );
            let cleared = f
                .call(
                    "PATCH",
                    &path,
                    json!({"invoice_line_override":null}),
                    Some(&same.2),
                    None,
                )
                .await;
            assert_eq!(
                cleared.0, 409,
                "{state}: clearing is a change too: {cleared:?}"
            );
        }
    }
}

#[tokio::test]
async fn products_contention_and_rate_limits_are_unavailability_not_refusals() {
    for kind in KINDS {
        // A reserve answered 409 UNIT_CONTENDED or 429: nothing was written, the key is free.
        for mode in [17, 19] {
            let (f, script, t, input) = setup_kind(mode, kind).await;
            let c = f.caller();
            let first = t.create(&c, input.clone(), "one").await;
            assert_eq!(first.0, 503, "{kind:?} {mode}: {first:?}");
            script.set(0);
            let retry = t.create(&c, input, "one").await;
            assert_eq!(retry.0, 201, "{kind:?} {mode}: {retry:?}");
        }
        // The SKU re-read after a successful reserve answered 409 CONTENDED. The door answers
        // 503, and a 503 writes nothing (spec section 13) even with a receipt in hand: the create is
        // cancelled, its key is free again, and the cancellation releases the receipt it holds.
        let (f, script, t, input) = setup_kind(18, kind).await;
        let c = f.caller();
        let first = t.create(&c, input.clone(), "one").await;
        assert_eq!(first.0, 503, "{kind:?}: {first:?}");
        script.set(0);
        let retry = t.create(&c, input, "one").await;
        assert_eq!(
            retry.0, 201,
            "{kind:?}: a same-key retry runs afresh: {retry:?}"
        );
        bss_pricing::infra::reference_ticker::Ticker::new(
            f.state.clone(),
            Arc::new(LaterClock),
            10,
            100,
        )
        .tick()
        .await
        .unwrap();
        assert_eq!(
            Script::count(&script.releases),
            1,
            "{kind:?}: the abandoned create's reservation is released"
        );
    }
}
/// A clock past the in-flight grace, so the ticker may take over abandoned work.
struct LaterClock;
impl bss_pricing::infra::reference_work::Clock for LaterClock {
    fn now(&self) -> time::OffsetDateTime {
        time::OffsetDateTime::now_utc() + time::Duration::days(2)
    }
}

#[tokio::test]
async fn period_and_dimension_refusals_are_400_before_any_reservation() {
    // D-403: pure input refusals are 400, and they cost no Products reservation. Each body has a
    // model every charge kind allows (D-427), so the refusal is the one named.
    for (mode, body, field, code) in [
        (
            11,
            json!({"model":"per_unit"}),
            "period",
            "ENTRY_PERIOD_INVALID",
        ),
        (
            0,
            json!({"period":"month","model":"per_unit"}),
            "period",
            "ENTRY_PERIOD_INVALID",
        ),
        (
            0,
            json!({"period":"week","model":"per_unit"}),
            "period",
            "ENTRY_PERIOD_INVALID",
        ),
        (
            0,
            json!({"dimension_key":"zone","model":"per_unit","usage_rating_policy":policy_support::input()}),
            "dimension_key",
            "DIM_NOT_DECLARED",
        ),
    ] {
        let (f, script, path, _) = setup(mode).await;
        let mut input = body.clone();
        input["sku_id"] = json!(Uuid::new_v4());
        let refused = f
            .call("POST", &path, input.clone(), None, Some("one"))
            .await;
        assert_eq!(refused.0, 400, "{body}: {refused:?}");
        assert!(refused.1.to_string().contains(code), "{refused:?}");
        assert!(refused.1.to_string().contains(field), "{refused:?}");
        assert_eq!(Script::count(&script.reserve_calls), 0, "{body}");
        // Nothing was claimed: the corrected request runs under the same key.
        let fixed = if mode == 11 {
            json!({"sku_id":input["sku_id"],"period":"month","model":"per_unit"})
        } else {
            json!({"usage_rating_policy":policy_support::input(),"sku_id":input["sku_id"],"model":"per_unit"})
        };
        let created = f.call("POST", &path, fixed, None, Some("one")).await;
        assert_eq!(created.0, 201, "{created:?}");
        let patched = f
            .call(
                "PATCH",
                &format!("/price-book-entries/{}", created.1["id"].as_str().unwrap()),
                json!({"dimension_key":"zone"}),
                Some(&created.2),
                None,
            )
            .await;
        assert_eq!(patched.0, 400, "{patched:?}");
        assert!(
            patched.1.to_string().contains("DIM_NOT_DECLARED"),
            "{patched:?}"
        );
    }
}

#[tokio::test]
async fn removing_a_dimension_key_an_entry_names_is_refused_409() {
    let (f, _, path, input) = setup(0).await;
    let (_, _, tag) = f
        .call("GET", "/dimension-keys", json!({}), None, None)
        .await;
    let declared = f
        .call(
            "PUT",
            "/dimension-keys",
            json!({"items":[{"key":"region","values":["eu","us"]}]}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(declared.0, 200, "{declared:?}");
    let mut input = input;
    input["dimension_key"] = json!("region");
    let created = f.call("POST", &path, input, None, Some("one")).await;
    assert_eq!(created.0, 201, "{created:?}");
    // The entry names the key but has no valued price: the key itself is still in use.
    let refused = f
        .call(
            "PUT",
            "/dimension-keys",
            json!({"items":[]}),
            Some(&declared.2),
            None,
        )
        .await;
    assert_eq!(refused.0, 409, "{refused:?}");
    assert!(
        refused.1.to_string().contains("DIMENSION_KEY_IN_USE"),
        "{refused:?}"
    );
    // Changing its values is still allowed: no price carries one.
    let changed = f
        .call(
            "PUT",
            "/dimension-keys",
            json!({"items":[{"key":"region","values":["ap","eu"]}]}),
            Some(&declared.2),
            None,
        )
        .await;
    assert_eq!(changed.0, 200, "{changed:?}");
}

// Docs F7, spec decision 4: a tenant's registry starts seeded with `region`, declared with no
// values yet; the first entry naming it stores the seed in the entry's own transaction.
#[tokio::test]
async fn the_registry_is_seeded_with_region_and_an_entry_naming_it_stores_the_seed() {
    let (f, _, path, input) = setup(0).await;
    let (status, dims, seed_tag) = f
        .call("GET", "/dimension-keys", json!({}), None, None)
        .await;
    assert_eq!(status, 200, "{dims}");
    assert_eq!(dims, json!({"items":[{"key":"region","values":[]}]}));
    let mut body = input.clone();
    body["dimension_key"] = json!("region");
    let (status, entry, _) = f.call("POST", &path, body, None, Some("region")).await;
    assert_eq!(status, 201, "{entry}");
    assert_eq!(entry["dimension_key"], "region");
    let (status, dims, stored_tag) = f
        .call("GET", "/dimension-keys", json!({}), None, None)
        .await;
    assert_eq!(status, 200, "{dims}");
    assert_eq!(dims, json!({"items":[{"key":"region","values":[]}]}));
    assert_ne!(stored_tag, seed_tag, "the seed is now a stored row");
    let prices = format!(
        "/price-book-entries/{}/prices",
        entry["id"].as_str().unwrap()
    );
    let eu = json!({"price":{"rate":"0.10"},"eligibility":"all",
        "effective_from":"2031-03-01","dim_value":"eu"});
    let (status, b, _) = f.call("POST", &prices, eu.clone(), None, Some("eu")).await;
    assert_eq!(status, 400, "no value is declared yet: {b}");
    assert!(b.to_string().contains("DIM_VALUE_UNKNOWN"), "{b}");
    let (status, b, _) = f
        .call(
            "PUT",
            "/dimension-keys",
            json!({"items":[{"key":"region","values":["eu","us"]}]}),
            Some(&stored_tag),
            None,
        )
        .await;
    assert_eq!(status, 200, "{b}");
    let (status, b, _) = f.call("POST", &prices, eu, None, Some("eu2")).await;
    assert_eq!(status, 201, "{b}");
    // PATCH names the seeded key on another new tenant the same way.
    let (f, _, path, input) = setup(0).await;
    let (status, entry, tag) = f.call("POST", &path, input, None, Some("plain")).await;
    assert_eq!(status, 201, "{entry}");
    let (status, b, _) = f
        .call(
            "PATCH",
            &format!("/price-book-entries/{}", entry["id"].as_str().unwrap()),
            json!({"dimension_key":"region"}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(status, 200, "{b}");
    let (_, _, stored) = f
        .call("GET", "/dimension-keys", json!({}), None, None)
        .await;
    let (_, _, empty) = setup(0)
        .await
        .0
        .call("GET", "/dimension-keys", json!({}), None, None)
        .await;
    assert_ne!(stored, empty, "PATCH stored the seed in its transaction");
}

async fn raw(f: &Fixture, sql: &str) {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    Database::connect(&f.dsn)
        .await
        .unwrap()
        .execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
        .await
        .unwrap();
}

// Behaviour LOW-2: a door that fails after its reserve and before its entry write cancels the
// create before it answers, as a 503 does. Here Tx B stays contended past its retries, so the
// door answers 409 CONTENDED. No ticker pass turns that answer into an entry: the receipt is
// released, and a same-key retry runs afresh.
#[tokio::test]
async fn a_contended_entry_write_after_the_reserve_cancels_the_create() {
    let (f, script, path, input) = setup(0).await;
    raw(
        &f,
        "CREATE TRIGGER entry_busy BEFORE INSERT ON pricing_price_book_entry \
         BEGIN SELECT RAISE(ABORT, '(code: 5) database is locked'); END",
    )
    .await;
    let first = f
        .call("POST", &path, input.clone(), None, Some("one"))
        .await;
    assert_eq!(first.0, 409, "{first:?}");
    assert!(first.1.to_string().contains("CONTENDED"), "{first:?}");
    assert_eq!(
        Script::count(&script.reserve_calls),
        1,
        "the reserve succeeded"
    );
    raw(&f, "DROP TRIGGER entry_busy").await;
    bss_pricing::infra::reference_ticker::Ticker::new(
        f.state.clone(),
        Arc::new(LaterClock),
        10,
        100,
    )
    .tick()
    .await
    .unwrap();
    let (status, listed, _) = f.call("GET", &path, json!({}), None, None).await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(
        listed["items"],
        json!([]),
        "the answered 409 wrote no entry"
    );
    assert_eq!(
        Script::count(&script.releases),
        1,
        "the cancellation released the receipt"
    );
    let retry = f.call("POST", &path, input, None, Some("one")).await;
    assert_eq!(retry.0, 201, "a same-key retry runs afresh: {retry:?}");
    assert_eq!(retry.1["reference_state"], "confirmed");
    let (_, listed, _) = f.call("GET", &path, json!({}), None, None).await;
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);
}

// Rename L1: a missing entry is a missing price_book_entry with ENTRY_NOT_FOUND on every entry
// door, never a missing price book or a missing price.
#[tokio::test]
async fn a_missing_entry_is_named_as_an_entry_on_every_door() {
    let (f, _, _, _) = setup(0).await;
    let path = format!("/price-book-entries/{}", Uuid::new_v4());
    let draft = json!({"price":{"rate":"0.10"},"eligibility":"all","effective_from":"2031-01-01"});
    for (method, path, body, tag, key) in [
        ("GET", path.clone(), json!({}), None, None),
        (
            "PATCH",
            path.clone(),
            json!({"invoice_line_override":"{sku}"}),
            Some("\"1\""),
            None,
        ),
        ("DELETE", path.clone(), json!({}), None, None),
        ("POST", format!("{path}/prices"), draft, None, Some("k")),
    ] {
        let (status, b, _) = f.call(method, &path, body, tag, key).await;
        assert_eq!(status, 404, "{method} {path}: {b}");
        assert_eq!(
            b["context"]["resource_name"], "price_book_entry",
            "{method}: {b}"
        );
        assert!(
            b["detail"]
                .as_str()
                .unwrap_or_default()
                .starts_with("ENTRY_NOT_FOUND"),
            "{method}: {b}"
        );
    }
}

/// The stored outcome of every op of an entry, by kind, read straight from the op table.
async fn op_inputs(f: &Fixture, entry: &str) -> Vec<(String, Value)> {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    let id: Uuid = entry.parse().unwrap();
    Database::connect(&f.dsn)
        .await
        .unwrap()
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            format!(
                "SELECT kind, outcome FROM pricing_reference_op WHERE ref_id = X'{}' ORDER BY op_id",
                id.simple()
            ),
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| {
            let outcome: String = row.try_get("", "outcome").unwrap();
            let work: Value = serde_json::from_str(&outcome).unwrap();
            (
                row.try_get::<String>("", "kind").unwrap(),
                work["target"]["price_book_entry"]["input"].clone(),
            )
        })
        .collect()
}

/// D-427: the create requires `model`, and it must be one the SKU's charge kind allows. Both are
/// input refusals, 400 on `model` at the door, before anything is claimed or reserved.
#[tokio::test]
async fn the_entry_create_requires_a_model_its_charge_kind_allows() {
    for (mode, model, code) in [
        (0, json!(null), None),
        (0, json!("stair"), Some("MODEL_INVALID")),
        (0, json!("flat"), Some("MODEL_KIND_CHARGEKIND_MISMATCH")),
        (
            11,
            json!("graduated"),
            Some("MODEL_KIND_CHARGEKIND_MISMATCH"),
        ),
    ] {
        let (f, script, path, mut input) = setup(mode).await;
        if model.is_null() {
            input.as_object_mut().unwrap().remove("model");
        } else {
            input["model"] = model.clone();
        }
        let refused = f
            .call("POST", &path, input.clone(), None, Some("one"))
            .await;
        assert_eq!(refused.0, 400, "{model}: {refused:?}");
        if let Some(code) = code {
            assert!(refused.1.to_string().contains(code), "{refused:?}");
            assert!(refused.1.to_string().contains("model"), "{refused:?}");
        }
        assert_eq!(Script::count(&script.reserve_calls), 0, "{model}");
        // Nothing was claimed: the corrected request runs under the same key.
        input["model"] = json!("per_unit");
        let created = f.call("POST", &path, input, None, Some("one")).await;
        assert_eq!(created.0, 201, "{created:?}");
        assert_eq!(created.1["model"], "per_unit");
        let read = f
            .call(
                "GET",
                &format!("/price-book-entries/{}", created.1["id"].as_str().unwrap()),
                json!({}),
                None,
                None,
            )
            .await;
        assert_eq!(read.1["model"], "per_unit", "{read:?}");
    }
}

/// D-427 with D-401: Tx B judges the model again, against the SKU type the reservation froze.
/// The door read a one-time SKU (flat allowed); the re-read after the reserve answers usage, so
/// the create is refused 400 `MODEL_KIND_CHARGEKIND_MISMATCH` as a receipt, its reservation is
/// released, no entry is written, and the key replays the refusal.
#[tokio::test]
async fn tx_b_judges_the_model_again_against_the_type_the_reservation_froze() {
    let (f, script, path, mut input) = setup(21).await;
    input["model"] = json!("flat");
    let refused = f
        .call("POST", &path, input.clone(), None, Some("one"))
        .await;
    assert_eq!(refused.0, 400, "{refused:?}");
    assert!(
        refused
            .1
            .to_string()
            .contains("MODEL_KIND_CHARGEKIND_MISMATCH"),
        "{refused:?}"
    );
    assert_eq!(Script::count(&script.reserve_calls), 1, "the reserve ran");
    assert_eq!(Script::count(&script.releases), 1, "and was released");
    let (_, listed, _) = f.call("GET", &path, json!({}), None, None).await;
    assert_eq!(listed["items"], json!([]), "no entry was written");
    assert_eq!(
        f.call("POST", &path, input, None, Some("one")).await,
        refused,
        "the key replays the refusal"
    );
}

/// D-427: the model is part of the key. The same SKU, charge kind and period with two models are
/// two entries of one book; the same model twice is `ENTRY_KEY_TAKEN`.
#[tokio::test]
async fn two_models_of_one_sku_kind_and_period_are_two_entries_of_one_book() {
    let (f, _, path, input) = setup(0).await;
    let with = |model: &str| {
        let mut body = input.clone();
        body["model"] = json!(model);
        body
    };
    let per_unit = f
        .call("POST", &path, with("per_unit"), None, Some("a"))
        .await;
    assert_eq!(per_unit.0, 201, "{per_unit:?}");
    let graduated = f
        .call("POST", &path, with("graduated"), None, Some("b"))
        .await;
    assert_eq!(graduated.0, 201, "{graduated:?}");
    assert_ne!(per_unit.1["id"], graduated.1["id"]);
    assert_eq!(per_unit.1["sku_id"], graduated.1["sku_id"]);
    let taken = f
        .call("POST", &path, with("per_unit"), None, Some("c"))
        .await;
    assert_eq!(taken.0, 409, "{taken:?}");
    assert!(taken.1.to_string().contains("ENTRY_KEY_TAKEN"), "{taken:?}");
    let (_, listed, _) = f.call("GET", &path, json!({}), None, None).await;
    let mut models: Vec<&str> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["model"].as_str().unwrap())
        .collect();
    models.sort_unstable();
    assert_eq!(models, ["graduated", "per_unit"]);
}

/// D-427: the model is fixed for the entry's life — the PATCH does not carry it.
#[tokio::test]
async fn the_entry_patch_does_not_carry_the_model() {
    let (f, _, path, mut input) = setup(0).await;
    input["model"] = json!("graduated");
    let created = f.call("POST", &path, input, None, Some("one")).await;
    assert_eq!(created.0, 201, "{created:?}");
    let entry = format!("/price-book-entries/{}", created.1["id"].as_str().unwrap());
    let refused = f
        .call(
            "PATCH",
            &entry,
            json!({"model":"per_unit"}),
            Some(&created.2),
            None,
        )
        .await;
    assert_eq!(refused.0, 400, "{refused:?}");
    let read = f.call("GET", &entry, json!({}), None, None).await;
    assert_eq!(read.1["model"], "graduated");
    assert_eq!(read.2, created.2, "nothing changed");
}

/// D-427 with D-401: every op of an entry carries its create input, `model` included: the create
/// as the door stored it, the rereserve a released receipt starts, and the delete.
#[tokio::test]
async fn every_op_of_an_entry_carries_its_model() {
    // Mode 7: the confirm finds the receipt released, so Tx C starts a rereserve op.
    let (f, _, path, mut input) = setup(7).await;
    input["model"] = json!("graduated");
    let created = f.call("POST", &path, input, None, Some("one")).await;
    assert_eq!(created.0, 201, "{created:?}");
    let ops = op_inputs(&f, created.1["id"].as_str().unwrap()).await;
    let kinds: Vec<&str> = ops.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(kinds, ["create", "rereserve"], "{ops:?}");
    for (kind, input) in &ops {
        assert_eq!(input["model"], "graduated", "{kind}: {input}");
    }
    // A confirmed entry's delete rebuilds the input from the entry, model included.
    let (f, _, path, mut input) = setup(0).await;
    input["model"] = json!("volume");
    let created = f.call("POST", &path, input, None, Some("one")).await;
    assert_eq!(created.0, 201, "{created:?}");
    let id = created.1["id"].as_str().unwrap();
    let deleted = f
        .call(
            "DELETE",
            &format!("/price-book-entries/{id}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(deleted.0, 204, "{deleted:?}");
    let ops = op_inputs(&f, id).await;
    assert_eq!(
        ops.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        ["create", "delete"]
    );
    for (kind, input) in &ops {
        assert_eq!(input["model"], "volume", "{kind}: {input}");
    }
}

/// D-439: a stored token outside its closed set (a row written around the gear, with its CHECK
/// bypassed) is `CorruptRow`: the read answers 500 — never a panic, never the token under an
/// `enum` that does not hold it — and the gear goes on serving.
#[tokio::test]
async fn a_stored_token_outside_its_closed_set_is_a_500_never_a_panic() {
    use sea_orm::{ConnectionTrait, Database};
    let (f, _, path, input) = setup(0).await;
    let created = f.call("POST", &path, input, None, Some("one")).await;
    assert_eq!(created.0, 201, "{created:?}");
    let id: Uuid = serde_json::from_value(created.1["id"].clone()).unwrap();
    let raw = Database::connect(&f.dsn).await.unwrap();
    let hex = id.simple().to_string().to_uppercase();
    let row = format!("WHERE id = '{id}' OR hex(id) = '{hex}'");
    // The premise: the CHECK holds both columns to their sets, so only a writer around it can.
    for set in ["charge_kind = 'weekly'", "period = 'week'"] {
        let refused = raw
            .execute_unprepared(&format!("UPDATE pricing_price_book_entry SET {set} {row}"))
            .await
            .unwrap_err();
        assert!(refused.to_string().contains("CHECK"), "{set}: {refused}");
    }
    raw.execute_unprepared("PRAGMA ignore_check_constraints = ON")
        .await
        .unwrap();
    let poisoned = raw
        .execute_unprepared(&format!(
            "UPDATE pricing_price_book_entry SET charge_kind = 'weekly' {row}"
        ))
        .await
        .unwrap();
    assert_eq!(poisoned.rows_affected(), 1);
    raw.close().await.unwrap();
    let entry = format!("/price-book-entries/{id}");
    let read = f.call("GET", &entry, json!({}), None, None).await;
    assert_eq!(read.0, 500, "{read:?}");
    assert!(!read.1.to_string().contains("weekly"), "{read:?}");
    let books = f.call("GET", "/price-books", json!({}), None, None).await;
    assert_eq!(books.0, 200, "the gear still serves: {books:?}");
}
