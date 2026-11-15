//! The checks JOIN runs before anything else, in process (#5494). A
//! `client_agent` outside the RFC 9110 User-Agent grammar is a `400` first,
//! then a zero `session_timeout`, as over REST. Then a caller whose subject or
//! tenant is the nil UUID is refused before the group lookup and before any
//! PDP call, whatever else it carries.
//! Each test records every request the PDP sees after the group exists, so an
//! empty list proves no PDP call was made. The REST entry point is covered in
//! `api/rest/handlers/delivery/subscriptions_tests.rs`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use authz_resolver_sdk::{EvaluationRequest, PolicyEnforcer};
use event_broker_sdk::api::{JoinRequest, SubscriptionInterest};
use event_broker_sdk::models::CreateConsumerGroupRequest;
use event_broker_sdk::{ConsumerGroupId, EventBrokerError, GtsIdPattern, GtsInstanceId};
use serde_json::{Value, json};
use toolkit_canonical_errors::Problem;
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::delivery::JoinRequest as DomainJoinRequest;
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::model::{BarrierMode, Interest, OwnershipFilter};
use crate::test_support::{DenyingAuthZ, EventBrokerHarness, StaticTypesRegistry};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~x.eb.joinguard.topic.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~x.eb.joinguard.foo.v1~");

const SUBJECT: Uuid = Uuid::from_u128(0x1801);
const TENANT: Uuid = Uuid::from_u128(0x1802);

type Recorded = Arc<Mutex<Vec<Value>>>;

/// A harness whose PDP allows every request and records each one, and a group
/// created by the harness's own caller. The group create's PDP call is
/// cleared, so the list holds only what the JOIN under test asks.
async fn harness_with_group() -> (EventBrokerHarness, ConsumerGroupId, Recorded) {
    let recorded = Recorded::default();
    let sink = Arc::clone(&recorded);
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": TOPIC, "partitions": 1 },
        ])))
        .with_policy_enforcer(PolicyEnforcer::new(Arc::new(DenyingAuthZ {
            deny_if: move |req: &EvaluationRequest| {
                sink.lock()
                    .unwrap()
                    .push(serde_json::to_value(req).unwrap());
                false
            },
        })))
        .build()
        .await;
    let group = harness
        .broker()
        .create_consumer_group(
            harness.security_context(),
            CreateConsumerGroupRequest {
                client_agent: "join-guard-test".to_owned(),
                description: None,
            },
        )
        .await
        .expect("the group is created")
        .id;
    recorded.lock().unwrap().clear();
    (harness, group, recorded)
}

/// A one-interest JOIN of `group` that passes every body check.
fn valid_join(group: ConsumerGroupId) -> JoinRequest {
    JoinRequest {
        consumer_group: group,
        client_agent: "join-guard-test".to_owned(),
        interests: vec![
            SubscriptionInterest::builder()
                .topic(GtsInstanceId::try_new(TOPIC).unwrap())
                .subject_tenant(TENANT)
                .ownership_filter(event_broker_sdk::OwnershipFilter::EntireTenant)
                .types([GtsIdPattern::try_new(EVENT_TYPE).unwrap()])
                .build()
                .unwrap(),
        ],
        session_timeout: None,
    }
}

fn recorded(recorded: &Recorded) -> Vec<Value> {
    recorded.lock().unwrap().clone()
}

