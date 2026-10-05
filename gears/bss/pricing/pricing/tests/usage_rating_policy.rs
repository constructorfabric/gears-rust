//! Entry-owned immutable policy authoring and identity.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod pg_support;
mod plan_support;
mod seam_support;
use plan_support::entry_support::{Fixture, Script};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

fn policy() -> Value {
    json!({
        "rating_window":{"kind":"calendar_hour","timezone":"UTC"},
        "aggregation_scope":"subscription_line","reset":"rating_window_start",
        "quantity_semantics":{"meter":{"usage_type_id":"vm-hours","version":"v1"},
            "unit":"VM\u{b7}hour","fold":"SUM","accrual_policy_version":"integrated-v1"},
        "partial_window":"actual_quantity_full_thresholds"
    })
}

/// The rules stored and served after D-514. The meter copy is not part of the policy.
fn stored_rules() -> Value {
    json!({
        "rating_window":{"kind":"calendar_hour","timezone":"UTC"},
        "aggregation_scope":"subscription_line",
        "reset":"rating_window_start",
        "partial_window":"actual_quantity_full_thresholds",
        "fold":"SUM"
    })
}

/// The same policy with the three single-valued fields left out (D-513).
fn policy_without_single_valued_fields() -> Value {
    let mut body = policy();
    body.as_object_mut().unwrap().remove("reset");
    body.as_object_mut().unwrap().remove("partial_window");
    body["quantity_semantics"]
        .as_object_mut()
        .unwrap()
        .remove("fold");
    body
}

#[tokio::test]
async fn policy_is_immutable_deduplicated_and_part_of_the_entry_key() {
    authoring_case(Fixture::new(Arc::new(Script::default())).await).await;
}

#[expect(
    clippy::cognitive_complexity,
    reason = "one ordered authoring and replay contract, shared by both databases"
)]
async fn authoring_case(f: Fixture) {
    let (book, _) = f.book().await;
    let path = format!("/price-books/{}/entries", book["id"].as_str().unwrap());
    let body = json!({"sku_id":Uuid::new_v4(),"model":"per_unit","usage_rating_policy":policy()});
    let first = f
        .call("POST", &path, body.clone(), None, Some("hour"))
        .await;
    assert_eq!(first.0, 201, "{first:?}");
    let original = first.1["usage_rating_policy"].clone();
    assert_eq!(original["content"], stored_rules());
    assert_eq!(first.1["usage_sku_version"], json!(1));
    assert_eq!(original["version"], "1");
    assert_eq!(original["digest"].as_str().unwrap().len(), 64);
    let read_path = format!("/price-book-entries/{}", first.1["id"].as_str().unwrap());
    assert_immutable(&f, &read_path, &first.2, &original).await;
    assert_policy_views(&f, &book["id"], &body["sku_id"], &original).await;
    let duplicate = f
        .call("POST", &path, body.clone(), None, Some("duplicate"))
        .await;
    assert_eq!(duplicate.0, 409, "{duplicate:?}");
    assert!(duplicate.1.to_string().contains("ENTRY_KEY_TAKEN"));
    let mut monthly = body.clone();
    monthly["usage_rating_policy"]["rating_window"] = json!({"kind":"billing_cycle"});
    assert_eq!(
        f.call("POST", &path, monthly, None, Some("month")).await.0,
        201
    );
    let mut resource = body.clone();
    resource["usage_rating_policy"]["aggregation_scope"] = json!("resource");
    let second = f
        .call("POST", &path, resource, None, Some("resource"))
        .await;
    assert_eq!(second.0, 201, "{second:?}");
    assert_ne!(
        second.1["usage_rating_policy"]["policy_id"],
        original["policy_id"]
    );
    let mut another_sku = body.clone();
    another_sku["sku_id"] = json!(Uuid::new_v4());
    let reused = f
        .call("POST", &path, another_sku, None, Some("reuse"))
        .await;
    assert_eq!(reused.0, 201, "{reused:?}");
    assert_eq!(reused.1["usage_rating_policy"], original);
    assert_eq!(f.call("POST", &path, body, None, Some("hour")).await, first);
    assert_eq!(
        f.call("GET", &read_path, json!({}), None, None).await.1["usage_rating_policy"],
        original
    );
}

async fn assert_policy_views(f: &Fixture, book: &Value, sku: &Value, original: &Value) {
    let book = book.as_str().unwrap();
    let sku = sku.as_str().unwrap();
    for (path, pointer) in [
        (
            format!("/price-books/{book}/entries"),
            "/items/0/usage_rating_policy",
        ),
        (
            format!("/price-books/{book}/export"),
            "/entries/0/entry/usage_rating_policy",
        ),
        (
            format!("/price-book-entries?sku_id={sku}"),
            "/items/0/usage_rating_policy",
        ),
    ] {
        let answer = f.call("GET", &path, json!({}), None, None).await;
        assert_eq!(answer.0, 200, "{answer:?}");
        assert_eq!(
            answer.1.pointer(pointer),
            Some(original),
            "{path}: {answer:?}"
        );
    }
}

async fn assert_immutable(f: &Fixture, read_path: &str, tag: &str, original: &Value) {
    assert_eq!(
        f.call("GET", read_path, json!({}), None, None).await.1["usage_rating_policy"],
        *original
    );
    for replacement in [Value::Null, policy()] {
        assert_eq!(
            f.call(
                "PATCH",
                read_path,
                json!({"usage_rating_policy":replacement}),
                Some(tag),
                None
            )
            .await
            .0,
            400
        );
    }
}

#[test]
fn a_window_change_is_a_different_entry_key() {
    use bss_pricing::domain::usage_policy::entry_policy_key;
    use bss_pricing_sdk::terms::{RatingWindow, Timezone};
    let monthly = seam_support::vm_hour_policy().content;
    let mut hourly = monthly.clone();
    hourly.rating_window = RatingWindow::CalendarHour {
        timezone: Timezone::Utc,
    };
    assert_ne!(entry_policy_key(&hourly), entry_policy_key(&monthly));
    assert_eq!(entry_policy_key(&hourly), entry_policy_key(&hourly.clone()));
}

#[tokio::test]
async fn policy_shape_and_ownership_are_enforced_before_reservation() {
    let script = Arc::new(Script::default());
    let f = Fixture::new(script.clone()).await;
    let (book, _) = f.book().await;
    let path = format!("/price-books/{}/entries", book["id"].as_str().unwrap());
    let base = json!({"sku_id":Uuid::new_v4(),"model":"per_unit"});
    let missing = f
        .call("POST", &path, base.clone(), None, Some("missing"))
        .await;
    assert_eq!(missing.0, 400);
    assert!(missing.1.to_string().contains("MISSING_RATING_POLICY"));
    let mut valid = base.clone();
    valid["usage_rating_policy"] = policy();
    for (key, value) in [
        ("policy_id", json!(Uuid::new_v4())),
        ("version", json!("1")),
        ("digest", json!("0".repeat(64))),
        ("included_qty", json!("1")),
    ] {
        let mut body = valid.clone();
        body["usage_rating_policy"][key] = value;
        let refused = f.call("POST", &path, body, None, Some(key)).await;
        assert_eq!(refused.0, 400, "{key}: {refused:?}");
        let text = refused.1.to_string();
        assert!(text.contains(key), "{text}");
        assert!(text.contains("unknown field"), "{text}");
    }
    for pointer in [
        "/quantity_semantics/meter/usage_type_id",
        "/quantity_semantics/meter/version",
        "/quantity_semantics/unit",
        "/quantity_semantics/accrual_policy_version",
    ] {
        let mut body = valid.clone();
        *body["usage_rating_policy"].pointer_mut(pointer).unwrap() = json!(" \t");
        let result = f.call("POST", &path, body, None, Some(pointer)).await;
        assert_eq!(result.0, 400, "{result:?}");
        assert!(result.1.to_string().contains("METER_POLICY_MISMATCH"));
    }
    for (pointer, value) in [
        ("/rating_window/kind", "rolling"),
        ("/rating_window/timezone", "Europe/Madrid"),
        ("/aggregation_scope", "tenant"),
        ("/quantity_semantics/fold", "MAX"),
        ("/reset", "never"),
        ("/partial_window", "prorate_thresholds"),
    ] {
        let mut body = valid.clone();
        *body["usage_rating_policy"].pointer_mut(pointer).unwrap() = json!(value);
        let refused = f.call("POST", &path, body, None, Some(pointer)).await;
        assert_eq!(refused.0, 400, "{pointer}: {refused:?}");
        let text = refused.1.to_string();
        assert!(text.contains(value), "{text}");
        assert!(text.contains("unknown variant"), "{text}");
    }
    let duplicate = valid.to_string().replace(
        "\"version\":\"v1\"",
        "\"version\":\"v1\",\"version\":\"v2\"",
    );
    assert_eq!(
        plan_support::entry_support::request_raw(
            &f.app,
            &f.ctx,
            "POST",
            &path,
            &duplicate,
            Some("duplicate-field")
        )
        .await
        .0,
        400
    );
    script.set(11);
    valid["period"] = json!("month");
    let nonusage = f.call("POST", &path, valid, None, Some("nonusage")).await;
    assert_eq!(nonusage.0, 400);
    assert!(nonusage.1.to_string().contains("UNEXPECTED_RATING_POLICY"));
    assert_eq!(Script::count(&script.reserve_calls), 0);
}

