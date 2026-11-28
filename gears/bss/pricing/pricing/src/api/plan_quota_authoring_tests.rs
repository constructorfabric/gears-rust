//! The local client sends a typed rule as the `PUT` body carries it (D-540): the body parses back
//! into the same rule, so both surfaces judge one rule alike.
#![allow(clippy::unwrap_used)]
use super::*;
use bss_pricing_sdk::plan_quota::{PlanQuotaBasis, QuotaSubjectSelector};
use std::collections::BTreeMap;

fn typed(n: u128) -> PlanQuotaRuleV1 {
    PlanQuotaRuleV1 {
        rule_id: Uuid::from_u128(n),
        definition_ref: QuotaDefinitionRef {
            id: Uuid::from_u128(0xd1),
            version: NonZeroU64::new(7).unwrap(),
            digest: [0xab; 32],
        },
        item_id: Uuid::from_u128(0x11),
        dimension_value: Some("eu".into()),
        logical_binding_key: "compute".into(),
        subject_selector: QuotaSubjectSelector::ResourceOwner,
        scope_selection: BTreeMap::from([("region".into(), "region-1".into())]),
        amount: "10".into(),
        unit_ref: "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1".into(),
        basis: PlanQuotaBasis::Fixed,
    }
}

#[test]
fn a_typed_rule_is_sent_as_the_put_body_carries_it() {
    let rules = vec![typed(2), typed(1)];
    let body = json!({ "quota_rules": rules.iter().map(rule_json).collect::<Vec<_>>() });
    assert_eq!(
        crate::domain::plan_quota::parse_rules(&body).unwrap(),
        rules,
        "the body parses back into the same rules, in the same order"
    );
    let one = rule_json(&rules[1]);
    assert_eq!(one["definition_ref"]["version"], "7");
    assert_eq!(one["definition_ref"]["digest"], "ab".repeat(32));
    assert_eq!(one["basis"], "fixed");
    assert_eq!(one["subject_selector"], "resource_owner");
    assert_eq!(one.as_object().unwrap().len(), 10, "the ten members");
}

#[test]
fn an_answer_that_does_not_read_back_is_500() {
    let body = PricingPlanQuotaRulesDto {
        revision_id: Uuid::from_u128(1),
        revision_version: -1,
        quota_rules: Vec::new(),
        quota_rules_digest: "0".repeat(64),
    };
    assert_eq!(rules_v1(body).unwrap_err().status_code(), 500);
    let body = PricingPlanQuotaRulesDto {
        revision_id: Uuid::from_u128(1),
        revision_version: 2,
        quota_rules: Vec::new(),
        quota_rules_digest: "0".repeat(63),
    };
    assert_eq!(rules_v1(body).unwrap_err().status_code(), 500);
}
