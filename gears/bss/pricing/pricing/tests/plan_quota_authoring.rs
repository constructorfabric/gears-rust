//! The SDK beside REST (D-540): `PricingQuotaAuthoringClientV1`'s local client answers each request
//! as the endpoints do: the same body, the same refusals and the same version, one test per
//! method, and the gear registers it in the `ClientHub` at init.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]
mod plan_support;
mod quota_support;
pub mod rest_support;
use bss_pricing::api::plan_quota_authoring::{PricingQuotaAuthoringProvider, rules_v1};
use bss_pricing::api::rest::authoring::dto::PricingPlanQuotaRulesDto;
use bss_pricing::domain::plan_quota::digest_from_hex;
use bss_pricing_sdk::plan_quota::{
    PlanQuotaBasis, PlanQuotaRuleV1, PlanRevisionChecksV1, PricingQuotaAuthoringClientV1,
    QuotaDefinitionRef, QuotaSubjectSelector,
};
use bss_products_sdk::quota::catalog::QuotaDefinition;
use quota_support::{
    Draft, disabled, draft_with_items, install, path, priced_draft_with_items, put, ram, rule_json,
    vcpu,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

/// The local client over the fixture's state, under the policy its routers run under.
fn client(f: &plan_support::Fixture) -> PricingQuotaAuthoringProvider {
    PricingQuotaAuthoringProvider::new(
        f.state.clone(),
        Arc::new(plan_support::entry_support::enforcer_for(
            f.ctx.subject_tenant_id(),
        )),
    )
}

/// Rule `n` as the SDK types it: the twin of `quota_support::rule_json`.
fn typed(n: u128, definition: &QuotaDefinition, item: Uuid, amount: &str) -> PlanQuotaRuleV1 {
    PlanQuotaRuleV1 {
        rule_id: Uuid::from_u128(n),
        definition_ref: QuotaDefinitionRef {
            id: definition.definition_id,
            version: NonZeroU64::new(definition.version.get()).unwrap(),
            digest: digest_from_hex(&definition.definition_digest).unwrap(),
        },
        item_id: item,
        dimension_value: None,
        logical_binding_key: "compute".into(),
        subject_selector: QuotaSubjectSelector::ResourceOwner,
        scope_selection: BTreeMap::from([
            ("region".into(), "region-1".into()),
            ("resource_pool".into(), "pool-1".into()),
        ]),
        amount: amount.into(),
        unit_ref: definition.canonical_unit_ref.clone(),
        basis: PlanQuotaBasis::PerPlanUnit,
    }
}

fn version(etag: &str) -> u64 {
    etag.trim_matches('"').parse().unwrap()
}

/// A problem as the wire shows it, without the members a transport adds per request.
fn problem(mut body: Value) -> Value {
    if let Some(members) = body.as_object_mut() {
        members.remove("instance");
        members.remove("trace_id");
    }
    body
}

/// The SDK's refusal is the endpoint's: the same problem, member for member.
fn same_refusal(status: u16, rest: Value, sdk: CanonicalError) {
    assert_eq!(status, sdk.status_code(), "{rest} / {sdk}");
    let sdk = serde_json::to_value(toolkit::api::canonical_prelude::Problem::from(sdk)).unwrap();
    assert_eq!(problem(rest), problem(sdk));
}

/// The SDK's checks in the wire's names, a quota subject's members only where it has them.
fn checks_json(checks: &PlanRevisionChecksV1) -> Value {
    let rows: Vec<Value> = checks
        .checks
        .iter()
        .map(|row| {
            let subjects: Vec<Value> = row
                .subjects
                .iter()
                .map(|s| {
                    let mut subject = json!({
                        "item_id": s.item_id,
                        "sku_id": s.sku_id,
                        "price_book_entry_id": s.price_book_entry_id,
                    });
                    if let Some(rule) = s.quota_rule_id {
                        subject["quota_rule_id"] = json!(rule);
                    }
                    if let Some(field) = &s.field {
                        subject["field"] = json!(field);
                    }
                    subject
                })
                .collect();
            let blocking: Vec<Value> = row
                .blocked_by_prices
                .iter()
                .map(|p| {
                    json!({
                        "unit_id": p.unit_id,
                        "price_id": p.price_id,
                        "price_book_entry_id": p.price_book_entry_id,
                    })
                })
                .collect();
            json!({
                "code": row.code,
                "ok": row.ok,
                "label": row.label,
                "detail": row.detail,
                "info": row.info,
                "blocked_by": row.blocked_by,
                "subjects": subjects,
                "blocked_by_prices": blocking,
            })
        })
        .collect();
    let digest = bss_pricing::infra::usage_policy_wire::digest_text(checks.quota_rules_digest);
    json!({
        "checks": rows,
        "ready": checks.ready,
        "sale_date": checks.sale_date.to_string(),
        "quorum_required": checks.quorum_required,
        "revision_version": checks.revision_version,
        "quota_rules_digest": digest,
    })
}

#[tokio::test]
async fn the_sdk_get_answers_what_rest_answers() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let (cpu, ram) = (fake.add(vcpu(d.sku)), fake.add(ram(d.sku)));
    let sdk = client(&f);
    let empty = sdk.get_quota_rules(&f.ctx, d.revision).await.unwrap();
    assert!(empty.quota_rules.is_empty());
    assert_eq!(empty.revision_version, version(&d.etag));
    let (s, _, _) = put(
        &f,
        &d,
        &[
            rule_json(1, &cpu, d.items[0], "10"),
            rule_json(2, &ram, d.items[1], "20"),
        ],
        "k1",
    )
    .await;
    assert_eq!(s, 200);
    let (s, rest, etag) = f.call("GET", &path(&d), json!(null), None, None).await;
    assert_eq!(s, 200, "{rest}");
    let answered = sdk.get_quota_rules(&f.ctx, d.revision).await.unwrap();
    let wire: PricingPlanQuotaRulesDto = serde_json::from_value(rest).unwrap();
    assert_eq!(rules_v1(wire).unwrap(), answered, "the same body");
    assert_eq!(
        answered.revision_version,
        version(&etag),
        "the same version"
    );
    assert_eq!(answered.quota_rules.len(), 2);
    // The same refusals: an unknown revision, another tenant's and a caller without plan read.
    let unknown = format!("/plan-revisions/{}/quota-rules", Uuid::now_v7());
    let missing = Uuid::parse_str(unknown.split('/').nth(2).unwrap()).unwrap();
    let (s, rest, _) = f.call("GET", &unknown, json!(null), None, None).await;
    same_refusal(
        s,
        rest,
        sdk.get_quota_rules(&f.ctx, missing).await.unwrap_err(),
    );
    let stranger = plan_support::stranger();
    let (s, rest, _) = f
        .call_as(&stranger, "GET", &path(&d), json!(null), None, None)
        .await;
    same_refusal(
        s,
        rest,
        sdk.get_quota_rules(&stranger, d.revision)
            .await
            .unwrap_err(),
    );
    let reader = plan_support::holding(&f, "price:read");
    let (s, rest, _) = f
        .call_as(&reader, "GET", &path(&d), json!(null), None, None)
        .await;
    same_refusal(
        s,
        rest,
        sdk.get_quota_rules(&reader, d.revision).await.unwrap_err(),
    );
}