#[test]
fn items_prices_and_entry_patch_refuse_all_policy_fields() {
    use bss_pricing::api::rest::authoring::dto::*;
    for key in [
        "usage_rating_policy",
        "usage_policy_id",
        "usage_policy_version",
        "usage_policy_digest",
    ] {
        let mut body = json!({"sku_id":Uuid::new_v4(),"price_book_entry_id":Uuid::new_v4()});
        body[key] = policy();
        assert!(serde_json::from_value::<PricingPlanItemCreate>(body).is_err());
        let mut body = json!({"price_book_entry_id":Uuid::new_v4()});
        body[key] = Value::Null;
        assert!(serde_json::from_value::<PricingPlanItemPatch>(body).is_err());
        let mut body = json!({});
        body[key] = Value::Null;
        assert!(serde_json::from_value::<PricingPriceBookEntryPatch>(body.clone()).is_err());
        assert!(serde_json::from_value::<PricingPricePatch>(body.clone()).is_err());
        body["price"] = json!({"rate":"1"});
        body["eligibility"] = json!("all");
        body["effective_from"] = json!("2026-09-01");
        assert!(serde_json::from_value::<PricingPriceCreate>(body).is_err());
    }
}

async fn create_entry(f: &Fixture, book: Uuid, sku: Uuid, content: Value) -> Uuid {
    let result = f
        .call(
            "POST",
            &format!("/price-books/{book}/entries"),
            json!({"sku_id":sku,"model":"per_unit","usage_rating_policy":content}),
            None,
            Some(&Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(result.0, 201, "{result:?}");
    plan_support::id_of(&result.1["id"])
}

#[tokio::test]
async fn book_switch_matches_policy_and_dimension_and_preserves_unmatched_entries() {
    use bss_products_sdk::models::SkuType;
    use plan_support::{book, id_of, item, items, plan, publish, setup};
    let (f, catalog) = setup().await;
    let sku = catalog.sku(SkuType::Usage);
    let source = book(&f, "SOURCE").await;
    let target = book(&f, "TARGET").await;
    let wrong = book(&f, "WRONG").await;
    let dimension = book(&f, "DIMENSION").await;
    let hourly = create_entry(&f, source, sku, policy()).await;
    let mut monthly = policy();
    monthly["rating_window"] = json!({"kind":"billing_cycle"});
    // Insert the wrong policy first: selection must match content, never first SKU/model.
    create_entry(&f, target, sku, monthly.clone()).await;
    let twin = create_entry(&f, target, sku, policy()).await;
    create_entry(&f, wrong, sku, monthly).await;
    let dimension_twin = create_entry(&f, dimension, sku, policy()).await;
    let epath = format!("/price-book-entries/{dimension_twin}");
    let tag = f.call("GET", &epath, json!({}), None, None).await.2;
    assert_eq!(
        f.call(
            "PATCH",
            &epath,
            json!({"dimension_key":"region"}),
            Some(&tag),
            None
        )
        .await
        .0,
        200
    );
    for (code, destination, expected) in [
        ("EXACT", target, twin),
        ("WRONG", wrong, hourly),
        ("DIM", dimension, hourly),
    ] {
        let (_, revision) = plan(&f, code, source).await;
        item(&f, revision, sku, Some(hourly), "paid").await;
        let path = format!("/plan-revisions/{revision}");
        let tag = f.call("GET", &path, json!({}), None, None).await.2;
        let patched = f
            .call(
                "PATCH",
                &path,
                json!({"book_id":destination}),
                Some(&tag),
                None,
            )
            .await;
        assert_eq!(patched.0, 200, "{patched:?}");
        assert_eq!(
            items(&f, revision).await[0].price_book_entry_id,
            Some(expected)
        );
        if expected == hourly {
            let checks = f
                .call("GET", &format!("{path}/checks"), json!({}), None, None)
                .await;
            assert_eq!(checks.0, 200, "{checks:?}");
            assert!(
                checks.1.to_string().contains("ITEM_BOOK_FOREIGN"),
                "{checks:?}"
            );
        }
    }
    // Copy and clone preserve the exact selected entry in the same book.
    let (p, revision) = plan(&f, "CLONESOURCE", source).await;
    item(&f, revision, sku, Some(hourly), "paid").await;
    publish(&f, id_of(&p["id"]), revision).await;
    for (suffix, body) in [
        ("revisions", json!({})),
        ("clone", json!({"code":"CLONED","name":"Cloned"})),
    ] {
        let response = f
            .call(
                "POST",
                &format!("/plans/{}/{suffix}", p["id"].as_str().unwrap()),
                body,
                None,
                Some(suffix),
            )
            .await;
        assert_eq!(response.0, 201, "{response:?}");
        let revision = if suffix == "clone" {
            id_of(&response.1["revisions"][0]["id"])
        } else {
            id_of(&response.1["id"])
        };
        assert_eq!(
            items(&f, revision).await[0].price_book_entry_id,
            Some(hourly)
        );
    }
}

struct RecoveryClock(time::OffsetDateTime);
impl bss_pricing::infra::reference_work::Clock for RecoveryClock {
    fn now(&self) -> time::OffsetDateTime {
        self.0
    }
}

#[tokio::test]
async fn crash_windows_preserve_policy_input_and_confirmed_receipt() {
    use bss_pricing::infra::{
        reference_ticker::Ticker,
        reference_work::{Target, Work},
        storage::repo::{price_book_entry_repo, reference_op_repo},
    };
    use toolkit_db::secure::AccessScope;
    for mode in [1, 2] {
        let script = Arc::new(Script::default());
        let f = Fixture::new(script.clone()).await;
        let provider =
            Arc::new(plan_support::entry_support::policy_support::MeterProvider::default());
        f.state
            .hub
            .register::<dyn bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1>(
                provider.clone(),
            );
        let (book, _) = f.book().await;
        let path = format!("/price-books/{}/entries", book["id"].as_str().unwrap());
        let body =
            json!({"sku_id":Uuid::new_v4(),"model":"per_unit","usage_rating_policy":policy()});
        script.set(mode);
        let mut door = Box::pin(f.call("POST", &path, body.clone(), None, Some("crash")));
        tokio::select! { result = &mut door => panic!("did not park: {result:?}"), () = script.parked.notified() => {} }
        drop(door);
        let now = time::OffsetDateTime::now_utc() + time::Duration::days(2);
        let scope = AccessScope::for_tenant(f.ctx.subject_tenant_id());
        let ops = reference_op_repo::due(&f.db.conn().unwrap(), &scope, now, 10)
            .await
            .unwrap();
        assert_eq!(ops.len(), 1);
        let Target::PriceBookEntry { input, .. } = Work::read(&ops[0]).unwrap().target else {
            panic!("entry op")
        };
        assert_eq!(input.schema_version, Some(3));
        assert_eq!(input.meter_evidence.as_ref().unwrap().digest, [7; 32]);
        assert_eq!(
            serde_json::to_value(input.usage_rating_policy).unwrap(),
            stored_rules()
        );
        let before = price_book_entry_repo::find(
            &f.db.conn().unwrap(),
            &scope,
            f.ctx.subject_tenant_id(),
            ops[0].ref_id,
        )
        .await
        .unwrap();
        assert_eq!(before.is_some(), mode == 2);
        provider
            .failure
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let provider_calls = provider.calls.load(std::sync::atomic::Ordering::SeqCst);
        script.set(0);
        if mode == 1 {
            // The ticker deliberately cancels unreserved creates (D-401). Resume the persisted
            // user's operation through the door driver to test the captured pre-Tx-B evidence.
            bss_pricing::infra::reference_work::drive(
                &f.state,
                &f.ctx,
                ops[0].op_id,
                Arc::new(RecoveryClock(now)),
                bss_pricing::infra::reference_work::Caller::Door,
            )
            .await
            .unwrap();
        }
        Ticker::new(f.state.clone(), Arc::new(RecoveryClock(now)), 10, 100)
            .tick()
            .await
            .unwrap();
        let result = f
            .call("POST", &path, body.clone(), None, Some("crash"))
            .await;
        assert_eq!(result.0, 201, "{result:?}");
        assert_eq!(result.1["usage_rating_policy"]["content"], stored_rules());
        if let Some(before) = before {
            assert_eq!(
                result.1["usage_rating_policy"]["policy_id"],
                before.usage_policy_id.unwrap().to_string()
            );
        }
        assert_eq!(
            provider.calls.load(std::sync::atomic::Ordering::SeqCst),
            provider_calls
        );
        let calls = Script::count(&script.reserve_calls);
        script
            .skus_down
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            f.call("POST", &path, body, None, Some("crash")).await,
            result
        );
        assert_eq!(Script::count(&script.reserve_calls), calls);
    }
}

#[tokio::test]
async fn legacy_published_usage_upgrades_without_inventing_policy_and_keeps_resolving() {
    use bss_pricing::{infra::storage::repo::price_book_entry_repo, module::BssPricingGear};
    use bss_products_sdk::models::SkuType;
    use plan_support::entry_support::TestDsn;
    use plan_support::{Catalog, book, id_of, scope};
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    use toolkit::contracts::DatabaseCapability;
    use toolkit_db::migration_runner::run_migrations_for_testing;
    let dsn = TestDsn::new("pricing-legacy-policy-");
    let db = toolkit_db::connect_db(&dsn, toolkit_db::ConnectOpts::default())
        .await
        .unwrap();
    let prior = BssPricingGear::default()
        .migrations()
        .into_iter()
        .filter(|m| {
            let name = m.name();
            !(name.contains("000018")
                || name.contains("000019")
                || name.contains("000020")
                || name.contains("000021")
                || name.contains("000022")
                || name.contains("000023"))
        })
        .collect();
    run_migrations_for_testing(&db, prior).await.unwrap();
    let catalog = Arc::new(Catalog::default());
    let sku = catalog.sku(SkuType::Usage);
    let f = Fixture::on(
        toolkit_db::DBProvider::new(db),
        Uuid::new_v4(),
        dsn,
        catalog,
    )
    .await;
    let tenant = f.ctx.subject_tenant_id();
    let book_id = Uuid::new_v4();
    let plan_id = Uuid::new_v4();
    let revision = Uuid::new_v4();
    let entry = Uuid::new_v4();
    let price = Uuid::new_v4();
    let now = time::OffsetDateTime::now_utc();
    let raw = Database::connect(&f.dsn).await.unwrap();
    raw.execute_raw(Statement::from_sql_and_values(DbBackend::Sqlite,
        "INSERT INTO pricing_price_book (id,tenant_id,code,name,currency,version,created_at,updated_at) VALUES (?,?,?,?,?,1,?,?)",
        vec![book_id.into(),tenant.into(),"LEGACY".into(),"Legacy".into(),"EUR".into(),now.into(),now.into()])).await.unwrap();
    raw.execute_raw(Statement::from_sql_and_values(DbBackend::Sqlite,
        "INSERT INTO pricing_plan (id,tenant_id,code,name,published_rev,version,created_by,created_at,updated_at) VALUES (?,?,?,?,1,1,?,?,?)",
        vec![plan_id.into(),tenant.into(),"LEGACY".into(),"Legacy".into(),f.ctx.subject_id().into(),now.into(),now.into()])).await.unwrap();
    raw.execute_raw(Statement::from_sql_and_values(DbBackend::Sqlite,
        "INSERT INTO pricing_plan_revision (id,tenant_id,plan_id,rev_no,book_id,state,version,created_by,created_at,updated_at) VALUES (?,?,?,1,?,'published',1,?,?,?)",
        vec![revision.into(),tenant.into(),plan_id.into(),book_id.into(),f.ctx.subject_id().into(),now.into(),now.into()])).await.unwrap();
    raw.execute_raw(Statement::from_sql_and_values(DbBackend::Sqlite,
        "INSERT INTO pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,model,reservation_id,reference_state,version,created_at,updated_at) VALUES (?,?,?,?,'usage','per_unit',?,'confirmed',1,?,?)",
        vec![entry.into(),tenant.into(),book_id.into(),sku.into(),Uuid::new_v4().into(),now.into(),now.into()])).await.unwrap();
    raw.execute_raw(Statement::from_sql_and_values(DbBackend::Sqlite,
        "INSERT INTO pricing_plan_item (id,tenant_id,revision_id,sku_id,price_book_entry_id,treatment,reservation_id,reference_state,version,created_by,created_at,updated_at) VALUES (?,?,?,?,?,'paid',?,'confirmed',1,?,?,?)",
        vec![Uuid::new_v4().into(),tenant.into(),revision.into(),sku.into(),entry.into(),Uuid::new_v4().into(),f.ctx.subject_id().into(),now.into(),now.into()])).await.unwrap();
    raw.execute_raw(Statement::from_sql_and_values(DbBackend::Sqlite,
        "INSERT INTO pricing_price (id,tenant_id,price_book_entry_id,version_no,price_json,eligibility,effective_from,state,created_by,version,created_at,updated_at) VALUES (?,?,?,1,?,'all','2026-09-01','approved',?,1,?,?)",
        vec![price.into(),tenant.into(),entry.into(),json!({"rate":"1"}).to_string().into(),f.ctx.subject_id().into(),now.into(),now.into()])).await.unwrap();
    run_migrations_for_testing(&f.db.db(), BssPricingGear::default().migrations())
        .await
        .unwrap();
    let stored = price_book_entry_repo::find(&f.db.conn().unwrap(), &scope(&f), tenant, entry)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            stored.usage_policy_id,
            stored.usage_policy_version,
            stored.usage_policy_digest
        ),
        (None, None, None)
    );
    let read = f
        .call(
            "GET",
            &format!("/price-book-entries/{entry}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(read.0, 200, "{read:?}");
    assert_eq!(read.1["usage_rating_policy"], Value::Null);
    let resolved = f
        .call(
            "GET",
            &format!("/resolve?plan_revision_id={revision}&date=2026-10-01"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(resolved.0, 200, "{resolved:?}");
    assert!(
        resolved.1.to_string().contains(&price.to_string()),
        "{resolved:?}"
    );
    // A legacy null-policy source cannot switch to a modern policy-bearing entry implicitly.
    let target = book(&f, "MODERN").await;
    create_entry(&f, target, sku, policy()).await;
    let copied = f
        .call(
            "POST",
            &format!("/plans/{plan_id}/revisions"),
            json!({}),
            None,
            Some("copy"),
        )
        .await;
    assert_eq!(copied.0, 201, "{copied:?}");
    let draft = id_of(&copied.1["id"]);
    let patched = f
        .call(
            "PATCH",
            &format!("/plan-revisions/{draft}"),
            json!({"book_id":target}),
            Some(&copied.2),
            None,
        )
        .await;
    assert_eq!(patched.0, 200, "{patched:?}");
    assert_eq!(
        plan_support::items(&f, draft).await[0].price_book_entry_id,
        Some(entry)
    );
    // Null-policy uniqueness remains enforced by the normalized-period key.
    assert!(raw.execute_raw(Statement::from_sql_and_values(DbBackend::Sqlite,
        "INSERT INTO pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,model,reservation_id,reference_state,version,created_at,updated_at) VALUES (?,?,?,?,'usage','per_unit',?,'confirmed',1,?,?)",
        vec![Uuid::new_v4().into(),tenant.into(),book_id.into(),sku.into(),Uuid::new_v4().into(),now.into(),now.into()])).await.is_err());
    assert!(
        raw.query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA foreign_key_check"
        ))
        .await
        .unwrap()
        .is_empty()
    );
}

#[tokio::test]
async fn database_rejects_partial_foreign_and_mismatched_references_and_policy_mutation() {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    let f = Fixture::new(Arc::new(Script::default())).await;
    let (book, _) = f.book().await;
    let entry = create_entry(
        &f,
        plan_support::id_of(&book["id"]),
        Uuid::new_v4(),
        policy(),
    )
    .await;
    let raw = Database::connect(&f.dsn).await.unwrap();
    for assignment in [
        "usage_policy_id = NULL",
        "usage_policy_version = NULL",
        "usage_policy_digest = NULL",
        "usage_policy_digest = ''",
        "usage_policy_digest = 'bad'",
        "usage_policy_version = 2",
    ] {
        assert!(
            raw.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                format!("UPDATE pricing_price_book_entry SET {assignment} WHERE id = ?"),
                [entry.into()]
            ))
            .await
            .is_err(),
            "{assignment}"
        );
    }
    assert!(
        raw.execute_raw(Statement::from_string(
            DbBackend::Sqlite,
            "UPDATE pricing_usage_rating_policy SET content = '{}'"
        ))
        .await
        .is_err()
    );
    assert!(
        raw.execute_raw(Statement::from_string(
            DbBackend::Sqlite,
            "DELETE FROM pricing_usage_rating_policy"
        ))
        .await
        .is_err()
    );
    let foreign = Uuid::new_v4();
    assert!(
        raw.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "UPDATE pricing_price_book_entry SET tenant_id = ? WHERE id = ?",
            [foreign.into(), entry.into()]
        ))
        .await
        .is_err()
    );
    // The same canonical content in another tenant has its own policy identity.
    let other = Fixture::on(
        f.db.clone(),
        foreign,
        f.dsn.clone(),
        Arc::new(Script::default()),
    )
    .await;
    let (other_book, _) = other.book().await;
    let other_entry = create_entry(
        &other,
        plan_support::id_of(&other_book["id"]),
        Uuid::new_v4(),
        policy(),
    )
    .await;
    let first = f
        .call(
            "GET",
            &format!("/price-book-entries/{entry}"),
            json!({}),
            None,
            None,
        )
        .await;
    let second = other
        .call(
            "GET",
            &format!("/price-book-entries/{other_entry}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_ne!(
        first.1["usage_rating_policy"]["policy_id"],
        second.1["usage_rating_policy"]["policy_id"]
    );
    assert_eq!(
        first.1["usage_rating_policy"]["digest"],
        second.1["usage_rating_policy"]["digest"]
    );
    assert_eq!(
        other
            .call(
                "GET",
                &format!("/price-book-entries/{entry}"),
                json!({}),
                None,
                None
            )
            .await
            .0,
        404
    );
}

