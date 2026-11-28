//! The quota catalog's in-process client (P-D-272): it answers what the REST reads answer, as its
//! caller, page by page with the same cursor, and another tenant's version is not found. Its
//! quota-set validation (P-D-275) asks `read` first, refuses a malformed set, reads a set in three
//! statements under the caller's grant and answers the judgement.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::collections::BTreeMap;
use std::sync::Arc;

use super::*;
use crate::api::rest::quota::dto::ProductsQuotaDefinition;
use crate::authz::labels;
use crate::quota_test_support::{
    FailingResolver, GIB_UNIT, QuotaApp, QuotaFakes, VCPU_METRIC, VCPU_UNIT, fresh_code, quota_app,
    quota_app_for, quota_app_recorded, ram_record_in, seed_sku_version, vcpu_record,
    vcpu_record_numbered,
};
use crate::test_support::{
    authed_ctx, body_json, get, problem_code, products_statements, tenant_user, violation_for,
};
use bss_products_sdk::quota::catalog::{QuotaAvailabilityFilter, QuotaBasis, QuotaDefinitionRef};
use bss_products_sdk::quota::validation::{
    QuotaChangeKind, QuotaPriorLimit, QuotaSetRule, QuotaValidationPurpose,
};
use serde_json::Value;

fn client(q: &QuotaApp) -> LocalQuotaCatalogClient {
    LocalQuotaCatalogClient::new(
        q.state.clone(),
        Arc::new(PolicyEnforcer::new(q.pdp.clone())),
    )
}

/// The REST mirror of an SDK definition, as JSON.
fn rest_of(definition: QuotaDefinition) -> Value {
    serde_json::to_value(ProductsQuotaDefinition::from(definition)).unwrap()
}

fn request(sku: Uuid, cursor: Option<String>) -> QuotaDefinitionListRequest {
    QuotaDefinitionListRequest {
        sku_id: sku,
        sku_version: QuotaVersion::new(1).unwrap(),
        subject_type_ref: None,
        resource_type_ref: None,
        profile_id: None,
        locale: Some("ru".to_owned()),
        availability: QuotaAvailabilityFilter::All,
        limit: Some(1),
        cursor,
    }
}

