use crate::odata_filters::CompareOperator::{self, *};
use crate::odata_filters::{Expr, Value, parse_str};
use bigdecimal::BigDecimal;
use std::str::FromStr;

#[test]
fn null_value() {
    let filter = "CompanyName ne null";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Compare(
            Expr::Identifier("CompanyName".to_owned()).into(),
            NotEqual,
            Expr::Value(Value::Null).into()
        )
    );
}

#[test]
fn boolean_value() {
    let filter = "isActive eq false and not isBlocked eq true";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::And(
            Expr::Compare(
                Expr::Identifier("isActive".to_owned()).into(),
                Equal,
                Expr::Value(Value::Bool(false)).into()
            )
            .into(),
            Expr::Not(
                Expr::Compare(
                    Expr::Identifier("isBlocked".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::Bool(true)).into()
                )
                .into()
            )
            .into()
        )
    );
}

#[test]
fn uuid_value() {
    let filter = [
        "AuthorId eq d1fdd9d1-8c73-4eb9-a341-3505d4efad78",
        "and PackageId ne C0BD12F1-9CAD-4081-977A-04B5AF7EDA0E",
    ]
    .join(" ");
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::And(
            Expr::Compare(
                Expr::Identifier("AuthorId".to_owned()).into(),
                Equal,
                Expr::Value(Value::Uuid(uuid::uuid!(
                    "d1fdd9d1-8c73-4eb9-a341-3505d4efad78"
                )))
                .into()
            )
            .into(),
            Expr::Compare(
                Expr::Identifier("PackageId".to_owned()).into(),
                NotEqual,
                Expr::Value(Value::Uuid(uuid::uuid!(
                    "c0bd12f1-9cad-4081-977a-04b5af7eda0e"
                )))
                .into()
            )
            .into()
        )
    );
}

#[test]
fn number_value() {
    let filter = "price lt 99.99 and code in (11, 27, 42)";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::And(
            Expr::Compare(
                Expr::Identifier("price".to_owned()).into(),
                LessThan,
                Expr::Value(Value::Number(BigDecimal::from_str("99.99").unwrap())).into()
            )
            .into(),
            Expr::In(
                Expr::Identifier("code".to_owned()).into(),
                vec![
                    Expr::Value(Value::Number(11.into())),
                    Expr::Value(Value::Number(27.into())),
                    Expr::Value(Value::Number(42.into())),
                ]
            )
            .into()
        )
    );
}

#[test]
fn signed_number_value() {
    let filter = "temperature gt -10 and offset eq +5 and delta lt -3.14";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::And(
            Expr::And(
                Expr::Compare(
                    Expr::Identifier("temperature".to_owned()).into(),
                    GreaterThan,
                    Expr::Value(Value::Number(BigDecimal::from_str("-10").unwrap())).into()
                )
                .into(),
                Expr::Compare(
                    Expr::Identifier("offset".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::Number(BigDecimal::from_str("+5").unwrap())).into()
                )
                .into()
            )
            .into(),
            Expr::Compare(
                Expr::Identifier("delta".to_owned()).into(),
                LessThan,
                Expr::Value(Value::Number(BigDecimal::from_str("-3.14").unwrap())).into()
            )
            .into()
        )
    );
}

#[test]
fn date_value() {
    let filter = "birthdate eq 2024-06-24";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Compare(
            Expr::Identifier("birthdate".to_owned()).into(),
            Equal,
            Expr::Value(Value::Date("2024-06-24".parse().unwrap())).into()
        )
    );
}

#[test]
fn time_value() {
    let filter = "(startTime lt 14:30:00 or pauseTime ge 13:00) and endTime le 8:00:00.001002003";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::And(
            Expr::Or(
                Expr::Compare(
                    Expr::Identifier("startTime".to_owned()).into(),
                    CompareOperator::LessThan,
                    Expr::Value(Value::Time("14:30:00".parse().unwrap())).into()
                )
                .into(),
                Expr::Compare(
                    Expr::Identifier("pauseTime".to_owned()).into(),
                    CompareOperator::GreaterOrEqual,
                    Expr::Value(Value::Time("13:00:00".parse().unwrap())).into()
                )
                .into()
            )
            .into(),
            Expr::Compare(
                Expr::Identifier("endTime".to_owned()).into(),
                CompareOperator::LessOrEqual,
                Expr::Value(Value::Time("08:00:00.001002003".parse().unwrap())).into()
            )
            .into()
        )
    );
}

