#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use admission_control_sdk::{
    AdmissionEnginePluginClientV1, AdmissionRequest, EngineFailure, EngineRequest, EngineResult,
    FailureCondition, PROPERTY_MAX_DEPTH, PolicyReference, RefusalCause, RefusalEvent,
    RefusalEventCause, SizeBound, Verdict,
};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{
    AdmissionService, EngineHandle, EngineSource, EngineUnavailable, EventEnvelope, EventSink,
    MAX_DOCUMENT_NAME_LEN, MAX_ENGINE_FINDINGS, MAX_REASON_CODE_LEN, ServiceSettings,
};
use crate::infra::metrics::AdmissionControlMetrics;

const WIDGET: &str = "gts.cf.core.test.widget.v1~";
const TENANT: Uuid = Uuid::from_u128(0xB1);

/// Records emitted payloads (`.0`) and their envelopes (`.1`).
#[derive(Default)]
struct Sink(Mutex<Vec<RefusalEvent>>, Mutex<Vec<EventEnvelope>>);

impl EventSink for Sink {
    fn emit(&self, envelope: EventEnvelope, event: RefusalEvent) {
        self.0.lock().push(event);
        self.1.lock().push(envelope);
    }
}

enum Behavior {
    Result(EngineResult),
    Fail(EngineFailure),
    Sleep,
}

struct StubEngine(Behavior);

#[async_trait]
impl AdmissionEnginePluginClientV1 for StubEngine {
    async fn evaluate(
        &self,
        _ctx: &SecurityContext,
        _request: &EngineRequest,
    ) -> Result<EngineResult, EngineFailure> {
        match &self.0 {
            Behavior::Result(result) => Ok(result.clone()),
            Behavior::Fail(failure) => Err(failure.clone()),
            Behavior::Sleep => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Err(EngineFailure::internal("unreachable"))
            }
        }
    }
}

fn reference(n: u128) -> PolicyReference {
    PolicyReference {
        bundle_id: Uuid::from_u128(n),
        version_id: Uuid::from_u128(n + 1),
        document_id: Uuid::from_u128(n + 2),
        document_name: format!("doc-{n}"),
    }
}

/// An engine source that always yields the same engine.
struct Resolved(EngineHandle);

#[async_trait]
impl EngineSource for Resolved {
    async fn engine(&self) -> Result<EngineHandle, EngineUnavailable> {
        Ok(self.0.clone())
    }
}

/// An engine source that cannot resolve the engine (now, or in time).
enum Unresolved {
    Unavailable,
    Hangs,
}

#[async_trait]
impl EngineSource for Unresolved {
    async fn engine(&self) -> Result<EngineHandle, EngineUnavailable> {
        if matches!(self, Self::Hangs) {
            std::future::pending::<()>().await;
        }
        Err(EngineUnavailable)
    }
}

fn service(engine: Option<Behavior>) -> (AdmissionService, Arc<Sink>) {
    let engine = engine.map(|behavior| {
        Arc::new(Resolved(EngineHandle {
            id: "engine".to_owned(),
            plugin: Arc::new(StubEngine(behavior)),
        })) as Arc<dyn EngineSource>
    });
    service_with(engine)
}

fn service_with(engine: Option<Arc<dyn EngineSource>>) -> (AdmissionService, Arc<Sink>) {
    let sink = Arc::new(Sink::default());
    let service = AdmissionService::new(
        ServiceSettings {
            engine_timeout: Duration::from_millis(100),
            max_properties: 4,
            max_context_bytes: 256,
        },
        engine,
        Arc::clone(&sink) as Arc<dyn EventSink>,
        Arc::new(AdmissionControlMetrics::global()),
    );
    (service, sink)
}

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(1))
        .subject_tenant_id(Uuid::from_u128(2))
        .build()
        .unwrap()
}

fn request(name: &str) -> AdmissionRequest {
    AdmissionRequest::new("gear", "create", WIDGET, TENANT).with_property("name", json!(name))
}

