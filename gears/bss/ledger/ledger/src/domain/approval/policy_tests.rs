//! Unit tests for the pure dual-control threshold policy.

use super::*;
use crate::domain::instant::utc_ymd_hms;
use time::OffsetDateTime;

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn ts(y: i32, m: u32, d: u32) -> OffsetDateTime {
    utc_ymd_hms(y, m, d, 0, 0, 0)
}

fn version(eff: OffsetDateTime, version: i64, d2: &str, a6: i32) -> PolicyVersion {
    PolicyVersion {
        effective_from: eff,
        version,
        policy: DualControlPolicy {
            d2_thresholds: D2Thresholds::try_new(vec![money(d2, "USD", 2)]).unwrap(),
            a6_backdating_biz_days: a6,
            pending_ttl_seconds: DEFAULT_PENDING_TTL_SECONDS,
        },
    }
}

#[test]
fn resolve_empty_yields_ratified_defaults() {
    assert_eq!(
        resolve_policy(&[], ts(2026, 6, 25)),
        DualControlPolicy::DEFAULT
    );
}

#[test]
fn resolve_picks_latest_effective_from() {
    let versions = [
        version(ts(2026, 1, 1), 1, "500", 5),
        version(ts(2026, 6, 1), 2, "2000", 10),
    ];
    let p = resolve_policy(&versions, ts(2026, 6, 25));
    assert_eq!(
        p.d2_thresholds.iter().next().unwrap().amount().to_string(),
        "2000"
    );
    assert_eq!(p.a6_backdating_biz_days, 10);
}

#[test]
fn resolve_breaks_effective_tie_on_highest_version() {
    let versions = [
        version(ts(2026, 6, 1), 1, "500", 5),
        version(ts(2026, 6, 1), 2, "3000", 7),
    ];
    let p = resolve_policy(&versions, ts(2026, 6, 25));
    assert_eq!(
        p.d2_thresholds.iter().next().unwrap().amount().to_string(),
        "3000"
    );
}

#[test]
fn resolve_ignores_not_yet_effective_versions() {
    let versions = [
        version(ts(2026, 1, 1), 1, "500", 5),
        version(ts(2026, 12, 1), 2, "9999.99", 30),
    ];
    let p = resolve_policy(&versions, ts(2026, 6, 25));
    assert_eq!(
        p.d2_thresholds.iter().next().unwrap().amount().to_string(),
        "500",
        "future version must not apply"
    );
}

fn amount_op(kind: ApprovalKind, amount: Option<&str>) -> OperationFacts {
    OperationFacts {
        kind,
        amount: amount.map(|v| money(v, "USD", 2)),
        effective_at: None,
        has_outstanding_balance: false,
    }
}

#[test]
fn amount_kinds_gate_at_or_above_threshold() {
    let policy = DualControlPolicy::DEFAULT; // d2 = "1000"
    let today = date(2026, 6, 25);
    for kind in [
        ApprovalKind::Reverse,
        ApprovalKind::CreditGrant,
        ApprovalKind::ChargebackLoss,
        ApprovalKind::RecognitionScheduleChange,
        // A refund shares the SAME D2 row as the other money-out kinds (Group D).
        ApprovalKind::Refund,
        // A governed manual adjustment shares the SAME D2 row (Group 5 / Phase 3).
        ApprovalKind::ManualAdjustment,
        // Credit + debit notes share the SAME D2 row (Slice 3 §5 D1–D2, Z6-1).
        ApprovalKind::CreditNote,
        ApprovalKind::DebitNote,
    ] {
        // At the threshold → gated (>=).
        assert!(requires_dual_control(&amount_op(kind, Some("1000")), &policy, today).unwrap());
        // Just below → single-actor.
        assert!(!requires_dual_control(&amount_op(kind, Some("999.99")), &policy, today).unwrap());
        // Well above → gated.
        assert!(requires_dual_control(&amount_op(kind, Some("50000")), &policy, today).unwrap());
        // No amount known → not gated by amount.
        assert!(!requires_dual_control(&amount_op(kind, None), &policy, today).unwrap());
    }
}

