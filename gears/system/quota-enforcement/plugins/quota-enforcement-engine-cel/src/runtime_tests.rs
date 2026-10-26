use std::num::NonZeroU64;

use quota_enforcement_sdk::{EngineError, EvaluationBudget};
use serde_json::{Value, json};

use super::*;

/// A cost budget of `limit` units with a timeout no slow CI runner reaches.
fn budget(limit: u64) -> Result<EvaluationBudget, EngineError> {
    let nonzero = |value| {
        NonZeroU64::new(value)
            .ok_or_else(|| EngineError::Internal("test budget must be nonzero".into()))
    };
    EvaluationBudget::new(Some(60_000), nonzero(60_000)?, nonzero(limit)?)
}

fn runtime() -> Result<Runtime, EngineError> {
    Ok(Runtime {
        meter: budget(1_000_000)?.start(),
        variables: BTreeMap::new(),
    })
}

fn parse(source: &str) -> Result<SpannedExpr, EngineError> {
    let parsed = cel_core::parse(source);
    if let Some(error) = parsed.errors.first() {
        return Err(EngineError::Internal(format!(
            "test expression did not parse: {}",
            error.message
        )));
    }
    parsed
        .ast
        .ok_or_else(|| EngineError::Internal("test expression produced no AST".into()))
}

fn eval(source: &str) -> Result<Value, EngineError> {
    runtime()?.eval(&parse(source)?)
}

/// `source` against one bound `record`, under a budget of `limit` units.
fn eval_bounded(source: &str, record: Value, limit: u64) -> Result<Value, EngineError> {
    let mut runtime = Runtime {
        meter: budget(limit)?.start(),
        variables: BTreeMap::from([("record".to_owned(), record)]),
    };
    runtime.eval(&parse(source)?)
}

fn large_record() -> Value {
    json!({ "region": "eu", "blob": "x".repeat(4000) })
}

#[test]
fn a_field_read_charges_the_field_not_the_whole_record() -> Result<(), EngineError> {
    assert_eq!(
        eval_bounded(r#"record.region == "eu""#, large_record(), 20)?,
        json!(true)
    );
    assert_eq!(
        eval_bounded("has(record.region)", large_record(), 20)?,
        json!(true)
    );
    Ok(())
}

#[test]
fn copying_a_large_value_is_still_charged() {
    assert_eq!(
        eval_bounded("record", large_record(), 20),
        Err(EngineError::CostExceeded)
    );
    assert_eq!(
        eval_bounded("record.blob", large_record(), 20),
        Err(EngineError::CostExceeded)
    );
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

#[test]
fn runtime_evaluates_collections_maps_and_the_remaining_operators() -> Result<(), EngineError> {
    let cases = [
        ("[1] + [2]", json!([1, 2])),
        ("1 + 2", json!(3)),
        ("1 <= 1", json!(true)),
        ("2 >= 3", json!(false)),
        ("null == null", json!(true)),
        ("1 != 2", json!(true)),
        ("!false", json!(true)),
        ("true && true", json!(true)),
        ("false || true", json!(true)),
        (r#"has({"a": 1}.a)"#, json!(true)),
        (r#"has({"a": 1}.b)"#, json!(false)),
        (r#"{"a": 1}["a"]"#, json!(1)),
        (r#"{"a": 1, "b": 2}.all(k, k != "c")"#, json!(true)),
    ];

    for (source, expected) in cases {
        assert_eq!(eval(source)?, expected, "expression: {source}");
    }
    Ok(())
}

#[test]
fn runtime_reports_map_call_and_node_errors() {
    let cases = [
        (r#"{"a": 1, "a": 2}"#, "duplicate map key"),
        ("{1: 2}", "map keys must be strings"),
        (r#"{"a": 1}[1]"#, "map index must be a string"),
        (r#"{"a": 1}.b"#, "missing CEL field"),
        ("1.all(x, true)", "comprehension requires collection"),
        ("size(1)", "size requires collection or string"),
        ("9223372036854775807 + 1", "integer overflow"),
        (r#""a".missing()"#, "unsupported function or argument types"),
        ("1.5", "unsupported compiled CEL node"),
    ];

    for (source, message) in cases {
        let error = eval(source).expect_err("invalid runtime operation must fail");
        assert!(error.to_string().contains(message), "{source}: {error}");
    }
}

#[test]
fn a_value_passed_through_ternaries_or_a_bind_is_charged_once() -> Result<(), EngineError> {
    let nested = r#"true ? (true ? (true ? record.blob : "") : "") : """#;
    assert!(eval_bounded(nested, large_record(), 80)?.is_string());
    let bound = "cel.bind(x, record.blob, x)";
    assert!(eval_bounded(bound, large_record(), 140)?.is_string());
    Ok(())
}

fn range(n: usize) -> String {
    let items: Vec<String> = (0..n).map(|i| i.to_string()).collect();
    format!("[{}]", items.join(","))
}

#[test]
fn an_appended_list_keeps_the_list_byte_bound() {
    // 300 copies of a 4 KB string pass 1 MiB in total, each well within 64 KiB.
    let past_bytes = format!("{}.map(i, record.blob)", range(300));
    assert_eq!(
        eval_bounded(&past_bytes, large_record(), 1_000_000),
        Err(EngineError::CostExceeded)
    );
    let within = format!("{}.map(i, record.region)", range(300));
    assert_eq!(
        eval_bounded(&within, large_record(), 1_000_000).map(|v| v.as_array().map(Vec::len)),
        Ok(Some(300))
    );
}

#[test]
fn a_step_that_reads_the_accumulator_keeps_its_meaning() -> Result<(), EngineError> {
    assert_eq!(eval("[1, 2].map(x, size(__result__))")?, json!([0, 1]));
    assert_eq!(
        eval("[1, 2, 3].filter(x, size(__result__) < 2)")?,
        json!([1, 2])
    );
    // The inner `__result__` is the inner accumulator, so the outer map still appends in place.
    assert_eq!(
        eval("[[1, 2], [3]].map(l, l.map(i, size(__result__)))")?,
        json!([[0, 1], [0]])
    );
    Ok(())
}
