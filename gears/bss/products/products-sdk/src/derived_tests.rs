//! The derived usage declaration's rules, its evaluator, its canonical bytes and its meter id
//! (P-D-230).

use super::{
    DeclarationError, DerivedInput, DerivedUsageDeclaration, EvalError, Expr, Granularity,
    GranuleFold, MeterId, MeterIdError, RoundMode, UnitSite, canonical_bytes, evaluate,
    evaluate_window, validate,
};
use rust_decimal::Decimal;
use std::collections::BTreeMap;

const RAM_REF: &str = "gts.cf.core.uc.usage_record.v1~cf.test.usage.ram_mb.v1";
const CPU_REF: &str = "gts.cf.core.uc.usage_record.v1~cf.test.usage.cpu_mhz.v1";
const LEFT_REF: &str = "gts.cf.core.uc.usage_record.v1~cf.test.usage.left.v1";
const RIGHT_REF: &str = "gts.cf.core.uc.usage_record.v1~cf.test.usage.right.v1";
const CLOUDLET_HOUR: &str = "cloudlet\u{b7}hour";

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}
fn input(name: &str) -> Expr {
    Expr::Input(name.to_owned())
}
fn konst(text: &str) -> Expr {
    Expr::Const(dec(text))
}
fn add(left: Expr, right: Expr) -> Expr {
    Expr::Add(Box::new(left), Box::new(right))
}
fn sub(left: Expr, right: Expr) -> Expr {
    Expr::Sub(Box::new(left), Box::new(right))
}
fn mul(left: Expr, right: Expr) -> Expr {
    Expr::Mul(Box::new(left), Box::new(right))
}
fn div(arg: Expr, divisor: &str) -> Expr {
    Expr::DivConst(Box::new(arg), dec(divisor))
}
fn ceil(arg: Expr) -> Expr {
    Expr::Ceil(Box::new(arg))
}
fn floor(arg: Expr) -> Expr {
    Expr::Floor(Box::new(arg))
}
fn round(arg: Expr, scale: u32, mode: RoundMode) -> Expr {
    Expr::Round(Box::new(arg), scale, mode)
}
fn sum_input(name: &str, usage_type_ref: &str) -> DerivedInput {
    DerivedInput {
        name: name.to_owned(),
        usage_type_ref: usage_type_ref.to_owned(),
        granule_fold: GranuleFold::Sum,
        max_hold_seconds: None,
        unit: "unit".to_owned(),
    }
}
fn peak_input(name: &str, usage_type_ref: &str, unit: &str) -> DerivedInput {
    DerivedInput {
        name: name.to_owned(),
        usage_type_ref: usage_type_ref.to_owned(),
        granule_fold: GranuleFold::Peak,
        max_hold_seconds: None,
        unit: unit.to_owned(),
    }
}

/// The cloudlet of decision 1: `max(ceil(ram_mb / 128), ceil(cpu_mhz / 400))` per hour.
fn cloudlet_with(ram_divisor: &str, cpu_divisor: &str) -> DerivedUsageDeclaration {
    DerivedUsageDeclaration {
        output_unit: CLOUDLET_HOUR.to_owned(),
        granularity: Granularity::Hour,
        inputs: vec![
            peak_input("ram_mb", RAM_REF, "MB"),
            peak_input("cpu_mhz", CPU_REF, "MHz"),
        ],
        formula: Expr::Max(vec![
            ceil(div(input("ram_mb"), ram_divisor)),
            ceil(div(input("cpu_mhz"), cpu_divisor)),
        ]),
        output_scale: 0,
        output_round: RoundMode::HalfEven,
    }
}
fn cloudlet() -> DerivedUsageDeclaration {
    cloudlet_with("128", "400")
}

/// Two `Sum` inputs, `left` and `right`, with the given formula.
fn two_inputs(formula: Expr) -> DerivedUsageDeclaration {
    DerivedUsageDeclaration {
        output_unit: "unit".to_owned(),
        granularity: Granularity::Hour,
        inputs: vec![sum_input("left", LEFT_REF), sum_input("right", RIGHT_REF)],
        formula,
        output_scale: 0,
        output_round: RoundMode::HalfEven,
    }
}
fn left_plus_right() -> DerivedUsageDeclaration {
    two_inputs(add(input("left"), input("right")))
}
fn granule(quantities: &[(&str, &str)]) -> BTreeMap<String, Decimal> {
    quantities
        .iter()
        .map(|(name, q)| ((*name).to_owned(), dec(q)))
        .collect()
}
fn hour(ram: &str, cpu: &str) -> BTreeMap<String, Decimal> {
    granule(&[("ram_mb", ram), ("cpu_mhz", cpu)])
}
fn lr(left: &str, right: &str) -> BTreeMap<String, Decimal> {
    granule(&[("left", left), ("right", right)])
}
/// `Ceil` wrapped around `right` until the chain is `depth` deep (`right` alone is depth 1).
fn chain(depth: usize) -> Expr {
    let mut expr = input("right");
    for _ in 1..depth {
        expr = ceil(expr);
    }
    expr
}

