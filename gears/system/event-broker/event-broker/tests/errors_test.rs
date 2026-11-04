//! The RFC 9457 error envelope (scenarios errors/1.01-1.08): each test
//! triggers one canonical error category and asserts the whole Problem
//! document. In-process: real services and router, `SQLite` gear database,
//! memory backend. `AuthN` is bypassed in-process (the security context is
//! injected), so 1.02 (401) is the e2e smoke's to check.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, EvaluationRequest, EvaluationResponse, PolicyEnforcer};
use event_broker::test_support::{DenyingAuthZ, EventBrokerHarness, Json, StaticTypesRegistry};
use http::HeaderValue;
use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;

const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.errors.stream.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.errors.happy.v1~";
const SUBJECT_TYPE: &str = "gts.x.eb.errors.subject.v1~";

fn registry() -> StaticTypesRegistry {
    StaticTypesRegistry::of(json!([
        { "id": TOPIC, "partitions": 1 },
        {
            "id": EVENT_TYPE,
            "topic": TOPIC,
            "allowed_subject_types": [SUBJECT_TYPE],
        },
    ]))
}

/// A policy decision point that cannot be reached: every evaluation fails as
/// infrastructure, which is the gear's own internal error, not a denial.
struct UnreachablePdp;

#[async_trait]
impl AuthZResolverApi for UnreachablePdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Err(CanonicalError::internal("the policy decision point is unreachable").create())
    }
}

// scenario: errors/1.01-positive-problem-details-envelope.md
#[tokio::test]
async fn every_error_carries_the_problem_details_envelope() {
    let harness = EventBrokerHarness::builder().build().await;

    let resp = harness
        .api_v1()
        .get_consumer_group(
            "gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-000000000000",
        )
        .send()
        .await;

    resp.assert_status(404);
    resp.assert_header("content-type", "application/problem+json");
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": "consumer group 'gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-000000000000' is not registered",
            "instance": "/event-broker/v1/consumer-groups/gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-000000000000",
            "context": {
                "resource_type": "gts.cf.core.events.consumer_group.v1~",
                "resource_name": "gts.cf.core.events.consumer_group.v1~00000000-0000-0000-0000-000000000000",
            },
        })
    );
}

// scenario: errors/1.03-negative-403-unauthorized.md
#[tokio::test]
async fn a_publish_the_policy_denies_answers_403() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(registry())
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            deny_if: |_: &EvaluationRequest| true,
        })))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": "0190f1a2-0000-7000-8000-000000000301",
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-8000-0000000003a1",
            "source": "errors-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:00Z",
        })))
        .send()
        .await;

    resp.assert_status(403);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~",
            "title": "Permission Denied",
            "status": 403,
            "detail": "You do not have permission to perform this operation",
            "instance": "/event-broker/v1/events",
            "context": {
                "reason": "access denied by policy",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
}

// scenario: errors/1.04-negative-404-not-found.md
#[tokio::test]
async fn an_unknown_consumer_group_answers_404() {
    let harness = EventBrokerHarness::builder().build().await;

    let resp = harness
        .api_v1()
        .get_consumer_group(
            "gts.cf.core.events.consumer_group.v1~deadbeef-0000-0000-0000-000000000000",
        )
        .send()
        .await;

    resp.assert_status(404);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
            "title": "Not Found",
            "status": 404,
            "detail": "consumer group 'gts.cf.core.events.consumer_group.v1~deadbeef-0000-0000-0000-000000000000' is not registered",
            "instance": "/event-broker/v1/consumer-groups/gts.cf.core.events.consumer_group.v1~deadbeef-0000-0000-0000-000000000000",
            "context": {
                "resource_type": "gts.cf.core.events.consumer_group.v1~",
                "resource_name": "gts.cf.core.events.consumer_group.v1~deadbeef-0000-0000-0000-000000000000",
            },
        })
    );
}

