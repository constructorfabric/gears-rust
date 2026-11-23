//! A revision's quota rules through their endpoints (D-540 to D-545): the read of the stored set
//! and of the revision with its rules, the replacement of the whole set under the revision's
//! `ETag` and an idempotency key, the checks with their quota rows, and the item guard.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]
mod plan_support;
mod quota_support;
use bss_pricing::infra::storage::repo::plan_quota_repo::empty_digest;
use quota_support::{
    draft_with_items, install, path, priced_draft_with_items, put, ram, rule_json, vcpu,
};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn get_answers_the_empty_set_with_the_revision_etag() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let (status, body, etag) = f.call("GET", &path(&d), json!(null), None, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["revision_id"], d.revision.to_string());
    assert_eq!(body["quota_rules"], json!([]));
    assert_eq!(body["quota_rules_digest"], empty_digest());
    assert_eq!(etag, d.etag, "the revision's own ETag");
    assert_eq!(
        format!("\"{}\"", body["revision_version"]),
        etag,
        "the version the ETag carries"
    );
    let (status, read, _) = f
        .call(
            "GET",
            &format!("/plan-revisions/{}", d.revision),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(status, 200, "{read}");
    assert_eq!(read["quota_rules"], json!([]));
    assert_eq!(read["quota_rules_digest"], empty_digest());
    assert!(
        fake.requests().is_empty(),
        "a stored read asks Products nothing"
    );
}

#[tokio::test]
async fn the_revision_read_carries_the_rules_and_digest() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (s, put_body, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200, "{put_body}");
    let (_, read, _) = f
        .call(
            "GET",
            &format!("/plan-revisions/{}", d.revision),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(read["quota_rules"], put_body["quota_rules"]);
    assert_eq!(read["quota_rules_digest"], put_body["quota_rules_digest"]);
    let (_, rules, _) = f.call("GET", &path(&d), json!(null), None, None).await;
    assert_eq!(
        rules, put_body,
        "the stored set reads as the PUT answered it"
    );
    assert_eq!(fake.requests().len(), 1, "no read asks Products");
}

#[tokio::test]
async fn another_tenants_revision_is_404_for_quota_rules() {
    let (f, catalog) = plan_support::setup().await;
    install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let (status, body, _) = f
        .call_as(
            &plan_support::stranger(),
            "GET",
            &path(&d),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(status, 404, "{body}");
    let (status, body, _) = f
        .call(
            "GET",
            &format!("/plan-revisions/{}/quota-rules", Uuid::now_v7()),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(status, 404, "{body}");
}

/// Another tenant's caller cannot replace a revision's rules: the PUT is refused as the gear
/// refuses a stranger's other plan writes (403), asks Products nothing and writes nothing.
#[tokio::test]
async fn another_tenants_caller_cannot_replace_quota_rules() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let before = stored(&f, &d).await;
    let (status, body, _) = f
        .call_as(
            &plan_support::stranger(),
            "PUT",
            &path(&d),
            json!({"quota_rules": [rule_json(1, &cpu, d.items[0], "10")]}),
            Some(&d.etag),
            Some("k1"),
        )
        .await;
    assert_eq!(status, 403, "a stranger may not write here: {body}");
    assert!(fake.requests().is_empty(), "Products is not asked");
    assert_eq!(stored(&f, &d).await, before, "nothing written");
    assert_eq!(before.0["quota_rules"], json!([]));
    assert_eq!(before.1, d.etag);
}

#[tokio::test]
async fn a_denied_caller_is_403_before_the_path_is_read() {
    let (f, _) = plan_support::setup().await;
    let unknown = format!("/plan-revisions/{}/quota-rules", Uuid::now_v7());
    let (status, body, _) =
        plan_support::request(&f.denied, &f.ctx, "GET", &unknown, json!(null), None, None).await;
    assert_eq!(status, 403, "{body}");
    let (status, body, _) = f
        .call_as(
            &plan_support::holding(&f, "price:read"),
            "GET",
            &unknown,
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(status, 403, "plan read is the grant: {body}");
}

/// The stored set and the revision's `ETag`, through the read.
async fn stored(
    f: &plan_support::Fixture,
    d: &quota_support::Draft,
) -> (serde_json::Value, String) {
    let (status, body, etag) = f.call("GET", &path(d), json!(null), None, None).await;
    assert_eq!(status, 200, "{body}");
    (body, etag)
}

/// The `plan_revision.quota_rules` audit rows of `revision`, in order: the version each names,
/// its actor, and its correlation id.
async fn quota_audit(
    f: &plan_support::Fixture,
    revision: Uuid,
) -> Vec<(i64, Uuid, Option<String>)> {
    Database::connect(&f.dsn)
        .await
        .unwrap()
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT subject_revision, actor_ref, correlation_id FROM pricing_audit \
             WHERE subject_id = ? AND action = 'plan_revision.quota_rules' \
             ORDER BY subject_revision",
            [revision.into()],
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.try_get::<i64>("", "subject_revision").unwrap(),
                r.try_get::<Uuid>("", "actor_ref").unwrap(),
                r.try_get::<Option<String>>("", "correlation_id").unwrap(),
            )
        })
        .collect()
}