// ----- the cloudlet vector -----

#[test]
fn a_cloudlet_hour_of_ram_300_and_cpu_500_is_3() {
    assert_eq!(evaluate(&cloudlet(), &hour("300", "500")), Ok(dec("3")));
}

#[test]
fn a_cloudlet_hour_of_ram_100_and_cpu_900_is_3() {
    assert_eq!(evaluate(&cloudlet(), &hour("100", "900")), Ok(dec("3")));
}

#[test]
fn an_idle_cloudlet_hour_is_0() {
    assert_eq!(evaluate(&cloudlet(), &hour("0", "0")), Ok(Decimal::ZERO));
}

#[test]
fn the_formula_applies_per_granule_and_a_window_sums_the_granule_outputs() {
    let decl = cloudlet();
    let hours = [hour("256", "0"), hour("0", "800")];
    assert_eq!(evaluate(&decl, &hours[0]), Ok(dec("2")));
    assert_eq!(evaluate(&decl, &hours[1]), Ok(dec("2")));
    assert_eq!(evaluate_window(&decl, &hours), Ok(dec("4")));
    // The formula applied to the summed hours is a different, wrong answer.
    assert_eq!(evaluate(&decl, &hour("256", "800")), Ok(dec("2")));
}

#[test]
fn an_empty_window_is_zero_and_a_window_answers_its_first_granule_error() {
    let decl = cloudlet();
    assert_eq!(evaluate_window(&decl, &[]), Ok(Decimal::ZERO));
    assert_eq!(
        evaluate_window(&decl, &[hour("1", "1"), granule(&[("ram_mb", "1")])]),
        Err(EvalError::MissingInput {
            name: "cpu_mhz".to_owned()
        })
    );
    assert_eq!(
        evaluate_window(&two_inputs(Expr::Max(vec![input("left")])), &[]),
        Err(EvalError::Invalid(DeclarationError::TooFewOperands {
            found: 1
        }))
    );
}

#[test]
fn a_window_sum_that_overflows_is_an_error() {
    let max = Decimal::MAX.to_string();
    let hours = [lr(&max, "0"), lr(&max, "0")];
    assert_eq!(evaluate(&left_plus_right(), &hours[0]), Ok(Decimal::MAX));
    assert_eq!(
        evaluate_window(&left_plus_right(), &hours),
        Err(EvalError::Overflow)
    );
}

// ----- validate: the refusals of decision 1 -----

#[test]
fn the_cloudlet_declaration_is_valid() {
    assert_eq!(validate(&cloudlet()), Ok(()));
    assert_eq!(validate(&left_plus_right()), Ok(()));
}

#[test]
fn a_formula_naming_an_unknown_input_is_refused() {
    let decl = two_inputs(add(add(input("left"), input("right")), input("disk")));
    assert_eq!(
        validate(&decl),
        Err(DeclarationError::UnknownInput {
            name: "disk".to_owned()
        })
    );
}

#[test]
fn an_input_the_formula_does_not_use_is_refused() {
    let decl = two_inputs(ceil(input("left")));
    assert_eq!(
        validate(&decl),
        Err(DeclarationError::UnusedInput {
            name: "right".to_owned()
        })
    );
}

#[test]
fn two_inputs_with_one_name_are_refused() {
    let mut decl = left_plus_right();
    decl.inputs[1].name = "left".to_owned();
    decl.formula = add(input("left"), input("left"));
    assert_eq!(
        validate(&decl),
        Err(DeclarationError::DuplicateInput {
            name: "left".to_owned()
        })
    );
}

