#![allow(clippy::expect_used, clippy::unwrap_used)]

use rating_gen1_derived as derived;
use rust_decimal::Decimal;

use super::{
    ArithmeticBudget, DerivedExpr, DerivedMeterEvaluator, ExactQuantity, Granule, GranuleFold,
    RoundMode, declaration,
};
use crate::test_input::cloudlet;

#[test]
fn a_frozen_declaration_becomes_the_evaluators_own() {
    let declaration = declaration(&cloudlet()).unwrap();
    let share = |name: &str, per: i64| {
        derived::Expr::Ceil(Box::new(derived::Expr::DivConst(
            Box::new(derived::Expr::Input(name.to_owned())),
            Decimal::from(per),
        )))
    };
    assert_eq!(
        declaration.formula,
        derived::Expr::Max(vec![share("ram", 128), share("cpu", 400)])
    );
    assert_eq!(declaration.output_round, derived::RoundMode::Up);
    assert_eq!(declaration.inputs[1].name, "cpu");
}

#[test]
fn every_operator_and_mode_maps_one_to_one() {
    let leaf = || {
        Box::new(DerivedExpr::Input {
            name: "ram".to_owned(),
        })
    };
    let mut frozen = cloudlet();
    frozen.declaration.formula = DerivedExpr::Min {
        operands: vec![
            DerivedExpr::Add {
                left: leaf(),
                right: Box::new(DerivedExpr::Const {
                    value: Decimal::ONE,
                }),
            },
            DerivedExpr::Sub {
                left: leaf(),
                right: leaf(),
            },
            DerivedExpr::Mul {
                left: leaf(),
                right: leaf(),
            },
            DerivedExpr::Floor { operand: leaf() },
            DerivedExpr::Round {
                operand: leaf(),
                scale: 2,
                mode: RoundMode::HalfEven,
            },
            DerivedExpr::Round {
                operand: leaf(),
                scale: 2,
                mode: RoundMode::HalfUp,
            },
            DerivedExpr::Round {
                operand: leaf(),
                scale: 2,
                mode: RoundMode::Down,
            },
            DerivedExpr::Input {
                name: "cpu".to_owned(),
            },
        ],
    };
    let input = || Box::new(derived::Expr::Input("ram".to_owned()));
    let rounded = |mode| derived::Expr::Round(input(), 2, mode);
    assert_eq!(
        declaration(&frozen).unwrap().formula,
        derived::Expr::Min(vec![
            derived::Expr::Add(input(), Box::new(derived::Expr::Const(Decimal::ONE))),
            derived::Expr::Sub(input(), input()),
            derived::Expr::Mul(input(), input()),
            derived::Expr::Floor(input()),
            rounded(derived::RoundMode::HalfEven),
            rounded(derived::RoundMode::HalfUp),
            rounded(derived::RoundMode::Down),
            derived::Expr::Input("cpu".to_owned()),
        ])
    );
}

#[test]
fn a_declaration_products_refuses_fails_the_child() {
    let mut frozen = cloudlet();
    frozen.declaration.output_scale = 13;
    let error = declaration(&frozen).unwrap_err();
    assert_eq!(error.reason_code(), "derived_evaluation_failed");
    assert!(error.detail().contains("cloudlet@1"), "{error}");
}

/// A granule of the cloudlet declaration's two raw inputs.
fn cloudlet_granule(hour: u8, ram: &str, cpu: &str) -> Granule {
    let input = |name: &str, q: &str| crate::input::InputQuantity {
        usage_type_ref: format!("gts.cf.core.uc.usage_record.v1~cf.test.{name}.v1"),
        q: ExactQuantity::parse(q).unwrap(),
    };
    Granule {
        granule_start: time::OffsetDateTime::new_utc(
            time::macros::date!(2026 - 09 - 15),
            time::Time::from_hms(hour, 0, 0).unwrap(),
        ),
        inputs: vec![input("ram", ram), input("cpu", cpu)],
    }
}

