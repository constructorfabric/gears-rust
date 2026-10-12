//! Wire codes of the canonical mappings the decimal-money change introduced or
//! moved: the reason / field-violation pair a client branches on, not only the
//! status family.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use toolkit::api::canonical_prelude::{CanonicalError, Problem};

use crate::domain::error::DomainError;

fn body(err: DomainError) -> (u16, serde_json::Value) {
    let canonical = CanonicalError::from(err);
    let status = canonical.status_code();
    (
        status,
        serde_json::to_value(Problem::from(canonical)).unwrap(),
    )
}

/// The `(field, reason)` pairs of every field violation in a Problem.
fn violations(value: &serde_json::Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![value];
    while let Some(v) = stack.pop() {
        match v {
            serde_json::Value::Object(map) => {
                if let (Some(serde_json::Value::String(f)), Some(serde_json::Value::String(r))) =
                    (map.get("field"), map.get("reason"))
                {
                    out.push((f.clone(), r.clone()));
                }
                stack.extend(map.values());
            }
            serde_json::Value::Array(items) => stack.extend(items),
            _ => {}
        }
    }
    out
}

#[test]
fn inconsistent_scale_is_currency_scale_mismatch_not_amount_out_of_range() {
    let (status, problem) = body(DomainError::InconsistentScale("USD 2 != USD 3".into()));
    assert_eq!(status, 400);
    assert_eq!(
        violations(&problem),
        vec![("lines".to_owned(), "CURRENCY_SCALE_MISMATCH".to_owned())]
    );
    assert!(!problem.to_string().contains("AMOUNT_OUT_OF_RANGE"));
}

#[test]
fn invalid_posting_increment_is_a_field_violation_on_amount() {
    let (status, problem) = body(DomainError::InvalidPostingIncrement(
        "0.001 at scale 2".into(),
    ));
    assert_eq!(status, 400);
    assert_eq!(
        violations(&problem),
        vec![("amount".to_owned(), "INVALID_POSTING_INCREMENT".to_owned())]
    );
}

#[test]
fn concurrent_modification_is_aborted_with_its_reason() {
    let (status, problem) = body(DomainError::ConcurrentModification("row moved".into()));
    assert_eq!(status, 409);
    let text = problem.to_string();
    assert!(text.contains("CONCURRENT_MODIFICATION"), "{text}");
    assert!(violations(&problem).is_empty());
}
