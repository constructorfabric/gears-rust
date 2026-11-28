//! A revision's quota rules through approval (D-545): the unit's content carries the rules and
//! their digest; submit and the final approve recheck them with Products' fresh verdict, so a red
//! quota row refuses either and an outage is 503 with nothing written; a vote that does not
//! apply still records; a price change, a scheduled switch and a submitted revision leave the
//! rules as they were.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]
mod plan_support;
mod quota_support;
use bss_pricing::infra::reference_ticker::Ticker;
use bss_pricing::infra::reference_work::Clock;
use plan_support::Fixture;
use quota_support::{Draft, disabled, install, priced_draft_with_items, put, rule_json, vcpu};
use serde_json::{Value, json};
use std::sync::Arc;
use time::{Date, Duration, OffsetDateTime};
use toolkit_security::SecurityContext;
use uuid::Uuid;

#[toolkit_canonical_errors::resource_error(toolkit_gts::gts_id!(
    "cf.bss.products.quota_definition.v1~"
))]
struct QuotaDefinitionResource;

/// The plan-revision quorum of the tenant's approval policy.
async fn set_quorum(f: &Fixture, quorum: u32) {
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let (s, b, _) = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"kind": "plan_revision", "quorum": quorum}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
}

/// `POST /plan-revisions/{id}/submit` as the fixture's author under `key`.
async fn submit(f: &Fixture, d: &Draft, key: &str) -> (u16, Value) {
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/plan-revisions/{}/submit", d.revision),
            json!({}),
            None,
            Some(key),
        )
        .await;
    (s, b)
}

/// `who` approves `unit` (the receipt's `unit`) at generation 1, under a fresh key.
async fn approve_as(f: &Fixture, who: &SecurityContext, unit: &Value) -> (u16, Value) {
    let (s, b, _) = f
        .call_as(
            who,
            "POST",
            &format!("/approval-units/{}/approve", unit["id"].as_str().unwrap()),
            json!({"generation": 1}),
            None,
            Some(&Uuid::new_v4().to_string()),
        )
        .await;
    (s, b)
}

