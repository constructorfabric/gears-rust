//! The quota-set judgement (P-D-275, P-D-276): each rule against its resolved definition, the
//! rules of one binding together, the change kinds, and a malformed request refused before any
//! judgement.
#![allow(clippy::unwrap_used)]
use super::*;
use crate::quota_test_support::{
    ALLOCATED_VCPU, BYTES_UNIT, GB_UNIT, GIB_UNIT, RAM_METRIC, REGIONAL_POOL, VCPU_METRIC,
    VCPU_UNIT,
};
use bss_products_sdk::quota::catalog::{
    QuotaAvailabilityState, QuotaBasis, QuotaDefinition, QuotaDefinitionRef, QuotaVersion,
};
use bss_products_sdk::quota::validation::{
    QuotaChangeKind, QuotaPriorLimit, QuotaSetRequest, QuotaSetRule, QuotaSetValidation,
    QuotaValidationPurpose,
};
use serde_json::json;
use std::collections::BTreeMap;
use uuid::Uuid;

const SKU: Uuid = Uuid::from_u128(0x51);
const VCPU_DEF: Uuid = Uuid::from_u128(0xd1);
const RAM_DEF: Uuid = Uuid::from_u128(0xd2);
const TARGET: Uuid = Uuid::from_u128(0x7a);

fn v(n: u64) -> QuotaVersion {
    QuotaVersion::new(n).unwrap()
}

/// One allowed unit: `(unit_ref, numerator, min, step, max)`.
type Unit<'a> = (&'a str, &'a str, &'a str, &'a str, Option<&'a str>);

/// An enabled definition approved at SKU version 3, in `canonical` with `units`.
fn definition(id: Uuid, metric: &str, canonical: &str, units: &[Unit<'_>]) -> QuotaDefinition {
    serde_json::from_value(json!({
        "definition_id": id, "version": "1", "definition_digest": "a".repeat(64),
        "sku_id": SKU, "sku_version": "3", "metric_ref": metric, "metric_digest": "b".repeat(64),
        "quota_kind": "allocation", "display_name": "Metric", "description": "Metric",
        "locale": "en",
        "canonical_unit_ref": canonical,
        "allowed_units": units.iter().map(|(u, n, min, step, max)| json!({
            "unit_ref": u, "digest": "c".repeat(64), "symbol": "u", "numerator": n,
            "denominator": "1", "min": min, "step": step, "max": max })).collect::<Vec<_>>(),
        "subject_type_ref": "gts.cf.core.compute.tenant.v1~",
        "resource_type_refs": ["gts.cf.core.compute.vm.v1~"],
        "mapping_ref": {"id": Uuid::from_u128(0xa1), "version": "1", "digest": "d".repeat(64)},
        "profile_ref": {"id": Uuid::from_u128(0xb1), "version": "1", "digest": "e".repeat(64)},
        "qualification_ref": null,
        "accounting_policy_ref": ALLOCATED_VCPU, "accounting_policy_digest": "2".repeat(64),
        "accounting_policy": {"id": ALLOCATED_VCPU, "quantity_basis": "allocated_capacity",
            "aggregation": "sum", "description": {"en": "Counts allocated capacity."}},
        "scope_policy_ref": REGIONAL_POOL, "scope_policy_digest": "3".repeat(64),
        "scope_policy": {"id": REGIONAL_POOL, "subject_selector": "resource_owner",
            "aggregation": "single_verified_scope",
            "required_dimensions": ["region", "resource_pool"]},
        "change_support": {"initial_set": true, "increase": true},
        "profile_conditions": "Counts stopped VMs.", "supported_bases": ["Fixed", "PerPlanUnit"],
        "availability": {"state": "enabled", "revision": "2",
            "observed_at": "2026-11-21T00:00:00Z", "reason_code": null}
    }))
    .unwrap()
}

fn vcpu() -> QuotaDefinition {
    definition(
        VCPU_DEF,
        VCPU_METRIC,
        VCPU_UNIT,
        &[(VCPU_UNIT, "1", "0", "1", Some("1024"))],
    )
}

fn ram() -> QuotaDefinition {
    // Bytes canonical; GiB and GB plan units with their bounds converted (P-D-270).
    definition(
        RAM_DEF,
        RAM_METRIC,
        BYTES_UNIT,
        &[
            (GIB_UNIT, "1073741824", "0", "1", Some("512")),
            (GB_UNIT, "1000000000", "0", "1", None),
        ],
    )
}

fn resolved(defs: &[QuotaDefinition]) -> BTreeMap<(Uuid, QuotaVersion), ResolvedDefinition> {
    defs.iter()
        .map(|d| {
            (
                (d.definition_id, d.version),
                ResolvedDefinition {
                    definition: d.clone(),
                    target_id: TARGET,
                    profile: json!({"profile": "bounded_allocation",
                        "aggregation": "single_verified_scope",
                        "accounting": "allocated_capacity", "required_operations": {"apply": true},
                        "change_support": {"initial_set": true,
                            "increase": d.change_support.increase}}),
                },
            )
        })
        .collect()
}

fn rule(d: &QuotaDefinition, amount: &str, unit: &str, basis: QuotaBasis) -> QuotaSetRule {
    QuotaSetRule {
        rule_id: Uuid::new_v4(),
        definition: QuotaDefinitionRef {
            id: d.definition_id,
            version: d.version,
            digest: d.definition_digest.clone(),
        },
        sku_id: SKU,
        sku_version: v(3),
        logical_binding_key: "compute".into(),
        alternative: None,
        subject_selector: "resource_owner".into(),
        scope_selection: BTreeMap::from([
            ("region".into(), "region-1".into()),
            ("resource_pool".into(), "pool-1".into()),
        ]),
        amount: amount.into(),
        unit_ref: unit.into(),
        basis,
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

fn codes(answer: &QuotaSetValidation) -> Vec<(&'static str, String)> {
    answer
        .checks
        .iter()
        .map(|c| (c.code.code(), c.field.clone()))
        .collect()
}

/// The `change_kind` checks: each detail with the rules it names.
fn change_checks(answer: &QuotaSetValidation) -> Vec<(&str, Vec<Uuid>)> {
    answer
        .checks
        .iter()
        .filter(|c| c.field == "change_kind")
        .map(|c| (c.detail.as_str(), c.rule_ids.clone()))
        .collect()
}

fn prior(amount: &str) -> QuotaPriorLimit {
    QuotaPriorLimit {
        logical_binding_key: "compute".into(),
        metric_ref: VCPU_METRIC.into(),
        unit_ref: VCPU_UNIT.into(),
        amount: amount.into(),
    }
}

#[test]
fn an_exact_valid_rule_passes_with_its_definition() {
    let answer = judge(
        &authoring(vec![rule(
            &vcpu(),
            "10",
            VCPU_UNIT,
            QuotaBasis::PerPlanUnit,
        )]),
        &resolved(&[vcpu()]),
    );
    assert!(answer.ok(), "{:?}", answer.checks);
    assert_eq!(
        answer.rules[0].definition.as_ref().unwrap().definition_id,
        VCPU_DEF
    );
    assert_eq!(answer.rules[0].limit, None);
}

#[test]
fn an_unknown_definition_is_rule_invalid_without_its_content() {
    let answer = judge(
        &authoring(vec![rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed)]),
        &BTreeMap::new(),
    );
    assert_eq!(
        codes(&answer),
        [("QUOTA_RULE_INVALID", "definition_ref".to_owned())]
    );
    assert!(answer.rules[0].definition.is_none());
}

#[test]
fn a_changed_digest_is_definition_changed() {
    let mut r = rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed);
    r.definition.digest = "f".repeat(64);
    assert_eq!(
        codes(&judge(&authoring(vec![r]), &resolved(&[vcpu()]))),
        [(
            "QUOTA_DEFINITION_CHANGED",
            "definition_ref.digest".to_owned()
        )]
    );
}

#[test]
fn a_sku_version_before_the_approval_is_rule_invalid() {
    let mut r = rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed);
    r.sku_version = v(2);
    assert_eq!(
        codes(&judge(&authoring(vec![r.clone()]), &resolved(&[vcpu()]))),
        [("QUOTA_RULE_INVALID", "definition_ref".to_owned())]
    );
    r.sku_version = v(9);
    assert!(
        judge(&authoring(vec![r.clone()]), &resolved(&[vcpu()])).ok(),
        "a later SKU version is covered"
    );
    r.sku_id = Uuid::from_u128(0x52);
    assert_eq!(
        codes(&judge(&authoring(vec![r]), &resolved(&[vcpu()]))),
        [("QUOTA_RULE_INVALID", "definition_ref".to_owned())],
        "another SKU"
    );
}

