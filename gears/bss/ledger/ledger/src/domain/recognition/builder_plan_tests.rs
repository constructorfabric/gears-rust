//! [`BuiltSchedule::validate_plan`]: each layout rule fails with its own
//! variant and a message naming the item, the segment and the rule.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

fn eur(text: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        bss_ledger_sdk::parse_decimal(text).unwrap(),
        bss_ledger_sdk::CurrencySpec::try_new("EUR".into(), scale).unwrap(),
    )
    .unwrap()
}

fn schedule(parts: &[(i32, &str, &str)], deferred: &str) -> BuiltSchedule {
    BuiltSchedule {
        deferred: eur(deferred, 2),
        segments: parts
            .iter()
            .map(|(no, period, amount)| PlannedSegment {
                segment_no: *no,
                period_id: (*period).to_owned(),
                amount: eur(amount, 2),
            })
            .collect(),
        policy_ref: "straight-line".into(),
        ssp_snapshot_ref: None,
        po_allocation_group: None,
        subscription_ref: None,
        vc_estimate_ref: None,
        vc_method_ref: None,
        revenue_stream: "usage".into(),
    }
}

fn conflict_detail(result: Result<(), DomainError>) -> String {
    match result {
        Err(DomainError::RecognitionPolicyConflict(detail)) => detail,
        other => panic!("expected RecognitionPolicyConflict, got {other:?}"),
    }
}

#[test]
fn a_well_formed_plan_passes() {
    let plan = schedule(&[(1, "202610", "0.1"), (2, "202611", "0.2")], "0.3");
    assert_eq!(plan.validate_plan("item-1", 120), Ok(()));
}

#[test]
fn each_layout_rule_names_the_item_segment_and_rule() {
    for (plan, needles) in [
        (
            schedule(&[(1, "202610", "0.1"), (3, "202611", "0.2")], "0.3"),
            vec!["item-1", "segment 3", "expected segment 2"],
        ),
        (
            schedule(&[(1, "202610", "0.5"), (2, "202611", "-0.2")], "0.3"),
            vec![
                "item-1",
                "segment 2",
                "period 202611",
                "-0.2 EUR",
                "negative",
            ],
        ),
        (
            schedule(&[(1, "2026-10", "0.3")], "0.3"),
            vec!["item-1", "segment 1", "\"2026-10\"", "YYYYMM"],
        ),
        (
            schedule(&[(1, "202613", "0.3")], "0.3"),
            vec!["segment 1", "\"202613\""],
        ),
        (
            schedule(&[(1, "202611", "0.1"), (2, "202610", "0.2")], "0.3"),
            vec![
                "segment 2",
                "period 202610",
                "does not follow period 202611",
            ],
        ),
        (
            schedule(&[(1, "202610", "0.1"), (2, "202610", "0.2")], "0.3"),
            vec!["segment 2", "does not follow period 202610"],
        ),
        (
            schedule(&[(1, "202610", "0.3")], "0.4"),
            vec!["item-1", "sum to 0.3", "0.4 EUR"],
        ),
        (
            schedule(&[(1, "202610", "0.3")], "-0.3"),
            vec!["item-1", "deferred amount -0.3 EUR is negative"],
        ),
    ] {
        let detail = conflict_detail(plan.validate_plan("item-1", 120));
        for needle in needles {
            assert!(
                detail.contains(needle),
                "{needle:?} missing from {detail:?}"
            );
        }
    }
}

#[test]
fn segment_count_and_spec_failures_keep_their_variants() {
    let empty = schedule(&[], "0");
    assert!(matches!(
        empty.validate_plan("item-1", 120),
        Err(DomainError::ScheduleTooLong(detail)) if detail.contains("item-1") && detail.contains("1..=120")
    ));
    let long = schedule(&[(1, "202610", "0.1"), (2, "202611", "0.2")], "0.3");
    assert!(matches!(
        long.validate_plan("item-1", 1),
        Err(DomainError::ScheduleTooLong(_))
    ));
    let mut scale = schedule(&[(1, "202610", "0.3")], "0.3");
    scale.segments[0].amount = eur("0.3", 3);
    assert!(matches!(
        scale.validate_plan("item-1", 120),
        Err(DomainError::InconsistentScale(_))
    ));
}