#[test]
fn datetime_value() {
    let filter = [
        "   AT eq 2024-06-24T12:34:56Z",
        "or AT gt 2024-06-24T12:34:56+02",
        "or AT lt 2024-06-24T12:34:56EST",
    ]
    .join(" ");
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Or(
            Expr::Or(
                Expr::Compare(
                    Expr::Identifier("AT".to_owned()).into(),
                    CompareOperator::Equal,
                    Expr::Value(Value::DateTime("2024-06-24T12:34:56Z".parse().unwrap())).into()
                )
                .into(),
                Expr::Compare(
                    Expr::Identifier("AT".to_owned()).into(),
                    CompareOperator::GreaterThan,
                    Expr::Value(Value::DateTime("2024-06-24T10:34:56Z".parse().unwrap())).into()
                )
                .into()
            )
            .into(),
            Expr::Compare(
                Expr::Identifier("AT".to_owned()).into(),
                CompareOperator::LessThan,
                Expr::Value(Value::DateTime("2024-06-24T17:34:56Z".parse().unwrap())).into()
            )
            .into()
        )
    );
}

#[test]
fn string_value() {
    let filter = "Name in ('Ada', 'Joey')";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::In(
            Expr::Identifier("Name".to_owned()).into(),
            vec![
                Expr::Value(Value::String("Ada".to_owned())),
                Expr::Value(Value::String("Joey".to_owned())),
            ],
        )
    );
}

#[test]
fn escaped_string_comparison() {
    let filter = "name eq '\u{03A9} S''mores'";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Compare(
            Expr::Identifier("name".to_owned()).into(),
            Equal,
            Expr::Value(Value::String(String::from("\u{3a9} S'mores"))).into()
        )
    );
}

#[test]
fn or_grouping() {
    let filter = "name eq 'John' or isActive eq true";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Or(
            Expr::Compare(
                Expr::Identifier("name".to_owned()).into(),
                Equal,
                Expr::Value(Value::String("John".to_owned())).into()
            )
            .into(),
            Expr::Compare(
                Expr::Identifier("isActive".to_owned()).into(),
                Equal,
                Expr::Value(Value::Bool(true)).into()
            )
            .into()
        )
    );
}

#[test]
fn and_grouping() {
    let filter = "name eq 'John' and isActive eq true";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::And(
            Expr::Compare(
                Expr::Identifier("name".to_owned()).into(),
                Equal,
                Expr::Value(Value::String("John".to_owned())).into()
            )
            .into(),
            Expr::Compare(
                Expr::Identifier("isActive".to_owned()).into(),
                Equal,
                Expr::Value(Value::Bool(true)).into()
            )
            .into()
        )
    );
}

#[test]
fn not_grouping() {
    let filter = "not (name eq 'John')";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Not(
            Expr::Compare(
                Expr::Identifier("name".to_owned()).into(),
                Equal,
                Expr::Value(Value::String("John".to_owned())).into()
            )
            .into()
        )
    );
}

#[test]
fn complex_and_or_grouping() {
    let filter = "(name eq 'John' and isActive eq true) or age gt 30";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Or(
            Expr::And(
                Expr::Compare(
                    Expr::Identifier("name".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::String("John".to_owned())).into()
                )
                .into(),
                Expr::Compare(
                    Expr::Identifier("isActive".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::Bool(true)).into()
                )
                .into()
            )
            .into(),
            Expr::Compare(
                Expr::Identifier("age".to_owned()).into(),
                GreaterThan,
                Expr::Value(Value::Number(BigDecimal::from_str("30").unwrap())).into()
            )
            .into()
        )
    );
}

#[test]
fn nested_grouping() {
    let filter = "((name eq 'John' and isActive eq true) or (age gt 30 and age lt 50))";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Or(
            Expr::And(
                Expr::Compare(
                    Expr::Identifier("name".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::String("John".to_owned())).into()
                )
                .into(),
                Expr::Compare(
                    Expr::Identifier("isActive".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::Bool(true)).into()
                )
                .into()
            )
            .into(),
            Expr::And(
                Expr::Compare(
                    Expr::Identifier("age".to_owned()).into(),
                    GreaterThan,
                    Expr::Value(Value::Number(BigDecimal::from_str("30").unwrap())).into()
                )
                .into(),
                Expr::Compare(
                    Expr::Identifier("age".to_owned()).into(),
                    LessThan,
                    Expr::Value(Value::Number(BigDecimal::from_str("50").unwrap())).into()
                )
                .into()
            )
            .into()
        )
    );
}