#[test]
fn a_unit_outside_the_allowed_units_is_rule_invalid() {
    let answer = judge(
        &authoring(vec![rule(&ram(), "20", VCPU_UNIT, QuotaBasis::Fixed)]),
        &resolved(&[ram()]),
    );
    assert_eq!(
        codes(&answer),
        [("QUOTA_RULE_INVALID", "unit_ref".to_owned())]
    );
    let mut fixed_only = vcpu();
    fixed_only.supported_bases = vec![QuotaBasis::Fixed];
    let answer = judge(
        &authoring(vec![rule(
            &fixed_only,
            "10",
            VCPU_UNIT,
            QuotaBasis::PerPlanUnit,
        )]),
        &resolved(&[fixed_only]),
    );
    assert_eq!(codes(&answer), [("QUOTA_RULE_INVALID", "basis".to_owned())]);
}

#[test]
fn amounts_off_the_grid_or_not_canonical_are_amount_invalid() {
    for amount in [
        "1.5",
        "-1",
        "1e3",
        "010",
        "",
        "2000",
        "340282366920938463463374607431768211456",
    ] {
        let answer = judge(
            &authoring(vec![rule(&vcpu(), amount, VCPU_UNIT, QuotaBasis::Fixed)]),
            &resolved(&[vcpu()]),
        );
        assert_eq!(
            codes(&answer),
            [("QUOTA_AMOUNT_INVALID", "amount".to_owned())],
            "{amount:?}"
        );
    }
}

#[test]
fn zero_is_an_explicit_limit_when_the_minimum_is_zero() {
    assert!(
        judge(
            &authoring(vec![rule(&vcpu(), "0", VCPU_UNIT, QuotaBasis::Fixed)]),
            &resolved(&[vcpu()])
        )
        .ok()
    );
    let mut d = vcpu();
    d.allowed_units[0].min = "2".into();
    d.allowed_units[0].step = "2".into();
    let answer = judge(
        &authoring(vec![rule(&d, "0", VCPU_UNIT, QuotaBasis::Fixed)]),
        &resolved(&[d.clone()]),
    );
    assert_eq!(
        codes(&answer),
        [("QUOTA_AMOUNT_INVALID", "amount".to_owned())]
    );
    assert!(
        judge(
            &authoring(vec![rule(&d, "4", VCPU_UNIT, QuotaBasis::Fixed)]),
            &resolved(&[d.clone()])
        )
        .ok()
    );
    let answer = judge(
        &authoring(vec![rule(&d, "5", VCPU_UNIT, QuotaBasis::Fixed)]),
        &resolved(&[d]),
    );
    assert_eq!(
        codes(&answer),
        [("QUOTA_AMOUNT_INVALID", "amount".to_owned())],
        "off the step"
    );
}