// scenario: errors/1.05-negative-409-conflict.md
#[tokio::test]
async fn opening_a_stream_before_seek_answers_409_positions_not_set() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(registry())
        .build()
        .await;
    let group = harness.api_v1().post_consumer_groups().send().await;
    group.assert_status(201);
    let group_id = group.json()["id"].as_str().unwrap().to_owned();
    let subscription = harness
        .api_v1()
        .post_subscriptions()
        .with_body(Json(&json!({
            "consumer_group": group_id,
            "client_agent": "errors-test",
            "interests": [{
                "topic": TOPIC,
                "tenant_id": "0190f1a2-0000-7000-8000-0000000005a1",
                "types": [EVENT_TYPE],
            }],
        })))
        .send()
        .await;
    subscription.assert_status(201);
    let subscription_id = subscription.json()["id"].as_str().unwrap().to_owned();

    let resp = harness
        .api_v1()
        .get_events_stream(&subscription_id)
        .with_header(
            http::header::ACCEPT,
            HeaderValue::from_static("multipart/mixed"),
        )
        .send()
        .await;

    resp.assert_status(409);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "status": 409,
            "detail": "Operation precondition not met",
            "instance": "/event-broker/v1/events:stream",
            "context": {
                "resource_type": "gts.cf.core.events.subscription.v1~",
                "violations": [{
                    "type": "positions_not_set",
                    "subject": format!("{TOPIC}:0"),
                    "description": format!(
                        "call POST /v1/subscriptions/{subscription_id}:seek before re-opening the stream"
                    ),
                }],
            },
        })
    );
}

// scenario: errors/1.06-negative-412-sequence-violation.md
#[tokio::test]
async fn a_chained_publish_off_the_head_answers_412() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(registry())
        .build()
        .await;
    let registration = harness
        .api_v1()
        .post_producers()
        .with_body(Json(
            &json!({ "mode": "chained", "client_agent": "errors-test" }),
        ))
        .send()
        .await;
    registration.assert_status(201);
    let producer_id = registration.json()["id"].as_str().unwrap().to_owned();
    let first = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": "0190f1a2-0000-7000-8000-000000000601",
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-8000-0000000006a1",
            "source": "errors-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:00Z",
            "meta": { "version": 1, "producer_id": producer_id, "previous": 0, "sequence": 1 },
        })))
        .send()
        .await;
    first.assert_status(202);
    assert_eq!(first.text(), "");

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": "0190f1a2-0000-7000-8000-000000000602",
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-8000-0000000006a1",
            "source": "errors-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:01Z",
            "meta": { "version": 1, "producer_id": producer_id, "previous": 999, "sequence": 1000 },
        })))
        .send()
        .await;

    resp.assert_status(412);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.failed_precondition.v1~",
            "title": "Failed Precondition",
            "status": 412,
            "detail": "Operation precondition not met",
            "instance": "/event-broker/v1/events",
            "context": {
                "resource_type": "gts.cf.core.events.topic.v1~",
                "resource_name": TOPIC,
                "violations": [{
                    "type": "sequence_mismatch",
                    "subject": "(producer)",
                    "description": format!("topic={TOPIC} expected_previous=1"),
                }],
            },
        })
    );
}

// scenario: errors/1.08-negative-500-internal.md
#[tokio::test]
async fn an_unreachable_policy_decision_point_answers_500() {
    let harness = EventBrokerHarness::builder()
        .with_type_registry(registry())
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(UnreachablePdp)))
        .build()
        .await;

    let resp = harness
        .api_v1()
        .post_events()
        .with_body(Json(&json!({
            "id": "0190f1a2-0000-7000-8000-000000000801",
            "type": EVENT_TYPE,
            "tenant_id": "0190f1a2-0000-7000-8000-0000000008a1",
            "source": "errors-test",
            "subject": "s1",
            "subject_type": SUBJECT_TYPE,
            "occurred_at": "2026-10-01T12:00:00Z",
        })))
        .send()
        .await;

    resp.assert_status(500);
    assert_eq!(
        resp.json(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
            "title": "Internal",
            "status": 500,
            "detail": "An internal error occurred. Please retry later.",
            "instance": "/event-broker/v1/events",
            "context": {},
        })
    );
}