/// One input, named `disk`, with the given formula (P-D-251).
fn one_input(formula: Expr) -> DerivedUsageDeclaration {
    DerivedUsageDeclaration {
        output_unit: "GB".to_owned(),
        granularity: Granularity::Hour,
        inputs: vec![sum_input("disk", LEFT_REF)],
        formula,
        output_scale: 0,
        output_round: RoundMode::HalfEven,
    }
}

/// P-D-251: one input is a declaration. The identity formula is `{"op":"input","name":<the input>}`.
/// Any formula the grammar already allows is valid. It evaluates per granule, and a window sums those
/// outputs. Zero inputs is still `TooFewInputs`.
#[test]
fn a_one_input_identity_validates_evaluates_and_sums_a_window() {
    let mut identity = one_input(input("disk"));
    identity.inputs[0].unit = "GB".to_owned();
    assert_eq!(validate(&identity), Ok(()));
    let hours = [granule(&[("disk", "3")]), granule(&[("disk", "4")])];
    assert_eq!(evaluate(&identity, &hours[0]), Ok(dec("3")));
    assert_eq!(evaluate(&identity, &hours[1]), Ok(dec("4")));
    assert_eq!(evaluate_window(&identity, &hours), Ok(dec("7")));
    // A one-input formula the grammar already allows, not only the identity.
    assert_eq!(validate(&one_input(ceil(input("disk")))), Ok(()));
    let mut none = identity;
    none.inputs.clear();
    none.formula = konst("1");
    assert_eq!(
        validate(&none),
        Err(DeclarationError::TooFewInputs { count: 0 })
    );
}

#[test]
fn a_derived_input_is_refused() {
    let mut decl = left_plus_right();
    decl.inputs[1].usage_type_ref = "products.derived/cloudlets@1".to_owned();
    assert_eq!(
        validate(&decl),
        Err(DeclarationError::DerivedInput {
            name: "right".to_owned()
        })
    );
}

#[test]
fn an_empty_or_over_cap_input_ref_is_refused() {
    for blank in ["", "   "] {
        let mut decl = left_plus_right();
        decl.inputs[0].usage_type_ref = blank.to_owned();
        assert_eq!(
            validate(&decl),
            Err(DeclarationError::EmptyInputRef {
                name: "left".to_owned()
            }),
            "{blank:?}"
        );
    }
    let mut at_cap = left_plus_right();
    at_cap.inputs[0].usage_type_ref = "x".repeat(512);
    assert_eq!(validate(&at_cap), Ok(()));
    let mut over = at_cap;
    over.inputs[0].usage_type_ref = "x".repeat(513);
    assert_eq!(
        validate(&over),
        Err(DeclarationError::InputRefTooLong {
            name: "left".to_owned()
        })
    );
}

#[test]
fn an_input_name_off_the_grammar_is_refused() {
    let thirty_three = format!("a{}", "b".repeat(32));
    for bad in [
        "",
        "Ram",
        "1ram",
        "_ram",
        "ram-mb",
        "ram mb",
        thirty_three.as_str(),
    ] {
        let mut decl = left_plus_right();
        decl.inputs[0].name = bad.to_owned();
        decl.formula = add(input(bad), input("right"));
        assert_eq!(
            validate(&decl),
            Err(DeclarationError::InvalidInputName {
                name: bad.to_owned()
            }),
            "{bad:?}"
        );
    }
    let thirty_two = format!("a{}", "b0_".repeat(11).chars().take(31).collect::<String>());
    assert_eq!(thirty_two.chars().count(), 32);
    let mut decl = left_plus_right();
    decl.inputs[0].name.clone_from(&thirty_two);
    decl.formula = add(input(&thirty_two), input("right"));
    assert_eq!(validate(&decl), Ok(()));
}

#[test]
fn division_by_a_zero_constant_is_refused() {
    for zero in ["0", "0.00", "-0"] {
        let decl = two_inputs(div(add(input("left"), input("right")), zero));
        assert_eq!(
            validate(&decl),
            Err(DeclarationError::DivisionByZero),
            "{zero}"
        );
    }
}

#[test]
fn max_or_min_with_fewer_than_two_operands_is_refused() {
    let both = add(input("left"), input("right"));
    for (decl, found) in [
        (two_inputs(Expr::Max(vec![both.clone()])), 1),
        (two_inputs(Expr::Min(vec![both.clone()])), 1),
        (two_inputs(add(both.clone(), Expr::Max(vec![]))), 0),
        (two_inputs(add(both, Expr::Min(vec![]))), 0),
    ] {
        assert_eq!(
            validate(&decl),
            Err(DeclarationError::TooFewOperands { found })
        );
    }
}