#[test]
fn function_call_endswith() {
    let filter = "endswith(name, 'Smith')";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Function(
            "endswith".to_owned(),
            vec![
                Expr::Identifier("name".to_owned()),
                Expr::Value(Value::String("Smith".to_owned()))
            ]
        )
    );
}

#[test]
fn function_call_complex() {
    let filter = "concat(concat(city, ', '), country) eq 'Berlin, Germany'";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Compare(
            Expr::Function(
                "concat".to_owned(),
                vec![
                    Expr::Function(
                        "concat".to_owned(),
                        vec![
                            Expr::Identifier("city".to_owned()),
                            Expr::Value(Value::String(", ".to_owned()))
                        ]
                    ),
                    Expr::Identifier("country".to_owned())
                ]
            )
            .into(),
            Equal,
            Expr::Value(Value::String("Berlin, Germany".to_owned())).into()
        )
    );
}

#[test]
fn in_operator() {
    let filter = "name in ('John', 'Jane', 'Doe')";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::In(
            Expr::Identifier("name".to_owned()).into(),
            vec![
                Expr::Value(Value::String("John".to_owned())),
                Expr::Value(Value::String("Jane".to_owned())),
                Expr::Value(Value::String("Doe".to_owned()))
            ]
        )
    );
}

#[test]
fn nested_not() {
    let filter = "not (not (isActive eq false))";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Not(
            Expr::Not(
                Expr::Compare(
                    Expr::Identifier("isActive".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::Bool(false)).into()
                )
                .into()
            )
            .into()
        )
    );
}

#[test]
fn complex_nested() {
    let filter = "((name eq 'John' and isActive eq true) or (age gt 30 and age lt 50)) and (city eq 'Berlin' or city eq 'Paris')";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::And(
            Expr::Or(
                Expr::And(
                    Expr::Compare(
                        Expr::Identifier("name".to_owned()).into(),
                        Equal,
                        Expr::Value(Value::String("John".to_owned())).into()
                    )
                    .into(),
                    Expr::Compare(
                        Expr::Identifier("isActive".to_owned()).into(),
                        Equal,
                        Expr::Value(Value::Bool(true)).into()
                    )
                    .into()
                )
                .into(),
                Expr::And(
                    Expr::Compare(
                        Expr::Identifier("age".to_owned()).into(),
                        GreaterThan,
                        Expr::Value(Value::Number(BigDecimal::from_str("30").unwrap())).into()
                    )
                    .into(),
                    Expr::Compare(
                        Expr::Identifier("age".to_owned()).into(),
                        LessThan,
                        Expr::Value(Value::Number(BigDecimal::from_str("50").unwrap())).into()
                    )
                    .into()
                )
                .into()
            )
            .into(),
            Expr::Or(
                Expr::Compare(
                    Expr::Identifier("city".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::String("Berlin".to_owned())).into()
                )
                .into(),
                Expr::Compare(
                    Expr::Identifier("city".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::String("Paris".to_owned())).into()
                )
                .into()
            )
            .into()
        )
    );
}

#[test]
fn function_and_comparison() {
    let filter = "substring(name, 1, 3) eq 'Joh'";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Compare(
            Expr::Function(
                "substring".to_owned(),
                vec![
                    Expr::Identifier("name".to_owned()),
                    Expr::Value(Value::Number(BigDecimal::from_str("1").unwrap())),
                    Expr::Value(Value::Number(BigDecimal::from_str("3").unwrap()))
                ]
            )
            .into(),
            Equal,
            Expr::Value(Value::String("Joh".to_owned())).into()
        )
    );
}

#[test]
fn nested_function_calls() {
    let filter = "concat(substring(name, 1, 3), ' Doe') eq 'Joh Doe'";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Compare(
            Expr::Function(
                "concat".to_owned(),
                vec![
                    Expr::Function(
                        "substring".to_owned(),
                        vec![
                            Expr::Identifier("name".to_owned()),
                            Expr::Value(Value::Number(BigDecimal::from_str("1").unwrap())),
                            Expr::Value(Value::Number(BigDecimal::from_str("3").unwrap()))
                        ]
                    ),
                    Expr::Value(Value::String(" Doe".to_owned()))
                ]
            )
            .into(),
            Equal,
            Expr::Value(Value::String("Joh Doe".to_owned())).into()
        )
    );
}

#[test]
fn not_and_function() {
    let filter = "not endswith(name, 'Smith')";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Not(
            Expr::Function(
                "endswith".to_owned(),
                vec![
                    Expr::Identifier("name".to_owned()),
                    Expr::Value(Value::String("Smith".to_owned()))
                ]
            )
            .into()
        )
    );
}