fn cause(verdict: &Verdict) -> &RefusalCause {
    match verdict {
        Verdict::Refused(refusal) => &refusal.cause,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn permit(shadow: Vec<PolicyReference>) -> Behavior {
    Behavior::Result(EngineResult::Permit {
        shadow_denials: shadow,
    })
}

#[tokio::test]
async fn anonymous_context_is_unauthenticated() {
    let (service, sink) = service(Some(permit(Vec::new())));
    let err = service
        .admit(&SecurityContext::anonymous(), &request("x"))
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 401);
    assert!(sink.0.lock().is_empty());
}

#[tokio::test]
async fn invalid_identifiers_are_invalid_argument_without_echo() {
    let (service, _) = service(Some(permit(Vec::new())));
    for bad in [
        AdmissionRequest::new("Bad Gear", "create", WIDGET, TENANT),
        AdmissionRequest::new("gear", "CREATE!", WIDGET, TENANT),
        AdmissionRequest::new("gear", "create", "not-a-gts-type", TENANT),
    ] {
        let err = service.admit(&ctx(), &bad).await.unwrap_err();
        assert_eq!(err.status_code(), 400);
        for value in ["Bad Gear", "CREATE!", "not-a-gts-type"] {
            assert!(!err.detail().contains(value));
        }
    }
}

#[tokio::test]
async fn oversized_requests_are_refused_with_one_event_and_no_names() {
    let (service, sink) = service(Some(permit(Vec::new())));
    let mut many = request("x");
    for i in 0..5 {
        many = many.with_property(format!("p{i}"), json!(i));
    }
    let verdict = service.admit(&ctx(), &many).await.unwrap();
    assert_eq!(
        cause(&verdict),
        &RefusalCause::RequestTooLarge {
            bound: SizeBound::PropertyCount
        }
    );

    let verdict = service
        .admit(&ctx(), &request(&"x".repeat(300)))
        .await
        .unwrap();
    assert_eq!(
        cause(&verdict),
        &RefusalCause::RequestTooLarge {
            bound: SizeBound::ContextBytes
        }
    );
    let events = sink.0.lock();
    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .all(|e| e.cause == RefusalEventCause::RequestTooLarge && e.property_names.is_empty())
    );
}

/// A property value at depth `depth`: `depth - 1` arrays around a scalar.
fn nested(depth: usize) -> serde_json::Value {
    (1..depth).fold(json!(0), |inner, _| json!([inner]))
}

#[tokio::test]
async fn properties_nested_past_the_depth_bound_are_refused() {
    let (service, sink) = service(Some(permit(Vec::new())));
    let at_bound = AdmissionRequest::new("gear", "create", WIDGET, TENANT)
        .with_property("deep", nested(PROPERTY_MAX_DEPTH));
    assert!(
        service
            .admit(&ctx(), &at_bound)
            .await
            .unwrap()
            .is_admitted()
    );

    // Arrays and objects both count as a level.
    let past_bound_arrays = AdmissionRequest::new("gear", "create", WIDGET, TENANT)
        .with_property("deep", nested(PROPERTY_MAX_DEPTH + 1));
    let past_bound_objects = AdmissionRequest::new("gear", "create", WIDGET, TENANT).with_property(
        "deep",
        (1..=PROPERTY_MAX_DEPTH).fold(json!(0), |inner, _| json!({ "k": inner })),
    );
    for past_bound in [past_bound_arrays, past_bound_objects] {
        let verdict = service.admit(&ctx(), &past_bound).await.unwrap();
        assert_eq!(
            cause(&verdict),
            &RefusalCause::RequestTooLarge {
                bound: SizeBound::ContextDepth
            }
        );
    }
    let events = sink.0.lock();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| event.property_names.is_empty()));
}