#[test]
fn an_unbounded_definition_still_needs_a_finite_amount() {
    // GB has no max: any canonical integer on its grid passes; "unlimited" is not an amount.
    assert!(
        judge(
            &authoring(vec![rule(&ram(), "20", GB_UNIT, QuotaBasis::PerPlanUnit)]),
            &resolved(&[ram()])
        )
        .ok()
    );
    let answer = judge(
        &authoring(vec![rule(
            &ram(),
            "unlimited",
            GB_UNIT,
            QuotaBasis::PerPlanUnit,
        )]),
        &resolved(&[ram()]),
    );
    assert_eq!(
        codes(&answer),
        [("QUOTA_AMOUNT_INVALID", "amount".to_owned())]
    );
}

#[test]
fn per_plan_unit_limits_are_judged_at_the_acceptance_quantity() {
    let mut request = authoring(vec![rule(
        &vcpu(),
        "600",
        VCPU_UNIT,
        QuotaBasis::PerPlanUnit,
    )]);
    request.purpose = QuotaValidationPurpose::Acceptance;
    request.quantity = Some("2".into());
    let answer = judge(&request, &resolved(&[vcpu()]));
    assert_eq!(
        codes(&answer),
        [("QUOTA_AMOUNT_INVALID", "quantity".to_owned())],
        "1200 is above 1024"
    );
    assert_eq!(answer.rules[0].limit, None);
    request.rules[0].amount = "10".into();
    let answer = judge(&request, &resolved(&[vcpu()]));
    assert!(answer.ok());
    assert_eq!(answer.rules[0].limit.as_deref(), Some("20"));
    request.rules[0].basis = QuotaBasis::Fixed;
    assert_eq!(
        judge(&request, &resolved(&[vcpu()])).rules[0]
            .limit
            .as_deref(),
        Some("10"),
        "Fixed applies once"
    );
}

#[test]
fn an_overflow_is_amount_invalid() {
    // GB has no max and a factor of 10^9 bytes: only 128 bits bound it.
    let ten_to = |n: usize| format!("1{}", "0".repeat(n));
    let answer = judge(
        &authoring(vec![rule(&ram(), &ten_to(30), GB_UNIT, QuotaBasis::Fixed)]),
        &resolved(&[ram()]),
    );
    assert_eq!(
        codes(&answer),
        [("QUOTA_AMOUNT_INVALID", "amount".to_owned())],
        "10^30 GB is 10^39 bytes"
    );
    let mut request = authoring(vec![rule(
        &ram(),
        &ten_to(20),
        GB_UNIT,
        QuotaBasis::PerPlanUnit,
    )]);
    request.purpose = QuotaValidationPurpose::Acceptance;
    for (quantity, why) in [
        (19, "10^20 GB times 10^19 does not fit"),
        (10, "10^30 GB fits, its 10^39 bytes do not"),
    ] {
        request.quantity = Some(ten_to(quantity));
        let answer = judge(&request, &resolved(&[ram()]));
        assert_eq!(
            codes(&answer),
            [("QUOTA_AMOUNT_INVALID", "quantity".to_owned())],
            "{why}"
        );
        assert!(
            answer.checks[0]
                .detail
                .ends_with("does not fit in 128 bits"),
            "{why}: {}",
            answer.checks[0].detail
        );
        assert_eq!(answer.rules[0].limit, None, "{why}");
    }
    request.quantity = Some(ten_to(9));
    let answer = judge(&request, &resolved(&[ram()]));
    assert!(answer.ok(), "10^29 GB is 10^38 bytes: {:?}", answer.checks);
    assert_eq!(answer.rules[0].limit, Some(ten_to(29)));
}

#[test]
fn an_amount_that_does_not_convert_exactly_is_unit_inexact() {
    // A unit worth 1/1000 of the canonical unit: 500 of it is half a canonical unit.
    let mut d = vcpu();
    d.allowed_units[0].denominator = "1000".into();
    let answer = judge(
        &authoring(vec![rule(&d, "500", VCPU_UNIT, QuotaBasis::Fixed)]),
        &resolved(&[d.clone()]),
    );
    assert_eq!(
        codes(&answer),
        [("QUOTA_UNIT_INEXACT", "amount".to_owned())]
    );
    assert!(
        judge(
            &authoring(vec![rule(&d, "1000", VCPU_UNIT, QuotaBasis::Fixed)]),
            &resolved(&[d])
        )
        .ok()
    );
}

#[test]
fn scope_selection_must_name_exactly_the_required_dimensions() {
    let mut missing = rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed);
    missing.scope_selection.remove("resource_pool");
    let mut extra = rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed);
    extra.scope_selection.insert("zone".into(), "z1".into());
    let mut empty = rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed);
    empty.scope_selection.insert("region".into(), String::new());
    let mut long = rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed);
    long.scope_selection
        .insert("region".into(), "r".repeat(256));
    let mut unprintable = rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed);
    unprintable
        .scope_selection
        .insert("resource_pool".into(), "pool\t1".into());
    let mut subject = rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed);
    subject.subject_selector = "tenant".into();
    for (r, field) in [
        (missing, "scope_selection.resource_pool"),
        (extra, "scope_selection.zone"),
        (empty, "scope_selection.region"),
        (long, "scope_selection.region"),
        (unprintable, "scope_selection.resource_pool"),
        (subject, "subject_selector"),
    ] {
        let answer = judge(&authoring(vec![r]), &resolved(&[vcpu()]));
        assert_eq!(
            codes(&answer),
            [("QUOTA_SCOPE_UNSUPPORTED", field.to_owned())]
        );
    }
}