#[tokio::test]
async fn a_digest_collision_is_an_integrity_error_not_content_reuse() {
    use bss_pricing::infra::{
        storage::repo::usage_policy_repo,
        usage_policy_wire::{UsageRatingPolicyInput, digest_text},
    };
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    let f = Fixture::new(Arc::new(Script::default())).await;
    let input: UsageRatingPolicyInput = serde_json::from_value(policy()).unwrap();
    let digest = digest_text(bss_pricing_sdk::digest::policy_digest(&(&input).into()));
    let mut corrupt = policy();
    corrupt["aggregation_scope"] = json!("resource");
    let raw = Database::connect(&f.dsn).await.unwrap();
    raw.execute_raw(Statement::from_sql_and_values(DbBackend::Sqlite,
        "INSERT INTO pricing_usage_rating_policy (tenant_id,policy_id,version,digest,content,created_at,created_by) VALUES (?,?,1,?,?,?,?)",
        vec![f.ctx.subject_tenant_id().into(),Uuid::new_v4().into(),digest.into(),corrupt.to_string().into(),time::OffsetDateTime::now_utc().into(),f.ctx.subject_id().into()])).await.unwrap();
    let error = usage_policy_repo::intern(
        &f.db.conn().unwrap(),
        &plan_support::scope(&f),
        f.ctx.subject_tenant_id(),
        f.ctx.subject_id(),
        &input,
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        bss_pricing::infra::storage::RepoError::CorruptRow(_)
    ));
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_policy_authoring_uses_the_same_content_key_and_receipt() {
    let pg = pg_support::Pg::applied().await;
    let f = Fixture::on(
        toolkit_db::DBProvider::new(pg.db().await),
        Uuid::new_v4(),
        plan_support::entry_support::TestDsn::of(pg.url(true)),
        Arc::new(Script::default()),
    )
    .await;
    authoring_case(f).await;
}