fn rule_ids(body: &serde_json::Value) -> Vec<String> {
    body["quota_rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rule_id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn put_replaces_the_complete_set_and_answers_the_new_etag() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let (cpu, ram) = (fake.add(vcpu(d.sku)), fake.add(ram(d.sku)));
    let (s, first, etag1) = put(
        &f,
        &d,
        &[
            rule_json(2, &ram, d.items[0], "20"),
            rule_json(1, &cpu, d.items[0], "10"),
        ],
        "k1",
    )
    .await;
    assert_eq!(s, 200, "{first}");
    assert_ne!(etag1, d.etag);
    assert_eq!(format!("\"{}\"", first["revision_version"]), etag1);
    assert_eq!(
        rule_ids(&first),
        [
            Uuid::from_u128(1).to_string(),
            Uuid::from_u128(2).to_string()
        ],
        "the saved set is ordered by rule_id"
    );
    assert_eq!(first["quota_rules"][0]["availability"]["state"], "enabled");
    assert_eq!(first["quota_rules"][0]["basis"], "per_plan_unit");
    assert_eq!(
        first["quota_rules"][0]["subject_selector"],
        "resource_owner"
    );
    // Edit rule 1, remove rule 2, add rule 3.
    let d2 = quota_support::Draft {
        etag: etag1.clone(),
        ..d.clone()
    };
    let (s, second, etag2) = put(
        &f,
        &d2,
        &[
            rule_json(1, &cpu, d.items[0], "12"),
            rule_json(3, &ram, d.items[1], "40"),
        ],
        "k2",
    )
    .await;
    assert_eq!(s, 200, "{second}");
    assert_eq!(
        rule_ids(&second),
        [
            Uuid::from_u128(1).to_string(),
            Uuid::from_u128(3).to_string()
        ]
    );
    assert_eq!(second["quota_rules"][0]["amount"], "12");
    assert_ne!(second["quota_rules_digest"], first["quota_rules_digest"]);
    assert_ne!(etag2, etag1);
    let audit = quota_audit(&f, d.revision).await;
    assert_eq!(
        audit
            .iter()
            .map(|(version, actor, _)| (*version, *actor))
            .collect::<Vec<_>>(),
        [
            (
                first["revision_version"].as_i64().unwrap(),
                f.ctx.subject_id()
            ),
            (
                second["revision_version"].as_i64().unwrap(),
                f.ctx.subject_id()
            ),
        ],
        "one audit row per saved set, at its new version, by its author"
    );
    assert!(
        audit
            .iter()
            .all(|(_, _, correlation)| correlation.is_some())
    );
    // A replay and a refusal write no audit row.
    let (s, _, _) = put(
        &f,
        &d,
        &[
            rule_json(2, &ram, d.items[0], "20"),
            rule_json(1, &cpu, d.items[0], "10"),
        ],
        "k1",
    )
    .await;
    assert_eq!(s, 200, "the first answer again");
    let (s, _, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "1")], "k3").await;
    assert_eq!(s, 409, "a stale If-Match");
    assert_eq!(quota_audit(&f, d.revision).await.len(), 2);
    assert_eq!(stored(&f, &d).await, (second, etag2));
    let request = &fake.requests()[1];
    assert!(request.quantity.is_none(), "authoring sends no quantity");
    assert_eq!(
        request
            .rules
            .iter()
            .map(|r| r.sku_version.get())
            .collect::<Vec<_>>(),
        [3, 3],
        "each rule carries its item's SKU version on the sale date"
    );
}

#[tokio::test]
async fn an_empty_array_clears_every_rule() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (_, _, etag) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    let (calls, version_reads) = (
        fake.requests().len(),
        catalog.version_readers.lock().unwrap().len(),
    );
    fake.go_down();
    let cleared = quota_support::Draft { etag, ..d.clone() };
    let (s, body, etag) = put(&f, &cleared, &[], "k2").await;
    assert_eq!(s, 200, "{body}: an outage never blocks clearing the rules");
    assert_eq!(body["quota_rules"], json!([]));
    assert_eq!(body["quota_rules_digest"], empty_digest());
    assert_eq!(
        fake.requests().len(),
        calls,
        "an empty set asks Products nothing"
    );
    assert_eq!(
        catalog.version_readers.lock().unwrap().len(),
        version_reads,
        "and reads no SKU version"
    );
    assert_eq!(stored(&f, &d).await, (body, etag));
}

