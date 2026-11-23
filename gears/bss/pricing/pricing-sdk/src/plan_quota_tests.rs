//! The rule's v1 shape, its tokens and the set digest (D-541).
#![allow(clippy::unwrap_used)]

use super::*;
use crate::digest::quota_rules_digest;

fn rule(n: u128, amount: &str) -> PlanQuotaRuleV1 {
    PlanQuotaRuleV1 {
        rule_id: Uuid::from_u128(n),
        definition_ref: QuotaDefinitionRef {
            id: Uuid::from_u128(0xd1),
            version: NonZeroU64::new(1).unwrap(),
            digest: [0xaa; 32],
        },
        item_id: Uuid::from_u128(0x11),
        dimension_value: None,
        logical_binding_key: "compute".into(),
        subject_selector: QuotaSubjectSelector::ResourceOwner,
        scope_selection: BTreeMap::from([
            ("region".into(), "region-1".into()),
            ("resource_pool".into(), "pool-1".into()),
        ]),
        amount: amount.into(),
        unit_ref: "gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1".into(),
        basis: PlanQuotaBasis::PerPlanUnit,
    }
}

#[test]
fn the_binding_key_rule_takes_one_to_64_of_its_characters() {
    assert!(binding_key_is_valid("compute"));
    assert!(binding_key_is_valid("a.b-c_1"));
    assert!(binding_key_is_valid(&"k".repeat(64)));
    // The fifth is a Cyrillic word: letters, but not ASCII ones.
    for bad in [
        "",
        " compute",
        "comp ute",
        "compute/1",
        "\u{43a}\u{43b}\u{44e}\u{447}",
        &"k".repeat(65),
    ] {
        assert!(!binding_key_is_valid(bad), "{bad:?}");
    }
}

#[test]
fn basis_and_subject_tokens_round_trip() {
    for basis in [PlanQuotaBasis::Fixed, PlanQuotaBasis::PerPlanUnit] {
        assert_eq!(PlanQuotaBasis::from_token(basis.token()), Some(basis));
    }
    assert_eq!(PlanQuotaBasis::Fixed.token(), "fixed");
    assert_eq!(PlanQuotaBasis::PerPlanUnit.token(), "per_plan_unit");
    assert_eq!(PlanQuotaBasis::from_token("PerPlanUnit"), None);
    assert_eq!(
        QuotaSubjectSelector::from_token("resource_owner"),
        Some(QuotaSubjectSelector::ResourceOwner)
    );
    assert_eq!(PlanQuotaKind::Allocation.token(), "allocation");
    assert_eq!(
        PlanQuotaKind::from_token("allocation"),
        Some(PlanQuotaKind::Allocation)
    );
    assert_eq!(PlanQuotaKind::from_token("Allocation"), None);
    for state in [
        PlanQuotaAvailabilityState::Enabled,
        PlanQuotaAvailabilityState::Disabled,
        PlanQuotaAvailabilityState::Withdrawn,
    ] {
        assert_eq!(
            PlanQuotaAvailabilityState::from_token(state.token()),
            Some(state)
        );
    }
    assert_eq!(PlanQuotaAvailabilityState::from_token("Enabled"), None);
}

#[test]
fn the_rule_set_digest_ignores_input_order() {
    let (a, b) = (rule(1, "10"), rule(2, "20"));
    assert_eq!(
        quota_rules_digest(&[a.clone(), b.clone()]),
        quota_rules_digest(&[b, a])
    );
}

#[test]
fn every_rule_member_moves_the_digest() {
    let base = rule(1, "10");
    let digest = quota_rules_digest(std::slice::from_ref(&base));
    let mut changes: Vec<PlanQuotaRuleV1> = Vec::new();
    let mut r = base.clone();
    r.rule_id = Uuid::from_u128(2);
    changes.push(r);
    let mut r = base.clone();
    r.definition_ref.id = Uuid::from_u128(0xd2);
    changes.push(r);
    let mut r = base.clone();
    r.amount = "11".into();
    changes.push(r);
    let mut r = base.clone();
    r.item_id = Uuid::from_u128(0x12);
    changes.push(r);
    let mut r = base.clone();
    r.dimension_value = Some("eu".into());
    changes.push(r);
    let mut r = base.clone();
    r.logical_binding_key = "storage".into();
    changes.push(r);
    let mut r = base.clone();
    r.scope_selection.insert("region".into(), "region-2".into());
    changes.push(r);
    let mut r = base.clone();
    r.scope_selection.insert("zone".into(), "zone-1".into());
    changes.push(r);
    let mut r = base.clone();
    r.scope_selection.remove("resource_pool");
    changes.push(r);
    let mut r = base.clone();
    r.basis = PlanQuotaBasis::Fixed;
    changes.push(r);
    let mut r = base.clone();
    r.definition_ref.version = NonZeroU64::new(2).unwrap();
    changes.push(r);
    let mut r = base.clone();
    r.definition_ref.digest = [0xbb; 32];
    changes.push(r);
    let mut r = base;
    r.unit_ref.push('x');
    changes.push(r);
    for changed in changes {
        assert_ne!(
            quota_rules_digest(std::slice::from_ref(&changed)),
            digest,
            "{changed:?}"
        );
    }
}

#[test]
fn the_empty_set_has_its_own_digest() {
    assert_ne!(quota_rules_digest(&[]), quota_rules_digest(&[rule(1, "0")]));
}

/// Rules that share a `rule_id` (a set the parse refuses) are ordered by their canonical bytes, so
/// even they digest alike in either input order.
#[test]
fn rules_that_share_a_rule_id_digest_alike_in_either_order() {
    let (a, b) = (rule(1, "10"), rule(1, "20"));
    assert_eq!(
        quota_rules_digest(&[a.clone(), b.clone()]),
        quota_rules_digest(&[b.clone(), a.clone()])
    );
    assert_ne!(
        quota_rules_digest(&[a.clone(), b]),
        quota_rules_digest(&[a])
    );
}