#[tokio::test]
async fn the_sdk_client_answers_what_rest_answers() {
    let tenant = Uuid::new_v4();
    let q = quota_app_for(&[tenant], QuotaFakes::default()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    for i in 0..2 {
        q.approved(tenant, vcpu_record_numbered(i), sku, 1).await;
    }
    let caller = tenant_user(tenant);
    let sdk = client(&q);
    let first = sdk
        .list_quota_definitions(&caller, request(sku, None))
        .await
        .unwrap();
    let filter = format!("sku_id eq {sku} and sku_version eq '1'")
        .replace(' ', "%20")
        .replace('\'', "%27");
    let uri = format!(
        "/bss-products/v1/quota-definitions?$filter={filter}&availability=all&locale=ru&limit=1"
    );
    let rest = body_json(get(&q.app, tenant, &uri).await).await;
    assert_eq!(first.items.len(), 1);
    assert_eq!(rest_of(first.items[0].clone()), rest["items"][0]);
    assert_eq!(
        first.next_cursor.as_deref(),
        rest["page_info"]["next_cursor"].as_str()
    );
    // The SDK's cursor walks on in the SDK and in REST alike.
    let second = sdk
        .list_quota_definitions(&caller, request(sku, first.next_cursor.clone()))
        .await
        .unwrap();
    let rest_second = body_json(
        get(
            &q.app,
            tenant,
            &format!("{uri}&cursor={}", first.next_cursor.unwrap()),
        )
        .await,
    )
    .await;
    assert_eq!(rest_of(second.items[0].clone()), rest_second["items"][0]);
    assert_eq!(second.next_cursor, None);
    let definition = second.items[0].clone();
    let one = sdk
        .get_quota_definition(
            &caller,
            definition.definition_id,
            definition.version,
            Some("ru"),
        )
        .await
        .unwrap();
    assert_eq!(one, definition);
    let rest_one = body_json(
        get(
            &q.app,
            tenant,
            &format!(
                "/bss-products/v1/quota-definitions/{}/versions/1?locale=ru",
                definition.definition_id
            ),
        )
        .await,
    )
    .await;
    assert_eq!(rest_of(one), rest_one);
}

#[tokio::test]
async fn the_sdk_client_refuses_another_tenants_version() {
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let q = quota_app_for(&[tenant, other], QuotaFakes::default()).await;
    let sku = seed_sku_version(&q.dsn, tenant, "COMPUTE", 1).await;
    let (d, version) = q.approved(tenant, vcpu_record_numbered(1), sku, 1).await;
    let sdk = client(&q);
    assert!(
        sdk.get_quota_definition(&tenant_user(tenant), d, version, None)
            .await
            .is_ok()
    );
    let refused = sdk
        .get_quota_definition(&tenant_user(other), d, version, None)
        .await
        .unwrap_err();
    assert!(
        matches!(refused, CanonicalError::NotFound { .. }),
        "{refused:?}"
    );
    let page = sdk
        .list_quota_definitions(&tenant_user(other), request(sku, None))
        .await
        .unwrap();
    assert!(page.items.is_empty(), "another tenant lists none");
}

/// The example vCPU record approved in `tenant` for a fresh SKU at version 3: the definition,
/// its version and the SKU.
async fn approved_vcpu(q: &QuotaApp, tenant: Uuid) -> (Uuid, QuotaVersion, Uuid) {
    let sku = seed_sku_version(&q.dsn, tenant, &fresh_code(), 3).await;
    let (id, version) = q.approved(tenant, vcpu_record(), sku, 3).await;
    (id, version, sku)
}

/// The stored digest of a version, as the caller reads it.
async fn digest_of(q: &QuotaApp, tenant: Uuid, id: Uuid, version: QuotaVersion) -> String {
    client(q)
        .get_quota_definition(&authed_ctx(tenant), id, version, None)
        .await
        .unwrap()
        .definition_digest
}

fn set_rule(
    id: Uuid,
    version: QuotaVersion,
    digest: &str,
    sku: Uuid,
    amount: &str,
) -> QuotaSetRule {
    QuotaSetRule {
        rule_id: Uuid::new_v4(),
        definition: QuotaDefinitionRef {
            id,
            version,
            digest: digest.into(),
        },
        sku_id: sku,
        sku_version: QuotaVersion::new(3).unwrap(),
        logical_binding_key: "compute".into(),
        alternative: None,
        subject_selector: "resource_owner".into(),
        scope_selection: BTreeMap::from([
            ("region".into(), "region-1".into()),
            ("resource_pool".into(), "pool-1".into()),
        ]),
        amount: amount.into(),
        unit_ref: VCPU_UNIT.into(),
        basis: QuotaBasis::PerPlanUnit,
    }
}

fn authoring(rules: Vec<QuotaSetRule>) -> QuotaSetRequest {
    QuotaSetRequest {
        purpose: QuotaValidationPurpose::Authoring,
        change_kind: QuotaChangeKind::InitialSet,
        quantity: None,
        prior: Vec::new(),
        rules,
        locale: None,
    }
}

fn check_codes(answer: &QuotaSetValidation) -> Vec<&'static str> {
    answer.checks.iter().map(|c| c.code.code()).collect()
}

/// A refusal as the wire answers it.
fn problem_of(error: CanonicalError) -> Value {
    serde_json::to_value(toolkit::api::canonical_prelude::Problem::from(error)).unwrap()
}