#[test]
fn mixed_operators() {
    let filter = "price gt 50.0 and (name eq 'John' or endswith(name, 'Doe'))";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::And(
            Expr::Compare(
                Expr::Identifier("price".to_owned()).into(),
                GreaterThan,
                Expr::Value(Value::Number(BigDecimal::from_str("50.0").unwrap())).into()
            )
            .into(),
            Expr::Or(
                Expr::Compare(
                    Expr::Identifier("name".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::String("John".to_owned())).into()
                )
                .into(),
                Expr::Function(
                    "endswith".to_owned(),
                    vec![
                        Expr::Identifier("name".to_owned()),
                        Expr::Value(Value::String("Doe".to_owned()))
                    ]
                )
                .into()
            )
            .into()
        )
    );
}

#[test]
fn not_in_operator() {
    let filter = "not name in ('John', 'Jane', 'Doe')";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Not(
            Expr::In(
                Expr::Identifier("name".to_owned()).into(),
                vec![
                    Expr::Value(Value::String("John".to_owned())),
                    Expr::Value(Value::String("Jane".to_owned())),
                    Expr::Value(Value::String("Doe".to_owned()))
                ]
            )
            .into()
        )
    );
}

#[test]
fn nested_comparisons() {
    let filter =
        "((price gt 50.0 and price lt 100.0) or (discount eq 10.0 and isAvailable eq true))";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Or(
            Expr::And(
                Expr::Compare(
                    Expr::Identifier("price".to_owned()).into(),
                    GreaterThan,
                    Expr::Value(Value::Number(BigDecimal::from_str("50.0").unwrap())).into()
                )
                .into(),
                Expr::Compare(
                    Expr::Identifier("price".to_owned()).into(),
                    LessThan,
                    Expr::Value(Value::Number(BigDecimal::from_str("100.0").unwrap())).into()
                )
                .into()
            )
            .into(),
            Expr::And(
                Expr::Compare(
                    Expr::Identifier("discount".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::Number(BigDecimal::from_str("10.0").unwrap())).into()
                )
                .into(),
                Expr::Compare(
                    Expr::Identifier("isAvailable".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::Bool(true)).into()
                )
                .into()
            )
            .into()
        )
    );
}

#[test]
fn multiple_functions() {
    let filter = "startswith(name, 'J') and length(name) gt 3";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::And(
            Expr::Function(
                "startswith".to_owned(),
                vec![
                    Expr::Identifier("name".to_owned()),
                    Expr::Value(Value::String("J".to_owned()))
                ]
            )
            .into(),
            Expr::Compare(
                Expr::Function(
                    "length".to_owned(),
                    vec![Expr::Identifier("name".to_owned())]
                )
                .into(),
                GreaterThan,
                Expr::Value(Value::Number(BigDecimal::from_str("3").unwrap())).into()
            )
            .into()
        )
    );
}

#[test]
fn boolean_function() {
    let filter = "isActive eq true and not contains(name, 'Admin')";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::And(
            Expr::Compare(
                Expr::Identifier("isActive".to_owned()).into(),
                Equal,
                Expr::Value(Value::Bool(true)).into()
            )
            .into(),
            Expr::Not(
                Expr::Function(
                    "contains".to_owned(),
                    vec![
                        Expr::Identifier("name".to_owned()),
                        Expr::Value(Value::String("Admin".to_owned()))
                    ]
                )
                .into()
            )
            .into()
        )
    );
}

#[test]
fn nested_and_or_not() {
    let filter =
        "not ((price gt 50.0 or price lt 30.0) and not (discount eq 5.0 or discount eq 10.0))";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Not(
            Expr::And(
                Expr::Or(
                    Expr::Compare(
                        Expr::Identifier("price".to_owned()).into(),
                        GreaterThan,
                        Expr::Value(Value::Number(BigDecimal::from_str("50.0").unwrap())).into()
                    )
                    .into(),
                    Expr::Compare(
                        Expr::Identifier("price".to_owned()).into(),
                        LessThan,
                        Expr::Value(Value::Number(BigDecimal::from_str("30.0").unwrap())).into()
                    )
                    .into()
                )
                .into(),
                Expr::Not(
                    Expr::Or(
                        Expr::Compare(
                            Expr::Identifier("discount".to_owned()).into(),
                            Equal,
                            Expr::Value(Value::Number(BigDecimal::from_str("5.0").unwrap())).into()
                        )
                        .into(),
                        Expr::Compare(
                            Expr::Identifier("discount".to_owned()).into(),
                            Equal,
                            Expr::Value(Value::Number(BigDecimal::from_str("10.0").unwrap()))
                                .into()
                        )
                        .into()
                    )
                    .into()
                )
                .into()
            )
            .into()
        )
    );
}