#[tokio::test]
async fn only_unversioned_persisted_creates_can_recover_without_a_policy() {
    use bss_pricing::{
        domain::reference_op::{OpKind, RefKind},
        infra::{
            reference_work::{self, Caller, EntryInput, Ref, Target, WallClock, Work},
            storage::repo::{price_book_entry_repo, reference_op_repo},
        },
    };
    use bss_products_sdk::{ReferenceRegistryV1, models::ReferenceKind};
    for versioned in [false, true] {
        let script = Arc::new(Script::default());
        let f = Fixture::new(script.clone()).await;
        let (book, _) = f.book().await;
        let sku = Uuid::new_v4();
        let id = Uuid::new_v4();
        let mut persisted = json!({"sku_id":sku,"model":"per_unit","period":null,"dimension_key":null,"invoice_line_override":null});
        if versioned {
            persisted["schema_version"] = json!(1);
        }
        let input: EntryInput = serde_json::from_value(persisted).unwrap();
        let work = Work {
            target: Target::PriceBookEntry {
                book_id: plan_support::id_of(&book["id"]),
                input,
            },
            correlation: Uuid::new_v4(),
            refusal: None,
            receipt: None,
            outcome: None,
            reason: None,
        };
        let reservation = script
            .reserve(
                &f.ctx,
                f.ctx.subject_tenant_id(),
                sku,
                ReferenceKind::PriceBookEntry,
                id,
            )
            .await
            .unwrap();
        let op = reference_work::new_op(
            &f.ctx,
            Ref {
                kind: RefKind::Entry,
                id,
                sku_id: sku,
            },
            &work,
            OpKind::Create,
            Some(reservation.reservation_id),
            None,
            time::OffsetDateTime::now_utc() - time::Duration::days(1),
        )
        .unwrap();
        let op_id = op.op_id;
        reference_op_repo::insert(&f.db.conn().unwrap(), &plan_support::scope(&f), op)
            .await
            .unwrap();
        let receipt =
            reference_work::drive(&f.state, &f.ctx, op_id, Arc::new(WallClock), Caller::Ticker)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            receipt.status,
            if versioned { 400 } else { 201 },
            "{}",
            receipt.body
        );
        let entry = price_book_entry_repo::find(
            &f.db.conn().unwrap(),
            &plan_support::scope(&f),
            f.ctx.subject_tenant_id(),
            id,
        )
        .await
        .unwrap();
        if versioned {
            assert!(entry.is_none());
            assert!(receipt.body.contains("MISSING_RATING_POLICY"));
        } else {
            let entry = entry.unwrap();
            assert_eq!(
                (
                    entry.usage_policy_id,
                    entry.usage_policy_version,
                    entry.usage_policy_digest
                ),
                (None, None, None)
            );
            assert_eq!(
                serde_json::from_str::<Value>(&receipt.body).unwrap()["usage_rating_policy"],
                Value::Null
            );
        }
    }
}

#[tokio::test]
async fn rereserve_and_delete_retain_the_original_immutable_policy() {
    use bss_pricing::infra::{
        reference_work::{self, Caller, Target, WallClock, Work},
        storage::repo::{price_book_entry_repo, reference_op_repo, usage_policy_repo},
    };
    use bss_products_sdk::ReferenceRegistryV1;
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    let script = Arc::new(Script::default());
    let f = Fixture::new(script.clone()).await;
    let (book, _) = f.book().await;
    let id = create_entry(
        &f,
        plan_support::id_of(&book["id"]),
        Uuid::new_v4(),
        policy(),
    )
    .await;
    let scope = plan_support::scope(&f);
    let tenant = f.ctx.subject_tenant_id();
    let original = price_book_entry_repo::find(&f.db.conn().unwrap(), &scope, tenant, id)
        .await
        .unwrap()
        .unwrap();
    let reference = Some(bss_pricing::infra::reference_work::UsagePolicyReference {
        policy_id: original.usage_policy_id.unwrap(),
        version: original.usage_policy_version.unwrap(),
        digest: original.usage_policy_digest.clone().unwrap(),
    });
    let now = time::OffsetDateTime::now_utc();
    script
        .release(&f.ctx, tenant, original.reservation_id)
        .await
        .unwrap();
    let op = reference_work::rereserve_op(&f.ctx, &original, now, now).unwrap();
    let Target::PriceBookEntry { input, .. } = Work::read(&op).unwrap().target else {
        panic!("entry op")
    };
    assert_eq!(input.usage_policy_reference, reference);
    let op_id = op.op_id;
    reference_op_repo::insert(&f.db.conn().unwrap(), &scope, op)
        .await
        .unwrap();
    reference_work::drive(&f.state, &f.ctx, op_id, Arc::new(WallClock), Caller::Door)
        .await
        .unwrap();
    let after = price_book_entry_repo::find(&f.db.conn().unwrap(), &scope, tenant, id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(after.reservation_id, original.reservation_id);
    assert_eq!(
        (
            after.usage_policy_id,
            after.usage_policy_version,
            after.usage_policy_digest
        ),
        (
            original.usage_policy_id,
            original.usage_policy_version,
            original.usage_policy_digest.clone()
        )
    );
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
    let ops = plan_support::ops_for(&f, id).await;
    let deleted = ops.iter().find(|o| o.kind == "delete").unwrap();
    let Target::PriceBookEntry { input, .. } = Work::read(deleted).unwrap().target else {
        panic!("entry op")
    };
    assert_eq!(input.usage_policy_reference, reference);
    let remaining = usage_policy_repo::for_entries(&f.db.conn().unwrap(), tenant, &[original])
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&remaining[&id].content).unwrap(),
        stored_rules()
    );
    // No entry references this policy now; its delete is still forbidden by append-only storage.
    let raw = Database::connect(&f.dsn).await.unwrap();
    assert!(
        raw.execute_raw(Statement::from_string(
            DbBackend::Sqlite,
            "DELETE FROM pricing_usage_rating_policy"
        ))
        .await
        .is_err()
    );
}