#[test]
fn rules_of_one_binding_need_one_compatible_profile_and_one_adapter_target() {
    let cpu = rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::PerPlanUnit);
    let mem = rule(&ram(), "20", GB_UNIT, QuotaBasis::PerPlanUnit);
    let mut both = resolved(&[vcpu(), ram()]);
    assert!(
        judge(&authoring(vec![cpu.clone(), mem.clone()]), &both).ok(),
        "10 vCPU and 20 GB on one binding"
    );
    both.get_mut(&(RAM_DEF, v(1))).unwrap().target_id = Uuid::from_u128(0x7b);
    let answer = judge(&authoring(vec![cpu.clone(), mem.clone()]), &both);
    assert_eq!(
        codes(&answer),
        [("QUOTA_SET_INCOMPATIBLE", "logical_binding_key".to_owned())]
    );
    assert_eq!(answer.checks[0].rule_ids, [cpu.rule_id, mem.rule_id]);
    let mut other_profile = resolved(&[vcpu(), ram()]);
    other_profile.get_mut(&(RAM_DEF, v(1))).unwrap().profile["accounting"] = json!("other");
    assert_eq!(
        codes(&judge(
            &authoring(vec![cpu.clone(), mem.clone()]),
            &other_profile
        )),
        [("QUOTA_SET_INCOMPATIBLE", "logical_binding_key".to_owned())]
    );
    let mut other_change_support = resolved(&[vcpu(), ram()]);
    other_change_support
        .get_mut(&(RAM_DEF, v(1)))
        .unwrap()
        .profile["change_support"]["increase"] = json!(false);
    assert!(
        judge(&authoring(vec![cpu, mem]), &other_change_support).ok(),
        "the change support is not compared"
    );
}

#[test]
fn rules_of_one_binding_share_one_scope_selection() {
    let cpu = rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed);
    let mut mem = rule(&ram(), "20", GB_UNIT, QuotaBasis::Fixed);
    mem.scope_selection
        .insert("region".into(), "region-2".into());
    // A binding installs one target and one scope, whatever the rule's item or dimension value:
    // one value on both, an item without a dimension beside a `large` one, two values.
    for values in [
        (None, None),
        (None, Some("large")),
        (Some("small"), Some("large")),
    ] {
        let mut cpu = cpu.clone();
        let mut mem = mem.clone();
        cpu.alternative = values.0.map(str::to_owned);
        mem.alternative = values.1.map(str::to_owned);
        let answer = judge(
            &authoring(vec![cpu.clone(), mem.clone()]),
            &resolved(&[vcpu(), ram()]),
        );
        assert_eq!(
            codes(&answer),
            [("QUOTA_SET_INCOMPATIBLE", "scope_selection".to_owned())],
            "{values:?}"
        );
        assert_eq!(answer.checks[0].rule_ids, [cpu.rule_id, mem.rule_id]);
        mem.scope_selection = cpu.scope_selection.clone();
        assert!(
            judge(&authoring(vec![cpu, mem]), &resolved(&[vcpu(), ram()])).ok(),
            "one scope on both: {values:?}"
        );
    }
}

#[test]
fn a_definition_not_enabled_is_qualification_unavailable() {
    let mut d = vcpu();
    d.availability.state = QuotaAvailabilityState::Disabled;
    d.availability.reason_code = Some("qualification_pending".into());
    let answer = judge(
        &authoring(vec![rule(&d, "10", VCPU_UNIT, QuotaBasis::Fixed)]),
        &resolved(&[d]),
    );
    assert_eq!(
        codes(&answer),
        [(
            "QUOTA_QUALIFICATION_UNAVAILABLE",
            "definition_ref".to_owned()
        )]
    );
    assert_eq!(answer.checks[0].detail, "disabled: qualification_pending");
}

#[test]
fn an_increase_needs_increase_support_and_a_decrease_is_refused() {
    let mut request = authoring(vec![rule(&vcpu(), "20", VCPU_UNIT, QuotaBasis::Fixed)]);
    let id = request.rules[0].rule_id;
    request.change_kind = QuotaChangeKind::Increase;
    request.prior = vec![prior("30")];
    let answer = judge(&request, &resolved(&[vcpu()]));
    assert_eq!(
        codes(&answer),
        [("QUOTA_SET_INCOMPATIBLE", "change_kind".to_owned())]
    );
    assert_eq!(
        change_checks(&answer),
        [("a cap decrease is refused", vec![id])]
    );
    request.prior = vec![prior("10")];
    let mut initial_only = vcpu();
    initial_only.change_support.increase = false;
    let answer = judge(&request, &resolved(&[initial_only]));
    assert_eq!(
        codes(&answer),
        [("QUOTA_SET_INCOMPATIBLE", "change_kind".to_owned())]
    );
    assert_eq!(
        change_checks(&answer),
        [("the profile does not support an increase", vec![id])]
    );
    assert!(judge(&request, &resolved(&[vcpu()])).ok());
    request.prior = vec![prior("20")];
    let answer = judge(&request, &resolved(&[vcpu()]));
    assert_eq!(
        codes(&answer),
        [("QUOTA_SET_INCOMPATIBLE", "change_kind".to_owned())],
        "nothing increases"
    );
    assert_eq!(
        change_checks(&answer),
        [("nothing increases: the change is unchanged", vec![id])]
    );
    request.prior[0].metric_ref = RAM_METRIC.into();
    let answer = judge(&request, &resolved(&[vcpu()]));
    assert_eq!(
        codes(&answer),
        [
            ("QUOTA_SET_INCOMPATIBLE", "prior".to_owned()),
            ("QUOTA_SET_INCOMPATIBLE", "prior".to_owned())
        ],
        "no prior limit for the rule's binding and metric, and no rule for the prior limit"
    );
    assert_eq!(answer.checks[0].rule_ids, [id]);
    assert!(answer.checks[1].rule_ids.is_empty());
}

