//! The schema-1 receipt's optional quota members (D-548): a receipt without them reads and writes
//! the same bytes, a frozen snapshot and an expected quota digest round-trip, and a stored member
//! that is not exact is a corrupt row.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::super::{decode_acceptance, encode_acceptance};
use crate::domain::commercial_terms::validate_stored_terms;
use crate::infra::storage::RepoError;
use crate::infra::usage_policy_wire::digest_text;
use bss_pricing_sdk::{
    acceptance::{AcceptanceReceipt, AcceptedQuotaRule, AcceptedQuotaSnapshot, CommercialReason},
    digest::{
        quota_rules_digest, quota_snapshot_digest, request_digest, selected_quota_digest,
        terms_digest,
    },
    plan_quota::{
        PlanQuotaBasis, PlanQuotaKind, PlanQuotaRuleV1, QuotaDefinitionRef, QuotaSubjectSelector,
    },
    read::{PinnedQuotaEvidence, QuotaOwnerRef, QuotaTerms, ResolvedQuotaRule},
};
use std::collections::BTreeMap;
use std::num::NonZeroU64;
use uuid::Uuid;

const GOLDEN: &str = include_str!("../../../../tests/commercial_receipts/acceptance-v1.json");
const VCPU_UNIT: &str = "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1";

fn version(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).unwrap()
}

/// One rule on the golden receipt's selected cell, every evidence member set.
fn resolved_rule(receipt: &AcceptanceReceipt) -> ResolvedQuotaRule {
    let selection = &receipt.query.selections[0];
    ResolvedQuotaRule {
        rule: PlanQuotaRuleV1 {
            rule_id: Uuid::from_u128(1),
            definition_ref: QuotaDefinitionRef {
                id: Uuid::from_u128(0xd1),
                version: version(1),
                digest: [0xaa; 32],
            },
            item_id: selection.item_id,
            dimension_value: selection.dimension_value.clone(),
            logical_binding_key: "compute".into(),
            subject_selector: QuotaSubjectSelector::ResourceOwner,
            scope_selection: BTreeMap::from([
                ("region".into(), "region-1".into()),
                ("resource_pool".into(), "pool-1".into()),
            ]),
            amount: "10".into(),
            unit_ref: VCPU_UNIT.into(),
            basis: PlanQuotaBasis::PerPlanUnit,
        },
        evidence: PinnedQuotaEvidence {
            sku_id: Uuid::from_u128(0x51),
            sku_version: version(3),
            metric_ref: "gts.cf.bss.products.quota_metric.v1~cf.compute.quota_adapter.vcpu.v1"
                .into(),
            metric_digest: [0xb1; 32],
            quota_kind: PlanQuotaKind::Allocation,
            canonical_unit_ref: VCPU_UNIT.into(),
            unit_digest: [0xb2; 32],
            unit_numerator: "1".into(),
            unit_denominator: "1".into(),
            unit_min: "1".into(),
            unit_step: "1".into(),
            unit_max: Some("1024".into()),
            mapping: QuotaOwnerRef {
                id: Uuid::from_u128(0xa1),
                version: version(1),
                digest: [0xb3; 32],
            },
            profile: QuotaOwnerRef {
                id: Uuid::from_u128(0xa2),
                version: version(2),
                digest: [0xb4; 32],
            },
            accounting_policy_ref: "gts.cf.bss.products.quota_accounting_policy.v1~cf.compute.\
                                    quota_adapter.allocated.v1"
                .into(),
            accounting_policy_digest: [0xb5; 32],
            scope_policy_ref: "gts.cf.bss.products.quota_scope_policy.v1~cf.compute.\
                               quota_adapter.regional_pool.v1"
                .into(),
            scope_policy_digest: [0xb6; 32],
            initial_set_supported: true,
            increase_supported: false,
        },
    }
}

/// A second rule on the same cell: another definition, 20 per unit.
fn second_rule(receipt: &AcceptanceReceipt) -> ResolvedQuotaRule {
    let mut rule = resolved_rule(receipt);
    rule.rule.rule_id = Uuid::from_u128(2);
    rule.rule.definition_ref.id = Uuid::from_u128(0xd2);
    rule.rule.amount = "20".into();
    rule
}