#[tokio::test]
async fn the_set_validation_answers_the_catalogs_definitions_and_their_availability() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let (id, version, sku) = approved_vcpu(&q, tenant).await;
    let client = client(&q);
    let stored = client
        .get_quota_definition(&authed_ctx(tenant), id, version, None)
        .await
        .unwrap();
    let request = authoring(vec![set_rule(
        id,
        version,
        &stored.definition_digest,
        sku,
        "10",
    )]);
    let answer = client
        .validate_quota_set(&authed_ctx(tenant), request.clone())
        .await
        .unwrap();
    assert_eq!(
        check_codes(&answer),
        ["QUOTA_QUALIFICATION_UNAVAILABLE"],
        "every approved version is disabled until qualification"
    );
    assert_eq!(answer.checks[0].detail, "disabled: qualification_pending");
    q.enable(tenant, id, version).await;
    let answer = client
        .validate_quota_set(&authed_ctx(tenant), request)
        .await
        .unwrap();
    assert!(answer.ok(), "{:?}", answer.checks);
    let resolved = answer.rules[0].definition.as_ref().unwrap();
    assert_eq!(
        (
            resolved.definition_id,
            resolved.version,
            &resolved.definition_digest
        ),
        (
            stored.definition_id,
            stored.version,
            &stored.definition_digest
        ),
        "the stored version, as read"
    );
    assert_eq!(
        resolved.availability.revision.get(),
        2,
        "its current availability"
    );
}

#[tokio::test]
async fn the_set_validation_reads_in_three_statements_after_the_pdp() {
    let tenant = Uuid::new_v4();
    let (q, recorder) = quota_app_recorded(tenant, QuotaFakes::default()).await;
    let sku = seed_sku_version(&q.dsn, tenant, &fresh_code(), 3).await;
    let mut keys = Vec::new();
    for i in 1..=3 {
        let (id, version) = q.approved(tenant, vcpu_record_numbered(i), sku, 3).await;
        keys.push((id, version, digest_of(&q, tenant, id, version).await));
    }
    let before = q.pdp.asked().len();
    recorder.clear();
    // Fifty rules over three distinct definition versions: still three statements.
    let rules = (0..50)
        .map(|n| {
            let (id, version, digest) = &keys[n % keys.len()];
            set_rule(*id, *version, digest, sku, "10")
        })
        .collect::<Vec<_>>();
    let answer = client(&q)
        .validate_quota_set(&authed_ctx(tenant), authoring(rules.clone()))
        .await
        .unwrap();
    assert_eq!(answer.rules.len(), 50);
    for (verdict, rule) in answer.rules.iter().zip(&rules) {
        let definition = verdict.definition.as_ref().expect("each version resolves");
        assert_eq!(
            (definition.definition_id, definition.version),
            (rule.definition.id, rule.definition.version),
            "each rule is answered its own version"
        );
    }
    let statements = products_statements(&recorder);
    assert_eq!(statements.len(), 3, "{statements:#?}");
    assert!(
        statements[2].contains("products_quota_mapping_version"),
        "{statements:#?}"
    );
    assert_eq!(q.pdp.asked().len(), before + 1, "one PDP question");
}

#[tokio::test]
async fn a_definition_outside_the_grant_is_unknown_to_the_set() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let (id, version, sku) = approved_vcpu(&q, tenant).await;
    let digest = digest_of(&q, tenant, id, version).await;
    q.pdp.narrow_to(&[Uuid::new_v4()]);
    let answer = client(&q)
        .validate_quota_set(
            &authed_ctx(tenant),
            authoring(vec![set_rule(id, version, &digest, sku, "10")]),
        )
        .await
        .unwrap();
    assert_eq!(check_codes(&answer), ["QUOTA_RULE_INVALID"]);
    assert_eq!(answer.checks[0].field, "definition_ref");
    assert!(
        answer.rules[0].definition.is_none(),
        "no content of a definition outside the grant"
    );
}

