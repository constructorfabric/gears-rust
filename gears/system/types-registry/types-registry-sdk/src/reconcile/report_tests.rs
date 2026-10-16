use serde_json::{Value, json};
use toolkit_canonical_errors::{CanonicalError, Http, Problem};

use super::{ReconcileOutcome, ReconcilePendingCause, ReconcileReport, ReconcileStop};
use crate::gts::{OperationResource, TypeResource};
use crate::{AdmissionFailure, AdmissionFailureReason, GtsId};

fn id(index: usize) -> GtsId {
    GtsId::try_new(&format!("gts.cf.test.pkg.n{index}.v1~")).expect("fixture identifier")
}

fn report(stop: ReconcileStop) -> ReconcileReport {
    let outcomes = [
        ReconcileOutcome::Admitted,
        ReconcileOutcome::Rejected(
            AdmissionFailure::new(
                AdmissionFailureReason::Unknown("future_reason".into()),
                "refused",
            )
            .with_context("future_context", "retained")
            .into_canonical(id(1).id()),
        ),
        ReconcileOutcome::Pending(ReconcilePendingCause::Dependency(
            AdmissionFailure::new(AdmissionFailureReason::DependencyNotFound, "missing")
                .with_context("dependency_id", id(0).id())
                .into_canonical(id(2).id()),
        )),
        ReconcileOutcome::Pending(ReconcilePendingCause::Conflict(
            OperationResource::aborted("concurrent writer")
                .with_reason("CONCURRENT_UPDATE")
                .create(),
        )),
        ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(
            CanonicalError::service_unavailable()
                .with_retry_after_seconds(7)
                .with_override(Http::status_code(502))
                .create(),
        )),
        ReconcileOutcome::Pending(ReconcilePendingCause::Refused(
            TypeResource::invalid_argument()
                .with_resource(id(5).to_string())
                .with_constraint("invalid input")
                .create(),
        )),
        ReconcileOutcome::Pending(ReconcilePendingCause::Skipped),
    ];
    ReconcileReport {
        stop,
        outcomes: outcomes
            .into_iter()
            .enumerate()
            .map(|(i, o)| (id(i), o))
            .collect(),
    }
}

fn wire_outcome(outcome: &ReconcileOutcome) -> Value {
    let (kind, error) = match outcome {
        ReconcileOutcome::Admitted => return json!({ "kind": "admitted" }),
        ReconcileOutcome::Rejected(error) => ("rejected", error),
        ReconcileOutcome::Pending(cause) => match cause {
            ReconcilePendingCause::Dependency(error) => ("dependency", error),
            ReconcilePendingCause::Conflict(error) => ("conflict", error),
            ReconcilePendingCause::Unavailable(error) => ("unavailable", error),
            ReconcilePendingCause::Refused(error) => ("refused", error),
            ReconcilePendingCause::Skipped => return json!({ "kind": "skipped" }),
        },
    };
    json!({ "kind": kind, "error": Problem::from_error(error).expect("canonical context") })
}

#[test]
fn reports_and_nested_errors_survive_the_problem_json_round_trip() {
    for stop in [
        ReconcileStop::Completed,
        ReconcileStop::PassesExhausted,
        ReconcileStop::Deadline,
        ReconcileStop::ReadFailed,
        ReconcileStop::InvalidInput,
    ] {
        let label = match stop {
            ReconcileStop::Completed => "completed",
            ReconcileStop::PassesExhausted => "passes_exhausted",
            ReconcileStop::Deadline => "deadline",
            ReconcileStop::ReadFailed => "read_failed",
            ReconcileStop::InvalidInput => "invalid_input",
        };
        assert_eq!(stop.as_str(), label);
        assert_eq!(ReconcileStop::from_wire(label), Some(stop));
        let expected = report(stop);
        let error = expected.clone().into_canonical().expect("encodes");
        let CanonicalError::FailedPrecondition { ctx, .. } = &error else {
            panic!("expected reconciliation error");
        };
        assert_eq!(ctx.violations[0].description, expected.to_string());
        assert!(AdmissionFailure::from_canonical(&error).is_none());
        let problem = Problem::from_error(&error).expect("canonical context");
        let bytes = serde_json::to_vec(&problem).expect("serializes");
        let problem: Problem = serde_json::from_slice(&bytes).expect("deserializes");
        let transported = CanonicalError::try_from(problem).expect("canonical problem");
        let decoded = ReconcileReport::from_canonical(&transported).expect("report");
        assert_eq!(decoded.stop, stop);
        assert_eq!(decoded.to_string(), expected.to_string());
        assert_eq!(decoded.outcomes.len(), expected.outcomes.len());
        for (id, outcome) in &expected.outcomes {
            assert_eq!(wire_outcome(&decoded.outcomes[id]), wire_outcome(outcome));
        }
    }
}