#[test]
fn a_formula_32_deep_is_accepted_and_33_deep_is_refused() {
    assert_eq!(
        validate(&two_inputs(Expr::Max(vec![input("left"), chain(31)]))),
        Ok(())
    );
    assert_eq!(
        validate(&two_inputs(Expr::Max(vec![input("left"), chain(32)]))),
        Err(DeclarationError::TooDeep)
    );
}

#[test]
fn a_formula_of_256_nodes_is_accepted_and_257_is_refused() {
    let with_consts = |consts: usize| {
        let mut args = vec![input("left"), input("right")];
        args.extend((0..consts).map(|_| konst("0")));
        two_inputs(Expr::Max(args))
    };
    // The Max itself, two inputs and the constants.
    assert_eq!(validate(&with_consts(253)), Ok(()));
    assert_eq!(
        validate(&with_consts(254)),
        Err(DeclarationError::TooManyNodes)
    );
}

#[test]
fn a_scale_of_12_is_accepted_and_13_is_refused() {
    let mut output = left_plus_right();
    output.output_scale = 12;
    assert_eq!(validate(&output), Ok(()));
    output.output_scale = 13;
    assert_eq!(
        validate(&output),
        Err(DeclarationError::ScaleTooLarge { scale: 13 })
    );
    let rounded = |scale| {
        two_inputs(round(
            add(input("left"), input("right")),
            scale,
            RoundMode::Up,
        ))
    };
    assert_eq!(validate(&rounded(12)), Ok(()));
    assert_eq!(
        validate(&rounded(13)),
        Err(DeclarationError::ScaleTooLarge { scale: 13 })
    );
}

fn with_left_fold(fold: GranuleFold, hold: Option<u32>) -> DerivedUsageDeclaration {
    let mut decl = left_plus_right();
    decl.inputs[0].granule_fold = fold;
    decl.inputs[0].max_hold_seconds = hold;
    decl
}

#[test]
fn a_sum_input_takes_no_hold() {
    assert_eq!(validate(&with_left_fold(GranuleFold::Sum, None)), Ok(()));
    assert_eq!(
        validate(&with_left_fold(GranuleFold::Sum, Some(60))),
        Err(DeclarationError::HoldNotAllowed {
            name: "left".to_owned()
        })
    );
}

#[test]
fn a_peak_input_takes_no_hold() {
    assert_eq!(validate(&with_left_fold(GranuleFold::Peak, None)), Ok(()));
    assert_eq!(
        validate(&with_left_fold(GranuleFold::Peak, Some(60))),
        Err(DeclarationError::HoldNotAllowed {
            name: "left".to_owned()
        })
    );
}

#[test]
fn a_time_weighted_input_requires_a_hold_of_1_to_86400_seconds() {
    let tw = |hold| validate(&with_left_fold(GranuleFold::TimeWeighted, hold));
    assert_eq!(
        tw(None),
        Err(DeclarationError::HoldMissing {
            name: "left".to_owned()
        })
    );
    for seconds in [0, 86_401, u32::MAX] {
        assert_eq!(
            tw(Some(seconds)),
            Err(DeclarationError::HoldOutOfRange {
                name: "left".to_owned(),
                seconds
            })
        );
    }
    assert_eq!(tw(Some(1)), Ok(()));
    assert_eq!(tw(Some(86_400)), Ok(()));
}

#[test]
fn an_empty_or_blank_unit_is_refused() {
    for blank in ["", " \t"] {
        let mut output = left_plus_right();
        output.output_unit = blank.to_owned();
        assert_eq!(
            validate(&output),
            Err(DeclarationError::EmptyUnit {
                site: UnitSite::Output
            })
        );
        let mut inp = left_plus_right();
        inp.inputs[1].unit = blank.to_owned();
        assert_eq!(
            validate(&inp),
            Err(DeclarationError::EmptyUnit {
                site: UnitSite::Input("right".to_owned())
            })
        );
    }
}

