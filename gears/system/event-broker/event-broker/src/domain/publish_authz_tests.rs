//! The PDP requests a publish sends, and the order it sends them in (#5492).
//!
//! A publish asks the tenant check (R: `produce` on the tenant-scope resource,
//! rooted at the event's tenant, barriers respected), then the barrier right
//! (I: `produce_ignore_barrier` on the event type, rooted at the event's
//! tenant, barriers ignored) only when R is denied, then the type check (T:
//! `produce` on the event type, rooted at the caller's own tenant). Each test
//! records every request the PDP sees and asserts the whole list, so its
//! length is the call count. A publish whose event tenant is nil is refused
//! before any of them, so its list is empty.
//!
//! The caller and event tenants follow the issue's example tree: the caller
//! sits in tenant `0x123`, the event belongs to `0x234` (or `0x999`, outside
//! the caller's reach in the batch test). The caller tenant never equals the
//! event tenant, so a request rooted at the wrong one shows in the literal.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverApi, Constraint, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
    InPredicate, PolicyEnforcer, Predicate,
};
use chrono::{TimeZone, Utc};
use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::ingest::PublishRequest;
use crate::test_support::{
    DenyingAuthZ, EventBrokerHarness, NeverStoringBackend, StaticTypesRegistry,
};

const REQUEST_RESOURCE: &str = "gts.cf.core.events.request.v1~";
const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.pubauthz.topic.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~x.eb.pubauthz.foo.v1~";
const SUBJECT_TYPE: &str = "gts.x.eb.pubauthz.subject.v1~";

const SUBJECT: Uuid = Uuid::from_u128(0x456);
const CALLER_TENANT: Uuid = Uuid::from_u128(0x123);
const EVENT_TENANT: Uuid = Uuid::from_u128(0x234);
const UNREACHABLE_TENANT: Uuid = Uuid::from_u128(0x999);
/// The owner of an owned event: a principal of the event's tenant, not the caller.
const OWNER: Uuid = Uuid::from_u128(0x501);

type Recorded = Arc<Mutex<Vec<Value>>>;

/// A `PolicyEnforcer` that records each request it sees and denies the ones
/// `deny` picks from the serialized request.
fn recording_enforcer(
    deny: impl Fn(&Value) -> bool + Send + Sync + 'static,
) -> (PolicyEnforcer, Recorded) {
    let recorded = Recorded::default();
    let sink = Arc::clone(&recorded);
    let enforcer = PolicyEnforcer::new(Arc::new(DenyingAuthZ {
        deny_if: move |req: &EvaluationRequest| {
            let req = serde_json::to_value(req).unwrap();
            let denied = deny(&req);
            sink.lock().unwrap().push(req);
            denied
        },
    }));
    (enforcer, recorded)
}

fn recorded(recorded: &Recorded) -> Vec<Value> {
    recorded.lock().unwrap().clone()
}

/// A PDP that cannot evaluate the tenant check (the evaluation itself fails,
/// as an unreachable PDP does) and allows every other request.
struct UnavailableForTheTenantCheck {
    recorded: Recorded,
}

#[async_trait]
impl AuthZResolverApi for UnavailableForTheTenantCheck {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.recorded
            .lock()
            .unwrap()
            .push(serde_json::to_value(&request).unwrap());
        if request.resource.resource_type == REQUEST_RESOURCE {
            return Err(
                CanonicalError::internal("the policy decision point is unreachable").create(),
            );
        }
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext::default(),
        })
    }
}

/// A PDP that answers "allowed" with one constraint on `owner_id`, a property
/// no publish-path resource declares, to the requests `uncompilable` picks, so
/// the enforcer cannot compile the answer. It denies the requests `deny` picks
/// (when `uncompilable` does not) and allows every other request.
struct UncompilableAnswer {
    recorded: Recorded,
    deny: fn(&Value) -> bool,
    uncompilable: fn(&Value) -> bool,
}

#[async_trait]
impl AuthZResolverApi for UncompilableAnswer {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let request = serde_json::to_value(&request).unwrap();
        self.recorded.lock().unwrap().push(request.clone());
        if (self.uncompilable)(&request) {
            return Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: vec![Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            "owner_id",
                            [Uuid::from_u128(0x77)],
                        ))],
                    }],
                    ..Default::default()
                },
            });
        }
        Ok(EvaluationResponse {
            decision: !(self.deny)(&request),
            context: EvaluationResponseContext::default(),
        })
    }
}