#[test]
fn nothing_increases_is_reported_only_without_a_decrease() {
    // vCPU 20 under 30 installed, RAM 8 GiB as installed: the decrease is the one failure.
    let cpu = rule(&vcpu(), "20", VCPU_UNIT, QuotaBasis::Fixed);
    let mem = rule(&ram(), "8", GIB_UNIT, QuotaBasis::Fixed);
    let mut request = authoring(vec![cpu.clone(), mem]);
    request.change_kind = QuotaChangeKind::Increase;
    request.prior = vec![
        prior("30"),
        QuotaPriorLimit {
            logical_binding_key: "compute".into(),
            metric_ref: RAM_METRIC.into(),
            unit_ref: GIB_UNIT.into(),
            amount: "8".into(),
        },
    ];
    let answer = judge(&request, &resolved(&[vcpu(), ram()]));
    assert_eq!(
        change_checks(&answer),
        [("a cap decrease is refused", vec![cpu.rule_id])]
    );
    assert_eq!(answer.checks.len(), 1, "{:?}", answer.checks);
}

#[test]
fn a_prior_limit_without_a_rule_is_set_incompatible() {
    // Dropping a rule would remove an installed cap: an increase or an unchanged set keeps a
    // rule for every prior limit, matched by binding and metric.
    let ram_prior = QuotaPriorLimit {
        logical_binding_key: "compute".into(),
        metric_ref: RAM_METRIC.into(),
        unit_ref: GIB_UNIT.into(),
        amount: "8".into(),
    };
    for (change_kind, amount) in [
        (QuotaChangeKind::Increase, "20"),
        (QuotaChangeKind::Unchanged, "10"),
    ] {
        for rules in [
            Vec::new(),
            vec![rule(&vcpu(), amount, VCPU_UNIT, QuotaBasis::Fixed)],
        ] {
            let mut request = authoring(rules);
            request.change_kind = change_kind;
            request.prior = vec![prior("10"), ram_prior.clone()];
            let answer = judge(&request, &resolved(&[vcpu()]));
            let dropped: Vec<(&str, &str, Vec<Uuid>)> = answer
                .checks
                .iter()
                .map(|c| (c.code.code(), c.field.as_str(), c.rule_ids.clone()))
                .collect();
            let mut expected = Vec::new();
            if request.rules.is_empty() {
                expected.push(("QUOTA_SET_INCOMPATIBLE", "prior", Vec::new()));
            }
            expected.push(("QUOTA_SET_INCOMPATIBLE", "prior", Vec::new()));
            assert_eq!(
                dropped,
                expected,
                "{change_kind:?}, {} rules",
                request.rules.len()
            );
            // The detail names the binding and the metric, each quoted and cut after 32
            // characters.
            let detail = &answer.checks[expected.len() - 1].detail;
            let metric: String = RAM_METRIC.chars().take(32).collect();
            assert!(
                detail.contains("\"compute\"") && detail.contains(&format!("\"{metric}\"...")),
                "{detail}"
            );
        }
        // One failure per cause: the RAM rule's definition did not resolve, so its own check is
        // the only one; its binding's prior limit is not reported again as one no rule keeps.
        let mut request = authoring(vec![
            rule(&vcpu(), amount, VCPU_UNIT, QuotaBasis::Fixed),
            rule(&ram(), "8", GIB_UNIT, QuotaBasis::Fixed),
        ]);
        request.change_kind = change_kind;
        request.prior = vec![prior("10"), ram_prior.clone()];
        let answer = judge(&request, &resolved(&[vcpu()]));
        let ram_rule = request.rules[1].rule_id;
        assert_eq!(
            answer
                .checks
                .iter()
                .map(|c| (c.code.code(), c.field.as_str(), c.rule_ids.clone()))
                .collect::<Vec<_>>(),
            [("QUOTA_RULE_INVALID", "definition_ref", vec![ram_rule])],
            "{change_kind:?}"
        );
    }
}

#[test]
fn an_initial_set_needs_initial_set_support() {
    let mut increase_only = vcpu();
    increase_only.change_support.initial_set = false;
    let answer = judge(
        &authoring(vec![rule(
            &increase_only,
            "10",
            VCPU_UNIT,
            QuotaBasis::Fixed,
        )]),
        &resolved(&[increase_only]),
    );
    assert_eq!(
        codes(&answer),
        [("QUOTA_SET_INCOMPATIBLE", "change_kind".to_owned())]
    );
}

#[test]
fn an_increase_judges_the_final_limits() {
    // A qualified 20 to 40 increase: quantity 2 of 20 per plan unit.
    let mut request = authoring(vec![rule(
        &vcpu(),
        "20",
        VCPU_UNIT,
        QuotaBasis::PerPlanUnit,
    )]);
    request.purpose = QuotaValidationPurpose::Acceptance;
    request.quantity = Some("2".into());
    request.change_kind = QuotaChangeKind::Increase;
    request.prior = vec![prior("20")];
    let answer = judge(&request, &resolved(&[vcpu()]));
    assert!(answer.ok(), "{:?}", answer.checks);
    assert_eq!(answer.rules[0].limit.as_deref(), Some("40"));
}