/// The golden receipt as a sale with one quota rule issues it: the query carries the expected
/// quota digest, the receipt the frozen snapshot (10 per unit at quantity 1), and both digests
/// are sealed over the new members.
fn receipt_with_snapshot() -> AcceptanceReceipt {
    let receipt = decode_acceptance(GOLDEN).unwrap();
    let rule = resolved_rule(&receipt);
    seal(receipt, vec![(rule, "10")])
}

/// `receipt` as a sale that selects `rules` (in `rule_id` order, each with its final limit at
/// quantity 1 and availability revision 2) issues it: the expected quota digest, the frozen
/// snapshot, and the request and terms digests sealed over them.
fn seal(
    mut receipt: AcceptanceReceipt,
    rules: Vec<(ResolvedQuotaRule, &str)>,
) -> AcceptanceReceipt {
    let set: Vec<PlanQuotaRuleV1> = rules.iter().map(|(rule, _)| rule.rule.clone()).collect();
    let terms = QuotaTerms {
        quota_rules_digest: quota_rules_digest(&set),
        rules: rules.iter().map(|(rule, _)| rule.clone()).collect(),
    };
    let selected = selected_quota_digest(&terms, &receipt.query.selections);
    receipt.query.expected_quota_digest = Some(selected);
    let rules: Vec<AcceptedQuotaRule> = rules
        .into_iter()
        .map(|(rule, limit)| AcceptedQuotaRule {
            rule,
            limit: limit.into(),
            availability_revision: version(2),
        })
        .collect();
    let snapshot = AcceptedQuotaSnapshot {
        selected_quota_digest: selected,
        quota_snapshot_digest: quota_snapshot_digest(&selected, &rules, &receipt.query),
        original_quantity: receipt.query.quantity,
        rules,
    };
    receipt.request_digest = request_digest(&receipt.query);
    receipt.terms_digest = terms_digest(&receipt.query, &receipt.bindings, Some(&snapshot));
    receipt.quota = Some(snapshot);
    receipt
}

/// Validate a stored receipt the way fulfilment does, after it went through the stored bytes.
fn stored_check(receipt: &AcceptanceReceipt) -> Result<(), Option<CommercialReason>> {
    let stored = decode_acceptance(&encode_acceptance(receipt).unwrap()).unwrap();
    validate_stored_terms(
        &stored.query,
        &stored.bindings,
        stored.quota.as_ref(),
        &stored.terms_digest,
    )
    .map_err(|refusal| refusal.reason)
}

#[test]
fn a_receipt_without_quota_reads_and_writes_as_before() {
    assert!(
        !GOLDEN.contains("quota"),
        "the golden predates the quota members"
    );
    let receipt = decode_acceptance(GOLDEN).unwrap();
    assert_eq!(receipt.quota, None);
    assert_eq!(receipt.query.expected_quota_digest, None);
    assert_eq!(encode_acceptance(&receipt).unwrap(), GOLDEN);
}

#[test]
fn a_receipt_with_a_snapshot_round_trips() {
    let receipt = receipt_with_snapshot();
    let raw = encode_acceptance(&receipt).unwrap();
    let selected = digest_text(receipt.query.expected_quota_digest.unwrap());
    for spelling in [
        format!("\"expected_quota_digest\":\"{selected}\""),
        format!("\"selected_quota_digest\":\"{selected}\""),
        "\"original_quantity\":\"1\"".to_owned(),
        "\"availability_revision\":\"2\"".to_owned(),
        "\"limit\":\"10\"".to_owned(),
        "\"basis\":\"per_plan_unit\"".to_owned(),
        "\"subject_selector\":\"resource_owner\"".to_owned(),
        "\"sku_version\":\"3\"".to_owned(),
        format!("\"metric_digest\":\"{}\"", "b1".repeat(32)),
        "\"quota_kind\":\"allocation\"".to_owned(),
        "\"unit_min\":\"1\"".to_owned(),
        "\"unit_step\":\"1\"".to_owned(),
        "\"unit_max\":\"1024\"".to_owned(),
    ] {
        assert!(raw.contains(&spelling), "{spelling} in {raw}");
    }
    let decoded = decode_acceptance(&raw).unwrap();
    assert_eq!(decoded, receipt);
    assert_eq!(encode_acceptance(&decoded).unwrap(), raw);
    assert!(
        validate_stored_terms(
            &decoded.query,
            &decoded.bindings,
            decoded.quota.as_ref(),
            &decoded.terms_digest
        )
        .is_ok(),
        "a stored receipt verifies with its own snapshot"
    );
    let without = validate_stored_terms(
        &decoded.query,
        &decoded.bindings,
        None,
        &decoded.terms_digest,
    )
    .unwrap_err();
    assert_eq!(without.reason, Some(CommercialReason::AcceptanceMismatch));
}

