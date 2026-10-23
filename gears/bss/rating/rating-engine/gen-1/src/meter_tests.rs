#![allow(clippy::expect_used, clippy::unwrap_used)]

use rust_decimal::Decimal;

use super::*;
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