#[tokio::test]
async fn an_invalid_set_writes_nothing_and_names_every_failure() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let mut changed = rule_json(2, &cpu, d.items[1], "10");
    changed["definition_ref"]["digest"] = json!("f".repeat(64));
    let (s, problem, _) = put(
        &f,
        &d,
        &[rule_json(1, &cpu, Uuid::new_v4(), "10"), changed],
        "k1",
    )
    .await;
    assert_eq!(
        s, 400,
        "{problem}: a conflict code beside an invalid one is still 400"
    );
    let text = problem.to_string();
    assert!(
        text.contains("quota_rules[0].item_id") && text.contains("QUOTA_RULE_INVALID"),
        "{text}"
    );
    assert!(
        text.contains("quota_rules[1].definition_ref") && text.contains("QUOTA_DEFINITION_CHANGED"),
        "{text}"
    );
    let (read, etag) = stored(&f, &d).await;
    assert_eq!(
        (read["quota_rules"].clone(), etag),
        (json!([]), d.etag.clone()),
        "nothing written, version unchanged"
    );
    let (s, problem, _) = put(
        &f,
        &d,
        &[
            json!({"rule_id": "not a uuid"}),
            rule_json(1, &cpu, d.items[0], "1.5"),
        ],
        "k1",
    )
    .await;
    assert_eq!(s, 400, "{problem}");
    let text = problem.to_string();
    for (path, code) in [
        ("quota_rules[0].rule_id", "QUOTA_RULE_INVALID"),
        ("quota_rules[1].amount", "QUOTA_AMOUNT_INVALID"),
    ] {
        assert!(text.contains(path) && text.contains(code), "{path}: {text}");
    }
    assert_eq!(
        fake.requests().len(),
        1,
        "a body that does not parse asks Products nothing, and its key stays free"
    );
}

#[tokio::test]
async fn a_rule_whose_sku_has_no_version_on_the_sale_date_is_refused_on_item_id() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    catalog
        .versions
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .remove(&d.other_sku);
    let (s, problem, _) = put(
        &f,
        &d,
        &[
            rule_json(1, &cpu, d.items[0], "10"),
            rule_json(2, &cpu, d.items[1], "10"),
        ],
        "k1",
    )
    .await;
    assert_eq!(s, 400, "{problem}");
    let text = problem.to_string();
    assert!(
        text.contains("quota_rules[1].item_id") && text.contains("QUOTA_RULE_INVALID"),
        "{text}"
    );
    assert_eq!(
        fake.requests()[0]
            .rules
            .iter()
            .map(|r| r.rule_id)
            .collect::<Vec<_>>(),
        [Uuid::from_u128(1)],
        "a rule without a SKU version is judged by Pricing and never sent"
    );
}

#[tokio::test]
async fn a_stale_if_match_is_409_and_writes_nothing() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (s, saved, etag) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200, "{saved}");
    // The old ETag, a new key.
    let (s, problem, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "99")], "k2").await;
    assert_eq!(s, 409, "{problem}");
    assert!(problem.to_string().contains("STALE_REVISION"), "{problem}");
    assert_eq!(stored(&f, &d).await, (saved, etag));
}

#[tokio::test]
async fn the_same_key_and_content_replays_after_the_etag_moved() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let rules = [rule_json(1, &cpu, d.items[0], "10")];
    let (s1, b1, e1) = put(&f, &d, &rules, "k1").await;
    let moved = quota_support::Draft {
        etag: e1.clone(),
        ..d.clone()
    };
    let (s, _, _) = put(&f, &moved, &[], "k-clear").await;
    assert_eq!(s, 200);
    // The same old If-Match and body: a retry after a lost answer.
    let (s2, b2, e2) = put(&f, &d, &rules, "k1").await;
    assert_eq!((s1, s2), (200, 200));
    assert_eq!((b1, e1), (b2, e2), "the stored answer, byte for byte");
    assert_eq!(fake.requests().len(), 1, "a replay calls no other gear");
}

/// The key's content is the rules as they parse, not the body's spelling: the same rules in
/// another order, with an absent `dimension_value` and upper-case UUIDs, replay the stored answer.
#[tokio::test]
async fn the_same_rules_spelled_otherwise_replay_under_the_key() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let (cpu, ram) = (fake.add(vcpu(d.sku)), fake.add(ram(d.sku)));
    let one = rule_json(1, &cpu, d.items[0], "10");
    let two = rule_json(2, &ram, d.items[0], "20");
    let (s1, b1, e1) = put(&f, &d, &[one.clone(), two.clone()], "k1").await;
    assert_eq!(s1, 200, "{b1}");
    let mut respelled = one;
    respelled.as_object_mut().unwrap().remove("dimension_value");
    respelled["item_id"] = json!(d.items[0].to_string().to_uppercase());
    respelled["rule_id"] = json!(Uuid::from_u128(1).to_string().to_uppercase());
    let (s2, b2, e2) = put(&f, &d, &[two, respelled], "k1").await;
    assert_eq!(s2, 200, "{b2}");
    assert_eq!((b1, e1), (b2, e2), "the stored answer");
    assert_eq!(fake.requests().len(), 1, "a replay calls no other gear");
}

#[tokio::test]
async fn the_same_key_with_other_content_is_idempotency_conflict() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (s, saved, etag) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200, "{saved}");
    let (s, problem, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "11")], "k1").await;
    assert_eq!(s, 409, "{problem}");
    assert!(
        problem.to_string().contains("IDEMPOTENCY_CONFLICT"),
        "{problem}"
    );
    let moved = quota_support::Draft { etag, ..d.clone() };
    let (s, problem, _) = put(&f, &moved, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 409, "another If-Match is other content: {problem}");
    assert!(
        problem.to_string().contains("IDEMPOTENCY_CONFLICT"),
        "{problem}"
    );
}