/// A grant narrowed to the definition's own id admits it to the set: the definition, its
/// availability and its mapping (read with the grant's tenant-only scope) all read under the
/// narrowing, and the enabled version resolves with no check.
#[tokio::test]
async fn a_grant_narrowed_to_the_definition_resolves_it_in_the_set() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let (id, version, sku) = approved_vcpu(&q, tenant).await;
    let digest = digest_of(&q, tenant, id, version).await;
    q.enable(tenant, id, version).await;
    q.pdp.narrow_to(&[id]);
    let answer = client(&q)
        .validate_quota_set(
            &authed_ctx(tenant),
            authoring(vec![set_rule(id, version, &digest, sku, "10")]),
        )
        .await
        .unwrap();
    assert!(answer.ok(), "{:?}", answer.checks);
    let resolved = answer.rules[0]
        .definition
        .as_ref()
        .expect("the definition the grant names resolves");
    assert_eq!((resolved.definition_id, resolved.version), (id, version));
    assert_eq!(
        resolved.availability.revision.get(),
        2,
        "its current availability, read under the narrowing"
    );
}

/// The validation asks `read` on `quota_definition` for the caller's tenant only, once.
#[tokio::test]
async fn validation_asks_read_on_quota_definition_for_the_callers_tenant() {
    use authz_resolver_sdk::models::TenantMode;
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let (id, version, sku) = approved_vcpu(&q, tenant).await;
    let digest = digest_of(&q, tenant, id, version).await;
    let before = q.pdp.asked().len();
    client(&q)
        .validate_quota_set(
            &authed_ctx(tenant),
            authoring(vec![set_rule(id, version, &digest, sku, "10")]),
        )
        .await
        .unwrap();
    let read = (
        labels::QUOTA_DEFINITION.to_owned(),
        "read".to_owned(),
        Some(TenantMode::RootOnly),
    );
    assert_eq!(q.pdp.asked()[before..], [read]);
}

#[tokio::test]
async fn a_malformed_set_is_400_validation() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let (id, version, sku) = approved_vcpu(&q, tenant).await;
    let digest = digest_of(&q, tenant, id, version).await;
    let one = set_rule(id, version, &digest, sku, "10");
    let mut acceptance = authoring(vec![one.clone()]);
    acceptance.purpose = QuotaValidationPurpose::Acceptance;
    let mut too_many = authoring(Vec::new());
    too_many.rules = (0..201)
        .map(|_| set_rule(id, version, &digest, sku, "10"))
        .collect();
    let mut repeated_prior = authoring(vec![one.clone()]);
    repeated_prior.change_kind = QuotaChangeKind::Increase;
    let prior = QuotaPriorLimit {
        logical_binding_key: "compute".into(),
        metric_ref: VCPU_METRIC.into(),
        unit_ref: VCPU_UNIT.into(),
        amount: "5".into(),
    };
    repeated_prior.prior = vec![prior.clone(), prior];
    for (request, field) in [
        (too_many, "rules"),
        (authoring(vec![one.clone(), one]), "rules[1].rule_id"),
        (acceptance, "quantity"),
        (repeated_prior, "prior[1]"),
    ] {
        let refused = client(&q)
            .validate_quota_set(&authed_ctx(tenant), request)
            .await
            .unwrap_err();
        let body = problem_of(refused);
        assert_eq!(body["status"], 400, "{body}");
        assert_eq!(problem_code(&body), "VALIDATION", "{body}");
        assert!(violation_for(&body, field).is_some(), "{field}: {body}");
        assert_eq!(body["context"]["resource_type"], labels::QUOTA_DEFINITION);
    }
}

#[tokio::test]
async fn a_pdp_outage_is_503_for_a_set() {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let (id, version, sku) = approved_vcpu(&q, tenant).await;
    let digest = digest_of(&q, tenant, id, version).await;
    let unreachable = LocalQuotaCatalogClient::new(
        q.state.clone(),
        Arc::new(PolicyEnforcer::new(Arc::new(FailingResolver))),
    );
    let refused = unreachable
        .validate_quota_set(
            &authed_ctx(tenant),
            authoring(vec![set_rule(id, version, &digest, sku, "10")]),
        )
        .await
        .unwrap_err();
    let body = problem_of(refused);
    assert_eq!(body["status"], 503, "{body}");
}