#[tokio::test]
async fn an_engine_result_past_its_bounds_is_an_engine_error() {
    let named = |name: &str| PolicyReference {
        document_name: name.to_owned(),
        ..reference(10)
    };
    let deny = |reason_code: &str, denials: Vec<PolicyReference>| {
        Behavior::Result(EngineResult::Deny {
            reason_code: reason_code.to_owned(),
            denials,
            shadow_denials: Vec::new(),
        })
    };
    let out_of_bounds = [
        deny("", vec![reference(10)]),
        deny("POLICY DENIED", vec![reference(10)]),
        deny(&"R".repeat(MAX_REASON_CODE_LEN + 1), vec![reference(10)]),
        deny("POLICY_DENIED", vec![named("")]),
        deny("POLICY_DENIED", vec![named("doc\nforged")]),
        deny(
            "POLICY_DENIED",
            vec![named(&"d".repeat(MAX_DOCUMENT_NAME_LEN + 1))],
        ),
        deny(
            "POLICY_DENIED",
            (0..=MAX_ENGINE_FINDINGS as u128).map(reference).collect(),
        ),
        permit((0..=MAX_ENGINE_FINDINGS as u128).map(reference).collect()),
        permit(vec![named("doc\u{7}")]),
    ];
    for behavior in out_of_bounds {
        let (service, sink) = service(Some(behavior));
        let verdict = service.admit(&ctx(), &request("x")).await.unwrap();
        assert_eq!(
            cause(&verdict),
            &RefusalCause::CouldNotRun {
                condition: FailureCondition::Internal
            }
        );
        assert_eq!(
            sink.0.lock().len(),
            1,
            "one could-not-run event, no findings"
        );
    }

    // At the bounds, the result stands.
    let at_bounds = deny(
        &"R".repeat(MAX_REASON_CODE_LEN),
        (0..MAX_ENGINE_FINDINGS as u128)
            .map(|n| PolicyReference {
                document_name: "d".repeat(MAX_DOCUMENT_NAME_LEN),
                ..reference(n)
            })
            .collect(),
    );
    let (service, sink) = service(Some(at_bounds));
    let verdict = service.admit(&ctx(), &request("x")).await.unwrap();
    assert!(matches!(cause(&verdict), RefusalCause::Policy { .. }));
    assert_eq!(sink.0.lock().len(), MAX_ENGINE_FINDINGS);
}

#[tokio::test]
async fn an_engine_that_cannot_be_resolved_is_engine_unavailable() {
    let (service, sink) = service_with(Some(Arc::new(Unresolved::Unavailable)));
    let verdict = service.admit(&ctx(), &request("x")).await.unwrap();
    let expected = RefusalCause::CouldNotRun {
        condition: FailureCondition::EngineUnavailable,
    };
    assert_eq!(cause(&verdict), &expected);
    assert!(expected.is_retryable());
    assert_eq!(
        sink.0.lock()[0].condition,
        Some(FailureCondition::EngineUnavailable)
    );
}

#[tokio::test(start_paused = true)]
async fn resolving_the_engine_counts_against_the_engine_timeout() {
    let (service, _) = service_with(Some(Arc::new(Unresolved::Hangs)));
    let verdict = service.admit(&ctx(), &request("x")).await.unwrap();
    assert_eq!(
        cause(&verdict),
        &RefusalCause::CouldNotRun {
            condition: FailureCondition::EngineTimeout
        }
    );
}

#[tokio::test]
async fn no_engine_is_could_not_run() {
    let (service, sink) = service(None);
    let verdict = service.admit(&ctx(), &request("x")).await.unwrap();
    assert_eq!(
        cause(&verdict),
        &RefusalCause::CouldNotRun {
            condition: FailureCondition::NoEngine
        }
    );
    assert_eq!(sink.0.lock()[0].condition, Some(FailureCondition::NoEngine));
}

#[tokio::test]
async fn engine_permit_admits_and_emits_only_shadow_findings() {
    let (service, sink) = service(Some(permit(vec![reference(10), reference(20)])));
    let verdict = service.admit(&ctx(), &request("x")).await.unwrap();
    assert!(verdict.is_admitted());
    let events = sink.0.lock();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| !e.enforced && e.policy.is_some()));
    let envelopes = sink.1.lock();
    assert!(envelopes.iter().all(|envelope| {
        envelope.correlation_id == verdict.correlation_id() && envelope.tenant_id == TENANT
    }));
}