#[test]
fn encoding_is_deterministic_and_does_not_expose_internal_diagnostics() {
    let mut first = report(ReconcileStop::Completed);
    first.outcomes.insert(
        id(1),
        ReconcileOutcome::Rejected(CanonicalError::internal("PRIVATE_DIAGNOSTIC").create()),
    );
    let mut second = first.clone();
    second.outcomes = first
        .outcomes
        .clone()
        .into_iter()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let encode = |report: ReconcileReport| {
        let error = report.into_canonical().expect("encodes");
        serde_json::to_string(&Problem::from_error(&error).expect("context")).expect("serializes")
    };
    let encoded = encode(first);
    assert_eq!(encoded, encode(second));
    assert!(!encoded.contains("PRIVATE_DIAGNOSTIC"));
}

#[test]
fn malformed_or_foreign_reports_are_not_partially_decoded() {
    let error = report(ReconcileStop::Completed)
        .into_canonical()
        .expect("encodes");
    let original = Problem::from_error(&error).expect("context");
    for mutation in 0..18 {
        let mut problem = original.clone();
        match mutation {
            0 => problem.context["violations"][0]["subject"] = json!("future_stop"),
            1 => problem.context["violations"][1]["type"] = json!("reconciliation.future"),
            2 => problem.context["violations"][2]["subject"] = json!(id(0).id()),
            3 => problem.context["violations"][1]["subject"] = json!("invalid-id"),
            4 => problem.context["violations"][2]["description"] = json!("{"),
            5 => problem.context["violations"][1]["description"] = json!("unexpected payload"),
            6 => problem.context["resource_type"] = json!(crate::TYPE_RESOURCE_TYPE),
            7 => {
                problem.context["violations"]
                    .as_array_mut()
                    .expect("violations")
                    .truncate(1);
            }
            8 => problem.context["resource_name"] = json!("some-operation"),
            9 => problem.context["violations"][0]["type"] = json!("OTHER_REPORT"),
            10 => problem.context["violations"][7]["description"] = json!("unexpected payload"),
            11 => problem.context["violations"][2]["description"] = json!(""),
            12 => problem.context["violations"][1]["subject"] = json!(format!(" {} ", id(0))),
            13 => {
                for item in problem.context["violations"]
                    .as_array_mut()
                    .expect("violations")
                    .iter_mut()
                    .skip(1)
                {
                    item["type"] = json!("reconciliation.admitted");
                    item["description"] = json!("");
                }
            }
            14 => problem.context["violations"] = json!([]),
            15..=17 => {
                let description = problem.context["violations"][2]["description"]
                    .as_str()
                    .unwrap();
                let mut nested: Value = serde_json::from_str(description).unwrap();
                match mutation {
                    15 => nested["status"] = json!(500),
                    16 => nested["context"] = json!(null),
                    17 => nested["type"] = json!("unknown-error-type"),
                    _ => unreachable!(),
                }
                problem.context["violations"][2]["description"] = json!(nested.to_string());
            }
            _ => unreachable!(),
        }
        let error = CanonicalError::try_from(problem).expect("valid canonical envelope");
        assert!(
            ReconcileReport::from_canonical(&error).is_none(),
            "mutation {mutation}"
        );
    }
    assert!(ReconcileReport::from_canonical(&OperationResource::cancelled().create()).is_none());
}

#[test]
fn decoding_does_not_depend_on_human_readable_summaries() {
    let error = report(ReconcileStop::Completed)
        .into_canonical()
        .expect("encodes");
    let mut problem = Problem::from_error(&error).expect("context");
    problem.detail = "translated detail".to_owned();
    problem.context["violations"][0]["description"] = json!("translated summary");
    let error = CanonicalError::try_from(problem).expect("canonical envelope");
    let decoded = ReconcileReport::from_canonical(&error).expect("report");
    assert_eq!(decoded.stop, ReconcileStop::Completed);
    assert_eq!(decoded.outcomes.len(), 7);
}

#[test]
fn sdk_undecided_diagnostic_survives_report_encoding() {
    let desired = [(id(0), json!({}))];
    let (wanted, outcomes) = super::validate(&desired);
    let run = super::Run::new(tokio::time::Instant::now(), 1, outcomes);
    let error = super::conclude(run, &wanted, false, ReconcileStop::PassesExhausted)
        .expect_err("identifier not decided");
    let decoded = ReconcileReport::from_canonical(&error).expect("report");
    let ReconcileOutcome::Pending(ReconcilePendingCause::Unavailable(error)) =
        &decoded.outcomes[&id(0)]
    else {
        panic!("expected an undecided identifier: {decoded:?}");
    };
    assert_eq!(
        error.detail(),
        "reconciliation ended before this identifier was decided"
    );
}