#[tokio::test]
async fn another_tenants_definition_is_unknown_to_the_set() {
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let q = quota_app_for(&[tenant, other], QuotaFakes::default()).await;
    let (id, version, sku) = approved_vcpu(&q, other).await;
    let digest = digest_of(&q, other, id, version).await;
    let answer = client(&q)
        .validate_quota_set(
            &authed_ctx(tenant),
            authoring(vec![set_rule(id, version, &digest, sku, "10")]),
        )
        .await
        .unwrap();
    assert_eq!(check_codes(&answer), ["QUOTA_RULE_INVALID"]);
    assert_eq!(answer.checks[0].field, "definition_ref");
    assert!(
        answer.rules[0].definition.is_none(),
        "no content of another tenant's version"
    );
}

/// Runs `statements` on the test database with the definition version's no-update trigger
/// dropped, as only a damaged store would.
async fn damage(q: &QuotaApp, statements: &[&str]) {
    use sea_orm::{ConnectionTrait, Database};
    let raw = Database::connect(&*q.dsn).await.unwrap();
    raw.execute_unprepared("DROP TRIGGER products_quota_definition_version_no_update")
        .await
        .unwrap();
    for statement in statements {
        raw.execute_unprepared(statement).await.unwrap();
    }
    raw.close().await.unwrap();
}

/// The validation of one rule on the example vCPU version, after `statements` damaged it: the
/// error, and the definition id.
async fn validated_after(statements: &[&str]) -> (CanonicalError, Uuid, Uuid) {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let (id, version, sku) = approved_vcpu(&q, tenant).await;
    let digest = digest_of(&q, tenant, id, version).await;
    damage(&q, statements).await;
    let refused = client(&q)
        .validate_quota_set(
            &authed_ctx(tenant),
            authoring(vec![set_rule(id, version, &digest, sku, "10")]),
        )
        .await
        .expect_err("a stored row that does not read is no answer");
    (refused, id, tenant)
}

/// A 500 naming no row: the fixed sentence on the wire, the row's table, tenant and id in the
/// log.
fn assert_corrupt(refused: CanonicalError, id: Uuid) {
    let body = problem_of(refused);
    assert_eq!(body["status"], 500, "{body}");
    assert!(
        body.to_string()
            .contains("a stored quota row does not read"),
        "{body}"
    );
    assert!(!body.to_string().contains(&id.to_string()), "{body}");
    assert_eq!(body["context"]["resource_type"], labels::QUOTA_DEFINITION);
}

/// A resolved version whose content does not decode is a 500, never a partial answer
/// (P-D-275).
#[tokio::test]
async fn a_stored_definition_that_does_not_read_is_500_for_a_set() {
    let (logs, guard) = crate::quota_test_support::LogCapture::install();
    let (refused, id, tenant) = validated_after(&[
        "UPDATE products_quota_definition_version SET content_json = '{\"metric\": 1}'",
    ])
    .await;
    drop(guard);
    assert_corrupt(refused, id);
    assert!(
        logs.lines().iter().any(|line| {
            line.starts_with("a stored quota row does not read")
                && line.contains("products_quota_definition_version")
                && line.contains(&tenant.to_string())
                && line.contains(&id.to_string())
        }),
        "{:?}",
        logs.lines()
    );
}

/// An allowed unit whose stored bound or factor does not read is the catalog's defect, a 500,
/// not the author's `QUOTA_AMOUNT_INVALID` (P-D-275).
#[tokio::test]
async fn a_stored_unit_grid_that_does_not_read_is_500_for_a_set() {
    for (member, text) in [
        ("min", "zero"),
        ("step", "1.5"),
        ("max", "-1"),
        ("numerator", "0"),
        ("denominator", "two"),
    ] {
        let statement = format!(
            "UPDATE products_quota_definition_version SET content_json = \
             json_set(content_json, '$.allowed_units[0].{member}', '{text}')"
        );
        let (refused, id, _) = validated_after(&[&statement]).await;
        assert_corrupt(refused, id);
    }
}