/// Read after the PDP list is asserted: listing asks the PDP per subscription.
async fn assert_no_subscription(harness: &EventBrokerHarness) {
    assert_eq!(
        harness.api_v1().get_subscriptions().send().await.json(),
        json!({
            "items": [],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}

/// What a REST client and the in-process broker both read back for the
/// guard's `403`: the permission-denied detail, the guard's message riding
/// only in the wire body's `context.reason`.
fn assert_refused_with_no_identity(result: Result<impl std::fmt::Debug, EventBrokerError>) {
    let Err(EventBrokerError::Unauthorized { detail }) = result else {
        panic!("expected Unauthorized, got {result:?}");
    };
    assert_eq!(
        detail,
        "You do not have permission to perform this operation"
    );
}

#[tokio::test]
async fn a_join_by_a_token_only_caller_with_nil_ids_is_refused_in_process() {
    let (harness, group, pdp) = harness_with_group().await;
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::nil())
        .subject_tenant_id(Uuid::nil())
        .bearer_token("a-token-the-broker-does-not-read".to_owned())
        .build()
        .unwrap();

    let result = harness.broker().join(&ctx, valid_join(group)).await;

    assert_refused_with_no_identity(result);
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_no_subscription(&harness).await;
}

#[tokio::test]
async fn a_join_with_a_nil_subject_alone_is_refused() {
    let (harness, group, pdp) = harness_with_group().await;
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::nil())
        .subject_tenant_id(TENANT)
        .build()
        .unwrap();

    let result = harness.broker().join(&ctx, valid_join(group)).await;

    assert_refused_with_no_identity(result);
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_no_subscription(&harness).await;
}

#[tokio::test]
async fn a_join_with_a_nil_tenant_alone_is_refused() {
    let (harness, group, pdp) = harness_with_group().await;
    let ctx = SecurityContext::builder()
        .subject_id(SUBJECT)
        .subject_tenant_id(Uuid::nil())
        .build()
        .unwrap();

    let result = harness.broker().join(&ctx, valid_join(group)).await;

    assert_refused_with_no_identity(result);
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_no_subscription(&harness).await;
}

/// A domain JOIN of `group` with `interests`.
fn domain_join(group: ConsumerGroupId, interests: Vec<Interest>) -> DomainJoinRequest {
    DomainJoinRequest {
        consumer_group: GtsInstanceId::try_new(&group.to_gts()).unwrap(),
        client_agent: "join-guard-test".to_owned(),
        interests,
        session_timeout: Duration::from_secs(30),
    }
}

/// An interest on the test topic in `tenant_id` with `types` distinct selectors.
fn interest(tenant_id: Uuid, types: usize) -> Interest {
    Interest {
        topic: GtsInstanceId::try_new(TOPIC).unwrap(),
        subject_tenant: tenant_id,
        ownership: OwnershipFilter::EntireTenant,
        barrier_mode: BarrierMode::Respect,
        types: (0..types)
            .map(|i| {
                GtsIdPattern::try_new(&format!(
                    "gts.cf.core.events.event.v1~x.eb.joinguard.t{i}.v1~"
                ))
                .unwrap()
            })
            .collect(),
        subject_types: Vec::new(),
        filter: None,
    }
}

#[tokio::test]
async fn the_identity_guard_is_decided_before_the_body_checks() {
    let (harness, group, pdp) = harness_with_group().await;
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::nil())
        .subject_tenant_id(Uuid::nil())
        .build()
        .unwrap();

    let result = harness
        .delivery()
        .join(&ctx, domain_join(group, vec![interest(TENANT, 1); 65]))
        .await;

    let Err(DomainError::Forbidden {
        code,
        message,
        resource,
    }) = result
    else {
        panic!("expected Forbidden, got {result:?}");
    };
    assert_eq!(
        (code, message.as_str(), resource.as_str()),
        (
            ErrorCode::TenantIdNotAuthorized,
            "the caller has no identity: its subject or tenant is the nil UUID",
            "",
        )
    );
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_no_subscription(&harness).await;
}

#[tokio::test]
async fn the_interest_count_is_checked_before_each_interests_contents() {
    let (harness, group, pdp) = harness_with_group().await;
    let mut interests = vec![interest(TENANT, 1); 65];
    interests[0] = interest(Uuid::nil(), 33);

    let result = harness
        .delivery()
        .join(harness.security_context(), domain_join(group, interests))
        .await;

    let Err(DomainError::Validation { code, message }) = result else {
        panic!("expected Validation, got {result:?}");
    };
    assert_eq!(
        (code, message.as_str()),
        (
            ErrorCode::TooManyInterests,
            "interests count 65 exceeds the maximum of 64",
        )
    );
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_no_subscription(&harness).await;
}