/// A pending definition is saved, and the checks show Products' current verdict: the row names
/// the state and reason Products gave, and the revision, green but for it, is not ready.
#[tokio::test]
async fn a_definition_pending_qualification_is_saved_and_checks_red() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = priced_draft_with_items(&f, &catalog).await;
    let (_, before) = checks_of(&f, d.revision).await;
    assert_eq!(before["ready"], true, "green without rules: {before}");
    let pending = fake.add(quota_support::disabled(vcpu(d.sku)));
    let (s, body, _) = put(&f, &d, &[rule_json(1, &pending, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200, "{body}");
    assert_eq!(body["quota_rules"][0]["availability"]["state"], "disabled");
    assert_eq!(
        body["quota_rules"][0]["availability"]["reason_code"],
        "qualification_pending"
    );
    let (s, checks) = checks_of(&f, d.revision).await;
    assert_eq!(s, 200, "{checks}");
    assert_eq!(checks["ready"], false);
    let red: Vec<&str> = checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["ok"] == false)
        .map(|c| c["code"].as_str().unwrap())
        .collect();
    assert_eq!(red, ["QUOTA_QUALIFICATION_UNAVAILABLE"], "{checks}");
    let row = row(&checks, "QUOTA_QUALIFICATION_UNAVAILABLE");
    let detail = row["detail"].as_str().unwrap();
    assert!(
        detail.contains("disabled") && detail.contains("qualification_pending"),
        "Products' verdict, read fresh: {row}"
    );
    assert_eq!(fake.requests().len(), 2, "the PUT and the checks");
}

#[tokio::test]
async fn a_duplicate_set_is_409_quota_rule_duplicate() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (s, problem, _) = put(
        &f,
        &d,
        &[
            rule_json(1, &cpu, d.items[0], "10"),
            rule_json(2, &cpu, d.items[1], "10"),
        ],
        "k1",
    )
    .await;
    assert_eq!(s, 409, "{problem}");
    let text = problem.to_string();
    for path in [
        "quota_rules[0].logical_binding_key",
        "quota_rules[1].logical_binding_key",
    ] {
        assert!(text.contains(path), "{path}: {text}");
    }
    assert!(text.contains("QUOTA_RULE_DUPLICATE"), "{text}");
    assert!(!text.contains("QUOTA_RULE_INVALID"), "{text}");
    // The shape of a conflict refusal: an invalid-argument problem answered 409, whose field
    // violations carry the codes, and no `reason`.
    assert!(
        problem["type"]
            .as_str()
            .unwrap()
            .contains("invalid_argument"),
        "{problem}"
    );
    assert_eq!(problem["status"], 409);
    assert!(problem["context"].get("reason").is_none(), "{problem}");
    let violations: Vec<(String, String)> = problem["context"]["field_violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["field"].as_str().unwrap().to_owned(),
                v["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        violations,
        [
            (
                "quota_rules[0].logical_binding_key".to_owned(),
                "QUOTA_RULE_DUPLICATE".to_owned()
            ),
            (
                "quota_rules[1].logical_binding_key".to_owned(),
                "QUOTA_RULE_DUPLICATE".to_owned()
            ),
        ]
    );
    assert_eq!(stored(&f, &d).await.0["quota_rules"], json!([]));
}

/// A write that stays contended past the transaction's own retries answers 409 `CONTENDED`; it
/// writes nothing and leaves the key free.
#[tokio::test]
async fn a_write_contended_past_its_retries_is_409_contended() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    plan_support::raw(
        &f,
        "CREATE TRIGGER quota_set_busy BEFORE INSERT ON pricing_plan_quota_set \
         BEGIN SELECT RAISE(ABORT, '(code: 5) database is locked'); END",
    )
    .await;
    let rules = [rule_json(1, &cpu, d.items[0], "10")];
    let (s, problem, _) = put(&f, &d, &rules, "k1").await;
    assert_eq!(s, 409, "{problem}");
    assert_eq!(problem["context"]["reason"], "CONTENDED", "{problem}");
    assert_eq!(stored(&f, &d).await.0["quota_rules"], json!([]));
    plan_support::raw(&f, "DROP TRIGGER quota_set_busy").await;
    let (s, saved, _) = put(&f, &d, &rules, "k1").await;
    assert_eq!(s, 200, "the key is free: {saved}");
}

#[tokio::test]
async fn a_changed_definition_digest_is_409() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let mut changed = rule_json(1, &cpu, d.items[0], "10");
    changed["definition_ref"]["digest"] = json!("e".repeat(64));
    let (s, problem, _) = put(&f, &d, &[changed], "k1").await;
    assert_eq!(s, 409, "{problem}");
    let text = problem.to_string();
    assert!(
        text.contains("quota_rules[0].definition_ref") && text.contains("QUOTA_DEFINITION_CHANGED"),
        "{text}"
    );
    assert_eq!(stored(&f, &d).await.0["quota_rules"], json!([]));
}