#[test]
fn multiple_nested_functions() {
    let filter = "concat(concat(city, ', '), country) eq 'Berlin, Germany' and contains(description, 'sample')";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::And(
            Expr::Compare(
                Expr::Function(
                    "concat".to_owned(),
                    vec![
                        Expr::Function(
                            "concat".to_owned(),
                            vec![
                                Expr::Identifier("city".to_owned()),
                                Expr::Value(Value::String(", ".to_owned()))
                            ]
                        ),
                        Expr::Identifier("country".to_owned())
                    ]
                )
                .into(),
                Equal,
                Expr::Value(Value::String("Berlin, Germany".to_owned())).into()
            )
            .into(),
            Expr::Function(
                "contains".to_owned(),
                vec![
                    Expr::Identifier("description".to_owned()),
                    Expr::Value(Value::String("sample".to_owned()))
                ]
            )
            .into()
        )
    );
}

#[test]
fn and_binds_tighter_than_or() {
    let filter = "aa eq 1 and bb eq 2 or cc eq 3";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Or(
            Expr::And(
                Expr::Compare(
                    Expr::Identifier("aa".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::Number(BigDecimal::from_str("1").unwrap())).into()
                )
                .into(),
                Expr::Compare(
                    Expr::Identifier("bb".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::Number(BigDecimal::from_str("2").unwrap())).into()
                )
                .into()
            )
            .into(),
            Expr::Compare(
                Expr::Identifier("cc".to_owned()).into(),
                Equal,
                Expr::Value(Value::Number(BigDecimal::from_str("3").unwrap())).into()
            )
            .into()
        )
    );
}

#[test]
fn or_does_not_capture_and_rhs() {
    let filter = "aa eq 1 or bb eq 2 and cc eq 3";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Or(
            Expr::Compare(
                Expr::Identifier("aa".to_owned()).into(),
                Equal,
                Expr::Value(Value::Number(BigDecimal::from_str("1").unwrap())).into()
            )
            .into(),
            Expr::And(
                Expr::Compare(
                    Expr::Identifier("bb".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::Number(BigDecimal::from_str("2").unwrap())).into()
                )
                .into(),
                Expr::Compare(
                    Expr::Identifier("cc".to_owned()).into(),
                    Equal,
                    Expr::Value(Value::Number(BigDecimal::from_str("3").unwrap())).into()
                )
                .into()
            )
            .into()
        )
    );
}

#[test]
fn single_char_identifier() {
    let filter = "x eq 1";
    let result = parse_str(filter).expect("valid filter tree");

    assert_eq!(
        result,
        Expr::Compare(
            Expr::Identifier("x".to_owned()).into(),
            Equal,
            Expr::Value(Value::Number(BigDecimal::from_str("1").unwrap())).into()
        )
    );
}

#[test]
fn deeply_nested_unmatched_parens_does_not_hang() {
    // Pathological input: unmatched parentheses must fail fast, not consume
    // exponential memory/time due to backtracking.
    let result = parse_str("((((EAEAEAE(((EAEA((AE(((EAEAEEE");
    assert!(result.is_err());
}

#[test]
fn datetime_rejects_nonexistent_dst_spring_forward() {
    use crate::odata_filters::ParseError;

    // 2024-03-10 02:30 does not exist in America/New_York (clocks spring forward
    // from 02:00 to 03:00), so .earliest() returns None → ParsingDateTime.
    let filter = "AT eq 2024-03-10T02:30:00America/New_York";
    let err = parse_str(filter).unwrap_err();
    assert_eq!(err, ParseError::ParsingDateTime);
}

#[test]
fn parse_error_preserves_position_info() {
    use crate::odata_filters::ParseError;

    let result = parse_str("name eq AND broken");
    let err = result.unwrap_err();

    // Must be Parsing(String) with position detail, not a bare unit variant
    match &err {
        ParseError::Parsing(msg) => {
            assert!(
                msg.contains("error at") && msg.contains("expected"),
                "PEG error should contain position and expectation info, got: {msg}"
            );
        }
        other => panic!("expected ParseError::Parsing(String), got: {other:?}"),
    }
}