#[tokio::test]
async fn permit_without_findings_emits_nothing() {
    let (service, sink) = service(Some(permit(Vec::new())));
    assert!(
        service
            .admit(&ctx(), &request("x"))
            .await
            .unwrap()
            .is_admitted()
    );
    assert!(sink.0.lock().is_empty());
}

#[tokio::test]
async fn engine_denial_emits_one_event_per_denial_plus_shadow() {
    let (service, sink) = service(Some(Behavior::Result(EngineResult::Deny {
        reason_code: "POLICY_DENIED".to_owned(),
        denials: vec![reference(10), reference(20)],
        shadow_denials: vec![reference(30)],
    })));
    let verdict = service.admit(&ctx(), &request("x")).await.unwrap();
    assert_eq!(
        cause(&verdict),
        &RefusalCause::Policy {
            reason_code: "POLICY_DENIED".to_owned(),
            denials: vec![reference(10), reference(20)],
        }
    );
    let events = sink.0.lock();
    assert_eq!(events.len(), 3);
    assert_eq!(events.iter().filter(|e| e.enforced).count(), 2);
    assert_eq!(events.iter().filter(|e| !e.enforced).count(), 1);
    assert!(events.iter().all(|e| e.cause == RefusalEventCause::Policy));
    // Subjects come from the context, names (never values) from the request.
    assert!(events.iter().all(|e| {
        e.subject_id == Uuid::from_u128(1)
            && e.subject_tenant_id == Uuid::from_u128(2)
            && e.property_names == ["name"]
    }));
}

#[tokio::test]
async fn engine_failures_map_to_their_refusals() {
    let could_not_run = |condition| RefusalCause::CouldNotRun { condition };
    for (failure, expected, event_cause) in [
        (
            EngineFailure::unavailable("d"),
            could_not_run(FailureCondition::EngineUnavailable),
            RefusalEventCause::CouldNotRun,
        ),
        (
            EngineFailure::timeout("d"),
            could_not_run(FailureCondition::EngineTimeout),
            RefusalEventCause::CouldNotRun,
        ),
        (
            EngineFailure::internal("d"),
            could_not_run(FailureCondition::EngineError),
            RefusalEventCause::CouldNotRun,
        ),
        (
            EngineFailure::invalid_request("d"),
            RefusalCause::InvalidRequest,
            RefusalEventCause::InvalidRequest,
        ),
        (
            EngineFailure::contract_violation("d"),
            could_not_run(FailureCondition::Internal),
            RefusalEventCause::CouldNotRun,
        ),
    ] {
        let (service, sink) = service(Some(Behavior::Fail(failure)));
        let verdict = service.admit(&ctx(), &request("x")).await.unwrap();
        assert_eq!(cause(&verdict), &expected);
        let events = sink.0.lock();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cause, event_cause);
        // Names of the properties are kept, so audit shows what was rejected.
        assert_eq!(events[0].property_names, ["name"]);
    }
}

#[tokio::test(start_paused = true)]
async fn slow_engine_times_out() {
    let (service, _) = service(Some(Behavior::Sleep));
    let verdict = service.admit(&ctx(), &request("x")).await.unwrap();
    assert_eq!(
        cause(&verdict),
        &RefusalCause::CouldNotRun {
            condition: FailureCondition::EngineTimeout
        }
    );
}

#[tokio::test]
#[tracing_test::traced_test]
async fn log_lines_of_an_admission_carry_its_correlation_id() {
    // An engine failure: the log call names no correlation id, the
    // `admission` span puts it on the line anyway.
    let (service, _) = service(Some(Behavior::Fail(EngineFailure::internal("d"))));
    let verdict = service.admit(&ctx(), &request("x")).await.unwrap();
    let field = format!("correlation_id={}", verdict.correlation_id());
    logs_assert(|lines: &[&str]| {
        let line = lines
            .iter()
            .find(|line| line.contains("admission engine failed"))
            .ok_or("no engine failure line")?;
        for expected in [field.as_str(), "action=create", "enforcing_gear=gear"] {
            if !line.contains(expected) {
                return Err(format!("`{expected}` missing from {line}"));
            }
        }
        Ok(())
    });
}
