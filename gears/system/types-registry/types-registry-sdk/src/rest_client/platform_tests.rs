#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "malformed test fixtures must fail the test immediately"
)]

use std::time::Duration;

use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

use super::{Answer, Receipt, requested};
use crate::models::{Operation, OperationStatus, RegistrationOperation};

const OPERATION: &str = "0190f1a2-0000-7000-8000-000000000001";
const CEILING: Duration = Duration::from_secs(2);

fn answer(
    status: u16,
    location: Option<&str>,
    retry_after: Option<u64>,
    body: &serde_json::Value,
) -> Answer {
    Answer {
        status,
        location: location.map(str::to_owned),
        retry_after: retry_after.map(Duration::from_secs),
        body: body.to_string().into(),
    }
}

fn receipt(status: &str, replayed: bool) -> serde_json::Value {
    json!({"operation_id": OPERATION, "status": status, "replayed": replayed})
}

fn location() -> String {
    format!("/cf/types-registry/v2/platform/operations/{OPERATION}")
}

#[test]
fn a_fresh_operation_waits_the_advised_delay_before_its_first_read() {
    let read = Receipt::read(&answer(
        202,
        Some(&location()),
        Some(1),
        &receipt("pending", false),
    ))
    .expect("a fresh receipt");
    assert_eq!(read.operation_id, Uuid::parse_str(OPERATION).unwrap());
    assert_eq!(read.pause(CEILING), Some(Duration::from_secs(1)));

    let long = Receipt::read(&answer(
        202,
        Some(&location()),
        Some(600),
        &receipt("running", true),
    ))
    .expect("a replay still running");
    assert_eq!(
        long.pause(CEILING),
        Some(CEILING),
        "bounded like a retry delay"
    );
}

#[test]
fn a_terminal_operation_is_read_at_once() {
    for answer in [
        answer(
            202,
            Some(&location()),
            Some(1),
            &receipt("completed", false),
        ),
        answer(200, Some(&location()), None, &receipt("completed", true)),
        answer(202, Some(&location()), None, &receipt("pending", false)),
    ] {
        let read = Receipt::read(&answer).expect("a consistent receipt");
        assert_eq!(read.pause(CEILING), None);
    }
}

#[test]
fn an_inconsistent_receipt_names_the_operation() {
    let other = format!("/types-registry/v2/platform/operations/{}", Uuid::nil());
    for answer in [
        // `200` only for a terminal replay, `202` never for one.
        answer(200, Some(&location()), None, &receipt("running", true)),
        answer(200, Some(&location()), None, &receipt("completed", false)),
        answer(202, Some(&location()), None, &receipt("completed", true)),
        answer(201, Some(&location()), None, &receipt("pending", false)),
        // The `Location` must name this operation.
        answer(202, None, Some(1), &receipt("pending", false)),
        answer(202, Some(&other), Some(1), &receipt("pending", false)),
    ] {
        let refused = Receipt::read(&answer).err().expect("refused");
        let CanonicalError::Unknown { resource_name, .. } = &refused else {
            panic!("Unknown, got {refused:?}");
        };
        assert_eq!(resource_name.as_deref(), Some(OPERATION));
    }
}

#[test]
fn a_receipt_that_does_not_parse_is_internal() {
    let refused = Receipt::read(&answer(
        202,
        Some(&location()),
        None,
        &json!({"status": "pending"}),
    ))
    .err()
    .expect("refused");
    assert!(
        matches!(refused, CanonicalError::Internal { .. }),
        "{refused:?}"
    );
}

#[test]
fn an_operation_read_must_answer_the_operation_asked_for() {
    let asked = Uuid::parse_str(OPERATION).unwrap();
    let operation = |operation_id| {
        Operation::Registration(RegistrationOperation {
            operation_id,
            status: OperationStatus::Completed,
            items: Vec::new(),
        })
    };
    assert!(requested(asked, operation(asked)).is_ok());
    let refused = requested(asked, operation(Uuid::from_u128(2)))
        .expect_err("another operation is not this one");
    assert!(
        matches!(refused, CanonicalError::Internal { .. }),
        "{refused:?}"
    );
}