/// D-547: the frozen evidence keeps the quota kind and the rule unit's grid and maximum; a unit
/// without a maximum keeps `unit_max` as `null`, and the receipt still verifies.
#[test]
fn a_snapshot_keeps_the_kind_and_the_unit_grid() {
    let golden = decode_acceptance(GOLDEN).unwrap();
    let mut rule = resolved_rule(&golden);
    rule.evidence.unit_min = "2".into();
    rule.evidence.unit_step = "0".into();
    rule.evidence.unit_max = None;
    let receipt = seal(golden, vec![(rule.clone(), "2")]);
    let raw = encode_acceptance(&receipt).unwrap();
    for spelling in [
        "\"quota_kind\":\"allocation\"",
        "\"unit_min\":\"2\"",
        "\"unit_step\":\"0\"",
        "\"unit_max\":null",
    ] {
        assert!(raw.contains(spelling), "{spelling} in {raw}");
    }
    let decoded = decode_acceptance(&raw).unwrap();
    assert_eq!(decoded, receipt);
    assert_eq!(decoded.quota.unwrap().rules[0].rule.evidence, rule.evidence);
    assert_eq!(stored_check(&receipt), Ok(()));
}

#[test]
fn a_snapshot_with_a_bad_digest_is_a_corrupt_row() {
    let receipt = receipt_with_snapshot();
    let raw = encode_acceptance(&receipt).unwrap();
    let selected = digest_text(receipt.query.expected_quota_digest.unwrap());
    let member = format!("\"selected_quota_digest\":\"{selected}\"");
    let short = format!("\"selected_quota_digest\":\"{}\"", &selected[1..]);
    let bad = raw.replacen(&member, &short, 1);
    assert_ne!(bad, raw);
    assert!(
        matches!(decode_acceptance(&bad), Err(RepoError::CorruptRow(_))),
        "63 hex characters"
    );
    let upper = raw.replacen(&"b1".repeat(32), &"B1".repeat(32), 1);
    assert_ne!(upper, raw);
    assert!(matches!(
        decode_acceptance(&upper),
        Err(RepoError::CorruptRow(_))
    ));
    let expected = format!("\"expected_quota_digest\":\"{selected}\"");
    let short_expected = format!("\"expected_quota_digest\":\"{}\"", &selected[1..]);
    let bad_expected = raw.replacen(&expected, &short_expected, 1);
    assert_ne!(bad_expected, raw);
    assert!(matches!(
        decode_acceptance(&bad_expected),
        Err(RepoError::CorruptRow(_))
    ));
}