#[tokio::test]
async fn a_published_or_pending_revision_refuses_rules() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let pending = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(pending.sku));
    plan_support::lock(&f, pending.revision).await;
    let (s, problem, _) = put(
        &f,
        &pending,
        &[rule_json(1, &cpu, pending.items[0], "10")],
        "k1",
    )
    .await;
    assert_eq!(s, 409, "{problem}");
    assert!(
        problem.to_string().contains("REVISION_NOT_DRAFT"),
        "{problem}"
    );
    let published = draft_with_items(&f, &catalog).await;
    plan_support::publish(&f, published.plan, published.revision).await;
    let (s, problem, _) = put(&f, &published, &[], "k2").await;
    assert_eq!(s, 409, "{problem}");
    assert!(
        problem.to_string().contains("REVISION_NOT_DRAFT"),
        "{problem}"
    );
    assert!(fake.requests().is_empty(), "the revision is judged first");
}

#[tokio::test]
async fn another_author_is_not_draft_author() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (s, problem, _) = f
        .call_as(
            &f.user(),
            "PUT",
            &path(&d),
            json!({"quota_rules": [rule_json(1, &cpu, d.items[0], "10")]}),
            Some(&d.etag),
            Some("k1"),
        )
        .await;
    assert_eq!(s, 403, "{problem}");
    assert!(
        problem.to_string().contains("NOT_DRAFT_AUTHOR"),
        "{problem}"
    );
    let (s, problem, _) = f
        .call(
            "PUT",
            &format!("/plan-revisions/{}/quota-rules", Uuid::now_v7()),
            json!({"quota_rules": []}),
            Some(&d.etag),
            Some("k1"),
        )
        .await;
    assert_eq!(s, 404, "{problem}");
    let (s, problem, _) = f
        .call_as(
            &plan_support::holding(&f, "plan:read"),
            "PUT",
            &path(&d),
            json!("not a body"),
            None,
            None,
        )
        .await;
    assert_eq!(s, 403, "plan author before any header or body: {problem}");
}

#[tokio::test]
async fn a_quota_catalog_outage_is_503_and_writes_nothing() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    fake.go_down();
    let (s, problem, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 503, "{problem}");
    assert!(
        problem.to_string().contains("QUOTA_CATALOG_UNAVAILABLE"),
        "{problem}"
    );
    assert_eq!(
        stored(&f, &d).await,
        (stored(&f, &d).await.0, d.etag.clone())
    );
    assert_eq!(stored(&f, &d).await.0["quota_rules"], json!([]));
    // No catalog in the hub at all.
    let (bare, bare_catalog) = plan_support::setup().await;
    let e = draft_with_items(&bare, &bare_catalog).await;
    let (s, problem, _) = put(&bare, &e, &[rule_json(1, &cpu, e.items[0], "10")], "k1").await;
    assert_eq!(s, 503, "{problem}");
    assert!(
        problem.to_string().contains("QUOTA_CATALOG_UNAVAILABLE"),
        "{problem}"
    );
}

#[tokio::test]
async fn the_package_example_body_is_accepted() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let mut example: serde_json::Value = serde_json::from_str(include_str!(
        "../../../docs/quotas/schemas/plan-quota-rules.replace.example.json"
    ))
    .unwrap();
    let rule = &mut example["quota_rules"][0];
    rule["item_id"] = json!(d.items[0]);
    rule["definition_ref"] = json!({
        "id": cpu.definition_id,
        "version": cpu.version.to_string(),
        "digest": cpu.definition_digest,
    });
    let (s, body, _) = f
        .call("PUT", &path(&d), example.clone(), Some(&d.etag), Some("k1"))
        .await;
    assert_eq!(s, 200, "{body}");
    assert_eq!(
        body["quota_rules"][0]["rule_id"],
        example["quota_rules"][0]["rule_id"]
    );
    assert_eq!(body["quota_rules"][0]["amount"], "10");
}

#[tokio::test]
async fn a_missing_if_match_or_key_is_400() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let body = json!({"quota_rules": []});
    for (tag, key) in [(None, Some("k1")), (Some(d.etag.as_str()), None)] {
        let (s, problem, _) = f.call("PUT", &path(&d), body.clone(), tag, key).await;
        assert_eq!(s, 400, "{tag:?} {key:?}: {problem}");
    }
    let (s, problem, _) = f
        .call("PUT", &path(&d), body, Some("W/\"1\""), Some("k1"))
        .await;
    assert_eq!(s, 400, "a weak tag: {problem}");
    let (s, problem) =
        plan_support::entry_support::request_raw(&f.app, &f.ctx, "PUT", &path(&d), "", Some("k1"))
            .await;
    assert_eq!(s, 400, "an empty body: {problem}");
    assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn a_moved_rule_set_rolls_back_and_is_captured_again() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    // A third item arrives between the capture and the transaction.
    let sku = catalog.sku(bss_products_sdk::models::SkuType::Recurring);
    let (dsn, tenant, author, revision) = (
        plan_support::entry_support::toolkit_dsn(&f.dsn),
        f.ctx.subject_tenant_id(),
        f.ctx.subject_id(),
        d.revision,
    );
    fake.before_next_answer(async move {
        let db = toolkit_db::connect_db(&dsn, toolkit_db::ConnectOpts::default())
            .await
            .unwrap();
        let db = toolkit_db::DBProvider::<toolkit_db::DbError>::new(db);
        let now = time::OffsetDateTime::now_utc();
        bss_pricing::infra::storage::repo::plan_item_repo::insert_as_given(
            &db.conn().unwrap(),
            &toolkit_db::secure::AccessScope::for_tenant(tenant),
            bss_pricing::infra::storage::entity::plan_item::Model {
                id: Uuid::now_v7(),
                tenant_id: tenant,
                revision_id: revision,
                sku_id: sku,
                price_book_entry_id: None,
                treatment: "included".into(),
                included_qty: None,
                qty_min: None,
                reservation_id: Some(Uuid::new_v4()),
                reference_state: "confirmed".into(),
                version: 1,
                created_by: author,
                created_at: now,
                updated_at: now,
            },
        )
        .await
        .unwrap();
    });
    let (s, body, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200, "{body}");
    assert_eq!(
        fake.requests().len(),
        2,
        "the moved items rolled the transaction back and the set was validated again"
    );
    assert_eq!(rule_ids(&body), [Uuid::from_u128(1).to_string()]);
}