#[test]
fn a_unit_over_64_characters_is_refused_and_64_is_accepted() {
    // Counted in characters: each `·` is two bytes.
    let at_cap = "\u{b7}".repeat(64);
    let over = "\u{b7}".repeat(65);
    let mut output = left_plus_right();
    output.output_unit.clone_from(&at_cap);
    output.inputs[0].unit.clone_from(&at_cap);
    assert_eq!(validate(&output), Ok(()));
    output.output_unit.clone_from(&over);
    assert_eq!(
        validate(&output),
        Err(DeclarationError::UnitTooLong {
            site: UnitSite::Output
        })
    );
    let mut inp = left_plus_right();
    inp.inputs[0].unit = over;
    assert_eq!(
        validate(&inp),
        Err(DeclarationError::UnitTooLong {
            site: UnitSite::Input("left".to_owned())
        })
    );
}

// ----- evaluate: every EvalError -----

#[test]
fn evaluate_refuses_an_invalid_declaration() {
    let decl = two_inputs(div(add(input("left"), input("right")), "0"));
    assert_eq!(
        evaluate(&decl, &lr("1", "1")),
        Err(EvalError::Invalid(DeclarationError::DivisionByZero))
    );
}

#[test]
fn a_missing_input_quantity_is_refused() {
    assert_eq!(
        evaluate(&cloudlet(), &granule(&[("ram_mb", "128")])),
        Err(EvalError::MissingInput {
            name: "cpu_mhz".to_owned()
        })
    );
}

#[test]
fn a_quantity_for_a_name_that_is_not_an_input_is_refused() {
    assert_eq!(
        evaluate(
            &cloudlet(),
            &granule(&[("ram_mb", "1"), ("cpu_mhz", "1"), ("disk_gb", "1")])
        ),
        Err(EvalError::ExtraInput {
            name: "disk_gb".to_owned()
        })
    );
}

#[test]
fn a_negative_input_quantity_is_refused_and_negative_zero_is_not_negative() {
    assert_eq!(
        evaluate(&cloudlet(), &hour("-1", "0")),
        Err(EvalError::NegativeInput {
            name: "ram_mb".to_owned()
        })
    );
    assert_eq!(
        evaluate(&cloudlet(), &hour("0", "-0.001")),
        Err(EvalError::NegativeInput {
            name: "cpu_mhz".to_owned()
        })
    );
    let mut negative_zero = Decimal::ZERO;
    negative_zero.set_sign_negative(true);
    assert!(negative_zero.is_sign_negative());
    let mut quantities = hour("0", "0");
    quantities.insert("ram_mb".to_owned(), negative_zero);
    assert_eq!(evaluate(&cloudlet(), &quantities), Ok(Decimal::ZERO));
}

#[test]
fn a_negative_result_is_refused() {
    let decl = two_inputs(sub(input("left"), input("right")));
    assert_eq!(
        evaluate(&decl, &lr("1", "2")),
        Err(EvalError::NegativeResult)
    );
}

#[test]
fn an_overflow_is_an_error_never_a_panic() {
    let max = Decimal::MAX.to_string();
    let cases = [
        (
            two_inputs(mul(input("left"), input("right"))),
            lr(&max, &max),
        ),
        (left_plus_right(), lr(&max, &max)),
        (
            two_inputs(sub(
                sub(input("left"), Expr::Const(Decimal::MAX)),
                input("right"),
            )),
            lr("0", &max),
        ),
        (
            two_inputs(div(add(input("left"), input("right")), "0.0000000001")),
            lr(&max, "0"),
        ),
    ];
    for (decl, quantities) in cases {
        assert_eq!(
            evaluate(&decl, &quantities),
            Err(EvalError::Overflow),
            "{:?}",
            decl.formula
        );
    }
}

#[test]
fn round_of_minus_0_3_is_zero_not_negative_zero() {
    let minus_0_3 = || sub(add(input("left"), input("right")), konst("0.3"));
    // `Round` rounds -0.3 to 0; `Ceil` truncates it to a negative zero, which the result's
    // normalization turns into 0.
    for formula in [
        round(minus_0_3(), 0, RoundMode::HalfEven),
        ceil(minus_0_3()),
    ] {
        let decl = two_inputs(formula);
        let out = evaluate(&decl, &lr("0", "0")).unwrap();
        assert_eq!(out, Decimal::ZERO);
        assert!(!out.is_sign_negative(), "{out:?} is negative zero");
        assert_eq!(out.to_string(), "0");
    }
}

#[test]
fn the_result_drops_its_trailing_zeros() {
    let mut decl = left_plus_right();
    decl.output_scale = 2;
    let out = evaluate(&decl, &lr("1.50", "1.50")).unwrap();
    assert_eq!(out.to_string(), "3");
    assert_eq!(out.scale(), 0);
}

