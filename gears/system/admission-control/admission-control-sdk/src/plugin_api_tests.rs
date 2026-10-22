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

fn reference(n: u128) -> PolicyReference {
    PolicyReference {
        bundle_id: Uuid::from_u128(n),
        version_id: Uuid::from_u128(n),
        document_id: Uuid::from_u128(n),
        document_name: format!("doc{n}"),
    }
}

fn named(name: &str) -> PolicyReference {
    PolicyReference {
        document_name: name.to_owned(),
        ..reference(1)
    }
}

fn deny(reason_code: &str, denials: Vec<PolicyReference>) -> EngineResult {
    EngineResult::Deny {
        reason_code: reason_code.to_owned(),
        denials,
        shadow_denials: Vec::new(),
    }
}

#[test]
fn results_within_the_bounds_pass() {
    let at_the_bounds = deny(
        &"R".repeat(MAX_REASON_CODE_LEN),
        (0..MAX_ENGINE_FINDINGS as u128)
            .map(|n| PolicyReference {
                document_name: "d".repeat(MAX_DOCUMENT_NAME_LEN),
                ..reference(n)
            })
            .collect(),
    );
    assert_eq!(at_the_bounds.bounds_violation(), None);
    assert_eq!(EngineResult::deny("POLICY_DENIED").bounds_violation(), None);
}

#[test]
fn each_bound_is_reported() {
    let too_many = (0..=MAX_ENGINE_FINDINGS as u128)
        .map(reference)
        .collect::<Vec<_>>();
    let cases = [
        (
            deny("", vec![reference(1)]),
            EngineResultViolation::ReasonCode,
        ),
        (
            deny("HAS SPACE", Vec::new()),
            EngineResultViolation::ReasonCode,
        ),
        (
            deny(&"R".repeat(MAX_REASON_CODE_LEN + 1), Vec::new()),
            EngineResultViolation::ReasonCode,
        ),
        (
            deny("R", vec![named("")]),
            EngineResultViolation::DocumentName,
        ),
        (
            deny("R", vec![named("a\nb")]),
            EngineResultViolation::DocumentName,
        ),
        (
            deny("R", vec![named(&"d".repeat(MAX_DOCUMENT_NAME_LEN + 1))]),
            EngineResultViolation::DocumentName,
        ),
        (
            deny("R", too_many.clone()),
            EngineResultViolation::TooManyFindings,
        ),
        (
            EngineResult::Permit {
                shadow_denials: too_many,
            },
            EngineResultViolation::TooManyFindings,
        ),
    ];
    for (result, expected) in cases {
        assert_eq!(result.bounds_violation(), Some(expected), "{result:?}");
    }
    assert_eq!(
        EngineResultViolation::TooManyFindings.to_string(),
        "too_many_findings"
    );
}

#[test]
fn the_builder_denies_only_on_an_enforced_denial() {
    let mut shadow_only = EngineResultBuilder::new();
    shadow_only.record_denial(reference(1), false);
    assert!(!shadow_only.is_denied());
    assert_eq!(
        shadow_only.finish("POLICY_DENIED"),
        EngineResult::Permit {
            shadow_denials: vec![reference(1)]
        }
    );

    let mut builder = EngineResultBuilder::new();
    builder.record_denial(reference(1), false);
    builder.record_denial(reference(2), true);
    builder.record_denial(reference(3), true);
    assert!(builder.is_denied());
    assert_eq!(builder.finding_count(), 3);
    assert_eq!(
        builder.finish("POLICY_DENIED"),
        EngineResult::Deny {
            reason_code: "POLICY_DENIED".to_owned(),
            denials: vec![reference(2), reference(3)],
            shadow_denials: vec![reference(1)],
        }
    );
}

#[test]
fn the_builder_stays_within_the_findings_bound_dropping_shadow_denials_first() {
    let mut builder = EngineResultBuilder::new();
    for n in 0..MAX_ENGINE_FINDINGS as u128 {
        builder.record_denial(reference(n), false);
    }
    builder.record_denial(reference(1_000), true);
    assert_eq!(builder.finding_count(), MAX_ENGINE_FINDINGS + 1);
    let result = builder.finish("POLICY_DENIED");
    assert_eq!(result.bounds_violation(), None);
    let EngineResult::Deny {
        denials,
        shadow_denials,
        ..
    } = result
    else {
        panic!("an enforced denial must survive truncation: {result:?}");
    };
    assert_eq!(denials, vec![reference(1_000)]);
    assert_eq!(shadow_denials.len(), MAX_ENGINE_FINDINGS - 1);

    let mut enforced = EngineResultBuilder::new();
    for n in 0..=MAX_ENGINE_FINDINGS as u128 {
        enforced.record_denial(reference(n), true);
    }
    assert_eq!(
        enforced.finish("POLICY_DENIED").bounds_violation(),
        None,
        "enforced denials are capped as well"
    );
}