#[tokio::test]
async fn the_sdk_replace_answers_what_rest_answers() {
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let d = draft_with_items(&f, &catalog).await;
    let (cpu, ram) = (fake.add(vcpu(d.sku)), fake.add(ram(d.sku)));
    let sdk = client(&f);
    let seen = version(&d.etag);
    // A caller with plan read only, and another tenant's caller, are refused alike on both
    // surfaces, before anything is read: Products is not asked.
    for caller in [
        plan_support::holding(&f, "plan:read"),
        plan_support::stranger(),
    ] {
        let (s, rest, _) = f
            .call_as(
                &caller,
                "PUT",
                &path(&d),
                json!({"quota_rules": [rule_json(1, &cpu, d.items[0], "10")]}),
                Some(&d.etag),
                Some("k0"),
            )
            .await;
        assert_eq!(s, 403, "{rest}");
        let refused = sdk
            .replace_quota_rules(
                &caller,
                d.revision,
                seen,
                "k0",
                vec![typed(1, &cpu, d.items[0], "10")],
            )
            .await
            .unwrap_err();
        same_refusal(s, rest, refused);
    }
    assert!(fake.requests().is_empty(), "neither surface asks Products");
    let saved = sdk
        .replace_quota_rules(
            &f.ctx,
            d.revision,
            seen,
            "k1",
            vec![
                typed(2, &ram, d.items[1], "20"),
                typed(1, &cpu, d.items[0], "10"),
            ],
        )
        .await
        .unwrap();
    assert_eq!(saved.revision_version, seen + 1);
    assert_eq!(
        saved
            .quota_rules
            .iter()
            .map(|r| r.rule.rule_id)
            .collect::<Vec<_>>(),
        [Uuid::from_u128(1), Uuid::from_u128(2)]
    );
    // The same key, If-Match and content through REST: the SDK's stored answer, as REST renders
    // it, with the same ETag.
    let (s, rest, etag) = put(
        &f,
        &d,
        &[
            rule_json(2, &ram, d.items[1], "20"),
            rule_json(1, &cpu, d.items[0], "10"),
        ],
        "k1",
    )
    .await;
    assert_eq!(s, 200, "{rest}");
    let wire: PricingPlanQuotaRulesDto = serde_json::from_value(rest).unwrap();
    assert_eq!(rules_v1(wire).unwrap(), saved, "the same body");
    assert_eq!(version(&etag), saved.revision_version, "the same ETag");
    assert_eq!(fake.requests().len(), 1, "the REST call replayed");
    let moved = Draft {
        etag: etag.clone(),
        ..d.clone()
    };
    // The same fence: the old version with a new key is STALE_REVISION on both surfaces.
    let (s, rest, _) = put(&f, &d, &[rule_json(1, &cpu, d.items[0], "11")], "k2").await;
    let refused = sdk
        .replace_quota_rules(
            &f.ctx,
            d.revision,
            seen,
            "k3",
            vec![typed(1, &cpu, d.items[0], "11")],
        )
        .await
        .unwrap_err();
    same_refusal(s, rest, refused);
    // One invalid typed rule is judged with the parse's codes.
    let mut bad = typed(1, &cpu, d.items[0], "1.5");
    bad.logical_binding_key = "not a key".into();
    bad.scope_selection.insert("bad key".into(), "x".into());
    let mut bad_json = rule_json(1, &cpu, d.items[0], "1.5");
    bad_json["logical_binding_key"] = json!("not a key");
    bad_json["scope_selection"]["bad key"] = json!("x");
    let (s, rest, _) = put(&f, &moved, &[bad_json], "k4").await;
    assert_eq!(s, 400, "{rest}");
    let refused = sdk
        .replace_quota_rules(&f.ctx, d.revision, version(&etag), "k5", vec![bad])
        .await
        .unwrap_err();
    same_refusal(s, rest, refused);
    // The same key with other content, and a malformed key.
    let (s, rest, _) = put(&f, &moved, &[], "k1").await;
    let refused = sdk
        .replace_quota_rules(&f.ctx, d.revision, version(&etag), "k1", Vec::new())
        .await
        .unwrap_err();
    same_refusal(s, rest, refused);
    let refused = sdk
        .replace_quota_rules(&f.ctx, d.revision, version(&etag), "", Vec::new())
        .await
        .unwrap_err();
    assert_eq!(refused.status_code(), 400, "{refused}");
    // Another author's draft, and a revision that is no longer a draft.
    let other = f.user();
    let (s, rest, _) = f
        .call_as(
            &other,
            "PUT",
            &path(&moved),
            json!({"quota_rules": []}),
            Some(&etag),
            Some("k6"),
        )
        .await;
    let refused = sdk
        .replace_quota_rules(&other, d.revision, version(&etag), "k6", Vec::new())
        .await
        .unwrap_err();
    same_refusal(s, rest, refused);
    plan_support::lock(&f, d.revision).await;
    let (s, rest, _) = put(&f, &moved, &[], "k7").await;
    let refused = sdk
        .replace_quota_rules(&f.ctx, d.revision, version(&etag), "k8", Vec::new())
        .await
        .unwrap_err();
    same_refusal(s, rest, refused);
    assert!(
        refused_code(&f, &moved)
            .await
            .contains("REVISION_NOT_DRAFT"),
        "a locked draft"
    );
}