/// The revision as `GET /plan-revisions/{id}` reads it.
async fn revision(f: &Fixture, d: &Draft) -> Value {
    let (s, b, _) = f
        .call(
            "GET",
            &format!("/plan-revisions/{}", d.revision),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    b
}

/// A priced draft whose first item carries one enabled vCPU rule of 10: the draft and the saved
/// set.
async fn ruled(
    f: &Fixture,
    catalog: &plan_support::Catalog,
) -> (Draft, Value, Arc<quota_support::FakeQuotaCatalog>) {
    let fake = install(f);
    let d = priced_draft_with_items(f, catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (s, saved, etag) = put(f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200, "{saved}");
    (Draft { etag, ..d }, saved, fake)
}

#[tokio::test]
async fn submit_refuses_a_definition_pending_qualification() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = priced_draft_with_items(&f, &catalog).await;
    let pending = fake.add(disabled(vcpu(d.sku)));
    let (s, saved, _) = put(&f, &d, &[rule_json(1, &pending, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200, "{saved}");
    let (s, problem) = submit(&f, &d, "s1").await;
    assert_eq!(s, 400, "{problem}");
    let text = problem.to_string();
    assert!(
        text.contains("REVISION_CHECKS_RED") && text.contains("QUOTA_QUALIFICATION_UNAVAILABLE"),
        "{text}"
    );
    // Products' own verdict, not the fail-closed row of a revision checked without one.
    assert!(
        text.contains("disabled") && text.contains("qualification_pending"),
        "{text}"
    );
    assert_eq!(
        fake.requests().len(),
        2,
        "the PUT, and the submit's capture"
    );
    assert!(
        f.all_units("kind=plan_revision").await.is_empty(),
        "no unit"
    );
}

#[tokio::test]
async fn quorum_zero_publishes_a_revision_with_rules_unchanged() {
    let (f, catalog) = plan_support::setup().await;
    let (d, saved, fake) = ruled(&f, &catalog).await;
    set_quorum(&f, 0).await;
    let (s, receipt) = submit(&f, &d, "s1").await;
    assert_eq!(
        (s, receipt["applied"].clone()),
        (201, json!(true)),
        "{receipt}"
    );
    let read = revision(&f, &d).await;
    assert_eq!(read["state"], "published");
    assert_eq!(read["quota_rules_digest"], saved["quota_rules_digest"]);
    assert_eq!(read["quota_rules"], saved["quota_rules"]);
    assert_eq!(
        fake.requests().len(),
        2,
        "the PUT, and the submit's capture"
    );
}

/// Submit at quorum 1 records the unit; the final approve rechecks the rules with a fresh verdict
/// and publishes them unchanged.
#[tokio::test]
async fn a_final_approve_rechecks_and_publishes_a_revision_with_rules() {
    let (f, catalog) = plan_support::setup().await;
    let (d, saved, fake) = ruled(&f, &catalog).await;
    set_quorum(&f, 1).await;
    let (s, receipt) = submit(&f, &d, "s1").await;
    assert_eq!(
        (s, receipt["applied"].clone()),
        (201, json!(false)),
        "{receipt}"
    );
    let (s, vote) = approve_as(&f, &f.user(), &receipt["unit"]).await;
    assert_eq!(s, 200, "{vote}");
    assert_eq!(vote["outcome"], "applied", "{vote}");
    let read = revision(&f, &d).await;
    assert_eq!(read["state"], "published");
    assert_eq!(read["quota_rules_digest"], saved["quota_rules_digest"]);
    assert_eq!(fake.requests().len(), 3, "the PUT, the submit and the vote");
}

#[tokio::test]
async fn apply_rechecks_and_refuses_a_definition_withdrawn_after_submit() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = priced_draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    set_quorum(&f, 1).await;
    let (s, receipt) = submit(&f, &d, "s1").await;
    assert_eq!(s, 201, "{receipt}");
    fake.withdraw(cpu.definition_id, cpu.version.get());
    let (s, problem) = approve_as(&f, &f.user(), &receipt["unit"]).await;
    assert_eq!(s, 409, "{problem}");
    assert_eq!(problem["context"]["reason"], "APPLY_REFUSED", "{problem}");
    assert_eq!(
        problem["detail"], "REVISION_CHECKS_RED: QUOTA_QUALIFICATION_UNAVAILABLE",
        "{problem}"
    );
    // The vote asked Products again; `a_rule_products_leaves_out_refuses_the_final_approve` shows
    // that apply judges Products' fresh verdict, not the fail-closed row.
    assert_eq!(fake.requests().len(), 3, "the PUT, the submit and the vote");
    assert_eq!(
        revision(&f, &d).await["state"],
        "pending",
        "nothing published"
    );
}

#[tokio::test]
async fn the_unit_content_carries_the_rules() {
    let (f, catalog) = plan_support::setup().await;
    let (d, saved, _) = ruled(&f, &catalog).await;
    set_quorum(&f, 1).await;
    let (_, receipt) = submit(&f, &d, "s1").await;
    let (s, unit, _) = f
        .call(
            "GET",
            &format!(
                "/approval-units/{}",
                receipt["unit"]["id"].as_str().unwrap()
            ),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{unit}");
    let after = &unit["snapshot"]["after"];
    assert_eq!(after["quota_rules_digest"], saved["quota_rules_digest"]);
    let rule = &after["quota_rules"][0];
    assert_eq!(rule["amount"], "10");
    assert_eq!(rule["rule_id"], Uuid::from_u128(1).to_string());
    assert_eq!(rule["item_id"], d.items[0].to_string());
    assert_eq!(
        rule["definition_ref"],
        saved["quota_rules"][0]["definition_ref"]
    );
    assert_eq!(rule["basis"], "per_plan_unit");
}

/// A revision without rules fingerprints as it did before quota rules existed: no quota member.
#[tokio::test]
async fn a_quota_free_units_content_has_no_quota_member() {
    let (f, catalog) = plan_support::setup().await;
    install(&f);
    let d = priced_draft_with_items(&f, &catalog).await;
    set_quorum(&f, 1).await;
    let (s, receipt) = submit(&f, &d, "s1").await;
    assert_eq!(s, 201, "{receipt}");
    let after = &receipt["unit"]["snapshot"]["after"];
    assert!(
        after.get("quota_rules").is_none() && after.get("quota_rules_digest").is_none(),
        "{after}"
    );
}

#[tokio::test]
async fn a_submitted_revision_refuses_a_replace() {
    let (f, catalog) = plan_support::setup().await;
    let (d, _, fake) = ruled(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    set_quorum(&f, 1).await;
    let (s, receipt) = submit(&f, &d, "s1").await;
    assert_eq!(s, 201, "{receipt}");
    let (s, problem, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "20")], "k2").await;
    assert_eq!(s, 409, "{problem}");
    assert!(
        problem.to_string().contains("REVISION_NOT_DRAFT"),
        "{problem}"
    );
}

/// A catalog outage at submit is 503 `QUOTA_CATALOG_UNAVAILABLE` and records no unit.
#[tokio::test]
async fn a_catalog_outage_at_submit_is_503_and_records_no_unit() {
    let (f, catalog) = plan_support::setup().await;
    let (d, _, fake) = ruled(&f, &catalog).await;
    set_quorum(&f, 1).await;
    fake.go_down();
    let (s, problem) = submit(&f, &d, "s1").await;
    assert_eq!(s, 503, "{problem}");
    assert!(
        problem.to_string().contains("QUOTA_CATALOG_UNAVAILABLE"),
        "{problem}"
    );
    assert!(
        f.all_units("kind=plan_revision").await.is_empty(),
        "no unit"
    );
    assert_eq!(revision(&f, &d).await["state"], "draft");
    fake.come_back();
    let (s, receipt) = submit(&f, &d, "s1").await;
    assert_eq!(
        s, 201,
        "the key is free once the catalog answers: {receipt}"
    );
}

/// A catalog outage refuses only the final approve (503, nothing published); a vote that does
/// not apply needs no verdict and still records.
#[tokio::test]
async fn a_catalog_outage_refuses_only_the_final_approve() {
    let (f, catalog) = plan_support::setup().await;
    let (d, _, fake) = ruled(&f, &catalog).await;
    set_quorum(&f, 2).await;
    let (s, receipt) = submit(&f, &d, "s1").await;
    assert_eq!(s, 201, "{receipt}");
    fake.go_down();
    let (one, two) = (f.user(), f.user());
    let (s, vote) = approve_as(&f, &one, &receipt["unit"]).await;
    assert_eq!(s, 200, "{vote}");
    assert_eq!(vote["outcome"], "pending", "the first vote records: {vote}");
    let (s, problem) = approve_as(&f, &two, &receipt["unit"]).await;
    assert_eq!(s, 503, "{problem}");
    assert!(
        problem.to_string().contains("QUOTA_CATALOG_UNAVAILABLE"),
        "{problem}"
    );
    assert_eq!(
        revision(&f, &d).await["state"],
        "pending",
        "nothing published"
    );
    fake.come_back();
    let (s, vote) = approve_as(&f, &two, &receipt["unit"]).await;
    assert_eq!(s, 200, "{vote}");
    assert_eq!(vote["outcome"], "applied", "{vote}");
    assert_eq!(revision(&f, &d).await["state"], "published");
}

/// A refusal Products keeps whole is answered with its own status and code, and records no unit.
#[tokio::test]
async fn a_catalog_refusal_at_submit_is_answered_as_products_gave_it() {
    let (f, catalog) = plan_support::setup().await;
    let (d, _, fake) = ruled(&f, &catalog).await;
    set_quorum(&f, 1).await;
    fake.refusing(
        QuotaDefinitionResource::permission_denied()
            .with_reason("QUOTA_DEFINITION_READ_DENIED")
            .create(),
    );
    let (s, problem) = submit(&f, &d, "s1").await;
    assert_eq!(s, 403, "{problem}");
    assert!(
        problem.to_string().contains("QUOTA_DEFINITION_READ_DENIED"),
        "{problem}"
    );
    assert!(
        f.all_units("kind=plan_revision").await.is_empty(),
        "no unit"
    );
}

/// A new price on the ruled item's entry, published through publish-changes, moves neither the
/// published revision's rules nor their digest.
#[tokio::test]
async fn a_price_change_leaves_the_rules_and_their_digest() {
    let (f, catalog) = plan_support::setup().await;
    let (d, saved, _) = ruled(&f, &catalog).await;
    set_quorum(&f, 0).await;
    let (s, receipt) = submit(&f, &d, "s1").await;
    assert_eq!(s, 201, "{receipt}");
    let before = revision(&f, &d).await;
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let (s, b, _) = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"kind": "prices", "quorum": 0}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let entry = plan_support::items(&f, d.revision).await[0]
        .price_book_entry_id
        .unwrap();
    let today = OffsetDateTime::now_utc().date().to_string();
    let (s, drafted, _) = f
        .call(
            "POST",
            &format!("/price-book-entries/{entry}/prices"),
            json!({"price": {"rate": "0.25"}, "eligibility": "all", "effective_from": today}),
            None,
            Some("reprice"),
        )
        .await;
    assert_eq!(s, 201, "{drafted}");
    let book = before["book_id"].as_str().unwrap();
    let (s, published, _) = f
        .call(
            "POST",
            &format!("/price-books/{book}/publish-changes"),
            json!({}),
            None,
            Some("publish"),
        )
        .await;
    assert_eq!(s, 201, "{published}");
    assert_eq!(published["applied"], true, "{published}");
    let after = revision(&f, &d).await;
    assert_eq!(after["version"], before["version"]);
    assert_eq!(after["quota_rules"], saved["quota_rules"]);
    assert_eq!(after["quota_rules_digest"], saved["quota_rules_digest"]);
}

struct FixedClock(OffsetDateTime);
impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

/// The scheduled switch publishes a revision with rules on its date and asks Products nothing
/// (D-545): the catalog is down and its requests do not grow after the submit.
#[tokio::test]
async fn the_scheduled_switch_publishes_without_a_catalog_call() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = priced_draft_with_items(&f, &catalog).await;
    let tomorrow: Date = OffsetDateTime::now_utc().date() + Duration::days(1);
    let path = format!("/plan-revisions/{}", d.revision);
    let (s, patched, etag) = f
        .call(
            "PATCH",
            &path,
            json!({"available_from": tomorrow.to_string()}),
            Some(&d.etag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{patched}");
    let d = Draft { etag, ..d };
    let cpu = fake.add(vcpu(d.sku));
    let (s, saved, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200, "{saved}");
    set_quorum(&f, 0).await;
    let (s, receipt) = submit(&f, &d, "s1").await;
    assert_eq!(s, 201, "{receipt}");
    assert_eq!(receipt["revision"]["state"], "scheduled", "{receipt}");
    let asked = fake.requests().len();
    fake.go_down();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(
        tomorrow.midnight().assume_utc() + Duration::hours(1),
    ));
    Ticker::new(f.state.clone(), clock, 10, 100)
        .tick()
        .await
        .unwrap();
    let read = revision(&f, &d).await;
    assert_eq!(read["state"], "published", "{read}");
    assert_eq!(read["quota_rules_digest"], saved["quota_rules_digest"]);
    assert_eq!(
        fake.requests().len(),
        asked,
        "the switch asks Products nothing"
    );
}

/// The rules of `revision`, through their read.
async fn rules_of(f: &Fixture, revision: &str) -> Value {
    let (s, rules, _) = f
        .call(
            "GET",
            &format!("/plan-revisions/{revision}/quota-rules"),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{rules}");
    rules
}

/// A published revision whose first item carries one enabled vCPU rule of 10: the draft it was,
/// the saved set and the catalog double.
async fn published(
    f: &Fixture,
    catalog: &plan_support::Catalog,
) -> (
    Draft,
    Value,
    Arc<quota_support::FakeQuotaCatalog>,
    bss_products_sdk::quota::catalog::QuotaDefinition,
) {
    let fake = install(f);
    let d = priced_draft_with_items(f, catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (s, saved, etag) = put(f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200, "{saved}");
    set_quorum(f, 0).await;
    let (s, receipt) = submit(f, &d, "s1").await;
    assert_eq!(s, 201, "{receipt}");
    assert_eq!(receipt["revision"]["state"], "published", "{receipt}");
    (Draft { etag, ..d }, saved, fake, cpu)
}

#[tokio::test]
async fn a_copy_keeps_exact_definitions_and_remaps_items() {
    let (f, catalog) = plan_support::setup().await;
    let (d, saved, _, _) = published(&f, &catalog).await;
    let (s, copy, _) = f
        .call(
            "POST",
            &format!("/plans/{}/revisions", d.plan),
            json!({}),
            None,
            Some("c1"),
        )
        .await;
    assert_eq!(s, 201, "{copy}");
    let new_items = copy["items"].as_array().unwrap().clone();
    let copied = copy["id"].as_str().unwrap();
    let rules = rules_of(&f, copied).await;
    let rule = &rules["quota_rules"][0];
    let source = &saved["quota_rules"][0];
    assert_eq!(rule["rule_id"], source["rule_id"]);
    assert_eq!(rule["definition_ref"], source["definition_ref"]);
    assert_eq!(
        rule["availability"], source["availability"],
        "the observation is kept"
    );
    for member in [
        "amount",
        "unit_ref",
        "basis",
        "scope_selection",
        "logical_binding_key",
    ] {
        assert_eq!(rule[member], source[member], "{member}");
    }
    let copied_sku = new_items
        .iter()
        .find(|i| i["id"] == rule["item_id"])
        .unwrap_or_else(|| panic!("the rule points at a copied item: {copy}"));
    assert_eq!(
        copied_sku["sku_id"],
        d.sku.to_string(),
        "the ruled item's copy"
    );
    assert_ne!(rule["item_id"], source["item_id"]);
    assert_ne!(
        rules["quota_rules_digest"], saved["quota_rules_digest"],
        "item ids are in the digest"
    );
    let source_rules = rules_of(&f, &d.revision.to_string()).await;
    assert_eq!(
        source_rules["quota_rules"], saved["quota_rules"],
        "the source is unchanged"
    );
    // The copy's ruled item is guarded, and deleting the copied draft takes its rules with it.
    let (s, problem, _) = f
        .call(
            "DELETE",
            &format!("/plan-items/{}", rule["item_id"].as_str().unwrap()),
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
    let (s, body, _) = f
        .call(
            "DELETE",
            &format!("/plan-revisions/{copied}"),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 204, "{body}");
}

/// The digest a copy writes is the digest of the rules it holds, as a read of a PUT of them
/// would answer: computed again over the copied items, never the source's.
#[tokio::test]
async fn a_copys_digest_is_computed_again_over_its_items() {
    let (f, catalog) = plan_support::setup().await;
    let (d, _, _, _) = published(&f, &catalog).await;
    let (s, copy, _) = f
        .call(
            "POST",
            &format!("/plans/{}/revisions", d.plan),
            json!({}),
            None,
            Some("c1"),
        )
        .await;
    assert_eq!(s, 201, "{copy}");
    let copied = rules_of(&f, copy["id"].as_str().unwrap()).await;
    let typed: Vec<bss_pricing_sdk::plan_quota::PlanQuotaRuleV1> =
        bss_pricing::api::plan_quota_authoring::rules_v1(
            serde_json::from_value(copied.clone()).unwrap(),
        )
        .unwrap()
        .quota_rules
        .into_iter()
        .map(|saved| saved.rule)
        .collect();
    assert_eq!(
        copied["quota_rules_digest"],
        bss_pricing::infra::usage_policy_wire::digest_text(
            bss_pricing_sdk::digest::quota_rules_digest(&typed)
        )
    );
}

#[tokio::test]
async fn a_published_revision_reads_its_rules_after_withdrawal() {
    let (f, catalog) = plan_support::setup().await;
    let (d, saved, fake, cpu) = published(&f, &catalog).await;
    fake.withdraw(cpu.definition_id, cpu.version.get());
    fake.go_down();
    let asked = fake.requests().len();
    let rules = rules_of(&f, &d.revision.to_string()).await;
    assert_eq!(
        rules["quota_rules"], saved["quota_rules"],
        "a stored read asks nobody"
    );
    assert_eq!(fake.requests().len(), asked);
}

#[tokio::test]
async fn a_clone_copies_rules_into_the_new_plan() {
    let (f, catalog) = plan_support::setup().await;
    let (d, saved, _, _) = published(&f, &catalog).await;
    let (s, clone, _) = f
        .call(
            "POST",
            &format!("/plans/{}/clone", d.plan),
            json!({"code": "CLONED", "name": "Cloned"}),
            None,
            Some("clone"),
        )
        .await;
    assert_eq!(s, 201, "{clone}");
    let draft = clone["revisions"][0]["id"].as_str().unwrap().to_owned();
    let items = plan_support::items(&f, draft.parse().unwrap()).await;
    let rules = rules_of(&f, &draft).await;
    let rule = &rules["quota_rules"][0];
    assert_eq!(rule["rule_id"], saved["quota_rules"][0]["rule_id"]);
    assert_eq!(
        rule["definition_ref"],
        saved["quota_rules"][0]["definition_ref"]
    );
    let item = items
        .iter()
        .find(|i| i.id.to_string() == rule["item_id"].as_str().unwrap())
        .unwrap_or_else(|| panic!("the rule points at the clone's item: {rules}"));
    assert_eq!(item.sku_id, d.sku, "the ruled item's copy");
    let source = rules_of(&f, &d.revision.to_string()).await;
    assert_eq!(
        source["quota_rules"], saved["quota_rules"],
        "the source is unchanged"
    );
    assert_eq!(source["quota_rules_digest"], saved["quota_rules_digest"]);
}

/// Eligibility is judged again, never copied: after a withdrawal, the copy's checks show the rule
/// red although its stored observation was enabled.
#[tokio::test]
async fn the_copy_of_a_withdrawn_definition_checks_red() {
    let (f, catalog) = plan_support::setup().await;
    let (d, _, fake, cpu) = published(&f, &catalog).await;
    fake.withdraw(cpu.definition_id, cpu.version.get());
    let (s, copy, _) = f
        .call(
            "POST",
            &format!("/plans/{}/revisions", d.plan),
            json!({}),
            None,
            Some("c1"),
        )
        .await;
    assert_eq!(s, 201, "{copy}");
    let copied = copy["id"].as_str().unwrap();
    let rules = rules_of(&f, copied).await;
    assert_eq!(rules["quota_rules"][0]["availability"]["state"], "enabled");
    let (s, checks, _) = f
        .call(
            "GET",
            &format!("/plan-revisions/{copied}/checks"),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{checks}");
    assert_eq!(checks["ready"], false);
    let row = checks["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == "QUOTA_QUALIFICATION_UNAVAILABLE")
        .unwrap();
    assert_eq!(row["ok"], false, "{row}");
    assert!(
        row["detail"].as_str().unwrap().contains("withdrawn"),
        "{row}"
    );
}

/// Products' verdict that leaves every rule out and names none.
fn leaving_out() -> bss_products_sdk::quota::validation::QuotaSetValidation {
    bss_products_sdk::quota::validation::QuotaSetValidation {
        rules: Vec::new(),
        checks: Vec::new(),
    }
}

/// A rule Products leaves out of its verdict at submit reads red: submit refuses it as a red
/// check and records no unit.
#[tokio::test]
async fn a_rule_products_leaves_out_refuses_submit() {
    let (f, catalog) = plan_support::setup().await;
    let (d, _, fake) = ruled(&f, &catalog).await;
    set_quorum(&f, 1).await;
    fake.answering(leaving_out());
    let (s, problem) = submit(&f, &d, "s1").await;
    assert_eq!(s, 400, "{problem}");
    let text = problem.to_string();
    assert!(
        text.contains("REVISION_CHECKS_RED") && text.contains("QUOTA_RULE_INVALID"),
        "{text}"
    );
    assert!(
        f.all_units("kind=plan_revision").await.is_empty(),
        "no unit"
    );
}

/// A rule Products leaves out of its verdict at the final approve reads red: the apply refuses
/// (409 `APPLY_REFUSED`) and nothing is published.
#[tokio::test]
async fn a_rule_products_leaves_out_refuses_the_final_approve() {
    let (f, catalog) = plan_support::setup().await;
    let (d, _, fake) = ruled(&f, &catalog).await;
    set_quorum(&f, 1).await;
    let (s, receipt) = submit(&f, &d, "s1").await;
    assert_eq!(s, 201, "{receipt}");
    fake.answering(leaving_out());
    let (s, problem) = approve_as(&f, &f.user(), &receipt["unit"]).await;
    assert_eq!(s, 409, "{problem}");
    assert_eq!(problem["context"]["reason"], "APPLY_REFUSED", "{problem}");
    assert!(
        problem.to_string().contains("QUOTA_RULE_INVALID"),
        "{problem}"
    );
    assert_eq!(
        revision(&f, &d).await["state"],
        "pending",
        "nothing published"
    );
}

/// A refusal Products keeps whole at the final approve is answered with its own status and code;
/// the revision stays pending.
#[tokio::test]
async fn a_catalog_refusal_at_the_final_approve_is_answered_as_products_gave_it() {
    let (f, catalog) = plan_support::setup().await;
    let (d, _, fake) = ruled(&f, &catalog).await;
    set_quorum(&f, 1).await;
    let (s, receipt) = submit(&f, &d, "s1").await;
    assert_eq!(s, 201, "{receipt}");
    fake.refusing(
        QuotaDefinitionResource::permission_denied()
            .with_reason("QUOTA_DEFINITION_READ_DENIED")
            .create(),
    );
    let (s, problem) = approve_as(&f, &f.user(), &receipt["unit"]).await;
    assert_eq!(s, 403, "{problem}");
    assert!(
        problem.to_string().contains("QUOTA_DEFINITION_READ_DENIED"),
        "{problem}"
    );
    assert_eq!(
        revision(&f, &d).await["state"],
        "pending",
        "nothing published"
    );
}

/// A set replaced between submit's capture and its transaction moves the stored digest: the
/// transaction rolls back, the capture runs again on the new set, and submit judges that set.
#[tokio::test]
async fn a_set_replaced_during_submits_capture_is_captured_again() {
    let (f, catalog) = plan_support::setup().await;
    let (d, _, fake) = ruled(&f, &catalog).await;
    set_quorum(&f, 1).await;
    let pending = fake.add(disabled(vcpu(d.sku)));
    let (app, ctx, path, etag) = (
        f.app.clone(),
        f.ctx.clone(),
        quota_support::path(&d),
        d.etag.clone(),
    );
    let body = json!({"quota_rules": [rule_json(1, &pending, d.items[0], "10")]});
    fake.before_next_answer(async move {
        let (s, saved, _) = plan_support::entry_support::request(
            &app,
            &ctx,
            "PUT",
            &path,
            body,
            Some(&etag),
            Some("k2"),
        )
        .await;
        assert_eq!(s, 200, "the draft is still open: {saved}");
    });
    let (s, problem) = submit(&f, &d, "s1").await;
    assert_eq!(s, 400, "{problem}");
    let text = problem.to_string();
    assert!(
        text.contains("REVISION_CHECKS_RED") && text.contains("qualification_pending"),
        "submit judged the new set: {text}"
    );
    let requests = fake.requests();
    assert_eq!(
        requests.len(),
        4,
        "the first PUT, submit's capture, the PUT in between and submit's second capture"
    );
    assert_eq!(
        requests[3].rules[0].definition.id, pending.definition_id,
        "the second capture validated the new set"
    );
    assert!(
        f.all_units("kind=plan_revision").await.is_empty(),
        "no unit"
    );
}

/// Products' verdict that resolves rule 1 to the version after the one it names, and names it in
/// no check.
fn another_version(
    definition: &bss_products_sdk::quota::catalog::QuotaDefinition,
) -> bss_products_sdk::quota::validation::QuotaSetValidation {
    let mut other = definition.clone();
    other.version =
        bss_products_sdk::quota::catalog::QuotaVersion::new(definition.version.get() + 1).unwrap();
    bss_products_sdk::quota::validation::QuotaSetValidation {
        rules: vec![bss_products_sdk::quota::validation::QuotaRuleVerdict {
            rule_id: Uuid::from_u128(1),
            definition: Some(other),
            limit: None,
        }],
        checks: Vec::new(),
    }
}

/// A rule Products judged against another definition version than the one it names reads red:
/// submit refuses it and records no unit, and the final approve refuses it and publishes nothing.
#[tokio::test]
async fn a_rule_judged_against_another_version_refuses_submit_and_the_final_approve() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = priced_draft_with_items(&f, &catalog).await;
    let cpu = fake.add(vcpu(d.sku));
    let (s, saved, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200, "{saved}");
    set_quorum(&f, 1).await;
    fake.answering(another_version(&cpu));
    let (s, problem) = submit(&f, &d, "s1").await;
    assert_eq!(s, 400, "{problem}");
    let text = problem.to_string();
    assert!(
        text.contains("REVISION_CHECKS_RED") && text.contains("QUOTA_RULE_INVALID"),
        "{text}"
    );
    assert!(
        f.all_units("kind=plan_revision").await.is_empty(),
        "no unit"
    );
    fake.judging();
    let (s, receipt) = submit(&f, &d, "s2").await;
    assert_eq!(s, 201, "{receipt}");
    fake.answering(another_version(&cpu));
    let (s, problem) = approve_as(&f, &f.user(), &receipt["unit"]).await;
    assert_eq!(s, 409, "{problem}");
    assert_eq!(problem["context"]["reason"], "APPLY_REFUSED", "{problem}");
    assert!(
        problem.to_string().contains("QUOTA_RULE_INVALID"),
        "{problem}"
    );
    assert_eq!(
        revision(&f, &d).await["state"],
        "pending",
        "nothing published"
    );
}

/// A draft of the fixture's book whose one item is `sku` on `entry`, with rule `n` of `cpu` on it,
/// sold from today: its revision.
async fn ruled_on(
    f: &Fixture,
    fake: &quota_support::FakeQuotaCatalog,
    book: Uuid,
    sku: Uuid,
    entry: Uuid,
    n: u128,
) -> Uuid {
    let code = format!("q{}", &Uuid::new_v4().simple().to_string()[..8]);
    let (_, revision) = plan_support::plan(f, &code, book).await;
    let (s, item, _) = f
        .call(
            "POST",
            &format!("/plan-revisions/{revision}/items"),
            json!({"sku_id": sku, "price_book_entry_id": entry}),
            None,
            Some(&format!("{code}-item")),
        )
        .await;
    assert_eq!(s, 201, "{item}");
    let item = plan_support::id_of(&item["id"]);
    let (s, read, etag) = f
        .call(
            "GET",
            &format!("/plan-revisions/{revision}"),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{read}");
    let draft = Draft {
        plan: plan_support::id_of(&read["plan_id"]),
        revision,
        etag,
        items: vec![item],
        sku,
        other_sku: sku,
    };
    let cpu = fake.add(vcpu(sku));
    let (s, saved, _) = put(f, &draft, &[rule_json(n, &cpu, item, "10")], "k1").await;
    assert_eq!(s, 200, "{saved}");
    revision
}

/// A revision sold from a later day judges its rules against the SKU version in force that day,
/// not today's: the PUT, the checks, submit's capture and the final approve's capture each send
/// that version. The batch reads one version per SKU and sale date and gives each revision its
/// own: two drafts sold today on the same SKU get today's version.
#[tokio::test]
async fn each_rule_is_judged_against_the_sku_version_on_its_sale_date() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = priced_draft_with_items(&f, &catalog).await;
    let sale = OffsetDateTime::now_utc().date() + Duration::days(30);
    catalog.version(d.sku, 4, &sale.to_string(), catalog.content(d.sku));
    let (s, patched, etag) = f
        .call(
            "PATCH",
            &format!("/plan-revisions/{}", d.revision),
            json!({"available_from": sale.to_string()}),
            Some(&d.etag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{patched}");
    let d = Draft { etag, ..d };
    let cpu = fake.add(vcpu(d.sku));
    let (s, saved, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "10")], "k1").await;
    assert_eq!(s, 200, "{saved}");
    let last = || {
        let requests = fake.requests();
        requests.last().unwrap().rules[0].sku_version.get()
    };
    assert_eq!(last(), 4, "the PUT");
    let (s, checks, _) = f
        .call(
            "GET",
            &format!("/plan-revisions/{}/checks", d.revision),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{checks}");
    assert_eq!(checks["sale_date"], sale.to_string(), "{checks}");
    assert_eq!(last(), 4, "the checks");

    let book: Uuid = plan_support::id_of(&revision(&f, &d).await["book_id"]);
    let entry = plan_support::items(&f, d.revision)
        .await
        .into_iter()
        .find(|item| item.sku_id == d.sku)
        .unwrap()
        .price_book_entry_id
        .unwrap();
    let two = ruled_on(&f, &fake, book, d.sku, entry, 2).await;
    let three = ruled_on(&f, &fake, book, d.sku, entry, 3).await;
    assert_eq!(
        last(),
        3,
        "a draft sold today saves against today's version"
    );
    let (asked, reads) = (
        fake.requests().len(),
        catalog.version_readers.lock().unwrap().len(),
    );
    let (s, batch, _) = f
        .call(
            "GET",
            &format!(
                "/plan-revisions/checks?revision_ids={},{two},{three}",
                d.revision
            ),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{batch}");
    assert_eq!(
        catalog.version_readers.lock().unwrap().len() - reads,
        2,
        "one read per SKU and sale date"
    );
    let batched: Vec<(Uuid, u64)> = fake.requests()[asked..]
        .iter()
        .map(|request| (request.rules[0].rule_id, request.rules[0].sku_version.get()))
        .collect();
    assert_eq!(batched.len(), 3, "one validation per revision with rules");
    for (rule, version) in batched {
        let wanted = if rule == Uuid::from_u128(1) { 4 } else { 3 };
        assert_eq!(version, wanted, "rule {rule}");
    }

    set_quorum(&f, 1).await;
    let (s, receipt) = submit(&f, &d, "s1").await;
    assert_eq!(s, 201, "{receipt}");
    assert_eq!(last(), 4, "submit's capture");
    let (s, vote) = approve_as(&f, &f.user(), &receipt["unit"]).await;
    assert_eq!(s, 200, "{vote}");
    assert_eq!(vote["outcome"], "applied", "{vote}");
    assert_eq!(last(), 4, "the final approve's capture");
    assert_eq!(revision(&f, &d).await["state"], "scheduled");
}

/// Products is asked as the caller at every step, never as a system actor (products P-D-273):
/// the author at the PUT, a reader at the checks, a submitter who is not the author at submit and
/// a voter who is neither at the final approve.
#[tokio::test]
async fn products_is_asked_as_the_caller_at_every_step() {
    let (f, catalog) = plan_support::setup().await;
    let (d, _, fake) = ruled(&f, &catalog).await;
    let (reader, submitter, voter) = (f.user(), f.user(), f.user());
    let (s, checks, _) = f
        .call_as(
            &reader,
            "GET",
            &format!("/plan-revisions/{}/checks", d.revision),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{checks}");
    set_quorum(&f, 1).await;
    let (s, receipt, _) = f
        .call_as(
            &submitter,
            "POST",
            &format!("/plan-revisions/{}/submit", d.revision),
            json!({}),
            None,
            Some("s1"),
        )
        .await;
    assert_eq!(s, 201, "{receipt}");
    let (s, vote) = approve_as(&f, &voter, &receipt["unit"]).await;
    assert_eq!(s, 200, "{vote}");
    assert_eq!(vote["outcome"], "applied", "{vote}");
    let asked: Vec<Uuid> = fake.callers().into_iter().map(|(id, _)| id).collect();
    assert_eq!(
        asked,
        [
            f.ctx.subject_id(),
            reader.subject_id(),
            submitter.subject_id(),
            voter.subject_id()
        ],
        "the PUT, the checks, submit and the final approve"
    );
    assert!(
        fake.callers()
            .iter()
            .all(|(id, kind)| *id != bss_products_sdk::PRICING_SYSTEM_ACTOR
                && kind.as_deref() == Some("user")),
        "{:?}",
        fake.callers()
    );
}