/// A caller without the grant is 403 before anything is read: even a malformed set is not
/// judged, and no statement runs (P-D-275).
#[tokio::test]
async fn a_caller_without_the_grant_is_403_before_the_request_is_read() {
    let tenant = Uuid::new_v4();
    let (q, recorder) = quota_app_recorded(tenant, QuotaFakes::default()).await;
    let (id, version, sku) = approved_vcpu(&q, tenant).await;
    let digest = digest_of(&q, tenant, id, version).await;
    let stranger = authed_ctx(Uuid::new_v4());
    let one = authoring(vec![set_rule(id, version, &digest, sku, "10")]);
    let mut too_many = authoring(Vec::new());
    too_many.rules = (0..201)
        .map(|_| set_rule(id, version, &digest, sku, "10"))
        .collect();
    for request in [too_many, one] {
        recorder.clear();
        let refused = client(&q)
            .validate_quota_set(&stranger, request)
            .await
            .unwrap_err();
        let body = problem_of(refused);
        assert_eq!(body["status"], 403, "{body}");
        let statements = products_statements(&recorder);
        assert!(statements.is_empty(), "{statements:#?}");
    }
}

/// vCPU and RAM approved on `targets` (one target for both, or one each) for one SKU, enabled:
/// the validation of one rule on each in one binding, and the two rules.
async fn vcpu_and_ram_on(two_targets: bool) -> (QuotaSetValidation, [Uuid; 2]) {
    let tenant = Uuid::new_v4();
    let q = quota_app(tenant, QuotaFakes::default()).await;
    let sku = seed_sku_version(&q.dsn, tenant, &fresh_code(), 3).await;
    let target = q.fresh_target(tenant).await;
    let ram_target = if two_targets {
        q.fresh_target(tenant).await
    } else {
        target
    };
    let mut rules = Vec::new();
    for (on, record, unit, amount) in [
        (target, vcpu_record(), VCPU_UNIT, "10"),
        (ram_target, ram_record_in(GIB_UNIT), GIB_UNIT, "16"),
    ] {
        let (id, version) = q.approved_on(tenant, on, record, sku, 3).await;
        q.enable(tenant, id, version).await;
        let digest = digest_of(&q, tenant, id, version).await;
        let mut rule = set_rule(id, version, &digest, sku, amount);
        rule.unit_ref = unit.into();
        rules.push(rule);
    }
    let ids = [rules[0].rule_id, rules[1].rule_id];
    let answer = client(&q)
        .validate_quota_set(&authed_ctx(tenant), authoring(rules))
        .await
        .unwrap();
    (answer, ids)
}

/// Two real definitions on one adapter target share a binding: their stored profiles, without
/// the change support, are one content (P-D-276).
#[tokio::test]
async fn vcpu_and_ram_approved_on_one_target_share_a_binding() {
    let (answer, _) = vcpu_and_ram_on(false).await;
    assert!(answer.ok(), "{:?}", answer.checks);
}

/// The same two definitions on two adapter targets cannot share a binding: one check on
/// `logical_binding_key` naming both rules (P-D-276).
#[tokio::test]
async fn vcpu_and_ram_approved_on_two_targets_are_refused_in_one_binding() {
    let (answer, ids) = vcpu_and_ram_on(true).await;
    let found: Vec<(&str, &str, Vec<Uuid>)> = answer
        .checks
        .iter()
        .map(|c| (c.code.code(), c.field.as_str(), c.rule_ids.clone()))
        .collect();
    assert_eq!(
        found,
        [(
            "QUOTA_SET_INCOMPATIBLE",
            "logical_binding_key",
            ids.to_vec()
        )]
    );
}
