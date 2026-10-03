use std::num::NonZeroU64;

use quota_enforcement_sdk::{EngineError, EvaluationBudget};
use serde_json::{Value, json};

use super::*;

fn runtime() -> Result<Runtime, EngineError> {
    let limit = NonZeroU64::new(1_000_000)
        .ok_or_else(|| EngineError::Internal("test budget must be nonzero".into()))?;
    Ok(Runtime {
        meter: EvaluationBudget::new(None, limit, limit)?.start(),
        variables: BTreeMap::new(),
    })
}

fn eval(source: &str) -> Result<Value, EngineError> {
    let parsed = cel_core::parse(source);
    if let Some(error) = parsed.errors.first() {
        return Err(EngineError::Internal(format!(
            "test expression did not parse: {}",
            error.message
        )));
    }
    let expr = parsed
        .ast
        .ok_or_else(|| EngineError::Internal("test expression produced no AST".into()))?;
    runtime()?.eval(&expr)
}

#[test]
fn measure_rejects_oversized_collections_and_values() {
    let list = Value::Array(vec![Value::Null; MAX_ITEMS + 1]);
    assert_eq!(measure(&list), Err(EngineError::CostExceeded));

    let object = Value::Object(
        (0..=MAX_ITEMS)
            .map(|index| (index.to_string(), Value::Null))
            .collect(),
    );
    assert_eq!(measure(&object), Err(EngineError::CostExceeded));

    let string = Value::String("x".repeat(MAX_VALUE_BYTES));
    assert_eq!(measure(&string), Err(EngineError::CostExceeded));
}

#[test]
fn measure_rejects_values_nested_beyond_the_runtime_limit() {
    let nested = (0..=MAX_DEPTH).fold(Value::Null, |value, _| Value::Array(vec![value]));

    let error = measure(&nested).expect_err("runtime value depth must be bounded");
    assert_eq!(
        error,
        EngineError::TypeError("value nesting exceeds the CEL bound".into())
    );
}

#[test]
fn runtime_evaluates_supported_integer_and_string_operators() -> Result<(), EngineError> {
    let cases = [
        ("7 - 2", json!(5)),
        ("7 * 2", json!(14)),
        ("7 / 2", json!(3)),
        ("7 % 2", json!(1)),
        (r#""ab" + "cd""#, json!("abcd")),
        (r#""b" > "a""#, json!(true)),
        (r#""x" in {"x": 1}"#, json!(true)),
        ("2 in [1, 2, 3]", json!(true)),
    ];

    for (source, expected) in cases {
        assert_eq!(eval(source)?, expected, "expression: {source}");
    }
    Ok(())
}

#[test]
fn runtime_evaluates_bounded_standard_functions() -> Result<(), EngineError> {
    let cases = [
        ("size([1, 2])", json!(2)),
        (r#"size({"a": 1})"#, json!(1)),
        (r#"size("\u00e9")"#, json!(1)),
        (r#""abc".contains("b")"#, json!(true)),
        (r#""abc".startsWith("a")"#, json!(true)),
        (r#""abc".endsWith("c")"#, json!(true)),
        (r#"int("42")"#, json!(42)),
        ("int(7)", json!(7)),
        ("string(7)", json!("7")),
        (r#"string("value")"#, json!("value")),
    ];

    for (source, expected) in cases {
        assert_eq!(eval(source)?, expected, "expression: {source}");
    }
    Ok(())
}

#[test]
fn runtime_reports_arithmetic_and_collection_errors() {
    let cases = [
        ("1 / 0", "integer overflow or division by zero"),
        ("-(-9223372036854775807 - 1)", "integer overflow"),
        ("[1][-1]", "negative index"),
        ("[1][2]", "index outside collection"),
        (
            r#"1 in "not a collection""#,
            "membership requires collection",
        ),
        (r#"1 in {"x": 1}"#, "membership key must be a string"),
        (r#"int("not-an-integer")"#, "invalid integer"),
    ];

    for (source, message) in cases {
        let error = eval(source).expect_err("invalid runtime operation must fail");
        assert!(error.to_string().contains(message), "{source}: {error}");
    }
}

#[test]
fn runtime_short_circuits_unreachable_boolean_operands() -> Result<(), EngineError> {
    assert_eq!(eval("false && missing")?, json!(false));
    assert_eq!(eval("true || missing")?, json!(true));
    Ok(())
}