/// A PDP that answers the tenant check (R) "allowed" with a scope that matches
/// nothing, `In(owner_tenant_id, [])`, which the enforcer refuses as a denial.
/// It denies the requests `deny` picks and allows every other request.
struct MatchesNothingOnTheTenantCheck {
    recorded: Recorded,
    deny: fn(&Value) -> bool,
}

#[async_trait]
impl AuthZResolverApi for MatchesNothingOnTheTenantCheck {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let request = serde_json::to_value(&request).unwrap();
        self.recorded.lock().unwrap().push(request.clone());
        if is_tenant_check(&request) {
            return Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: vec![Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            pep_properties::OWNER_TENANT_ID,
                            Vec::<Uuid>::new(),
                        ))],
                    }],
                    ..Default::default()
                },
            });
        }
        Ok(EvaluationResponse {
            decision: !(self.deny)(&request),
            context: EvaluationResponseContext::default(),
        })
    }
}

/// The topic and event type on a backend that never stores, so an admitted
/// event stays in the outbox and `outbox_bodies` counts it.
async fn harness(enforcer: PolicyEnforcer) -> EventBrokerHarness {
    EventBrokerHarness::builder()
        .with_backend(Arc::new(NeverStoringBackend::new(Arc::new(
            memory_event_broker_plugin::MemoryBackend::new(),
        ))))
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": TOPIC, "partitions": 1 },
            {
                "id": EVENT_TYPE,
                "topic": TOPIC,
                "allowed_subject_types": [SUBJECT_TYPE],
            },
        ])))
        .with_policy_enforcer(enforcer)
        .build()
        .await
}

fn caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(SUBJECT)
        .subject_tenant_id(CALLER_TENANT)
        .build()
        .expect("valid security context")
}

fn request(id: u128, tenant: Uuid) -> PublishRequest {
    PublishRequest {
        id: Uuid::from_u128(id),
        r#type: crate::test_support::event_type_id(EVENT_TYPE),
        subject_tenant: tenant,
        source: "publish-authz-test".to_owned(),
        subject: "s1".to_owned(),
        subject_type: gts::GtsTypeId::new(SUBJECT_TYPE),
        subject_owner: None,
        occurred_at: Utc.with_ymd_and_hms(2026, 11, 12, 10, 0, 0).unwrap(),
        trace_parent: None,
        data: json!({}),
        meta: None,
    }
}

fn is_tenant_check(req: &Value) -> bool {
    req["resource"]["type"] == REQUEST_RESOURCE && req["action"]["name"] == "produce"
}

fn is_barrier_check(req: &Value) -> bool {
    req["action"]["name"] == "produce_ignore_barrier"
}

fn is_type_check(req: &Value) -> bool {
    req["resource"]["type"] != REQUEST_RESOURCE && req["action"]["name"] == "produce"
}