#[tokio::test]
async fn the_catalog_is_never_called_on_a_transactions_task() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    fake.watch(f.db.clone());
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (s, body, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200, "{body}");
    assert_eq!(fake.outside(), [true]);
}

async fn checks_of(f: &plan_support::Fixture, revision: Uuid) -> (u16, serde_json::Value) {
    let (status, body, _) = f
        .call(
            "GET",
            &format!("/plan-revisions/{revision}/checks"),
            json!(null),
            None,
            None,
        )
        .await;
    (status, body)
}

fn row<'a>(checks: &'a serde_json::Value, code: &str) -> &'a serde_json::Value {
    checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == code)
        .unwrap_or_else(|| panic!("no {code} row: {checks}"))
}

#[tokio::test]
async fn checks_name_the_rule_field_version_and_digest() {
    use bss_products_sdk::quota::validation::{QuotaCheck, QuotaCheckCode, QuotaSetValidation};
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = priced_draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (_, saved, etag) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    let (_, green) = checks_of(&f, d.revision).await;
    assert_eq!(green["ready"], true, "an enabled definition: {green}");
    fake.answering(QuotaSetValidation {
        rules: Vec::new(),
        checks: vec![QuotaCheck {
            code: QuotaCheckCode::ScopeUnsupported,
            rule_ids: vec![Uuid::from_u128(1)],
            field: "scope_selection.region".into(),
            detail: "not a region of the pool".into(),
        }],
    });
    let (s, checks) = checks_of(&f, d.revision).await;
    assert_eq!(s, 200, "{checks}");
    assert_eq!(
        format!("\"{}\"", checks["revision_version"]),
        etag,
        "the evaluated version"
    );
    assert_eq!(checks["quota_rules_digest"], saved["quota_rules_digest"]);
    assert_eq!(checks["ready"], false);
    let red = row(&checks, "QUOTA_SCOPE_UNSUPPORTED");
    assert_eq!(red["ok"], false);
    assert_eq!(
        red["subjects"][0]["quota_rule_id"],
        Uuid::from_u128(1).to_string()
    );
    assert_eq!(red["subjects"][0]["field"], "scope_selection");
    assert_eq!(red["subjects"][0]["item_id"], d.items[0].to_string());
    assert_eq!(red["subjects"][0]["sku_id"], d.sku.to_string());
    assert!(
        red["detail"]
            .as_str()
            .unwrap()
            .contains("quota_rules[0].scope_selection"),
        "{red}"
    );
    let codes: Vec<&str> = checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["code"].as_str().unwrap())
        .filter(|c| c.starts_with("QUOTA_"))
        .collect();
    assert_eq!(
        codes,
        [
            "QUOTA_RULE_INVALID",
            "QUOTA_DEFINITION_CHANGED",
            "QUOTA_AMOUNT_INVALID",
            "QUOTA_UNIT_INEXACT",
            "QUOTA_SCOPE_UNSUPPORTED",
            "QUOTA_RULE_DUPLICATE",
            "QUOTA_SET_INCOMPATIBLE",
            "QUOTA_QUALIFICATION_UNAVAILABLE",
        ],
        "the eight quota rows after every other row, in their order"
    );
    let last = checks["checks"].as_array().unwrap().last().unwrap();
    assert_eq!(last["code"], "QUOTA_QUALIFICATION_UNAVAILABLE");
    assert_eq!(
        fake.requests().len(),
        3,
        "each checks read validates fresh, once"
    );
}

