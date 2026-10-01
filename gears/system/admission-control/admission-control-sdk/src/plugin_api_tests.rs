#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

#[test]
fn engine_failures_map_to_could_not_run_conditions() {
    let cases = [
        (
            EngineFailure::unavailable("d"),
            FailureCondition::EngineUnavailable,
        ),
        (EngineFailure::timeout("d"), FailureCondition::EngineTimeout),
        (EngineFailure::internal("d"), FailureCondition::EngineError),
        (
            EngineFailure::invalid_request("d"),
            FailureCondition::EngineError,
        ),
    ];
    for (failure, expected) in cases {
        assert_eq!(failure.condition.failure_condition(), expected);
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