#[test]
fn material_backdating_gates_beyond_a6_window() {
    let policy = DualControlPolicy::DEFAULT; // a6 = 5 business days
    let backdate = |eff: NaiveDate, today: NaiveDate| {
        requires_dual_control(
            &OperationFacts {
                kind: ApprovalKind::MaterialBackdating,
                amount: None,
                effective_at: Some(eff),
                has_outstanding_balance: false,
            },
            &policy,
            today,
        )
        .unwrap()
    };
    // 2024-01-01 is a Monday. Exactly 5 business days later is Mon 2024-01-08 →
    // at the window, NOT beyond → single-actor.
    assert!(!backdate(date(2024, 1, 1), date(2024, 1, 8)));
    // One more business day (Tue 2024-01-09) → 6 > 5 → gated.
    assert!(backdate(date(2024, 1, 1), date(2024, 1, 9)));
    // Same day → 0 business days → not gated.
    assert!(!backdate(date(2024, 1, 9), date(2024, 1, 9)));
}

#[test]
fn payer_closure_gated_only_with_outstanding_balance() {
    let policy = DualControlPolicy::DEFAULT;
    let today = date(2026, 6, 25);
    let with_balance = OperationFacts {
        kind: ApprovalKind::PayerClosure,
        amount: None,
        effective_at: None,
        has_outstanding_balance: true,
    };
    let clean = OperationFacts {
        has_outstanding_balance: false,
        ..with_balance.clone()
    };
    assert!(requires_dual_control(&with_balance, &policy, today).unwrap());
    assert!(!requires_dual_control(&clean, &policy, today).unwrap());
}

#[test]
fn period_reopen_is_always_gated() {
    let policy = DualControlPolicy::DEFAULT;
    let op = OperationFacts {
        kind: ApprovalKind::PeriodReopen,
        amount: None,
        effective_at: None,
        has_outstanding_balance: false,
    };
    assert!(requires_dual_control(&op, &policy, date(2026, 6, 25)).unwrap());
}

#[test]
fn business_days_skips_weekends() {
    // Mon 2024-01-01 → Mon 2024-01-08 spans one weekend → 5 business days.
    assert_eq!(business_days_between(date(2024, 1, 1), date(2024, 1, 8)), 5);
    // Backwards / same day → 0.
    assert_eq!(business_days_between(date(2024, 1, 8), date(2024, 1, 1)), 0);
    assert_eq!(business_days_between(date(2024, 1, 1), date(2024, 1, 1)), 0);
    // Fri 2024-01-05 → Mon 2024-01-08: Sat/Sun skipped, only Mon counts → 1.
    assert_eq!(business_days_between(date(2024, 1, 5), date(2024, 1, 8)), 1);
}

#[test]
fn effective_version_returns_the_row_in_force() {
    let versions = [
        version(ts(2026, 6, 1), 1, "500", 5),
        version(ts(2026, 6, 20), 2, "2000", 7),
    ];
    let v = effective_version(&versions, ts(2026, 6, 25)).expect("a version is in force");
    assert_eq!(v.version, 2);
    assert_eq!(
        v.policy
            .d2_thresholds
            .iter()
            .next()
            .unwrap()
            .amount()
            .to_string(),
        "2000"
    );
    assert_eq!(v.effective_from, ts(2026, 6, 20));
}

#[test]
fn effective_version_breaks_effective_tie_on_highest_version() {
    let versions = [
        version(ts(2026, 6, 20), 1, "500", 5),
        version(ts(2026, 6, 20), 2, "2000", 7),
    ];
    let v = effective_version(&versions, ts(2026, 6, 25)).expect("a version is in force");
    assert_eq!(v.version, 2);
    assert_eq!(
        v.policy
            .d2_thresholds
            .iter()
            .next()
            .unwrap()
            .amount()
            .to_string(),
        "2000"
    );
}