#[tokio::test]
async fn a_quota_free_revisions_checks_keep_their_rows() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let (s, checks) = checks_of(&f, d.revision).await;
    assert_eq!(s, 200, "{checks}");
    let mut members: Vec<&str> = checks
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    members.sort_unstable();
    assert_eq!(
        members,
        [
            "checks",
            "quorum_required",
            "quota_rules_digest",
            "ready",
            "revision_version",
            "sale_date"
        ],
        "the two new members are the only change"
    );
    assert_eq!(checks["quota_rules_digest"], empty_digest());
    assert_eq!(format!("\"{}\"", checks["revision_version"]), d.etag);
    assert!(
        checks["checks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| !c["code"].as_str().unwrap().starts_with("QUOTA_"))
    );
    assert!(
        checks["checks"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| c["subjects"].as_array().unwrap())
            .all(|s| s.get("quota_rule_id").is_none() && s.get("field").is_none()),
        "{checks}"
    );
    assert!(fake.requests().is_empty(), "no catalog call without rules");
}

#[tokio::test]
async fn a_catalog_outage_makes_the_checks_503_not_green() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (s, _, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200);
    fake.go_down();
    let (s, problem) = checks_of(&f, d.revision).await;
    assert_eq!(s, 503, "{problem}");
    assert!(
        problem.to_string().contains("QUOTA_CATALOG_UNAVAILABLE"),
        "{problem}"
    );
    let (s, problem, _) = f
        .call(
            "GET",
            &format!("/plan-revisions/checks?revision_ids={}", d.revision),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 503, "the batch too: {problem}");
}

#[tokio::test]
async fn deleting_an_item_with_rules_is_409_item_has_quota_rules() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (s, _, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200);
    let (s, problem, _) = f
        .call(
            "DELETE",
            &format!("/plan-items/{}", d.items[0]),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 409, "{problem}");
    assert!(
        problem.to_string().contains("ITEM_HAS_QUOTA_RULES"),
        "{problem}"
    );
    let (s, item, _) = f
        .call(
            "GET",
            &format!("/plan-items/{}", d.items[0]),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "the ruled item stays: {item}");
    let (s, body, _) = f
        .call(
            "DELETE",
            &format!("/plan-items/{}", d.items[1]),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 204, "an item without rules still goes: {body}");
}

#[tokio::test]
async fn batch_checks_match_the_single_read_with_rules() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let ruled = draft_with_items(&f, &catalog).await;
    let free = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(quota_support::disabled(vcpu(ruled.sku)));
    let (s, _, _) = put(
        &f,
        &ruled,
        &[rule_json(1, &cpu, ruled.items[0], "10")],
        "k1",
    )
    .await;
    assert_eq!(s, 200);
    let (_, one) = checks_of(&f, ruled.revision).await;
    let (_, other) = checks_of(&f, free.revision).await;
    assert_eq!(row(&one, "QUOTA_QUALIFICATION_UNAVAILABLE")["ok"], false);
    let (s, batch, _) = f
        .call(
            "GET",
            &format!(
                "/plan-revisions/checks?revision_ids={},{}",
                free.revision, ruled.revision
            ),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{batch}");
    assert_eq!(
        batch["items"][0]["checks"].to_string(),
        other.to_string(),
        "byte for byte"
    );
    assert_eq!(batch["items"][1]["checks"].to_string(), one.to_string());
    assert_eq!(
        fake.requests().len(),
        3,
        "the PUT, the single read and one validation in the batch"
    );
}

#[tokio::test]
async fn deleting_a_draft_with_rules_removes_them() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (s, _, etag) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200);
    let (s, body, _) = f
        .call(
            "DELETE",
            &format!("/plan-revisions/{}", d.revision),
            json!(null),
            Some(&etag),
            None,
        )
        .await;
    assert_eq!(s, 204, "{body}");
    let (s, body, _) = f.call("GET", &path(&d), json!(null), None, None).await;
    assert_eq!(s, 404, "the draft and its rules are gone: {body}");
    let raw = Database::connect(&f.dsn).await.unwrap();
    for table in ["pricing_plan_quota_rule", "pricing_plan_quota_set"] {
        let count = raw
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!("SELECT count(*) AS n FROM {table}"),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "n")
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}

#[tokio::test]
async fn a_corrupt_stored_rule_answers_500() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (s, _, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200);
    plan_support::raw(
        &f,
        "UPDATE pricing_plan_quota_rule \
         SET amount = '01', definition_digest = upper(definition_digest)",
    )
    .await;
    for path in [
        path(&d),
        format!("/plan-revisions/{}", d.revision),
        format!("/plan-revisions/{}/checks", d.revision),
    ] {
        let (s, body, _) = f.call("GET", &path, json!(null), None, None).await;
        assert_eq!(s, 500, "{path}: {body}");
    }
}