#[tokio::test]
async fn entry_creation_rejects_a_policy_whose_unit_disagrees_with_the_sku() {
    let f = Fixture::new(Arc::new(Script::default())).await;
    let (book, _) = f.book().await;
    let mut mismatched = policy();
    mismatched["quantity_semantics"]["unit"] = json!("second");
    let answer = f
        .call(
            "POST",
            &format!("/price-books/{}/entries", book["id"].as_str().unwrap()),
            json!({"sku_id":Uuid::new_v4(),"model":"per_unit","usage_rating_policy":mismatched}),
            None,
            Some("mismatch"),
        )
        .await;
    assert_eq!(answer.0, 400, "{answer:?}");
    assert!(answer.1.to_string().contains("METER_POLICY_MISMATCH"));
}

#[tokio::test]
async fn omitted_single_valued_fields_default_on_create_and_match_an_explicit_policy() {
    let f = Fixture::new(Arc::new(Script::default())).await;
    let (book, _) = f.book().await;
    let path = format!("/price-books/{}/entries", book["id"].as_str().unwrap());
    let omitted = f
        .call(
            "POST",
            &path,
            json!({"sku_id":Uuid::new_v4(),"model":"per_unit","usage_rating_policy":policy_without_single_valued_fields()}),
            None,
            Some("omitted"),
        )
        .await;
    assert_eq!(omitted.0, 201, "{omitted:?}");
    let read = f
        .call(
            "GET",
            &format!("/price-book-entries/{}", omitted.1["id"].as_str().unwrap()),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(read.0, 200, "{read:?}");
    assert_eq!(read.1["usage_rating_policy"]["content"], stored_rules());
    let spelled = f
        .call(
            "POST",
            &path,
            json!({"sku_id":Uuid::new_v4(),"model":"per_unit","usage_rating_policy":policy()}),
            None,
            Some("spelled"),
        )
        .await;
    assert_eq!(spelled.0, 201, "{spelled:?}");
    assert_eq!(
        omitted.1["usage_rating_policy"]["digest"],
        spelled.1["usage_rating_policy"]["digest"]
    );
    assert_eq!(
        omitted.1["usage_rating_policy"]["content"],
        spelled.1["usage_rating_policy"]["content"]
    );
}

#[tokio::test]
async fn a_rules_only_create_records_the_sku_revision_and_matches_the_deploy3_digest() {
    let f = Fixture::new(Arc::new(Script::default())).await;
    let (book, _) = f.book().await;
    let path = format!("/price-books/{}/entries", book["id"].as_str().unwrap());
    let rules = f
        .call(
            "POST",
            &path,
            json!({"sku_id":Uuid::new_v4(),"model":"per_unit","usage_rating_policy":{
                "rating_window":{"kind":"calendar_hour","timezone":"UTC"},
                "aggregation_scope":"subscription_line"
            }}),
            None,
            Some("rules"),
        )
        .await;
    assert_eq!(rules.0, 201, "{rules:?}");
    assert_eq!(rules.1["usage_rating_policy"]["content"], stored_rules());
    assert!(
        rules.1["usage_rating_policy"]["content"]
            .get("quantity_semantics")
            .is_none()
    );
    assert_eq!(rules.1["usage_sku_version"], json!(1));
    let deploy3 = f
        .call(
            "POST",
            &path,
            json!({"sku_id":Uuid::new_v4(),"model":"per_unit","usage_rating_policy":policy()}),
            None,
            Some("deploy3"),
        )
        .await;
    assert_eq!(deploy3.0, 201, "{deploy3:?}");
    assert_eq!(
        rules.1["usage_rating_policy"]["digest"],
        deploy3.1["usage_rating_policy"]["digest"]
    );
}

#[tokio::test]
async fn a_raw_meter_sku_is_an_unconfigured_dependency() {
    use bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1;
    let script = Arc::new(Script::default());
    *script.usage_type_ref.lock().unwrap() =
        Some("gts.cf.core.uc.usage_record.v1~cf.test.usage.cpu".into());
    let f = Fixture::new(script).await;
    f.state.hub.remove::<dyn UsageMeterSemanticsV1>();
    f.state
        .hub
        .register::<dyn UsageMeterSemanticsV1>(Arc::new(RawMeter));
    let (book, _) = f.book().await;
    let answer = f
        .call(
            "POST",
            &format!("/price-books/{}/entries", book["id"].as_str().unwrap()),
            json!({"sku_id":Uuid::new_v4(),"model":"per_unit","usage_rating_policy":{
                "rating_window":{"kind":"calendar_hour","timezone":"UTC"},
                "aggregation_scope":"subscription_line"
            }}),
            None,
            Some("raw"),
        )
        .await;
    assert_eq!(answer.0, 400, "{answer:?}");
    assert!(
        answer.1.to_string().contains("UNCONFIGURED_DEPENDENCY"),
        "{answer:?}"
    );
}

struct RawMeter;

#[async_trait::async_trait]
impl bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1 for RawMeter {
    async fn resolve(
        &self,
        _ctx: &toolkit_security::SecurityContext,
        _meter: bss_pricing_sdk::terms::MeterRef,
    ) -> Result<
        bss_pricing_sdk::meter_semantics::MeterSemantics,
        toolkit_canonical_errors::CanonicalError,
    > {
        Err(toolkit_canonical_errors::CanonicalError::from(
            bss_pricing_sdk::meter_semantics::UnconfiguredMeterSemantics,
        ))
    }
}

#[test]
fn the_served_request_deprecates_quantity_semantics_and_content_omits_it() {
    use utoipa::PartialSchema;
    let request = serde_json::to_value(
        bss_pricing::infra::usage_policy_wire::UsageRatingPolicyRequest::schema(),
    )
    .unwrap();
    let content = serde_json::to_value(
        bss_pricing::infra::usage_policy_wire::UsageRatingPolicyInput::schema(),
    )
    .unwrap();
    let quantity = serde_json::to_value(
        bss_pricing::infra::usage_policy_wire::QuantitySemanticsRequest::schema(),
    )
    .unwrap();
    assert_eq!(quantity["deprecated"], true, "{request}");
    assert!(
        content["properties"].get("quantity_semantics").is_none(),
        "{content}"
    );
    assert!(content["properties"].get("fold").is_some(), "{content}");
}

#[test]
fn omitted_and_explicit_single_valued_fields_share_one_digest() {
    use bss_pricing::infra::usage_policy_wire::{
        UsageRatingPolicyInput, UsageRatingPolicyRequest, digest_text,
    };
    let accrual = "derived-v1:7354bbb184408c5965a4f84c539c1d38a5d4c8470f7341996bcf8e3f5b3b190b";
    // D-514: the pin is the rules-only digest. The former quantity_semantics pin was
    // 8d7119c7e77689f12980cc88b5f14051d54cff9a4234e9cd9106037079ca02a5.
    let pin = "0cab8c6e9792e0758c140b193c08716e5862544c792ea6880cce8c9727d5f3e4";
    let quantity = |fold: Option<&str>| {
        let mut semantics = json!({
            "meter":{"usage_type_id":"products.derived/vm-hour@1","version":"1"},
            "unit":"VM\u{b7}hour",
            "accrual_policy_version":accrual
        });
        if let Some(fold) = fold {
            semantics["fold"] = json!(fold);
        }
        semantics
    };
    let body = |reset: Option<&str>, partial: Option<&str>, fold: Option<&str>| {
        let mut policy = json!({
            "rating_window":{"kind":"billing_cycle"},
            "aggregation_scope":"subscription_line",
            "quantity_semantics":quantity(fold)
        });
        if let Some(reset) = reset {
            policy["reset"] = json!(reset);
        }
        if let Some(partial) = partial {
            policy["partial_window"] = json!(partial);
        }
        policy
    };
    let digest_of = |value: Value| {
        let request: UsageRatingPolicyRequest = serde_json::from_value(value).unwrap();
        let input = UsageRatingPolicyInput::from(request);
        digest_text(bss_pricing_sdk::digest::policy_digest(&(&input).into()))
    };
    assert!(serde_json::from_value::<UsageRatingPolicyInput>(body(None, None, None)).is_err());
    let spelled = digest_of(body(
        Some("rating_window_start"),
        Some("actual_quantity_full_thresholds"),
        Some("SUM"),
    ));
    let omitted = digest_of(body(None, None, None));
    let mut blanks = body(
        Some("rating_window_start"),
        Some("actual_quantity_full_thresholds"),
        Some("SUM"),
    );
    blanks["reset"] = Value::Null;
    blanks["partial_window"] = Value::Null;
    blanks["quantity_semantics"]["fold"] = Value::Null;
    assert!(serde_json::from_value::<UsageRatingPolicyInput>(blanks.clone()).is_err());
    assert_eq!(spelled, omitted);
    assert_eq!(spelled, digest_of(blanks));
    assert_eq!(spelled, pin);
}

#[tokio::test]
async fn an_explicit_unknown_fold_is_still_refused() {
    let f = Fixture::new(Arc::new(Script::default())).await;
    let (book, _) = f.book().await;
    let mut wrong = policy();
    wrong["quantity_semantics"]["fold"] = json!("MAX");
    let answer = f
        .call(
            "POST",
            &format!("/price-books/{}/entries", book["id"].as_str().unwrap()),
            json!({"sku_id":Uuid::new_v4(),"model":"per_unit","usage_rating_policy":wrong}),
            None,
            Some("max"),
        )
        .await;
    assert_eq!(answer.0, 400, "{answer:?}");
    let text = answer.1.to_string();
    assert!(text.contains("MAX"), "{text}");
    assert!(text.contains("unknown variant"), "{text}");
}

#[tokio::test]
async fn a_recurring_entry_still_refuses_a_policy_when_single_valued_fields_are_omitted() {
    let script = Arc::new(Script::default());
    script.set(11);
    let f = Fixture::new(script).await;
    let (book, _) = f.book().await;
    let answer = f
        .call(
            "POST",
            &format!("/price-books/{}/entries", book["id"].as_str().unwrap()),
            json!({
                "sku_id":Uuid::new_v4(),
                "model":"flat",
                "period":"month",
                "usage_rating_policy":policy_without_single_valued_fields()
            }),
            None,
            Some("recurring"),
        )
        .await;
    assert_eq!(answer.0, 400, "{answer:?}");
    assert!(
        answer.1.to_string().contains("UNEXPECTED_RATING_POLICY"),
        "{answer:?}"
    );
}

#[tokio::test]
async fn an_omitted_policy_with_the_wrong_unit_is_still_a_meter_mismatch() {
    let f = Fixture::new(Arc::new(Script::default())).await;
    let (book, _) = f.book().await;
    let mut mismatched = policy_without_single_valued_fields();
    mismatched["quantity_semantics"]["unit"] = json!("second");
    let answer = f
        .call(
            "POST",
            &format!("/price-books/{}/entries", book["id"].as_str().unwrap()),
            json!({"sku_id":Uuid::new_v4(),"model":"per_unit","usage_rating_policy":mismatched}),
            None,
            Some("omitted-unit"),
        )
        .await;
    assert_eq!(answer.0, 400, "{answer:?}");
    assert!(
        answer.1.to_string().contains("METER_POLICY_MISMATCH"),
        "{answer:?}"
    );
}

#[test]
fn complete_meter_evidence_must_match_every_immutable_field_and_sku_unit() {
    use bss_pricing::domain::usage_policy::validate_meter_policy;
    use bss_pricing_sdk::{meter_semantics::MeterSemantics, terms::Fold};
    let policy = seam_support::vm_hour_policy();
    let evidence = MeterSemantics {
        meter: bss_pricing_sdk::terms::MeterRef {
            usage_type_id: "vm-hours".into(),
            version: "v1".into(),
        },
        canonical_unit: "VM\u{b7}hour".into(),
        fold: Fold::Sum,
        accrual_policy_version: "integrated-v1".into(),
        source_integrated: true,
        digest: [7; 32],
    };
    assert!(validate_meter_policy(&policy.content, "vm-hours", "VM\u{b7}hour", &evidence).is_ok());
    assert_eq!(
        validate_meter_policy(&policy.content, "vm-hours", "second", &evidence)
            .unwrap_err()
            .code,
        "METER_POLICY_MISMATCH"
    );
    for field in ["unit", "version", "identity", "integration"] {
        let mut wrong = evidence.clone();
        match field {
            "unit" => wrong.canonical_unit = "second".into(),
            "version" => wrong.meter.version = "v2".into(),
            "identity" => wrong.meter.usage_type_id = "other".into(),
            _ => wrong.source_integrated = false,
        }
        assert_eq!(
            validate_meter_policy(&policy.content, "vm-hours", "VM\u{b7}hour", &wrong)
                .unwrap_err()
                .code,
            "METER_POLICY_MISMATCH",
            "{field}"
        );
    }
    let mut accrual = evidence;
    accrual.accrual_policy_version = "raw-v1".into();
    assert!(
        validate_meter_policy(&policy.content, "vm-hours", "VM\u{b7}hour", &accrual).is_ok(),
        "accrual is captured evidence, not a rating rule"
    );
    // SUM is the only representable SDK fold; wire input cannot smuggle another declaration.
    let mut wrong_fold = serde_json::to_value(
        bss_pricing::infra::usage_policy_wire::UsageRatingPolicyInput::from(&policy.content),
    )
    .unwrap();
    wrong_fold["quantity_semantics"]["fold"] = json!("MAX");
    assert!(
        serde_json::from_value::<bss_pricing::infra::usage_policy_wire::UsageRatingPolicyInput>(
            wrong_fold
        )
        .is_err()
    );
}

#[tokio::test]
async fn meter_provider_failure_classes_remain_distinct_and_resolve_as_the_caller() {
    use bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1;
    use plan_support::entry_support::policy_support::MeterProvider;
    use std::sync::atomic::Ordering;
    for mode in [0, 1, 2] {
        let f = Fixture::new(Arc::new(Script::default())).await;
        let (book, _) = f.book().await;
        let provider = Arc::new(MeterProvider::default());
        provider.failure.store(mode, Ordering::SeqCst);
        if mode == 0 {
            f.state.hub.remove::<dyn UsageMeterSemanticsV1>();
        } else {
            f.state
                .hub
                .register::<dyn UsageMeterSemanticsV1>(provider.clone());
        }
        let answer = f
            .call(
                "POST",
                &format!("/price-books/{}/entries", book["id"].as_str().unwrap()),
                json!({"sku_id":Uuid::new_v4(),"model":"per_unit","usage_rating_policy":policy()}),
                None,
                Some("failure"),
            )
            .await;
        assert_eq!(answer.0, [400, 503, 403][usize::from(mode)], "{answer:?}");
        if mode == 0 {
            assert!(answer.1.to_string().contains("UNCONFIGURED_DEPENDENCY"));
        }
        if mode == 2 {
            assert!(answer.1.to_string().contains("METER_DENIED"));
        }
        assert!(!answer.1.to_string().contains("MISSING_RATING_POLICY"));
        if mode != 0 {
            let callers = provider.callers.lock().unwrap().clone();
            assert_eq!(callers, vec![f.ctx.subject_id()]);
        }
    }
}

async fn quorum(f: &Fixture, kind: &str, count: u32) {
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let r = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"kind":kind,"quorum":count}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(r.0, 200, "{r:?}");
}
async fn draft_price(
    f: &Fixture,
    entry: Uuid,
    from: &str,
    fee: Option<&str>,
) -> (u16, Value, String) {
    f.call(
        "POST",
        &format!("/price-book-entries/{entry}/prices"),
        json!({"price":{"rate":"1"},"min_fee":fee,"eligibility":"all","effective_from":from}),
        None,
        Some(&Uuid::new_v4().to_string()),
    )
    .await
}
async fn submit_price(f: &Fixture, id: Uuid) -> (u16, Value, String) {
    f.call(
        "POST",
        &format!("/prices/{id}/submit"),
        json!({}),
        None,
        Some(&Uuid::new_v4().to_string()),
    )
    .await
}

