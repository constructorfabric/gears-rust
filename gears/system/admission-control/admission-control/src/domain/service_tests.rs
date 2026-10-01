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

use super::{AdmissionService, EngineHandle, EventSink, ServiceSettings};
use crate::config::{AdmissionControlConfig, BuiltinPolicyConfig};
use crate::infra::metrics::AdmissionControlMetrics;

const WIDGET: &str = "gts.cf.core.test.widget.v1~";
const TENANT: Uuid = Uuid::from_u128(0xB1);

#[derive(Default)]
struct Sink(Mutex<Vec<RefusalEvent>>);

impl EventSink for Sink {
    fn emit(&self, event: RefusalEvent) {
        self.0.lock().push(event);
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

fn service(engine: Option<Behavior>) -> (AdmissionService, Arc<Sink>) {
    let builtins = AdmissionControlConfig {
        builtin_policies: vec![BuiltinPolicyConfig {
            id: "reserved".to_owned(),
            description: None,
            resource_types: vec![WIDGET.to_owned()],
            actions: vec!["create".to_owned()],
            content: "package b\ndeny if startswith(input.properties.name, \"cf-\")".to_owned(),
        }],
        ..Default::default()
    }
    .compile_builtins()
    .unwrap();
    let sink = Arc::new(Sink::default());
    let service = AdmissionService::new(
        builtins,
        ServiceSettings {
            engine_timeout: Duration::from_millis(100),
            builtin_timeout: Duration::from_millis(500),
            max_properties: 4,
            max_context_bytes: 256,
        },
        Arc::clone(&sink) as Arc<dyn EventSink>,
        Arc::new(AdmissionControlMetrics::global()),
    );
    service.install_engine(engine.map(|behavior| EngineHandle {
        id: "engine".to_owned(),
        plugin: Arc::new(StubEngine(behavior)),
    }));
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
        Verdict::Admitted(_) => panic!("expected a refusal"),
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
async fn builtin_denial_wins_before_the_engine_and_emits_one_event() {
    let (service, sink) = service(Some(permit(Vec::new())));
    let verdict = service.admit(&ctx(), &request("cf-widget")).await.unwrap();
    assert_eq!(
        cause(&verdict),
        &RefusalCause::BuiltinPolicy {
            policy_id: "reserved".to_owned()
        }
    );
    let events = sink.0.lock();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].builtin_policy_id.as_deref(), Some("reserved"));
    assert_eq!(events[0].property_names, ["name"]);
    assert_eq!(events[0].subject_id, Uuid::from_u128(1));
    assert!(events[0].enforced);
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
    assert_eq!(events[0].correlation_id, verdict.correlation_id());
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
}

#[tokio::test]
async fn engine_failures_are_could_not_run() {
    for (failure, condition) in [
        (
            EngineFailure::unavailable("d"),
            FailureCondition::EngineUnavailable,
        ),
        (EngineFailure::timeout("d"), FailureCondition::EngineTimeout),
        (
            EngineFailure::invalid_request("d"),
            FailureCondition::EngineError,
        ),
    ] {
        let (service, sink) = service(Some(Behavior::Fail(failure)));
        let verdict = service.admit(&ctx(), &request("x")).await.unwrap();
        assert_eq!(cause(&verdict), &RefusalCause::CouldNotRun { condition });
        assert_eq!(sink.0.lock().len(), 1);
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
