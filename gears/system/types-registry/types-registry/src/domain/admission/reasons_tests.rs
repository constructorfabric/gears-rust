//! Delivery failure code checks. The candidate-refusal vocabulary is the SDK's
//! and is checked there (`types-registry-sdk/src/item_failure_tests.rs`).

// Malformed test fixtures and source guards must fail the test immediately.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::DeliveryFailure as Delivery;

fn delivery_codes() -> Vec<Delivery> {
    vec![
        Delivery::UnexpectedPayloadType,
        Delivery::InvalidOperationPayload,
        Delivery::ServiceFailure,
        Delivery::OperationNotFound,
        Delivery::DeliveryBudgetExhausted,
        Delivery::AdmissionDeadlineExceeded,
    ]
}

#[test]
fn delivery_codes_are_the_stable_wire_strings() {
    assert_eq!(
        Delivery::UnexpectedPayloadType.as_str(),
        "unexpected_payload_type"
    );
    assert_eq!(
        Delivery::InvalidOperationPayload.as_str(),
        "invalid_operation_payload"
    );
    assert_eq!(
        Delivery::ServiceFailure.as_str(),
        "admission_service_failure"
    );
    assert_eq!(Delivery::OperationNotFound.as_str(), "operation_not_found");
    assert_eq!(
        Delivery::DeliveryBudgetExhausted.as_str(),
        "delivery_budget_exhausted"
    );
    assert_eq!(
        Delivery::AdmissionDeadlineExceeded.as_str(),
        "admission_deadline_exceeded"
    );
}

#[test]
fn delivery_codes_are_distinct_and_snake_case() {
    let codes: Vec<&str> = delivery_codes().iter().map(|f| f.as_str()).collect();

    let mut unique = codes.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        codes.len(),
        "two delivery failures answer the same error_code: {codes:?}",
    );

    for code in &codes {
        assert!(
            !code.is_empty() && code.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "{code} is not a snake_case wire code",
        );
    }
}

#[test]
fn an_admission_failure_forwards_the_workers_own_code() {
    let worker = crate::domain::admission::errors::WorkerError::ItemOutcomeVanished { item_id: 7 };

    assert_eq!(
        Delivery::Admission(worker.code()).as_str(),
        worker.code(),
        "the worker's code must reach the wire unchanged",
    );
}

#[test]
fn the_operation_not_found_code_is_shared_with_the_worker_on_purpose() {
    let worker = crate::domain::admission::errors::WorkerError::OperationNotFound {
        operation_id: uuid::Uuid::nil(),
    };

    assert_eq!(Delivery::OperationNotFound.as_str(), worker.code());
}