#[tokio::test]
async fn a_publish_to_a_reachable_tenant_asks_the_tenant_check_then_the_type_check() {
    let (enforcer, requests) = recording_enforcer(|_| false);
    let harness = harness(enforcer).await;

    let result = harness
        .ingest()
        .publish_event(&caller(), request(0xe1, EVENT_TENANT))
        .await;

    assert!(
        result.is_ok(),
        "expected the publish to pass, got {result:?}"
    );
    assert_eq!(
        recorded(&requests),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": { "owner_tenant_id": EVENT_TENANT },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": EVENT_TYPE,
                    "properties": {
                        "event_type_id": EVENT_TYPE,
                        "owner_tenant_id": EVENT_TENANT,
                        "event_subject_type": SUBJECT_TYPE,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_eq!(harness.outbox_bodies().await, 1, "the event is enqueued");
}

/// An owned event's type check also carries its owner as `owner_id`, beside the
/// event's tenant and subject type; the tenant check is unchanged.
#[tokio::test]
async fn an_owned_events_type_check_carries_its_owner() {
    let (enforcer, requests) = recording_enforcer(|_| false);
    let harness = harness(enforcer).await;
    let owned = PublishRequest {
        subject_owner: Some(OWNER),
        ..request(0xe1, EVENT_TENANT)
    };

    let result = harness.ingest().publish_event(&caller(), owned).await;

    assert!(
        result.is_ok(),
        "expected the publish to pass, got {result:?}"
    );
    assert_eq!(
        recorded(&requests),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": { "owner_tenant_id": EVENT_TENANT },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": EVENT_TYPE,
                    "properties": {
                        "event_type_id": EVENT_TYPE,
                        "owner_tenant_id": EVENT_TENANT,
                        "event_subject_type": SUBJECT_TYPE,
                        "owner_id": OWNER,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
}

#[tokio::test]
async fn a_publish_to_a_barrier_tenant_asks_for_the_barrier_right_after_the_tenant_check_is_denied()
{
    let (enforcer, requests) = recording_enforcer(is_tenant_check);
    let harness = harness(enforcer).await;

    let result = harness
        .ingest()
        .publish_event(&caller(), request(0xe2, EVENT_TENANT))
        .await;

    assert!(
        result.is_ok(),
        "expected the publish to pass, got {result:?}"
    );
    assert_eq!(
        recorded(&requests),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": { "owner_tenant_id": EVENT_TENANT },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce_ignore_barrier" },
                "resource": {
                    "type": EVENT_TYPE,
                    "properties": { "owner_tenant_id": EVENT_TENANT, "event_type_id": EVENT_TYPE },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "ignore",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": EVENT_TYPE,
                    "properties": {
                        "event_type_id": EVENT_TYPE,
                        "owner_tenant_id": EVENT_TENANT,
                        "event_subject_type": SUBJECT_TYPE,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_eq!(harness.outbox_bodies().await, 1, "the event is enqueued");
}

#[tokio::test]
async fn a_publish_denied_both_reach_and_the_barrier_right_is_refused_with_tenant_id_not_authorized()
 {
    let (enforcer, requests) =
        recording_enforcer(|req| is_tenant_check(req) || is_barrier_check(req));
    let harness = harness(enforcer).await;

    let result = harness
        .ingest()
        .publish_event(&caller(), request(0xe3, EVENT_TENANT))
        .await;

    assert!(
        matches!(
            &result,
            Err(DomainError::Forbidden {
                code: ErrorCode::TenantIdNotAuthorized,
                ..
            })
        ),
        "expected Forbidden/TenantIdNotAuthorized, got {result:?}"
    );
    assert_eq!(
        recorded(&requests),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": { "owner_tenant_id": EVENT_TENANT },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce_ignore_barrier" },
                "resource": {
                    "type": EVENT_TYPE,
                    "properties": { "owner_tenant_id": EVENT_TENANT, "event_type_id": EVENT_TYPE },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "ignore",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_eq!(harness.outbox_bodies().await, 0, "nothing is enqueued");
}

#[tokio::test]
async fn a_publish_without_the_type_grant_is_refused_with_not_authorized_to_produce() {
    let (enforcer, requests) = recording_enforcer(is_type_check);
    let harness = harness(enforcer).await;

    let result = harness
        .ingest()
        .publish_event(&caller(), request(0xe4, EVENT_TENANT))
        .await;

    assert!(
        matches!(
            &result,
            Err(DomainError::Forbidden {
                code: ErrorCode::NotAuthorizedToProduce,
                ..
            })
        ),
        "expected Forbidden/NotAuthorizedToProduce, got {result:?}"
    );
    assert_eq!(
        recorded(&requests),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": { "owner_tenant_id": EVENT_TENANT },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": EVENT_TYPE,
                    "properties": {
                        "event_type_id": EVENT_TYPE,
                        "owner_tenant_id": EVENT_TENANT,
                        "event_subject_type": SUBJECT_TYPE,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_eq!(harness.outbox_bodies().await, 0, "nothing is enqueued");
}

#[tokio::test]
async fn a_batch_with_one_event_in_an_unreachable_tenant_enqueues_nothing() {
    let (enforcer, requests) = recording_enforcer(|req| {
        req["resource"]["properties"]["owner_tenant_id"] == json!(UNREACHABLE_TENANT)
            && (is_tenant_check(req) || is_barrier_check(req))
    });
    let harness = harness(enforcer).await;

    let result = harness
        .ingest()
        .publish_batch(
            &caller(),
            vec![
                request(0xe5_01, EVENT_TENANT),
                request(0xe5_02, UNREACHABLE_TENANT),
                request(0xe5_03, EVENT_TENANT),
            ],
        )
        .await;

    assert!(
        matches!(
            &result,
            Err(DomainError::Forbidden {
                code: ErrorCode::TenantIdNotAuthorized,
                ..
            })
        ),
        "expected Forbidden/TenantIdNotAuthorized, got {result:?}"
    );
    assert_eq!(
        recorded(&requests),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": { "owner_tenant_id": EVENT_TENANT },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": EVENT_TYPE,
                    "properties": {
                        "event_type_id": EVENT_TYPE,
                        "owner_tenant_id": EVENT_TENANT,
                        "event_subject_type": SUBJECT_TYPE,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": { "owner_tenant_id": UNREACHABLE_TENANT },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": UNREACHABLE_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce_ignore_barrier" },
                "resource": {
                    "type": EVENT_TYPE,
                    "properties": {
                        "owner_tenant_id": UNREACHABLE_TENANT,
                        "event_type_id": EVENT_TYPE,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": UNREACHABLE_TENANT,
                        "barrier_mode": "ignore",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_eq!(harness.outbox_bodies().await, 0, "nothing is enqueued");
}

#[tokio::test]
async fn an_unavailable_pdp_on_the_tenant_check_is_not_retried_as_the_barrier_check() {
    let requests = Recorded::default();
    let harness = harness(PolicyEnforcer::new(Arc::new(
        UnavailableForTheTenantCheck {
            recorded: Arc::clone(&requests),
        },
    )))
    .await;

    let result = harness
        .ingest()
        .publish_event(&caller(), request(0xe6, EVENT_TENANT))
        .await;

    let Err(DomainError::Internal(message)) = &result else {
        panic!("expected Internal, got {result:?}");
    };
    assert!(
        message.starts_with("authz evaluation failed:"),
        "expected an evaluation failure, got {message:?}"
    );
    assert_eq!(
        recorded(&requests),
        vec![json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "produce" },
            "resource": {
                "type": REQUEST_RESOURCE,
                "properties": { "owner_tenant_id": EVENT_TENANT },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": EVENT_TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": false,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id"],
            },
        })]
    );
    assert_eq!(harness.outbox_bodies().await, 0, "nothing is enqueued");
}

/// An answer the enforcer cannot compile is a broker failure, not a denial: it ends the publish
/// as `Internal` and is not retried as the barrier right (R is the only request).
#[tokio::test]
async fn a_pdp_answer_the_broker_cannot_compile_on_the_tenant_check_is_an_internal_error_not_a_barrier_check()
 {
    let requests = Recorded::default();
    let harness = harness(PolicyEnforcer::new(Arc::new(UncompilableAnswer {
        recorded: Arc::clone(&requests),
        deny: |_| false,
        uncompilable: is_tenant_check,
    })))
    .await;

    let result = harness
        .ingest()
        .publish_event(&caller(), request(0xf1, EVENT_TENANT))
        .await;

    let Err(DomainError::Internal(message)) = &result else {
        panic!("expected Internal, got {result:?}");
    };
    assert!(
        message.starts_with("authz constraint compilation failed:"),
        "expected a compilation failure, got {message:?}"
    );
    assert_eq!(
        recorded(&requests),
        vec![json!({
            "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
            "action": { "name": "produce" },
            "resource": {
                "type": REQUEST_RESOURCE,
                "properties": { "owner_tenant_id": EVENT_TENANT },
            },
            "context": {
                "tenant_context": {
                    "mode": "root_only",
                    "root_id": EVENT_TENANT,
                    "barrier_mode": "respect",
                },
                "token_scopes": [],
                "require_constraints": false,
                "capabilities": [],
                "supported_properties": ["owner_tenant_id"],
            },
        }),]
    );
    assert_eq!(harness.outbox_bodies().await, 0, "nothing is enqueued");
}

/// R is denied and the barrier right (I) answers with a constraint that cannot be compiled: the
/// publish fails as `Internal`, not as `TenantIdNotAuthorized`, and T is not asked.
#[tokio::test]
async fn a_pdp_answer_the_broker_cannot_compile_on_the_barrier_check_is_an_internal_error() {
    let requests = Recorded::default();
    let harness = harness(PolicyEnforcer::new(Arc::new(UncompilableAnswer {
        recorded: Arc::clone(&requests),
        deny: is_tenant_check,
        uncompilable: is_barrier_check,
    })))
    .await;

    let result = harness
        .ingest()
        .publish_event(&caller(), request(0xf2, EVENT_TENANT))
        .await;

    let Err(DomainError::Internal(message)) = &result else {
        panic!("expected Internal, got {result:?}");
    };
    assert!(
        message.starts_with("authz constraint compilation failed:"),
        "expected a compilation failure, got {message:?}"
    );
    assert_eq!(
        recorded(&requests),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": { "owner_tenant_id": EVENT_TENANT },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce_ignore_barrier" },
                "resource": {
                    "type": EVENT_TYPE,
                    "properties": { "owner_tenant_id": EVENT_TENANT, "event_type_id": EVENT_TYPE },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "ignore",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_eq!(harness.outbox_bodies().await, 0, "nothing is enqueued");
}

/// A tenant check (R) whose permitted scope matches nothing is a denial: the
/// publish asks for the barrier right (I), and passes when it is granted.
#[tokio::test]
async fn a_tenant_check_that_matches_nothing_falls_back_to_the_barrier_right() {
    let requests = Recorded::default();
    let harness = harness(PolicyEnforcer::new(Arc::new(
        MatchesNothingOnTheTenantCheck {
            recorded: Arc::clone(&requests),
            deny: |_| false,
        },
    )))
    .await;

    let result = harness
        .ingest()
        .publish_event(&caller(), request(0xf4, EVENT_TENANT))
        .await;

    assert!(
        result.is_ok(),
        "expected the publish to pass, got {result:?}"
    );
    assert_eq!(
        recorded(&requests),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": { "owner_tenant_id": EVENT_TENANT },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce_ignore_barrier" },
                "resource": {
                    "type": EVENT_TYPE,
                    "properties": { "owner_tenant_id": EVENT_TENANT, "event_type_id": EVENT_TYPE },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "ignore",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": EVENT_TYPE,
                    "properties": {
                        "event_type_id": EVENT_TYPE,
                        "owner_tenant_id": EVENT_TENANT,
                        "event_subject_type": SUBJECT_TYPE,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_eq!(harness.outbox_bodies().await, 1, "the event is enqueued");
}

/// A tenant check (R) whose permitted scope matches nothing, and a denied
/// barrier right (I): the publish is refused with `TenantIdNotAuthorized`
/// and T is not asked.
#[tokio::test]
async fn a_tenant_check_that_matches_nothing_without_the_barrier_right_is_refused_with_tenant_id_not_authorized()
 {
    let requests = Recorded::default();
    let harness = harness(PolicyEnforcer::new(Arc::new(
        MatchesNothingOnTheTenantCheck {
            recorded: Arc::clone(&requests),
            deny: is_barrier_check,
        },
    )))
    .await;

    let result = harness
        .ingest()
        .publish_event(&caller(), request(0xf5, EVENT_TENANT))
        .await;

    assert!(
        matches!(
            &result,
            Err(DomainError::Forbidden {
                code: ErrorCode::TenantIdNotAuthorized,
                ..
            })
        ),
        "expected Forbidden/TenantIdNotAuthorized, got {result:?}"
    );
    assert_eq!(
        recorded(&requests),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": { "owner_tenant_id": EVENT_TENANT },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce_ignore_barrier" },
                "resource": {
                    "type": EVENT_TYPE,
                    "properties": { "owner_tenant_id": EVENT_TENANT, "event_type_id": EVENT_TYPE },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "ignore",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_eq!(harness.outbox_bodies().await, 0, "nothing is enqueued");
}

/// The type check (T) answers with a constraint that cannot be compiled: the publish fails as
/// `Internal`, not as `NotAuthorizedToProduce`.
#[tokio::test]
async fn a_pdp_answer_the_broker_cannot_compile_on_the_type_check_is_an_internal_error() {
    let requests = Recorded::default();
    let harness = harness(PolicyEnforcer::new(Arc::new(UncompilableAnswer {
        recorded: Arc::clone(&requests),
        deny: |_| false,
        uncompilable: is_type_check,
    })))
    .await;

    let result = harness
        .ingest()
        .publish_event(&caller(), request(0xf3, EVENT_TENANT))
        .await;

    let Err(DomainError::Internal(message)) = &result else {
        panic!("expected Internal, got {result:?}");
    };
    assert!(
        message.starts_with("authz constraint compilation failed:"),
        "expected a compilation failure, got {message:?}"
    );
    assert_eq!(
        recorded(&requests),
        vec![
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": REQUEST_RESOURCE,
                    "properties": { "owner_tenant_id": EVENT_TENANT },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": EVENT_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
            json!({
                "subject": { "id": SUBJECT, "properties": { "tenant_id": CALLER_TENANT } },
                "action": { "name": "produce" },
                "resource": {
                    "type": EVENT_TYPE,
                    "properties": {
                        "event_type_id": EVENT_TYPE,
                        "owner_tenant_id": EVENT_TENANT,
                        "event_subject_type": SUBJECT_TYPE,
                    },
                },
                "context": {
                    "tenant_context": {
                        "mode": "root_only",
                        "root_id": CALLER_TENANT,
                        "barrier_mode": "respect",
                    },
                    "token_scopes": [],
                    "require_constraints": false,
                    "capabilities": [],
                    "supported_properties": ["owner_tenant_id"],
                },
            }),
        ]
    );
    assert_eq!(harness.outbox_bodies().await, 0, "nothing is enqueued");
}

#[tokio::test]
async fn a_publish_with_a_nil_event_tenant_is_refused_before_any_pdp_call() {
    let (enforcer, requests) = recording_enforcer(|_| false);
    let harness = harness(enforcer).await;

    let result = harness
        .ingest()
        .publish_event(&caller(), request(0xe7, Uuid::nil()))
        .await;

    let Err(DomainError::Validation { code, message }) = &result else {
        panic!("expected Validation, got {result:?}");
    };
    assert_eq!(
        (*code, message.as_str()),
        (
            ErrorCode::InvalidTenant,
            "event 0: the event's tenant must not be the nil UUID"
        )
    );
    assert_eq!(recorded(&requests), Vec::<Value>::new());
    assert_eq!(harness.outbox_bodies().await, 0, "nothing is enqueued");
}

#[tokio::test]
async fn a_sync_publish_with_a_nil_event_tenant_is_refused_before_any_pdp_call() {
    let (enforcer, requests) = recording_enforcer(|_| false);
    let harness = harness(enforcer).await;

    // A zero wait, so an admitted publish answers at once instead of waiting
    // on a backend that never stores.
    let result = harness
        .ingest()
        .publish_event_sync(
            &caller(),
            request(0xe8, Uuid::nil()),
            std::time::Duration::ZERO,
        )
        .await;

    let Err(DomainError::Validation { code, message }) = &result else {
        panic!("expected Validation, got {result:?}");
    };
    assert_eq!(
        (*code, message.as_str()),
        (
            ErrorCode::InvalidTenant,
            "event 0: the event's tenant must not be the nil UUID"
        )
    );
    assert_eq!(recorded(&requests), Vec::<Value>::new());
    assert_eq!(harness.outbox_bodies().await, 0, "nothing is enqueued");
}

#[tokio::test]
async fn a_batch_with_one_nil_event_tenant_is_refused_before_any_pdp_call() {
    let (enforcer, requests) = recording_enforcer(|_| false);
    let harness = harness(enforcer).await;

    let result = harness
        .ingest()
        .publish_batch(
            &caller(),
            vec![
                request(0xe9_01, EVENT_TENANT),
                request(0xe9_02, Uuid::nil()),
                request(0xe9_03, EVENT_TENANT),
            ],
        )
        .await;

    let Err(DomainError::Validation { code, message }) = &result else {
        panic!("expected Validation, got {result:?}");
    };
    assert_eq!(
        (*code, message.as_str()),
        (
            ErrorCode::InvalidTenant,
            "event 1: the event's tenant must not be the nil UUID"
        )
    );
    assert_eq!(recorded(&requests), Vec::<Value>::new());
    assert_eq!(harness.outbox_bodies().await, 0, "nothing is enqueued");
}