#[tokio::test]
async fn an_interests_type_count_is_checked_before_its_tenant() {
    let (harness, group, pdp) = harness_with_group().await;

    let result = harness
        .delivery()
        .join(
            harness.security_context(),
            domain_join(group, vec![interest(Uuid::nil(), 33)]),
        )
        .await;

    let Err(DomainError::Validation { code, message }) = result else {
        panic!("expected Validation, got {result:?}");
    };
    assert_eq!(
        (code, message.as_str()),
        (
            ErrorCode::TooManyTypes,
            "interest 0: types count 33 exceeds the maximum of 32",
        )
    );
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_no_subscription(&harness).await;
}

/// The in-process broker refuses a `client_agent` that breaks the User-Agent
/// grammar (a product version may not be empty) as REST JOIN does, before any
/// PDP call and with nothing stored. The answer crosses the canonical error;
/// the SDK's read-back of it keeps the reason but not the field name or the
/// rule's wording.
#[tokio::test]
async fn an_in_process_join_with_a_client_agent_off_the_user_agent_grammar_is_refused() {
    let (harness, group, pdp) = harness_with_group().await;

    let result = harness
        .broker()
        .join(
            harness.security_context(),
            JoinRequest {
                client_agent: "agent/".to_owned(),
                ..valid_join(group)
            },
        )
        .await;

    let Err(EventBrokerError::InvalidTextField {
        field,
        detail,
        reason,
    }) = result
    else {
        panic!("expected InvalidTextField, got {result:?}");
    };
    assert_eq!(
        (field, detail.as_str(), reason),
        ("", "Request validation failed", "user_agent_grammar")
    );
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_no_subscription(&harness).await;
}

/// A malformed `client_agent` is a `400` whoever sends it: it is judged ahead
/// of the identity guard, so a caller with no identity learns the field fault
/// first, as with a consumer-group create.
#[tokio::test]
async fn a_malformed_client_agent_is_decided_before_the_identity_guard() {
    let (harness, group, pdp) = harness_with_group().await;
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::nil())
        .subject_tenant_id(Uuid::nil())
        .build()
        .unwrap();

    let result = harness
        .delivery()
        .join(
            &ctx,
            DomainJoinRequest {
                client_agent: "agent/".to_owned(),
                ..domain_join(group, vec![interest(TENANT, 1)])
            },
        )
        .await;

    let Err(DomainError::TextField {
        field,
        detail,
        reason,
    }) = result
    else {
        panic!("expected TextField, got {result:?}");
    };
    assert_eq!(
        (field, detail.as_str(), reason),
        (
            "client_agent",
            "must follow the RFC 9110 User-Agent grammar: product *( SP ( product / comment ) )",
            "user_agent_grammar"
        )
    );
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_no_subscription(&harness).await;
}

/// The in-process broker refuses a zero `session_timeout` as REST JOIN does,
/// before any PDP call and with nothing stored: such a member would be reaped
/// at the first sweep after its stream closes. The answer crosses the
/// canonical error as a plain invalid argument with the code in its detail.
#[tokio::test]
async fn an_in_process_join_with_a_zero_session_timeout_is_refused() {
    let (harness, group, pdp) = harness_with_group().await;

    let result = harness
        .broker()
        .join(
            harness.security_context(),
            JoinRequest {
                session_timeout: Some(Duration::ZERO),
                ..valid_join(group)
            },
        )
        .await;

    let Err(EventBrokerError::Other { canonical }) = result else {
        panic!("expected Other, got {result:?}");
    };
    assert_eq!(
        serde_json::to_value(Problem::from(canonical)).unwrap(),
        json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
            "title": "Invalid Argument",
            "status": 400,
            "detail": "InvalidSessionTimeout: session_timeout must be a positive duration",
            "context": {
                "format": "InvalidSessionTimeout: session_timeout must be a positive duration",
                "resource_type": "gts.cf.core.events.request.v1~",
            },
        })
    );
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_no_subscription(&harness).await;
}

