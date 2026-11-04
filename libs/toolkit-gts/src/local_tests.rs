use gts::GtsId;
use serde_json::json;

use super::{DeclarationMerge, DeclarationSource, merge_declarations, same_canonical_content};

const A: &str = "gts.cf.test.local.a.v1~";
const B: &str = "gts.cf.test.local.b.v1~";
const C: &str = "gts.cf.test.local.c.v1~";

fn source(origin: &str, declarations: &[(&str, &str)]) -> DeclarationSource {
    DeclarationSource::new(
        origin,
        declarations
            .iter()
            .map(|(id, text)| ((*id).to_owned(), (*text).to_owned()))
            .collect(),
    )
}

fn id(id: &str) -> GtsId {
    GtsId::try_new(id).expect("a valid GTS id")
}

#[test]
fn merges_sorted_by_id_and_keeps_one_of_equal_duplicates() {
    let merged = merge_declarations([
        source("b-sdk", &[(B, r#"{"x":1,"y":[1,2]}"#)]),
        source(
            "a-gear",
            &[
                (A, r#"{"k":"v"}"#),
                // Same content, keys reordered: one entry.
                (B, r#"{"y":[1,2],"x":1}"#),
            ],
        ),
    ])
    .expect("no conflict");

    assert_eq!(
        merged,
        vec![
            (id(A), json!({"k": "v"})),
            (id(B), json!({"x": 1, "y": [1, 2]})),
        ]
    );
}

#[test]
fn a_conflicting_id_names_both_origins() {
    let err = merge_declarations([
        source("tenant-resolver-sdk", &[(A, r#"{"v":1}"#)]),
        source("account-management", &[(A, r#"{"v":2}"#)]),
    ])
    .expect_err("conflict");

    let message = err.to_string();
    assert!(
        message.contains(A)
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
            source("a", &[(A, r#"{"n":0.0}"#)]),
            source("b", &[(A, r#"{"n":-0.0}"#)]),
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
        source(
            "a",
            &[(A, "{"), (B, "1"), ("gts.cf.test.local.Bad.v1~", "{}")],
        ),
        source("b", &[(B, "2")]),
    ])
    .expect_err("invalid and conflicting");

    let message = err.to_string();
    assert!(
        message.contains(&format!("{A} from a: invalid JSON"))
            && message.contains(&format!("{B} is declared"))
            && message.contains("gts.cf.test.local.Bad.v1~ from a: invalid GTS id"),
        "{message}"
    );
}

/// A static id is validated as spelled: no other spelling of a valid id is accepted,
/// so ids that differ as text are different identifiers.
#[test]
fn a_static_id_in_another_spelling_is_rejected_with_its_origin() {
    for spelling in [
        format!("gts://{A}"),
        A.to_uppercase(),
        format!(" {A}"),
        format!("{A} "),
        "gts.cf.test.local.a.v01~".to_owned(),
        "gts.cf.test.local.a-b.v1~".to_owned(),
        "gts.a~".to_owned(),
    ] {
        let mut merge = DeclarationMerge::default();
        merge
            .add_source(source("my-gear-sdk", &[(A, "{}")]))
            .add_source(source("my-gear", &[(spelling.as_str(), "{}")]));

        let message = merge.finish().expect_err(&spelling).to_string();
        assert!(
            message.contains(&format!("{spelling} from my-gear: invalid GTS id")),
            "{message}"
        );
    }
}

/// Typed documents merge with the parsed static ones as they are: one order, one
/// dedup and one conflict check, by canonical content.
#[test]
fn typed_documents_merge_with_static_ones() {
    let mut merge = DeclarationMerge::default();
    merge
        .add_source(source(
            "my-gear-sdk",
            &[(C, r#"{"n":1.0,"k":"v"}"#), (A, "{}")],
        ))
        .add_documents(
            "entities",
            [
                (id(B), json!({ "b": 1 })),
                // The static content again, keys reordered: one entry.
                (id(C), json!({ "k": "v", "n": 1.0 })),
            ],
        );

    assert_eq!(
        merge.finish().expect("no conflict"),
        [
            (id(A), json!({})),
            (id(B), json!({ "b": 1 })),
            (id(C), json!({ "k": "v", "n": 1.0 })),
        ]
    );

    let mut merge = DeclarationMerge::default();
    merge
        .add_source(source("my-gear-sdk", &[(C, r#"{"n":0.0}"#)]))
        .add_documents("entities", [(id(C), json!({ "n": -0.0 }))]);
    let message = merge.finish().expect_err("conflict").to_string();
    assert!(
        message.contains(&format!(
            "{C} is declared with different content by my-gear-sdk and entities"
        )),
        "{message}"
    );
}