#[tokio::test]
async fn hourly_minimum_fee_is_rejected_at_create_submit_and_apply() {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    let f = Fixture::new(Arc::new(Script::default())).await;
    let (book, _) = f.book().await;
    let entry = create_entry(
        &f,
        plan_support::id_of(&book["id"]),
        Uuid::new_v4(),
        policy(),
    )
    .await;
    let today = time::OffsetDateTime::now_utc().date().to_string();
    let rejected = draft_price(&f, entry, &today, Some("1")).await;
    assert_eq!(rejected.0, 400, "{rejected:?}");
    assert!(rejected.1.to_string().contains("UNSUPPORTED_TERMS"));
    let draft = draft_price(&f, entry, &today, None).await;
    assert_eq!(draft.0, 201, "{draft:?}");
    let id = plan_support::id_of(&draft.1["items"][0]["id"]);
    let raw = Database::connect(&f.dsn).await.unwrap();
    raw.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "UPDATE pricing_price SET min_fee = '1' WHERE id = ?",
        [id.into()],
    ))
    .await
    .unwrap();
    let rejected = submit_price(&f, id).await;
    assert_eq!(rejected.0, 400, "{rejected:?}");
    assert!(rejected.1.to_string().contains("UNSUPPORTED_TERMS"));
    raw.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "UPDATE pricing_price SET min_fee = NULL WHERE id = ?",
        [id.into()],
    ))
    .await
    .unwrap();
    let pending = submit_price(&f, id).await;
    assert_eq!(pending.0, 201, "{pending:?}");
    raw.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "UPDATE pricing_price SET min_fee = '1' WHERE id = ?",
        [id.into()],
    ))
    .await
    .unwrap();
    let reviewer = plan_support::entry_support::user_of(f.ctx.subject_tenant_id());
    let path = format!(
        "/approval-units/{}/approve",
        pending.1["unit"]["id"].as_str().unwrap()
    );
    let stale = f
        .call_as(
            &reviewer,
            "POST",
            &path,
            json!({"generation":1}),
            None,
            Some("stale-fee"),
        )
        .await;
    assert_eq!(stale.0, 400, "{stale:?}");
    let refused = f
        .call_as(
            &reviewer,
            "POST",
            &path,
            json!({"generation":2}),
            None,
            Some("apply-fee"),
        )
        .await;
    assert_eq!(refused.0, 409, "{refused:?}");
    assert!(refused.1.to_string().contains("UNSUPPORTED_TERMS"));
}