#[test]
fn limits_are_compared_in_the_canonical_unit() {
    // 2 GiB installed; 3 GB is less than 2 GiB, 3 GiB is more.
    let mut request = authoring(vec![rule(&ram(), "2", GB_UNIT, QuotaBasis::Fixed)]);
    let id = request.rules[0].rule_id;
    request.change_kind = QuotaChangeKind::Increase;
    request.prior = vec![QuotaPriorLimit {
        logical_binding_key: "compute".into(),
        metric_ref: RAM_METRIC.into(),
        unit_ref: GIB_UNIT.into(),
        amount: "2".into(),
    }];
    let answer = judge(&request, &resolved(&[ram()]));
    assert_eq!(
        codes(&answer),
        [("QUOTA_SET_INCOMPATIBLE", "change_kind".to_owned())],
        "2 GB is a decrease from 2 GiB"
    );
    assert_eq!(
        change_checks(&answer),
        [("a cap decrease is refused", vec![id])]
    );
    request.rules[0].amount = "3".into();
    assert!(
        judge(&request, &resolved(&[ram()])).ok(),
        "3 GB is above 2 GiB"
    );
    request.prior[0].unit_ref = BYTES_UNIT.into();
    request.prior[0].amount = "3000000000".into();
    let answer = judge(&request, &resolved(&[ram()]));
    assert_eq!(
        codes(&answer),
        [("QUOTA_SET_INCOMPATIBLE", "change_kind".to_owned())],
        "a prior limit in the canonical unit: 3 GB is unchanged"
    );
    assert_eq!(
        change_checks(&answer),
        [("nothing increases: the change is unchanged", vec![id])]
    );
}

#[test]
fn unchanged_needs_equal_limits() {
    let mut request = authoring(vec![rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed)]);
    let id = request.rules[0].rule_id;
    request.change_kind = QuotaChangeKind::Unchanged;
    request.prior = vec![prior("10")];
    assert!(judge(&request, &resolved(&[vcpu()])).ok());
    for (installed, why) in [("8", "above"), ("12", "below")] {
        request.prior[0].amount = installed.into();
        let answer = judge(&request, &resolved(&[vcpu()]));
        assert_eq!(
            codes(&answer),
            [("QUOTA_SET_INCOMPATIBLE", "change_kind".to_owned())],
            "{why}"
        );
        assert_eq!(
            change_checks(&answer),
            [("an unchanged set keeps every limit as it is", vec![id])],
            "{why}"
        );
    }
}

#[test]
fn every_failure_is_reported_without_dropping_a_rule() {
    let mut bad_scope = rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed);
    bad_scope.scope_selection.clear();
    let rules = vec![
        rule(&vcpu(), "1.5", VCPU_UNIT, QuotaBasis::Fixed),
        bad_scope,
        rule(&ram(), "20", GB_UNIT, QuotaBasis::Fixed),
    ];
    let ids: Vec<Uuid> = rules.iter().map(|r| r.rule_id).collect();
    let answer = judge(&authoring(rules), &resolved(&[vcpu(), ram()]));
    assert_eq!(
        answer.rules.iter().map(|v| v.rule_id).collect::<Vec<_>>(),
        ids,
        "one verdict per rule, in request order"
    );
    // Rule checks in rule order, each rule's in check order, then the set checks.
    assert_eq!(
        answer
            .checks
            .iter()
            .map(|c| (c.code.code(), c.field.as_str(), c.rule_ids.clone()))
            .collect::<Vec<_>>(),
        [
            ("QUOTA_AMOUNT_INVALID", "amount", vec![ids[0]]),
            (
                "QUOTA_SCOPE_UNSUPPORTED",
                "scope_selection.region",
                vec![ids[1]]
            ),
            (
                "QUOTA_SCOPE_UNSUPPORTED",
                "scope_selection.resource_pool",
                vec![ids[1]]
            ),
            ("QUOTA_SET_INCOMPATIBLE", "scope_selection", ids.clone()),
        ]
    );
}

#[test]
fn a_malformed_request_is_refused_before_any_judgement() {
    let r = rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed);
    assert_eq!(
        request_refusal(&authoring(vec![r.clone(); 201])).unwrap().0,
        "rules"
    );
    assert_eq!(
        request_refusal(&authoring(vec![r.clone(), r.clone()]))
            .unwrap()
            .0,
        "rules[1].rule_id"
    );
    let mut acceptance = authoring(vec![r.clone()]);
    acceptance.purpose = QuotaValidationPurpose::Acceptance;
    assert_eq!(request_refusal(&acceptance).unwrap().0, "quantity");
    acceptance.quantity = Some("0".into());
    assert_eq!(request_refusal(&acceptance).unwrap().0, "quantity");
    acceptance.quantity = Some("1".into());
    assert_eq!(request_refusal(&acceptance), None);
    let mut initial = authoring(vec![r.clone()]);
    initial.prior = vec![prior("10")];
    assert_eq!(request_refusal(&initial).unwrap().0, "prior");
    let mut increase = authoring(vec![r]);
    increase.change_kind = QuotaChangeKind::Increase;
    assert_eq!(request_refusal(&increase).unwrap().0, "prior");
}

#[test]
fn a_repeated_or_non_canonical_prior_limit_is_refused() {
    let mut increase = authoring(vec![rule(&vcpu(), "20", VCPU_UNIT, QuotaBasis::Fixed)]);
    increase.change_kind = QuotaChangeKind::Increase;
    increase.prior = vec![prior("10")];
    assert_eq!(request_refusal(&increase), None);
    for amount in [
        "1.5",
        "010",
        "-1",
        "",
        "340282366920938463463374607431768211456",
    ] {
        increase.prior = vec![prior(amount)];
        assert_eq!(
            request_refusal(&increase)
                .map(|(field, _)| field)
                .as_deref(),
            Some("prior[0].amount"),
            "{amount:?}"
        );
    }
    // One binding and metric named twice, even in another unit: which one counts is unknown.
    let mut in_bytes = prior("10");
    in_bytes.unit_ref = BYTES_UNIT.into();
    increase.prior = vec![prior("10"), in_bytes];
    assert_eq!(
        request_refusal(&increase)
            .map(|(field, _)| field)
            .as_deref(),
        Some("prior[1]")
    );
    increase.prior[1].metric_ref = RAM_METRIC.into();
    assert_eq!(request_refusal(&increase), None, "another metric");
}