#[test]
fn every_operator_computes_exactly() {
    let cases = [
        (
            add(
                floor(div(input("left"), "3")),
                Expr::Min(vec![input("right"), konst("2")]),
            ),
            lr("10", "7"),
            "5",
        ),
        (
            sub(mul(input("left"), input("right")), konst("1")),
            lr("3", "4"),
            "11",
        ),
        (
            add(floor(sub(input("left"), input("right"))), konst("5")),
            lr("1", "1.5"),
            "4",
        ),
        (
            add(ceil(sub(input("left"), input("right"))), konst("5")),
            lr("1", "1.5"),
            "5",
        ),
        (
            Expr::Min(vec![input("left"), input("right"), konst("9")]),
            lr("4", "7"),
            "4",
        ),
        (
            div(add(input("left"), input("right")), "3"),
            lr("1", "0"),
            "0",
        ),
    ];
    for (formula, quantities, expected) in cases {
        let decl = two_inputs(formula);
        assert_eq!(
            evaluate(&decl, &quantities),
            Ok(dec(expected)),
            "{:?}",
            decl.formula
        );
    }
}

// ----- rounding -----

fn rounded_output(mode: RoundMode, scale: u32, left: &str) -> Decimal {
    let mut decl = left_plus_right();
    decl.output_round = mode;
    decl.output_scale = scale;
    evaluate(&decl, &lr(left, "0")).unwrap()
}

#[test]
fn each_round_mode_at_the_half_boundary() {
    let modes = [
        (RoundMode::HalfEven, ["2", "4", "0.12"]),
        (RoundMode::HalfUp, ["3", "4", "0.13"]),
        (RoundMode::Up, ["3", "4", "0.13"]),
        (RoundMode::Down, ["2", "3", "0.12"]),
    ];
    for (mode, [two_and_half, three_and_half, eighth]) in modes {
        assert_eq!(
            rounded_output(mode, 0, "2.5"),
            dec(two_and_half),
            "{mode:?}"
        );
        assert_eq!(
            rounded_output(mode, 0, "3.5"),
            dec(three_and_half),
            "{mode:?}"
        );
        assert_eq!(rounded_output(mode, 2, "0.125"), dec(eighth), "{mode:?}");
    }
}

#[test]
fn each_round_mode_off_the_half_boundary() {
    let modes = [
        (RoundMode::HalfEven, ["2", "3"]),
        (RoundMode::HalfUp, ["2", "3"]),
        (RoundMode::Up, ["3", "3"]),
        (RoundMode::Down, ["2", "2"]),
    ];
    for (mode, [two_one, two_nine]) in modes {
        assert_eq!(rounded_output(mode, 0, "2.1"), dec(two_one), "{mode:?}");
        assert_eq!(rounded_output(mode, 0, "2.9"), dec(two_nine), "{mode:?}");
    }
}

#[test]
fn a_round_inside_the_formula_rounds_a_negative_half_by_its_mode() {
    // round(left - right) + 10, with left - right = -2.5.
    let modes = [
        (RoundMode::HalfEven, "8"),
        (RoundMode::HalfUp, "7"),
        (RoundMode::Up, "7"),
        (RoundMode::Down, "8"),
    ];
    for (mode, expected) in modes {
        let decl = two_inputs(add(
            round(sub(input("left"), input("right")), 0, mode),
            konst("10"),
        ));
        assert_eq!(
            evaluate(&decl, &lr("0", "2.5")),
            Ok(dec(expected)),
            "{mode:?}"
        );
    }
}

// ----- canonical bytes -----

#[test]
fn canonical_bytes_do_not_depend_on_the_input_order() {
    let mut reversed = cloudlet();
    reversed.inputs.reverse();
    assert_eq!(canonical_bytes(&reversed), canonical_bytes(&cloudlet()));
}

#[test]
fn canonical_bytes_normalize_decimals() {
    assert_eq!(
        canonical_bytes(&cloudlet_with("128.0", "400.00")),
        canonical_bytes(&cloudlet())
    );
    assert_eq!(
        canonical_bytes(&two_inputs(add(
            add(input("left"), input("right")),
            konst("0.50")
        ))),
        canonical_bytes(&two_inputs(add(
            add(input("left"), input("right")),
            konst("0.5")
        )))
    );
}

#[test]
fn canonical_bytes_change_with_a_constant() {
    assert_ne!(
        canonical_bytes(&cloudlet_with("129", "400")),
        canonical_bytes(&cloudlet())
    );
}

