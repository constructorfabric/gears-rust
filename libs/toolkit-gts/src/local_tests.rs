use serde_json::json;

use super::{DeclarationSource, merge_declarations, same_canonical_content};

fn source(origin: &str, declarations: &[(&str, &str)]) -> DeclarationSource {
    DeclarationSource::new(
        origin,
        declarations
            .iter()
            .map(|(id, text)| ((*id).to_owned(), (*text).to_owned()))
            .collect(),
    )
}

#[test]
fn merges_sorted_by_id_and_keeps_one_of_equal_duplicates() {
    let merged = merge_declarations([
        source("b-sdk", &[("gts.b~", r#"{"x":1,"y":[1,2]}"#)]),
        source(
            "a-gear",
            &[
                ("gts.a~", r#"{"k":"v"}"#),
                // Same content, keys reordered: one entry.
                ("gts.b~", r#"{"y":[1,2],"x":1}"#),
            ],
        ),
    ])
    .expect("no conflict");

    assert_eq!(
        merged,
        vec![
            ("gts.a~".to_owned(), json!({"k": "v"})),
            ("gts.b~".to_owned(), json!({"x": 1, "y": [1, 2]})),
        ]
    );
}

#[test]
fn a_conflicting_id_names_both_origins() {
    let err = merge_declarations([
        source("tenant-resolver-sdk", &[("gts.t~", r#"{"v":1}"#)]),
        source("account-management", &[("gts.t~", r#"{"v":2}"#)]),
    ])
    .expect_err("conflict");

    let message = err.to_string();
    assert!(
        message.contains("gts.t~")
            && message.contains("tenant-resolver-sdk")
            && message.contains("account-management"),
        "{message}"
    );
}

/// The registry keeps each number's spelling: `-0.0` is not `0.0`.
#[test]
fn negative_zero_conflicts_with_zero() {
    assert!(
        merge_declarations([
            source("a", &[("gts.z~", r#"{"n":0.0}"#)]),
            source("b", &[("gts.z~", r#"{"n":-0.0}"#)]),
        ])
        .is_err()
    );
    assert_eq!(
        json!(0.0),
        json!(-0.0),
        "plain Value equality would merge them"
    );
}

#[test]
fn array_order_is_content() {
    assert!(!same_canonical_content(&json!([1, 2]), &json!([2, 1])));
    assert!(same_canonical_content(
        &json!({"a": [{"b": 1, "c": 2}]}),
        &json!({"a": [{"c": 2, "b": 1}]})
    ));
    assert!(!same_canonical_content(
        &json!({"a": 1}),
        &json!({"a": 1, "b": 2})
    ));
    assert!(!same_canonical_content(&json!(1), &json!(1.0)));
}

#[test]
fn every_problem_is_reported_at_once() {
    let err = merge_declarations([
        source("a", &[("gts.bad~", "{"), ("gts.c~", "1")]),
        source("b", &[("gts.c~", "2")]),
    ])
    .expect_err("invalid and conflicting");

    let message = err.to_string();
    assert!(
        message.contains("gts.bad~ from a: invalid JSON") && message.contains("gts.c~ is declared"),
        "{message}"
    );
}