/// One way to spoil a well-formed request.
type Spoil = Box<dyn Fn(&mut QuotaSetRequest)>;

#[test]
fn oversized_inputs_are_refused_without_echoing_them() {
    let long = |n: usize| "x".repeat(n);
    let base = || {
        let mut request = authoring(vec![rule(&vcpu(), "20", VCPU_UNIT, QuotaBasis::Fixed)]);
        request.change_kind = QuotaChangeKind::Increase;
        request.prior = vec![prior("10")];
        request
    };
    assert_eq!(request_refusal(&base()), None);
    let cases: Vec<(&str, Spoil)> = vec![
        ("prior", Box::new(|r| r.prior = vec![prior("10"); 201])),
        ("locale", Box::new(move |r| r.locale = Some(long(65)))),
        (
            "rules[0].logical_binding_key",
            Box::new(|r| r.rules[0].logical_binding_key = String::new()),
        ),
        (
            "rules[0].logical_binding_key",
            Box::new(move |r| r.rules[0].logical_binding_key = long(65)),
        ),
        (
            "rules[0].scope_selection",
            Box::new(|r| {
                r.rules[0].scope_selection =
                    (0..9).map(|i| (format!("k{i}"), "v".to_owned())).collect();
            }),
        ),
        (
            "rules[0].scope_selection",
            Box::new(|r| {
                r.rules[0].scope_selection.insert(String::new(), "v".into());
            }),
        ),
        (
            "rules[0].scope_selection",
            Box::new(move |r| {
                r.rules[0].scope_selection.insert(long(65), "v".into());
            }),
        ),
        (
            "rules[0].alternative",
            Box::new(move |r| r.rules[0].alternative = Some(long(513))),
        ),
        (
            "rules[0].subject_selector",
            Box::new(move |r| r.rules[0].subject_selector = long(513)),
        ),
        (
            "rules[0].unit_ref",
            Box::new(move |r| r.rules[0].unit_ref = long(MAX_REF_BYTES + 1)),
        ),
        (
            "prior[0].logical_binding_key",
            Box::new(move |r| r.prior[0].logical_binding_key = long(65)),
        ),
        (
            "prior[0].metric_ref",
            Box::new(move |r| r.prior[0].metric_ref = long(MAX_REF_BYTES + 1)),
        ),
        (
            "prior[0].unit_ref",
            Box::new(move |r| r.prior[0].unit_ref = long(MAX_REF_BYTES + 1)),
        ),
        (
            "prior[0].amount",
            Box::new(move |r| r.prior[0].amount = long(10_000)),
        ),
        (
            "quantity",
            Box::new(move |r| r.quantity = Some(long(10_000))),
        ),
    ];
    for (field, spoil) in cases {
        let mut request = base();
        spoil(&mut request);
        let (refused, detail) = request_refusal(&request).unwrap();
        assert_eq!(refused, field);
        assert!(
            !detail.contains(&long(65)),
            "{field}: the caller's text is not repeated: {detail}"
        );
    }
    // The rule-level fields at their bounds pass.
    let mut request = base();
    request.locale = Some(long(64));
    request.rules[0].logical_binding_key = long(64);
    request.prior[0].logical_binding_key = long(64);
    request.rules[0].alternative = Some(long(512));
    // A reference as long as the longest GTS id the catalog stores is not refused.
    assert_eq!(MAX_REF_BYTES, 1024, "the GTS id maximum");
    request.rules[0].unit_ref = long(MAX_REF_BYTES);
    request.prior[0].metric_ref = long(MAX_REF_BYTES);
    request.prior[0].unit_ref = long(MAX_REF_BYTES);
    assert_eq!(request_refusal(&request), None);
    // A detail of the judgement repeats a short prefix of an amount or a subject selector.
    let mut r = rule(&vcpu(), &long(10_000), VCPU_UNIT, QuotaBasis::Fixed);
    r.subject_selector = long(512);
    let answer = judge(&authoring(vec![r]), &resolved(&[vcpu()]));
    assert_eq!(
        codes(&answer),
        [
            ("QUOTA_AMOUNT_INVALID", "amount".to_owned()),
            ("QUOTA_SCOPE_UNSUPPORTED", "subject_selector".to_owned())
        ]
    );
    for check in &answer.checks {
        assert!(!check.detail.contains(&long(65)), "{}", check.detail);
    }
}

/// A caller's text of `len` bytes with a control character after `head`.
fn odd_text(head: &str, len: usize) -> String {
    let text = format!("{head}\n");
    let fill = "x".repeat(len - text.len());
    text + &fill
}

/// `detail` repeats `text` only as its escaped prefix of 32 characters, never whole.
fn assert_quoted(detail: &str, text: &str) {
    let prefix: String = text.chars().take(32).collect();
    let shown = format!("\"{}\"...", prefix.replace('\n', "\\n"));
    assert!(detail.contains(&shown), "{detail:?} repeats {shown:?}");
    assert!(
        !detail.contains('\n'),
        "a control character is escaped: {detail:?}"
    );
    assert!(!detail.contains(&"x".repeat(33)), "{detail:?}");
}