/// The code a PUT of the empty set answers now.
async fn refused_code(f: &plan_support::Fixture, d: &Draft) -> String {
    put(f, d, &[], &Uuid::new_v4().to_string())
        .await
        .1
        .to_string()
}

#[tokio::test]
async fn the_sdk_check_answers_what_rest_answers() {
    use bss_products_sdk::quota::validation::{QuotaCheck, QuotaCheckCode, QuotaSetValidation};
    let (f, catalog) = plan_support::setup().await;
    let fake = install(&f);
    let ruled = priced_draft_with_items(&f, &catalog).await;
    let free = priced_draft_with_items(&f, &catalog).await;
    let pending = fake.add(disabled(vcpu(ruled.sku)));
    let (s, _, _) = put(
        &f,
        &ruled,
        &[rule_json(1, &pending, ruled.items[0], "10")],
        "k1",
    )
    .await;
    assert_eq!(s, 200);
    let sdk = client(&f);
    let rest_checks = |revision: Uuid| {
        let f = &f;
        async move {
            f.call(
                "GET",
                &format!("/plan-revisions/{revision}/checks"),
                json!(null),
                None,
                None,
            )
            .await
        }
    };
    for revision in [ruled.revision, free.revision] {
        let (s, rest, _) = rest_checks(revision).await;
        assert_eq!(s, 200, "{rest}");
        let answered = sdk.check_plan_revision(&f.ctx, revision).await.unwrap();
        assert_eq!(checks_json(&answered), rest, "{revision}");
    }
    let answered = sdk
        .check_plan_revision(&f.ctx, ruled.revision)
        .await
        .unwrap();
    assert!(!answered.ready, "a pending definition checks red");
    assert!(
        sdk.check_plan_revision(&f.ctx, free.revision)
            .await
            .unwrap()
            .ready,
        "the same draft without rules is ready"
    );
    fake.answering(QuotaSetValidation {
        rules: Vec::new(),
        checks: vec![QuotaCheck {
            code: QuotaCheckCode::ScopeUnsupported,
            rule_ids: vec![Uuid::from_u128(1)],
            field: "scope_selection.region".into(),
            detail: "not a region of the pool".into(),
        }],
    });
    let (_, rest, _) = rest_checks(ruled.revision).await;
    let answered = sdk
        .check_plan_revision(&f.ctx, ruled.revision)
        .await
        .unwrap();
    assert_eq!(checks_json(&answered), rest);
    let subject = answered
        .checks
        .iter()
        .find(|c| c.code == "QUOTA_SCOPE_UNSUPPORTED")
        .unwrap()
        .subjects[0]
        .clone();
    assert_eq!(
        (subject.quota_rule_id, subject.field.as_deref()),
        (Some(Uuid::from_u128(1)), Some("scope_selection"))
    );
    fake.go_down();
    let (s, rest, _) = rest_checks(ruled.revision).await;
    same_refusal(
        s,
        rest,
        sdk.check_plan_revision(&f.ctx, ruled.revision)
            .await
            .unwrap_err(),
    );
    let missing = Uuid::now_v7();
    let (s, rest, _) = rest_checks(missing).await;
    same_refusal(
        s,
        rest,
        sdk.check_plan_revision(&f.ctx, missing).await.unwrap_err(),
    );
    // The same refusals for another tenant's caller and a caller without plan read.
    let checks_path = format!("/plan-revisions/{}/checks", ruled.revision);
    for caller in [
        plan_support::stranger(),
        plan_support::holding(&f, "price:read"),
    ] {
        let (s, rest, _) = f
            .call_as(&caller, "GET", &checks_path, json!(null), None, None)
            .await;
        same_refusal(
            s,
            rest,
            sdk.check_plan_revision(&caller, ruled.revision)
                .await
                .unwrap_err(),
        );
    }
}

#[tokio::test]
async fn the_hub_resolves_the_quota_authoring_client() {
    let harness = rest_support::Harness::new().await.unwrap();
    let client = harness
        .ctx
        .client_hub()
        .get::<dyn PricingQuotaAuthoringClientV1>()
        .expect("pricing registers its quota authoring client at init");
    let tenant = Uuid::new_v4();
    let author = toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant)
        .subject_type("user")
        .build()
        .unwrap();
    let refused = client
        .get_quota_rules(&author, Uuid::new_v4())
        .await
        .unwrap_err();
    assert_eq!(
        refused.status_code(),
        503,
        "the client authorizes first, and the harness's PDP is down: {refused}"
    );
    let system = toolkit_security::SecurityContext::builder()
        .subject_id(bss_products_sdk::PRICING_SYSTEM_ACTOR)
        .subject_tenant_id(tenant)
        .subject_type("bss-pricing.system")
        .build()
        .unwrap();
    let refused = client
        .replace_quota_rules(&system, Uuid::new_v4(), 1, "k1", Vec::new())
        .await
        .unwrap_err();
    assert_eq!(
        refused.status_code(),
        403,
        "pricing's own system actor is never a caller: {refused}"
    );
}