#[tokio::test]
async fn mixed_windows_survive_publication_successors_pairs_and_offline_history() {
    use bss_pricing::{api::pricing_read::PricingReadProvider, infra::storage::repo::price_repo};
    use bss_pricing_sdk::{
        meter_semantics::UsageMeterSemanticsV1,
        read::{CatalogRef, PricingReadV1, ResolveQuery},
        terms::RatingWindow,
    };
    use bss_products_sdk::models::{BillingTiming, SkuType};
    use plan_support::entry_support::policy_support::MeterProvider;
    use std::sync::atomic::Ordering;
    let (f, catalog) = plan_support::setup().await;
    quorum(&f, "prices", 0).await;
    quorum(&f, "plan_revision", 0).await;
    let book = plan_support::book(&f, "MIXED").await;
    let (_, revision) = plan_support::plan(&f, "MIXED", book).await;
    let today = time::OffsetDateTime::now_utc().date();
    let mut entries = Vec::new();
    let mut policies = Vec::new();
    for (meter, unit, window) in [
        ("vm-hours", "VM\u{b7}hour", json!({"kind":"billing_cycle"})),
        (
            "cloudlet-hours",
            "cloudlet\u{b7}hour",
            json!({"kind":"calendar_hour","timezone":"UTC"}),
        ),
    ] {
        let sku = catalog.sku(SkuType::Usage);
        {
            let mut skus = catalog.skus.lock().unwrap();
            let s = skus.get_mut(&sku).unwrap();
            s.meter = Some(meter.into());
            s.unit = Some(unit.into());
        }
        let mut version = catalog.content(sku);
        version.gl_code = Some("usage".into());
        version.tax_category = Some("standard".into());
        version.invoice_line_template = Some("{sku}".into());
        version.billing_timing = Some(BillingTiming::Arrears);
        catalog.version(sku, 1, "2020-01-01", version);
        let mut input = policy();
        input["rating_window"] = window;
        input["quantity_semantics"]["meter"]["usage_type_id"] = json!(meter);
        input["quantity_semantics"]["unit"] = json!(unit);
        let entry = create_entry(&f, book, sku, input).await;
        let draft = draft_price(&f, entry, &today.to_string(), None).await;
        assert_eq!(draft.0, 201, "{draft:?}");
        let submitted = submit_price(&f, plan_support::id_of(&draft.1["items"][0]["id"])).await;
        assert_eq!(submitted.0, 201, "{submitted:?}");
        assert_eq!(submitted.1["applied"], true);
        plan_support::item(&f, revision, sku, Some(entry), "paid").await;
        let read = f
            .call(
                "GET",
                &format!("/price-book-entries/{entry}"),
                json!({}),
                None,
                None,
            )
            .await;
        entries.push(entry);
        policies.push(read.1["usage_rating_policy"].clone());
    }
    let published = f
        .call(
            "POST",
            &format!("/plan-revisions/{revision}/submit"),
            json!({}),
            None,
            Some("publish-mixed"),
        )
        .await;
    assert_eq!(published.0, 201, "{published:?}");
    assert_eq!(published.1["revision"]["state"], "published");
    let cloud = entries[1];
    let start = (today + time::Duration::days(1)).to_string();
    let until = (today + time::Duration::days(2)).to_string();
    for field in [
        "rating_window",
        "aggregation_scope",
        "reset",
        "usage_rating_policy",
    ] {
        let mut body = json!({"price":{"rate":"2"},"effective_from":start,"eligibility":"all"});
        body[field] = json!("override");
        let refused = f
            .call(
                "POST",
                &format!("/price-book-entries/{cloud}/prices"),
                body,
                None,
                Some(field),
            )
            .await;
        assert_eq!(refused.0, 400, "{field}: {refused:?}");
    }
    let pair = f.call("POST", &format!("/price-book-entries/{cloud}/prices"),
        json!({"price":{"rate":"2"},"effective_from":start,"temporary_until":until,"eligibility":"all"}), None, Some("pair")).await;
    assert_eq!(pair.0, 201, "{pair:?}");
    assert_eq!(pair.1["items"].as_array().unwrap().len(), 2);
    let applied = f
        .call(
            "POST",
            &format!("/price-books/{book}/publish-changes"),
            json!({}),
            None,
            Some("pair-submit"),
        )
        .await;
    assert_eq!(applied.0, 201, "{applied:?}");
    let successor = draft_price(
        &f,
        cloud,
        &(today + time::Duration::days(3)).to_string(),
        None,
    )
    .await;
    assert_eq!(successor.0, 201, "{successor:?}");
    assert_eq!(
        submit_price(&f, plan_support::id_of(&successor.1["items"][0]["id"]))
            .await
            .0,
        201
    );
    let prices = price_repo::for_entry(
        &f.db.conn().unwrap(),
        &plan_support::scope(&f),
        f.ctx.subject_tenant_id(),
        cloud,
    )
    .await
    .unwrap();
    assert!(prices.len() >= 2, "the pair is two prices: {prices:?}");
    for p in &prices {
        assert_eq!(p.price_book_entry_id, cloud);
        assert!(p.version_no >= 1, "{}", p.version_no);
    }
    let offline = Arc::new(MeterProvider::default());
    offline.failure.store(1, Ordering::SeqCst);
    f.state
        .hub
        .register::<dyn UsageMeterSemanticsV1>(offline.clone());
    let provider = PricingReadProvider::new(
        f.state.clone(),
        Arc::new(plan_support::entry_support::enforcer_for(
            f.ctx.subject_tenant_id(),
        )),
    );
    for date in [today, today + time::Duration::days(4)] {
        let rest =
            seam_support::resolve(&f, &format!("plan_revision_id={revision}&date={date}")).await;
        assert_eq!(rest.0, 200, "{rest:?}");
        let sdk = provider
            .resolve(
                &f.ctx,
                ResolveQuery {
                    catalog: CatalogRef {
                        tenant_id: f.ctx.subject_tenant_id(),
                    },
                    revision_id: revision,
                    date,
                    item_id: None,
                    pins: vec![],
                },
            )
            .await
            .unwrap();
        assert_eq!(sdk.cells.len(), 2);
        for (index, entry) in entries.iter().enumerate() {
            let binding = sdk
                .cells
                .iter()
                .filter_map(|c| c.binding.as_ref())
                .find(|b| b.price_book_entry_id == *entry)
                .unwrap();
            assert_eq!(binding.price.price_book_entry_id, *entry);
            let stored = binding.usage_rating_policy.as_ref().unwrap();
            assert_eq!(stored.policy_id.to_string(), policies[index]["policy_id"]);
            assert_eq!(
                matches!(stored.content.rating_window, RatingWindow::BillingCycle),
                index == 0
            );
            let item = rest.1["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["price_book_entry_id"] == entry.to_string())
                .unwrap();
            assert_eq!(item["usage_rating_policy"], policies[index]);
        }
    }
    assert_eq!(
        offline.calls.load(Ordering::SeqCst),
        0,
        "historical reads never resolve meter semantics"
    );
}

#[tokio::test]
async fn price_and_plan_submit_and_apply_preserve_meter_refusals_outside_transactions() {
    use bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1;
    use plan_support::entry_support::policy_support::MeterProvider;
    use std::sync::atomic::Ordering;
    // Mode 5 changes only the provider's accrual text. That text is captured evidence, not a rating rule.
    for mode in [0, 1, 2, 3, 4, 6, 7] {
        let f = Fixture::new(Arc::new(Script::default())).await;
        let meter = Arc::new(MeterProvider {
            probe_db: Some(f.db.clone()),
            ..Default::default()
        });
        let install = |broken: bool| {
            meter
                .failure
                .store(if broken { mode } else { 0 }, Ordering::SeqCst);
            if broken && mode == 0 {
                f.state.hub.remove::<dyn UsageMeterSemanticsV1>();
            } else {
                f.state
                    .hub
                    .register::<dyn UsageMeterSemanticsV1>(meter.clone());
            }
        };
        install(false);
        let (book, _) = f.book().await;
        let book = plan_support::id_of(&book["id"]);
        let sku = Uuid::new_v4();
        let entry = create_entry(&f, book, sku, policy()).await;
        let draft = draft_price(
            &f,
            entry,
            &time::OffsetDateTime::now_utc().date().to_string(),
            None,
        )
        .await;
        assert_eq!(draft.0, 201, "{draft:?}");
        let price = plan_support::id_of(&draft.1["items"][0]["id"]);
        let expected = match mode {
            1 => 503,
            2 => 403,
            _ => 400,
        };
        let assert_error = |answer: &(u16, Value, String)| {
            assert_eq!(answer.0, expected, "mode {mode}: {answer:?}");
            let text = answer.1.to_string();
            assert!(!text.contains("MISSING_RATING_POLICY"));
            if mode == 0 {
                assert!(text.contains("UNCONFIGURED_DEPENDENCY"));
            }
            if mode >= 3 {
                assert!(text.contains("METER_POLICY_MISMATCH"));
            }
        };
        install(true);
        assert_error(&submit_price(&f, price).await);
        install(false);
        let submitted = submit_price(&f, price).await;
        assert_eq!(submitted.0, 201, "{submitted:?}");
        let reviewer = plan_support::entry_support::user_of(f.ctx.subject_tenant_id());
        let path = format!(
            "/approval-units/{}/approve",
            submitted.1["unit"]["id"].as_str().unwrap()
        );
        install(true);
        assert_error(
            &f.call_as(
                &reviewer,
                "POST",
                &path,
                json!({"generation":1}),
                None,
                Some("price-refused"),
            )
            .await,
        );
        install(false);
        let applied = f
            .call_as(
                &reviewer,
                "POST",
                &path,
                json!({"generation":1}),
                None,
                Some("price-applied"),
            )
            .await;
        assert_eq!(applied.0, 200, "{applied:?}");
        let (_, revision) = plan_support::plan(&f, "SEMANTICS", book).await;
        plan_support::item(&f, revision, sku, Some(entry), "paid").await;
        let path = format!("/plan-revisions/{revision}/submit");
        install(true);
        assert_error(
            &f.call("POST", &path, json!({}), None, Some("plan-refused"))
                .await,
        );
        install(false);
        let submitted = f
            .call("POST", &path, json!({}), None, Some("plan-submitted"))
            .await;
        assert_eq!(submitted.0, 201, "{submitted:?}");
        let path = format!(
            "/approval-units/{}/approve",
            submitted.1["unit"]["id"].as_str().unwrap()
        );
        install(true);
        assert_error(
            &f.call_as(
                &reviewer,
                "POST",
                &path,
                json!({"generation":1}),
                None,
                Some("plan-apply-refused"),
            )
            .await,
        );
        install(false);
        assert_eq!(
            f.call_as(
                &reviewer,
                "POST",
                &path,
                json!({"generation":1}),
                None,
                Some("plan-applied")
            )
            .await
            .0,
            200
        );
        install(true);
        assert_eq!(
            f.call_as(
                &reviewer,
                "POST",
                &path,
                json!({"generation":1}),
                None,
                Some("plan-applied")
            )
            .await
            .0,
            200,
            "replay precedes dependencies"
        );
    }
}

#[tokio::test]
async fn a_new_usage_price_approval_requires_a_policy_bearing_entry() {
    let (f, catalog) = plan_support::setup().await;
    let book = plan_support::book(&f, "LEGACY_PRICE").await;
    let sku = catalog.sku(bss_products_sdk::models::SkuType::Usage);
    let entry = plan_support::entry(&f, book, sku, "usage", None).await;
    let draft = draft_price(
        &f,
        entry,
        &time::OffsetDateTime::now_utc().date().to_string(),
        None,
    )
    .await;
    assert_eq!(draft.0, 201, "{draft:?}");
    let refusal = submit_price(&f, plan_support::id_of(&draft.1["items"][0]["id"])).await;
    assert_eq!(refusal.0, 400, "{refusal:?}");
    assert!(refusal.1.to_string().contains("MISSING_RATING_POLICY"));
}

#[test]
fn captured_evidence_digest_uses_strict_lowercase_hex_at_storage_boundaries() {
    use bss_pricing::infra::usage_policy_wire::MeterEvidence;
    let evidence = bss_pricing_sdk::meter_semantics::MeterSemantics {
        meter: bss_pricing_sdk::terms::MeterRef {
            usage_type_id: "vm-hours".into(),
            version: "v1".into(),
        },
        canonical_unit: "VM\u{b7}hour".into(),
        fold: bss_pricing_sdk::terms::Fold::Sum,
        accrual_policy_version: "integrated-v1".into(),
        source_integrated: true,
        digest: [0xab; 32],
    };
    let stored: MeterEvidence = evidence.clone().into();
    let encoded = serde_json::to_value(&stored).unwrap();
    assert_eq!(encoded["digest"], "ab".repeat(32));
    let decoded: MeterEvidence = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(
        bss_pricing_sdk::meter_semantics::MeterSemantics::from(&decoded),
        evidence
    );
    let mut invalid = encoded;
    invalid["digest"] = json!("AB".repeat(32));
    assert!(serde_json::from_value::<MeterEvidence>(invalid).is_err());
}

/// Change only provider evidence, or the local entry generation, between detached reads.
struct MovingMeter {
    calls: std::sync::atomic::AtomicUsize,
    local: Option<(String, Uuid)>,
}
#[async_trait::async_trait]
impl bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1 for MovingMeter {
    async fn resolve(
        &self,
        ctx: &toolkit_security::SecurityContext,
        meter: bss_pricing_sdk::terms::MeterRef,
    ) -> Result<
        bss_pricing_sdk::meter_semantics::MeterSemantics,
        toolkit_canonical_errors::CanonicalError,
    > {
        use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut evidence = plan_support::entry_support::policy_support::MeterProvider::default()
            .resolve(ctx, meter)
            .await?;
        if let Some((dsn, entry)) = &self.local {
            let conn = Database::connect(dsn).await.unwrap();
            conn.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "UPDATE pricing_price_book_entry SET version = version + 1 WHERE id = ?",
                [(*entry).into()],
            ))
            .await
            .unwrap();
        } else if call > 0 {
            // One real evidence change, then stable: an erroneous retry would accept it.
            evidence.digest = [8; 32];
        }
        Ok(evidence)
    }
}