#[test]
fn v28_the_exact_window_sum_equals_products_evaluate_window() {
    let frozen = cloudlet();
    let evaluator = DerivedMeterEvaluator::new(&frozen, ArithmeticBudget::GEN_1).unwrap();
    let granules = [
        cloudlet_granule(10, "300", "500"),
        cloudlet_granule(11, "1024", "4000"),
        cloudlet_granule(12, "0", "0"),
        cloudlet_granule(13, "129", "401.5"),
    ];
    let metered = evaluator.slice(&granules).unwrap();
    let outputs: Vec<&str> = metered
        .granules
        .iter()
        .map(|granule| granule.output.as_str())
        .collect();
    // max(ceil(ram / 128), ceil(cpu / 400)) per hour.
    assert_eq!(outputs, ["3", "10", "0", "2"]);
    let oracle = derived::evaluate_window(
        &evaluator.declaration,
        &granules
            .iter()
            .map(|granule| {
                [("ram", &granule.inputs[0]), ("cpu", &granule.inputs[1])]
                    .into_iter()
                    .map(|(name, input)| (name.to_owned(), input.q.as_str().parse().unwrap()))
                    .collect()
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert_eq!(metered.quantity.as_str(), oracle.to_string());
}

#[test]
fn a_one_input_sum_wrapper_reproduces_the_raw_hourly_sum() {
    let frozen = crate::test_input::identity_meter();
    let evaluator = DerivedMeterEvaluator::new(&frozen, ArithmeticBudget::GEN_1).unwrap();
    let granule = |hour: u8, q: &str| {
        crate::test_input::granule(
            time::OffsetDateTime::new_utc(
                time::macros::date!(2026 - 09 - 15),
                time::Time::from_hms(hour, 0, 0).unwrap(),
            ),
            q,
        )
    };
    let metered = evaluator
        .slice(&[granule(1, "0.000000000001"), granule(2, "2.5")])
        .unwrap();
    assert_eq!(metered.quantity.as_str(), "2.500000000001");
}

#[test]
fn v42_products_per_granule_rounding_applies_before_the_window_sum() {
    // output_scale 2, rounded half to even per granule: 0.125 → 0.12, 0.135 → 0.14.
    let mut frozen = crate::test_input::identity_meter();
    frozen.declaration.output_scale = 2;
    let evaluator = DerivedMeterEvaluator::new(&frozen, ArithmeticBudget::GEN_1).unwrap();
    let hour = |h: u8| {
        time::OffsetDateTime::new_utc(
            time::macros::date!(2026 - 09 - 15),
            time::Time::from_hms(h, 0, 0).unwrap(),
        )
    };
    let metered = evaluator
        .slice(&[
            crate::test_input::granule(hour(1), "0.125"),
            crate::test_input::granule(hour(2), "0.135"),
        ])
        .unwrap();
    assert_eq!(metered.quantity.as_str(), "0.26");
}

#[test]
fn a_granule_that_does_not_match_the_declaration_fails_the_child() {
    let frozen = cloudlet();
    let evaluator = DerivedMeterEvaluator::new(&frozen, ArithmeticBudget::GEN_1).unwrap();
    let mut missing = cloudlet_granule(10, "300", "500");
    missing.inputs.pop();
    let mut twice = cloudlet_granule(10, "300", "500");
    twice.inputs[1] = twice.inputs[0].clone();
    let mut unknown = cloudlet_granule(10, "300", "500");
    unknown.inputs[1].usage_type_ref = "gts.cf.core.uc.usage_record.v1~cf.test.disk.v1".into();
    let mut beyond = cloudlet_granule(10, "300", "500");
    beyond.inputs[0].q = ExactQuantity::parse(&format!("1{}", "0".repeat(40))).unwrap();
    for granule in [missing, twice, unknown, beyond] {
        let error = evaluator.granule(&granule).unwrap_err();
        assert_eq!(error.reason_code(), "derived_evaluation_failed", "{error}");
    }
}

#[test]
fn a_peak_or_time_weighted_input_is_not_rated_until_r06() {
    for (fold, hold) in [
        (GranuleFold::Peak, None),
        (GranuleFold::TimeWeighted, Some(3_600)),
    ] {
        let mut frozen = cloudlet();
        frozen.declaration.inputs[0].granule_fold = fold;
        frozen.declaration.inputs[0].max_hold_seconds = hold;
        let error = DerivedMeterEvaluator::new(&frozen, ArithmeticBudget::GEN_1).unwrap_err();
        assert_eq!(error.reason_code(), "unsupported_input_fold");
    }
}