#[test]
fn canonical_bytes_change_with_a_fold() {
    let mut summed = cloudlet();
    summed.inputs[0].granule_fold = GranuleFold::Sum;
    assert_ne!(canonical_bytes(&summed), canonical_bytes(&cloudlet()));
}

#[test]
fn canonical_bytes_change_with_a_hold() {
    let held = |seconds| {
        let mut decl = cloudlet();
        decl.inputs[0].granule_fold = GranuleFold::TimeWeighted;
        decl.inputs[0].max_hold_seconds = Some(seconds);
        decl
    };
    assert_ne!(canonical_bytes(&held(3600)), canonical_bytes(&held(7200)));
}

#[test]
fn the_cloudlet_canonical_bytes_are_pinned() {
    // A stored digest hashes these bytes: a change to them is a new CANONICAL_DOMAIN.
    let expected = concat!(
        r#"{"domain":"products.derived_usage_declaration.v1","payload":{"#,
        r#""formula":{"args":["#,
        r#"{"arg":{"arg":{"name":"ram_mb","op":"input"},"divisor":"128","op":"div_const"},"op":"ceil"},"#,
        r#"{"arg":{"arg":{"name":"cpu_mhz","op":"input"},"divisor":"400","op":"div_const"},"op":"ceil"}"#,
        r#"],"op":"max"},"#,
        r#""granularity":"hour","#,
        r#""inputs":["#,
        r#"{"granule_fold":"peak","max_hold_seconds":null,"name":"cpu_mhz","unit":"MHz","#,
        r#""usage_type_ref":"gts.cf.core.uc.usage_record.v1~cf.test.usage.cpu_mhz.v1"},"#,
        r#"{"granule_fold":"peak","max_hold_seconds":null,"name":"ram_mb","unit":"MB","#,
        r#""usage_type_ref":"gts.cf.core.uc.usage_record.v1~cf.test.usage.ram_mb.v1"}"#,
        r#"],"output_round":"half_even","output_scale":0,"output_unit":"cloudlet"#,
        "\u{b7}",
        r#"hour"}}"#,
    );
    assert_eq!(
        String::from_utf8(canonical_bytes(&cloudlet())).unwrap(),
        expected
    );
}

#[test]
fn canonical_bytes_spell_every_operator_mode_fold_and_hold() {
    let mut decl = two_inputs(round(
        add(
            sub(mul(input("left"), konst("2")), konst("1.0")),
            Expr::Min(vec![
                floor(input("right")),
                round(konst("3.5"), 0, RoundMode::Down),
            ]),
        ),
        2,
        RoundMode::HalfUp,
    ));
    decl.inputs[0].granule_fold = GranuleFold::TimeWeighted;
    decl.inputs[0].max_hold_seconds = Some(3600);
    decl.inputs[1].unit = "a\"b\\c\u{08}\t\n\u{0c}\r\u{01}".to_owned();
    decl.output_round = RoundMode::Up;
    decl.output_scale = 3;
    let expected = concat!(
        r#"{"domain":"products.derived_usage_declaration.v1","payload":{"#,
        r#""formula":{"arg":{"left":{"left":{"left":{"name":"left","op":"input"},"op":"mul","#,
        r#""right":{"op":"const","value":"2"}},"op":"sub","right":{"op":"const","value":"1"}},"#,
        r#""op":"add","right":{"args":[{"arg":{"name":"right","op":"input"},"op":"floor"},"#,
        r#"{"arg":{"op":"const","value":"3.5"},"mode":"down","op":"round","scale":0}],"op":"min"}},"#,
        r#""mode":"half_up","op":"round","scale":2},"#,
        r#""granularity":"hour","#,
        r#""inputs":["#,
        r#"{"granule_fold":"time_weighted","max_hold_seconds":3600,"name":"left","unit":"unit","#,
        r#""usage_type_ref":"gts.cf.core.uc.usage_record.v1~cf.test.usage.left.v1"},"#,
        r#"{"granule_fold":"sum","max_hold_seconds":null,"name":"right","unit":"a\"b\\c\b\t\n\f\r\u0001","#,
        r#""usage_type_ref":"gts.cf.core.uc.usage_record.v1~cf.test.usage.right.v1"}"#,
        r#"],"output_round":"up","output_scale":3,"output_unit":"unit"}}"#,
    );
    assert_eq!(String::from_utf8(canonical_bytes(&decl)).unwrap(), expected);
}