/// A present quota member holds a value: `null`, a version that is not a canonical integer from
/// 1, a quantity that is not canonical, a token outside the closed set (a basis, a quota kind), a
/// missing evidence member (`unit_max` included) or an unknown member is a corrupt row, never a
/// default.
#[test]
fn a_present_quota_member_is_exact_and_never_null() {
    let receipt = receipt_with_snapshot();
    let raw = encode_acceptance(&receipt).unwrap();
    let selected = digest_text(receipt.query.expected_quota_digest.unwrap());
    let quota_start = raw.find("\"quota\":{").unwrap();
    let quota_object = &raw[quota_start + "\"quota\":".len()..raw.len() - 2];
    let cases = [
        ("quota null", raw.replacen(quota_object, "null", 1)),
        (
            "expected digest null",
            raw.replacen(
                &format!("\"expected_quota_digest\":\"{selected}\""),
                "\"expected_quota_digest\":null",
                1,
            ),
        ),
        (
            "revision zero",
            raw.replacen(
                "\"availability_revision\":\"2\"",
                "\"availability_revision\":\"0\"",
                1,
            ),
        ),
        (
            "revision with a leading zero",
            raw.replacen(
                "\"availability_revision\":\"2\"",
                "\"availability_revision\":\"02\"",
                1,
            ),
        ),
        (
            "original quantity not canonical",
            raw.replacen(
                "\"original_quantity\":\"1\"",
                "\"original_quantity\":\"1.0\"",
                1,
            ),
        ),
        (
            "basis token",
            raw.replacen(
                "\"basis\":\"per_plan_unit\"",
                "\"basis\":\"Per_plan_unit\"",
                1,
            ),
        ),
        (
            "unknown member",
            raw.replacen("\"limit\":\"10\"", "\"limit\":\"10\",\"extra\":\"1\"", 1),
        ),
        (
            "kind token",
            raw.replacen(
                "\"quota_kind\":\"allocation\"",
                "\"quota_kind\":\"Allocation\"",
                1,
            ),
        ),
        (
            "unit max absent",
            raw.replacen("\"unit_max\":\"1024\",", "", 1),
        ),
        (
            "unit min absent",
            raw.replacen("\"unit_min\":\"1\",", "", 1),
        ),
    ];
    for (name, bad) in cases {
        assert_ne!(bad, raw, "{name}");
        assert!(
            serde_json::from_str::<serde_json::Value>(&bad).is_ok(),
            "{name}: well-formed JSON, so the member rule refuses it"
        );
        assert!(
            matches!(decode_acceptance(&bad), Err(RepoError::CorruptRow(_))),
            "{name}"
        );
    }
}

/// D-548: a stored snapshot is verified against its digests, not only through the terms digest.
/// Each edit below decodes as a valid row, keeps both issuer digests, and is refused as
/// `ACCEPTANCE_MISMATCH`, which fulfilment answers as a corrupt row; the receipt as issued passes.
#[test]
fn a_stored_snapshot_edited_without_its_digests_is_a_corrupt_row() {
    type Edit = fn(&mut AcceptedQuotaSnapshot);
    let golden = decode_acceptance(GOLDEN).unwrap();
    let rules = vec![(resolved_rule(&golden), "10"), (second_rule(&golden), "20")];
    let issued = seal(golden, rules);
    assert_eq!(stored_check(&issued), Ok(()), "the receipt as issued");
    let edits: [(&str, Edit); 8] = [
        ("a limit", |s| s.rules[0].limit = "1000".into()),
        ("an availability revision", |s| {
            s.rules[1].availability_revision = version(3);
        }),
        ("an evidence member", |s| {
            s.rules[0].rule.evidence.metric_digest = [0; 32];
        }),
        ("a unit grid", |s| {
            s.rules[0].rule.evidence.unit_step = "2".into();
        }),
        ("a unit maximum", |s| {
            s.rules[1].rule.evidence.unit_max = None;
        }),
        ("a dropped rule", |s| {
            s.rules.remove(1);
        }),
        ("the original quantity", |s| {
            s.original_quantity = rust_decimal::Decimal::from(2);
        }),
        ("the selected digest", |s| {
            s.selected_quota_digest = [0x5e; 32];
        }),
    ];
    let passed: Vec<&str> = edits
        .into_iter()
        .filter(|(_, edit)| {
            let mut receipt = issued.clone();
            edit(receipt.quota.as_mut().unwrap());
            stored_check(&receipt) != Err(Some(CommercialReason::AcceptanceMismatch))
        })
        .map(|(name, _)| name)
        .collect();
    assert!(passed.is_empty(), "edits the check let through: {passed:?}");
}