/// Products' verdicts on rule 1 (on `definition`) that do not judge it against the definition it
/// names: left out, judged without a definition and named in no check, resolved to another
/// version (alone, and beside a check that names the rule), and resolved to another digest with
/// no `QUOTA_DEFINITION_CHANGED` check.
fn unjudging_verdicts(
    definition: &bss_products_sdk::quota::catalog::QuotaDefinition,
) -> Vec<bss_products_sdk::quota::validation::QuotaSetValidation> {
    use bss_products_sdk::quota::catalog::QuotaVersion;
    use bss_products_sdk::quota::validation::{
        QuotaCheck, QuotaCheckCode, QuotaRuleVerdict, QuotaSetValidation,
    };
    let resolved = |definition: Option<bss_products_sdk::quota::catalog::QuotaDefinition>| {
        QuotaSetValidation {
            rules: vec![QuotaRuleVerdict {
                rule_id: Uuid::from_u128(1),
                definition,
                limit: None,
            }],
            checks: Vec::new(),
        }
    };
    let mut other_version = definition.clone();
    other_version.version = QuotaVersion::new(definition.version.get() + 1).unwrap();
    let mut other_digest = definition.clone();
    other_digest.definition_digest = "0".repeat(64);
    vec![
        QuotaSetValidation {
            rules: Vec::new(),
            checks: Vec::new(),
        },
        resolved(None),
        resolved(Some(other_version.clone())),
        QuotaSetValidation {
            checks: vec![QuotaCheck {
                code: QuotaCheckCode::ScopeUnsupported,
                rule_ids: vec![Uuid::from_u128(1)],
                field: "scope_selection".into(),
                detail: "a scope the author would be blamed for".into(),
            }],
            ..resolved(Some(other_version))
        },
        resolved(Some(other_digest)),
    ]
}

/// A rule Products was sent but did not judge against the definition it names is Products'
/// contract break, not the author's: the PUT answers 500, logs an error naming the rule and the
/// revision, and writes nothing.
#[tokio::test]
async fn a_rule_products_does_not_judge_is_a_logged_500_at_the_put() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let before = stored(&f, &d).await;
    for (n, verdict) in unjudging_verdicts(&cpu).into_iter().enumerate() {
        fake.answering(verdict);
        let (logs, guard) = quota_support::LogCapture::install();
        let (s, problem, _) = put(
            &f,
            &d,
            &[rule_json(1, &cpu, d.items[0], "10")],
            &format!("k{n}"),
        )
        .await;
        drop(guard);
        assert_eq!(s, 500, "{n}: {problem}");
        assert!(
            !problem.to_string().contains("QUOTA_RULE_INVALID")
                && !problem.to_string().contains("QUOTA_SCOPE_UNSUPPORTED"),
            "{n}: the author is not blamed: {problem}"
        );
        let (rule, revision) = (Uuid::from_u128(1).to_string(), d.revision.to_string());
        assert!(
            logs.lines().iter().any(|line| line.starts_with("ERROR")
                && line.contains(&rule)
                && line.contains(&revision)),
            "{n}: {:#?}",
            logs.lines()
        );
        assert_eq!(stored(&f, &d).await, before, "{n}: nothing written");
    }
}

/// A rule Products was sent but did not judge against the definition it names reads red, never
/// green: the checks and the batch show `QUOTA_RULE_INVALID` on the rule's definition reference.
#[tokio::test]
async fn a_rule_products_does_not_judge_checks_red() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = priced_draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (s, saved, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200, "{saved}");
    let (_, green) = checks_of(&f, d.revision).await;
    assert_eq!(green["ready"], true, "{green}");
    for (n, verdict) in unjudging_verdicts(&cpu).into_iter().enumerate() {
        fake.answering(verdict);
        let (s, checks) = checks_of(&f, d.revision).await;
        assert_eq!(s, 200, "{n}: {checks}");
        assert_eq!(checks["ready"], false, "{n}: {checks}");
        let red = row(&checks, "QUOTA_RULE_INVALID");
        assert_eq!(red["ok"], false, "{n}: {red}");
        assert_eq!(
            red["subjects"][0]["quota_rule_id"],
            Uuid::from_u128(1).to_string()
        );
        assert_eq!(red["subjects"][0]["field"], "definition_ref");
        let (s, batch, _) = f
            .call(
                "GET",
                &format!("/plan-revisions/checks?revision_ids={}", d.revision),
                json!(null),
                None,
                None,
            )
            .await;
        assert_eq!(s, 200, "{n}: {batch}");
        assert_eq!(batch["items"][0]["checks"], checks, "{n}: the batch agrees");
    }
}

/// A body of many stray members, and a rule of many unknown members, are refused with the first
/// ten of each named and the rest counted, before any other gear is asked; the key stays free.
#[tokio::test]
async fn a_body_of_many_unknown_members_is_refused_with_a_bounded_answer() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let mut rule = rule_json(1, &cpu, d.items[0], "10");
    let mut body = serde_json::Map::new();
    for n in 0..5000 {
        rule[format!("extra_{n:04}").as_str()] = json!(n);
        body.insert(format!("stray_{n:04}"), json!(n));
    }
    body.insert("quota_rules".to_owned(), json!([rule]));
    let (s, problem, _) = f
        .call(
            "PUT",
            &path(&d),
            serde_json::Value::Object(body),
            Some(&d.etag),
            Some("k1"),
        )
        .await;
    assert_eq!(s, 400, "{problem}");
    let violations = problem["context"]["field_violations"].as_array().unwrap();
    assert_eq!(violations.len(), 22, "{problem}");
    assert_eq!(
        violations[10]["description"],
        "4990 more members are not members of the body"
    );
    assert_eq!(
        violations[21]["description"],
        "4990 more members are not members of a rule"
    );
    assert!(fake.requests().is_empty(), "no other gear is asked");
    let (s, saved, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200, "the key stays free: {saved}");
}
