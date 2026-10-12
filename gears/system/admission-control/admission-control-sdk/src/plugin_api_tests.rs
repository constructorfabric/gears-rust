#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

#[test]
fn engine_failures_map_to_refusal_causes() {
    let could_not_run = |condition| RefusalCause::CouldNotRun { condition };
    let cases = [
        (
            EngineFailure::unavailable("d"),
            could_not_run(FailureCondition::EngineUnavailable),
            true,
        ),
        (
            EngineFailure::timeout("d"),
            could_not_run(FailureCondition::EngineTimeout),
            true,
        ),
        (
            EngineFailure::internal("d"),
            could_not_run(FailureCondition::EngineError),
            true,
        ),
        (
            EngineFailure::invalid_request("d"),
            RefusalCause::InvalidRequest,
            false,
        ),
        (
            EngineFailure::contract_violation("d"),
            could_not_run(FailureCondition::Internal),
            false,
        ),
    ];
    for (failure, expected, retryable) in cases {
        let cause = failure.condition.refusal_cause();
        assert_eq!(cause, expected);
        assert_eq!(cause.is_retryable(), retryable, "{failure}");
    }
}

#[test]
fn engine_request_is_stamped_from_admission_request() {
    let tenant = Uuid::from_u128(2);
    let request = AdmissionRequest::new("g", "create", "gts.cf.core.test.widget.v1~", tenant)
        .with_resource_id(Uuid::from_u128(3))
        .with_property("k", serde_json::json!(1));
    let engine = EngineRequest::from_admission(&request, Uuid::from_u128(1));
    assert_eq!(engine.correlation_id, Uuid::from_u128(1));
    assert_eq!(engine.resource_tenant_id, tenant);
    assert_eq!(engine.resource_id, Some(Uuid::from_u128(3)));
    assert_eq!(engine.properties, request.properties);
}

#[test]
fn failure_display_names_condition_and_detail() {
    assert_eq!(
        EngineFailure::timeout("slow").to_string(),
        "engine failure (timeout): slow"
    );
}