// ----- the meter id -----

#[test]
fn a_meter_id_round_trips() {
    for (text, code, version) in [
        ("products.derived/cloudlets@1", "cloudlets", 1),
        (
            "products.derived/acme.cloudlet-v2_x@120",
            "acme.cloudlet-v2_x",
            120,
        ),
    ] {
        let id = MeterId::parse(text).unwrap();
        assert_eq!(id.code(), code);
        assert_eq!(id.version(), version);
        assert_eq!(id.format(), text);
        assert_eq!(id.to_string(), text);
        assert_eq!(text.parse::<MeterId>().unwrap().format(), text);
        assert_eq!(id.meter_ref(), (text.to_owned(), version.to_string()));
        assert_eq!(MeterId::new(code, version).unwrap().format(), text);
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]

    #[test]
    fn a_meter_id_round_trips_and_parse_does_not_panic(
        code in "[a-z0-9][a-z0-9._-]{0,12}",
        version in 1u32..10_000,
        junk in "\\PC{0,24}",
    ) {
        let id = MeterId::new(&code, version).unwrap();
        let text = id.format();
        let parsed = text.parse::<MeterId>().unwrap();
        proptest::prop_assert_eq!(parsed.to_string(), text);
        assert!(matches!(junk.parse::<MeterId>(), Ok(_) | Err(_)));
        assert!(matches!(MeterId::parse(&junk), Ok(_) | Err(_)));
    }
}

#[test]
fn a_meter_id_refuses_a_leading_zero() {
    assert_eq!(
        MeterId::parse("products.derived/cloudlets@01"),
        Err(MeterIdError::NonCanonicalVersion)
    );
    assert_eq!(
        MeterId::parse_version("01"),
        Err(MeterIdError::NonCanonicalVersion)
    );
}

#[test]
fn a_meter_id_refuses_version_zero() {
    assert_eq!(
        MeterId::parse("products.derived/cloudlets@0"),
        Err(MeterIdError::ZeroVersion)
    );
    assert_eq!(MeterId::new("cloudlets", 0), Err(MeterIdError::ZeroVersion));
}

#[test]
fn a_meter_id_refuses_a_missing_prefix() {
    for text in [
        "cloudlets@1",
        "products.derived.cloudlets@1",
        "Products.derived/cloudlets@1",
        "gts.cf.core.uc.usage_record.v1~cf.test.usage.ram_mb.v1",
    ] {
        assert_eq!(
            MeterId::parse(text),
            Err(MeterIdError::MissingPrefix),
            "{text}"
        );
    }
}

#[test]
fn a_meter_id_refuses_a_bad_code() {
    let sixty_five = "c".repeat(65);
    for code in [
        "",
        "Cloudlets",
        ".cloudlets",
        "-c",
        "cloud lets",
        "cloud/lets",
        sixty_five.as_str(),
    ] {
        assert_eq!(
            MeterId::parse(&format!("products.derived/{code}@1")),
            Err(MeterIdError::InvalidCode),
            "{code:?}"
        );
        assert_eq!(
            MeterId::new(code, 1),
            Err(MeterIdError::InvalidCode),
            "{code:?}"
        );
    }
    let sixty_four = "c".repeat(64);
    assert_eq!(
        MeterId::parse(&format!("products.derived/{sixty_four}@1")).map(|id| id.version()),
        Ok(1)
    );
}

#[test]
fn a_meter_id_refuses_an_empty_or_missing_version() {
    assert_eq!(
        MeterId::parse("products.derived/cloudlets@"),
        Err(MeterIdError::EmptyVersion)
    );
    assert_eq!(
        MeterId::parse("products.derived/cloudlets"),
        Err(MeterIdError::MissingVersion)
    );
}

#[test]
fn a_meter_id_refuses_a_version_that_is_not_digits_or_out_of_range() {
    for version in ["+1", "-1", "1.0", "1 ", " 1", "1@2", "v1", "\u{661}"] {
        assert_eq!(
            MeterId::parse(&format!("products.derived/cloudlets@{version}")),
            Err(MeterIdError::NonCanonicalVersion),
            "{version:?}"
        );
    }
    assert_eq!(MeterId::parse_version("4294967295"), Ok(u32::MAX));
    assert_eq!(
        MeterId::parse_version("4294967296"),
        Err(MeterIdError::VersionOutOfRange)
    );
}