/// A zero `session_timeout` is a `400` whoever sends it: it is judged ahead of
/// the identity guard, as REST's handler judges it before the domain runs.
#[tokio::test]
async fn a_zero_session_timeout_is_decided_before_the_identity_guard() {
    let (harness, group, pdp) = harness_with_group().await;
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::nil())
        .subject_tenant_id(Uuid::nil())
        .build()
        .unwrap();

    let result = harness
        .delivery()
        .join(
            &ctx,
            DomainJoinRequest {
                session_timeout: Duration::ZERO,
                ..domain_join(group, vec![interest(TENANT, 1)])
            },
        )
        .await;

    let Err(DomainError::Validation { code, message }) = result else {
        panic!("expected Validation, got {result:?}");
    };
    assert_eq!(
        (code, message.as_str()),
        (
            ErrorCode::InvalidSessionTimeout,
            "session_timeout must be a positive duration",
        )
    );
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_no_subscription(&harness).await;
}

/// REST's check order holds in process too: `client_agent` is judged before
/// `session_timeout`, so a JOIN wrong in both learns the `client_agent` fault.
#[tokio::test]
async fn a_malformed_client_agent_is_decided_before_a_zero_session_timeout() {
    let (harness, group, pdp) = harness_with_group().await;

    let result = harness
        .delivery()
        .join(
            harness.security_context(),
            DomainJoinRequest {
                client_agent: "agent/".to_owned(),
                session_timeout: Duration::ZERO,
                ..domain_join(group, vec![interest(TENANT, 1)])
            },
        )
        .await;

    let Err(DomainError::TextField {
        field,
        detail,
        reason,
    }) = result
    else {
        panic!("expected TextField, got {result:?}");
    };
    assert_eq!(
        (field, detail.as_str(), reason),
        (
            "client_agent",
            "must follow the RFC 9110 User-Agent grammar: product *( SP ( product / comment ) )",
            "user_agent_grammar"
        )
    );
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_no_subscription(&harness).await;
}

/// Any positive `session_timeout` is accepted in process, a sub-second one
/// included: the check refuses only zero, not a value under one second.
#[tokio::test]
async fn an_in_process_join_accepts_a_sub_second_session_timeout() {
    let (harness, group, _pdp) = harness_with_group().await;

    let assignment = harness
        .broker()
        .join(
            harness.security_context(),
            JoinRequest {
                session_timeout: Some(Duration::from_millis(1)),
                ..valid_join(group)
            },
        )
        .await
        .expect("a 1 ms session_timeout is accepted");

    let listed = harness.api_v1().get_subscriptions().send().await;
    listed.assert_status(200);
    let body = listed.json();
    let Some([item]) = body["items"].as_array().map(Vec::as_slice) else {
        panic!("expected exactly one subscription, got {body}");
    };
    assert_eq!(
        body,
        json!({
            "items": [{
                "id": assignment.id.0,
                "consumer_group": group.to_gts(),
                "client_agent": "join-guard-test",
                "interests": [{
                    "topic": TOPIC,
                    "subject_tenant": TENANT,
                    "ownership_filter": "entire_tenant",
                    "barrier_mode": "respect",
                    "types": [EVENT_TYPE],
                    "filter": null,
                }],
                "assigned": [{ "topic": TOPIC, "partition": 0 }],
                "topology_version": 1,
                "created_at": item["created_at"],
            }],
            "page_info": { "next_cursor": null, "prev_cursor": null, "limit": 25 },
        })
    );
}