#[test]
fn a_binding_key_in_a_detail_is_quoted() {
    let binding = odd_text("compute", MAX_KEY_BYTES);
    // The set checks: one binding on two adapter targets with two scopes.
    let mut cpu = rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed);
    let mut mem = rule(&ram(), "20", GB_UNIT, QuotaBasis::Fixed);
    cpu.logical_binding_key.clone_from(&binding);
    mem.logical_binding_key.clone_from(&binding);
    mem.scope_selection
        .insert("region".into(), "region-2".into());
    let mut apart = resolved(&[vcpu(), ram()]);
    apart.get_mut(&(RAM_DEF, v(1))).unwrap().target_id = Uuid::from_u128(0x7b);
    let request = authoring(vec![cpu.clone(), mem]);
    assert_eq!(request_refusal(&request), None);
    let answer = judge(&request, &apart);
    assert_eq!(
        codes(&answer),
        [
            ("QUOTA_SET_INCOMPATIBLE", "logical_binding_key".to_owned()),
            ("QUOTA_SET_INCOMPATIBLE", "scope_selection".to_owned())
        ]
    );
    for check in &answer.checks {
        assert_quoted(&check.detail, &binding);
    }
    // A rule without its prior limit, and a prior limit that no rule keeps.
    let mut increase = authoring(vec![cpu]);
    increase.change_kind = QuotaChangeKind::Increase;
    let mut ram_prior = prior("8");
    ram_prior.logical_binding_key.clone_from(&binding);
    ram_prior.metric_ref = RAM_METRIC.into();
    ram_prior.unit_ref = GB_UNIT.into();
    increase.prior = vec![ram_prior.clone()];
    assert_eq!(request_refusal(&increase), None);
    let answer = judge(&increase, &resolved(&[vcpu()]));
    assert_eq!(
        codes(&answer),
        [
            ("QUOTA_SET_INCOMPATIBLE", "prior".to_owned()),
            ("QUOTA_SET_INCOMPATIBLE", "prior".to_owned())
        ]
    );
    for check in &answer.checks {
        assert_quoted(&check.detail, &binding);
    }
    // One binding and metric in two prior limits.
    increase.prior = vec![ram_prior.clone(), ram_prior];
    let (field, detail) = request_refusal(&increase).unwrap();
    assert_eq!(field, "prior[1]");
    assert_quoted(&detail, &binding);
}

#[test]
fn an_extra_scope_key_in_a_detail_is_quoted() {
    let key = odd_text("zone", MAX_KEY_BYTES);
    let mut extra = rule(&vcpu(), "10", VCPU_UNIT, QuotaBasis::Fixed);
    extra.scope_selection.insert(key.clone(), "z1".into());
    let request = authoring(vec![extra]);
    assert_eq!(request_refusal(&request), None);
    let answer = judge(&request, &resolved(&[vcpu()]));
    // The field path names the key, which its bound keeps at 64 bytes; the detail quotes it.
    assert_eq!(
        codes(&answer),
        [("QUOTA_SCOPE_UNSUPPORTED", format!("scope_selection.{key}"))]
    );
    assert_quoted(&answer.checks[0].detail, &key);
}

#[test]
fn a_rule_unit_in_a_detail_is_quoted() {
    let unit = odd_text("gts.cf.core.units.unit.v1~", MAX_REF_BYTES);
    let request = authoring(vec![rule(&vcpu(), "10", &unit, QuotaBasis::Fixed)]);
    assert_eq!(request_refusal(&request), None);
    let answer = judge(&request, &resolved(&[vcpu()]));
    assert_eq!(
        codes(&answer),
        [("QUOTA_RULE_INVALID", "unit_ref".to_owned())]
    );
    assert_quoted(&answer.checks[0].detail, &unit);
}

#[test]
fn a_prior_metric_in_a_detail_is_quoted() {
    let metric = odd_text("gts.cf.core.metrics.metric.v1~", MAX_REF_BYTES);
    let mut increase = authoring(vec![rule(&vcpu(), "20", VCPU_UNIT, QuotaBasis::Fixed)]);
    increase.change_kind = QuotaChangeKind::Increase;
    let mut other = prior("10");
    other.metric_ref.clone_from(&metric);
    // A prior limit that no rule keeps.
    increase.prior = vec![prior("10"), other.clone()];
    assert_eq!(request_refusal(&increase), None);
    let answer = judge(&increase, &resolved(&[vcpu()]));
    assert_eq!(
        codes(&answer),
        [("QUOTA_SET_INCOMPATIBLE", "prior".to_owned())]
    );
    assert!(answer.checks[0].rule_ids.is_empty());
    assert_quoted(&answer.checks[0].detail, &metric);
    // One binding and metric in two prior limits.
    increase.prior.push(other);
    let (field, detail) = request_refusal(&increase).unwrap();
    assert_eq!(field, "prior[2]");
    assert_quoted(&detail, &metric);
}

#[test]
fn a_prior_unit_in_a_detail_is_quoted() {
    let unit = odd_text("gts.cf.core.units.unit.v1~", MAX_REF_BYTES);
    let mut increase = authoring(vec![rule(&vcpu(), "20", VCPU_UNIT, QuotaBasis::Fixed)]);
    increase.change_kind = QuotaChangeKind::Increase;
    let mut other_unit = prior("10");
    other_unit.unit_ref.clone_from(&unit);
    increase.prior = vec![other_unit];
    assert_eq!(request_refusal(&increase), None);
    let answer = judge(&increase, &resolved(&[vcpu()]));
    assert_eq!(
        codes(&answer),
        [("QUOTA_SET_INCOMPATIBLE", "prior".to_owned())]
    );
    assert_eq!(answer.checks[0].rule_ids, [increase.rules[0].rule_id]);
    assert_quoted(&answer.checks[0].detail, &unit);
}