#[test]
fn effective_version_none_when_no_row_applies() {
    // No rows at all → None (the caller falls back to the platform defaults).
    assert!(effective_version(&[], ts(2026, 6, 25)).is_none());
    // A not-yet-effective row does not apply.
    let future = [version(ts(2026, 7, 1), 1, "500", 5)];
    assert!(effective_version(&future, ts(2026, 6, 25)).is_none());
}

#[test]
fn effective_version_agrees_with_resolve_policy() {
    let versions = [
        version(ts(2026, 6, 1), 1, "500", 5),
        version(ts(2026, 6, 20), 2, "2000", 7),
    ];
    let now = ts(2026, 6, 25);
    // resolve_policy is effective_version's thresholds, defaults when none.
    assert_eq!(
        resolve_policy(&versions, now),
        effective_version(&versions, now).expect("in force").policy
    );
    assert_eq!(resolve_policy(&[], now), DualControlPolicy::DEFAULT);
}

fn money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        bss_ledger_sdk::parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.into(), scale).unwrap(),
    )
    .unwrap()
}
#[test]
fn symbolic_defaults_and_scale_dependent_bounds() {
    for (scale, default, min, max) in [
        (0, "100000", "10000", "100000000"),
        (2, "1000", "100", "1000000"),
        (3, "100", "10", "100000"),
        (8, "0.001", "0.0001", "1"),
        (
            28,
            "0.00000000000000000000001",
            "0.000000000000000000000001",
            "0.00000000000000000001",
        ),
    ] {
        let spec = CurrencySpec::try_new("XTS".into(), scale).unwrap();
        assert_eq!(
            DualControlPolicy::DEFAULT.d2_threshold(&spec).unwrap(),
            money(default, "XTS", scale)
        );
        for text in [min, max] {
            assert!(validate_config(&[money(text, "XTS", scale)], 5, 1).is_ok());
        }
        for amount in [
            bss_ledger_sdk::parse_decimal(min).unwrap() - Decimal::new(1, u32::from(scale)),
            bss_ledger_sdk::parse_decimal(max).unwrap() + Decimal::new(1, u32::from(scale)),
        ] {
            let value = PostedMoney::try_new(amount, spec.clone()).unwrap();
            assert_eq!(
                validate_config(std::slice::from_ref(&value), 5, 1),
                Err(PolicyConfigError::D2OutOfRange {
                    threshold: value,
                    min: bss_ledger_sdk::parse_decimal(min).unwrap(),
                    max: bss_ledger_sdk::parse_decimal(max).unwrap(),
                })
            );
        }
        assert!(matches!(
            validate_config(&[money("0", "XTS", scale)], 5, 1),
            Err(PolicyConfigError::D2OutOfRange { .. })
        ));
    }
    assert!(DualControlPolicy::DEFAULT.d2_thresholds.is_empty());
}
#[test]
fn magnitude_and_metadata_are_exact() {
    let today = NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
    let mut policy = DualControlPolicy::DEFAULT;
    policy.d2_thresholds = D2Thresholds::try_new(vec![money("123.45", "EUR", 2)]).unwrap();
    for (text, expected) in [("123.44", false), ("123.45", true), ("-123.45", true)] {
        let op = OperationFacts {
            kind: ApprovalKind::Reverse,
            amount: Some(money(text, "EUR", 2)),
            effective_at: None,
            has_outstanding_balance: false,
        };
        assert_eq!(requires_dual_control(&op, &policy, today), Ok(expected));
    }
    assert!(matches!(
        policy.d2_threshold(&CurrencySpec::try_new("EUR".into(), 3).unwrap()),
        Err(PolicyConfigError::MetadataConflict {
            configured_scale: 2,
            other_scale: 3,
            ..
        })
    ));
    assert_eq!(
        valuation_basis(ApprovalKind::Reverse),
        ValuationBasis::Transaction
    );
    assert_eq!(
        valuation_basis(ApprovalKind::RecognitionScheduleChange),
        ValuationBasis::Transaction
    );
    assert_eq!(
        valuation_basis(ApprovalKind::Refund),
        ValuationBasis::Functional
    );
}
#[test]
fn duplicate_config_and_non_money_rules() {
    let x = money("1000", "EUR", 2);
    assert!(matches!(
        validate_config(&[x.clone(), x], 5, 1),
        Err(PolicyConfigError::DuplicateCurrency(_))
    ));
    assert!(matches!(
        validate_config(&[money("1000", "EUR", 2), money("100", "EUR", 3)], 5, 1),
        Err(PolicyConfigError::MetadataConflict {
            configured_scale: 2,
            other_scale: 3,
            ..
        })
    ));
    assert_eq!(
        validate_config(&[], 0, 1),
        Err(PolicyConfigError::A6OutOfRange(0))
    );
    assert_eq!(
        validate_config(&[], A6_MAX_DAYS + 1, 1),
        Err(PolicyConfigError::A6OutOfRange(31))
    );
    assert_eq!(validate_config(&[], A6_MIN_DAYS, 1), Ok(()));
    assert_eq!(validate_config(&[], A6_MAX_DAYS, 1), Ok(()));
    assert_eq!(
        validate_config(&[], 5, 0),
        Err(PolicyConfigError::TtlNotPositive(0))
    );
    let today = NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
    let op = OperationFacts {
        kind: ApprovalKind::PeriodReopen,
        amount: None,
        effective_at: None,
        has_outstanding_balance: false,
    };
    assert_eq!(
        requires_dual_control(&op, &DualControlPolicy::DEFAULT, today),
        Ok(true)
    );
    assert_eq!(
        business_days_between(NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(), today),
        5
    );
    let versions = vec![
        PolicyVersion {
            effective_from: OffsetDateTime::UNIX_EPOCH,
            version: 1,
            policy: DualControlPolicy::DEFAULT,
        },
        PolicyVersion {
            effective_from: OffsetDateTime::UNIX_EPOCH,
            version: 2,
            policy: DualControlPolicy::DEFAULT,
        },
    ];
    assert_eq!(
        effective_version(&versions, OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .version,
        2
    );
}

/// Several currency overrides resolve independently: each configured currency
/// gets its own threshold (not the first configured one), and an unconfigured
/// currency still falls back to the scale-derived platform default.
#[test]
fn d2_threshold_resolves_each_override_and_falls_back_for_others() {
    let mut policy = DualControlPolicy::DEFAULT;
    policy.d2_thresholds =
        D2Thresholds::try_new(vec![money("2500", "EUR", 2), money("300000", "JPY", 0)]).unwrap();
    let spec = |code: &str, scale| CurrencySpec::try_new(code.into(), scale).unwrap();
    assert_eq!(
        policy.d2_threshold(&spec("EUR", 2)),
        Ok(money("2500", "EUR", 2))
    );
    assert_eq!(
        policy.d2_threshold(&spec("JPY", 0)),
        Ok(money("300000", "JPY", 0))
    );
    assert_eq!(
        policy.d2_threshold(&spec("USD", 2)),
        Ok(money("1000", "USD", 2))
    );
}

/// A D2 lookup checks only the thresholds: an out-of-range A6 or TTL on the
/// policy is not reported as a D2 failure.
#[test]
fn d2_threshold_does_not_fail_on_unrelated_a6_or_ttl() {
    let mut policy = DualControlPolicy::DEFAULT;
    policy.a6_backdating_biz_days = 0;
    policy.pending_ttl_seconds = 0;
    let usd = CurrencySpec::try_new("USD".into(), 2).unwrap();
    assert_eq!(policy.d2_threshold(&usd), Ok(money("1000", "USD", 2)));
}

/// `amount_gated` agrees with the amount arm of `requires_dual_control` for
/// every kind, and material backdating keeps its captured transaction value.
#[test]
fn amount_gated_matches_requires_dual_control_and_backdating_is_transaction_basis() {
    let today = date(2026, 10, 9);
    let huge = money("1000000", "USD", 2);
    for kind in [
        ApprovalKind::Reverse,
        ApprovalKind::MaterialBackdating,
        ApprovalKind::CreditGrant,
        ApprovalKind::ChargebackLoss,
        ApprovalKind::PayerClosure,
        ApprovalKind::PeriodReopen,
        ApprovalKind::RecognitionScheduleChange,
        ApprovalKind::Refund,
        ApprovalKind::ManualAdjustment,
        ApprovalKind::CreditNote,
        ApprovalKind::DebitNote,
    ] {
        let decide = |amount: Option<PostedMoney>| {
            requires_dual_control(
                &OperationFacts {
                    kind,
                    amount,
                    effective_at: None,
                    has_outstanding_balance: false,
                },
                &DualControlPolicy::DEFAULT,
                today,
            )
            .unwrap()
        };
        assert_eq!(
            amount_gated(kind),
            decide(Some(huge.clone())) != decide(None),
            "{kind:?}"
        );
    }
    assert_eq!(
        valuation_basis(ApprovalKind::MaterialBackdating),
        ValuationBasis::Transaction
    );
}

/// Thresholds are validated once, by type: a repeated currency (at the same or
/// another scale) or an out-of-range value cannot be built, and lookups by
/// currency code see one threshold each, in currency-code order.
#[test]
fn d2_thresholds_are_a_validated_map_keyed_by_currency() {
    assert_eq!(
        D2Thresholds::try_new(vec![money("1000", "EUR", 2), money("2000", "EUR", 2)]),
        Err(PolicyConfigError::DuplicateCurrency("EUR".into()))
    );
    assert!(matches!(
        D2Thresholds::try_new(vec![money("1000", "EUR", 2), money("100", "EUR", 3)]),
        Err(PolicyConfigError::MetadataConflict { .. })
    ));
    assert!(matches!(
        D2Thresholds::try_new(vec![money("1", "EUR", 2)]),
        Err(PolicyConfigError::D2OutOfRange { .. })
    ));
    let map =
        D2Thresholds::try_new(vec![money("300000", "JPY", 0), money("2500", "EUR", 2)]).unwrap();
    assert_eq!(map.len(), 2);
    assert_eq!(map.get("EUR"), Some(&money("2500", "EUR", 2)));
    assert_eq!(map.get("USD"), None);
    assert_eq!(
        map.iter().map(|m| m.currency().code()).collect::<Vec<_>>(),
        vec!["EUR", "JPY"]
    );
    assert!(D2Thresholds::default().is_empty());
}

/// The policy constructor checks A6 and the TTL once; its thresholds are
/// already valid by type.
#[test]
fn dual_control_policy_try_new_validates_limits_once() {
    let thresholds = D2Thresholds::try_new(vec![money("2500", "EUR", 2)]).unwrap();
    let policy = DualControlPolicy::try_new(thresholds.clone(), 7, 3_600).unwrap();
    assert_eq!(
        policy.d2_thresholds.clone().into_vec(),
        vec![money("2500", "EUR", 2)]
    );
    assert_eq!(
        (policy.a6_backdating_biz_days, policy.pending_ttl_seconds),
        (7, 3_600)
    );
    assert_eq!(
        DualControlPolicy::try_new(thresholds.clone(), A6_MAX_DAYS + 1, 1),
        Err(PolicyConfigError::A6OutOfRange(A6_MAX_DAYS + 1))
    );
    assert_eq!(
        DualControlPolicy::try_new(thresholds, 5, 0),
        Err(PolicyConfigError::TtlNotPositive(0))
    );
    assert_eq!(validate_limits(A6_MIN_DAYS, 1), Ok(()));
}

/// One classification decides each kind's rule; the amount-gated set is the
/// `Amount` rule and nothing else.
#[test]
fn gate_rule_classifies_the_non_amount_kinds() {
    assert_eq!(
        gate_rule(ApprovalKind::MaterialBackdating),
        GateRule::Backdating
    );
    assert_eq!(
        gate_rule(ApprovalKind::PayerClosure),
        GateRule::OutstandingBalance
    );
    assert_eq!(gate_rule(ApprovalKind::PeriodReopen), GateRule::Always);
    assert_eq!(gate_rule(ApprovalKind::Refund), GateRule::Amount);
    assert!(!amount_gated(ApprovalKind::PeriodReopen));
    assert!(amount_gated(ApprovalKind::Reverse));
}