async fn moving_meter_fixture(local: bool) -> (Fixture, Uuid, Arc<MovingMeter>) {
    let f = Fixture::new(Arc::new(Script::default())).await;
    let (book, _) = f.book().await;
    let entry = create_entry(
        &f,
        plan_support::id_of(&book["id"]),
        Uuid::new_v4(),
        policy(),
    )
    .await;
    let draft = draft_price(
        &f,
        entry,
        &time::OffsetDateTime::now_utc().date().to_string(),
        None,
    )
    .await;
    assert_eq!(draft.0, 201, "{draft:?}");
    let id = plan_support::id_of(&draft.1["items"][0]["id"]);
    let provider = Arc::new(MovingMeter {
        calls: std::sync::atomic::AtomicUsize::new(0),
        local: local.then(|| (f.dsn.to_string(), entry)),
    });
    f.state
        .hub
        .register::<dyn bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1>(provider.clone());
    (f, id, provider)
}

#[tokio::test]
async fn unchanged_selection_with_changed_provider_evidence_is_not_retried() {
    let (f, id, provider) = moving_meter_fixture(false).await;
    let result = submit_price(&f, id).await;
    assert_eq!(result.0, 201, "{result:?}");
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let price = bss_pricing::infra::storage::repo::price_repo::find(
        &f.db.conn().unwrap(),
        &plan_support::scope(&f),
        f.ctx.subject_tenant_id(),
        id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(price.state, "pending");
}

#[tokio::test]
async fn local_identity_moving_past_the_capture_limit_is_unit_contended() {
    let (f, id, provider) = moving_meter_fixture(true).await;
    let result = submit_price(&f, id).await;
    assert_eq!(result.0, 409, "{result:?}");
    assert!(
        result.1.to_string().contains("UNIT_CONTENDED"),
        "{result:?}"
    );
    assert_eq!(
        provider.calls.load(std::sync::atomic::Ordering::SeqCst),
        toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS as usize
    );
    let price = bss_pricing::infra::storage::repo::price_repo::find(
        &f.db.conn().unwrap(),
        &plan_support::scope(&f),
        f.ctx.subject_tenant_id(),
        id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(price.state, "draft");
    assert!(price.pending_unit_id.is_none());
}
