//! The caller-identity guard of the management and point operations, in
//! process (#5494). A caller whose subject and tenant are the nil UUID is
//! refused with `403 TenantIdNotAuthorized` before any read and before any
//! PDP call: a list answers 403 instead of an empty page, and a point
//! operation answers 403 for an id that does not exist. Group creation runs
//! its text checks first, so a malformed request stays a 400 whoever sends
//! it. Each test records every request the PDP sees after its setup, so an
//! empty list proves no PDP call was made. The guard's two operands are pinned
//! for JOIN in `join_guard_tests.rs`; the REST entry points are covered in
//! `api/rest/handlers/delivery/`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use authz_resolver_sdk::{EvaluationRequest, PolicyEnforcer};
use chrono::{TimeZone, Utc};
use gts::GtsIdPattern;
use serde_json::{Value, json};
use toolkit_gts::{GtsInstanceId, gts_id};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::delivery::{JoinRequest, Position, SeekTarget};
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::model::{
    BarrierMode, ConsumerGroup, ConsumerGroupCreateInput, ConsumerGroupKind, Interest,
    OwnershipFilter,
};
use crate::domain::repo::{ConsumerGroupRepo, CursorRepo};
use crate::test_support::{DenyingAuthZ, EventBrokerHarness, StaticTypesRegistry};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~x.eb.mgmtguard.topic.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~x.eb.mgmtguard.foo.v1~");
const NAMED_GROUP: &str =
    "gts.cf.core.events.consumer_group.v1~0190f1c2-7a3b-7c4d-8e5f-6a7b8c9d0e2a";
const UNKNOWN_GROUP: &str =
    "gts.cf.core.events.consumer_group.v1~0190f1c2-7a3b-7c4d-8e5f-6a7b8c9d0e2b";
const UNKNOWN_SUBSCRIPTION: Uuid = Uuid::from_u128(0x1901);
const INTEREST_TENANT: Uuid = Uuid::from_u128(0x1902);

type Recorded = Arc<Mutex<Vec<Value>>>;

/// A harness whose PDP allows every request and records each one.
async fn recording_harness() -> (EventBrokerHarness, Recorded) {
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
    (harness, recorded)
}

/// An anonymous group created by the harness's own caller.
async fn create_group(harness: &EventBrokerHarness) -> GtsInstanceId {
    harness
        .delivery()
        .create_consumer_group(
            harness.security_context(),
            ConsumerGroupCreateInput {
                client_agent: Some("management-guard-test".to_owned()),
                description: None,
            },
        )
        .await
        .expect("the group is created")
        .id
}

/// A subscription the harness's own caller JOINs to `group`, with one
/// interest on the test topic.
async fn join(harness: &EventBrokerHarness, group: &GtsInstanceId) -> Uuid {
    harness
        .delivery()
        .join(
            harness.security_context(),
            JoinRequest {
                consumer_group: group.clone(),
                client_agent: "management-guard-test".to_owned(),
                interests: vec![Interest {
                    topic: GtsInstanceId::try_new(TOPIC).unwrap(),
                    subject_tenant: INTEREST_TENANT,
                    ownership: OwnershipFilter::EntireTenant,
                    barrier_mode: BarrierMode::Respect,
                    types: vec![GtsIdPattern::try_new(EVENT_TYPE).unwrap()],
                    subject_types: Vec::new(),
                    filter: None,
                }],
                session_timeout: Duration::from_secs(30),
            },
        )
        .await
        .expect("the JOIN is allowed")
        .subscription
        .id
}

/// Seeds a named group: the REST and broker surfaces cannot create one.
async fn seed_named_group(harness: &EventBrokerHarness) {
    harness
        .repo()
        .create_consumer_group(ConsumerGroup {
            id: GtsInstanceId::try_new(NAMED_GROUP).unwrap(),
            kind: ConsumerGroupKind::Named,
            tenant_id: Uuid::from_u128(0x1903),
            owner_principal_id: Uuid::from_u128(0x1904),
            description: None,
            created_at: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
        })
        .await
        .expect("seeding a consumer group must not fail");
}

fn nil_caller() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::nil())
        .subject_tenant_id(Uuid::nil())
        .build()
        .unwrap()
}

/// Empties the recorded list, so it holds only what the call under test asks.
fn clear(recorded: &Recorded) {
    recorded.lock().unwrap().clear();
}

fn recorded(recorded: &Recorded) -> Vec<Value> {
    recorded.lock().unwrap().clone()
}

/// The guard's refusal, as the domain returns it.
fn assert_refused_with_no_identity<T: std::fmt::Debug>(result: Result<T, DomainError>) {
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
}

/// The ids of every subscription the coordinator holds.
fn subscription_ids(harness: &EventBrokerHarness) -> Vec<Uuid> {
    harness.groups().list().iter().map(|s| s.id).collect()
}

/// The ids of every stored consumer group, sorted.
async fn group_ids(harness: &EventBrokerHarness) -> Vec<String> {
    let mut ids: Vec<String> = harness
        .repo()
        .list_consumer_groups()
        .await
        .expect("listing the stored groups must not fail")
        .into_iter()
        .map(|group| group.id.to_string())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn a_leave_by_a_caller_with_no_identity_is_refused_and_keeps_the_member() {
    let (harness, pdp) = recording_harness().await;
    let group = create_group(&harness).await;
    let id = join(&harness, &group).await;
    clear(&pdp);

    let result = harness.delivery().leave(&nil_caller(), id).await;

    assert_refused_with_no_identity(result);
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_eq!(subscription_ids(&harness), vec![id]);
}

#[tokio::test]
async fn a_get_of_an_unknown_subscription_by_a_caller_with_no_identity_is_refused_not_missing() {
    let (harness, pdp) = recording_harness().await;

    let result = harness
        .delivery()
        .get_subscription(&nil_caller(), UNKNOWN_SUBSCRIPTION)
        .await;

    assert_refused_with_no_identity(result);
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
}

#[tokio::test]
async fn a_seek_by_a_caller_with_no_identity_is_refused_and_writes_no_cursor() {
    let (harness, pdp) = recording_harness().await;
    let group = create_group(&harness).await;
    let id = join(&harness, &group).await;
    let topic = GtsInstanceId::try_new(TOPIC).unwrap();
    let earliest = || {
        vec![SeekTarget {
            topic: topic.clone(),
            partition: 0,
            value: Position::Earliest,
        }]
    };
    clear(&pdp);

    let result = harness
        .delivery()
        .seek(&nil_caller(), id, 1, earliest())
        .await;

    assert_refused_with_no_identity(result);
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    let cursor = harness
        .repo()
        .find_cursor(&group, &topic, 0)
        .await
        .expect("reading the cursor must not fail");
    assert!(cursor.is_none(), "the refused seek wrote {cursor:?}");
    // The same seek by the member's own caller writes the cursor, so the read
    // above can see one.
    harness
        .delivery()
        .seek(harness.security_context(), id, 1, earliest())
        .await
        .expect("the member's own seek is allowed");
    let cursor = harness
        .repo()
        .find_cursor(&group, &topic, 0)
        .await
        .expect("reading the cursor must not fail");
    assert!(cursor.is_some(), "the allowed seek wrote no cursor");
}

#[tokio::test]
async fn a_list_of_no_subscriptions_by_a_caller_with_no_identity_is_refused() {
    let (harness, pdp) = recording_harness().await;

    let result = harness.delivery().list_subscriptions(&nil_caller()).await;

    assert_refused_with_no_identity(result);
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
}

#[tokio::test]
async fn a_list_of_subscriptions_by_a_caller_with_no_identity_is_refused() {
    let (harness, pdp) = recording_harness().await;
    let group = create_group(&harness).await;
    let id = join(&harness, &group).await;
    clear(&pdp);

    let result = harness.delivery().list_subscriptions(&nil_caller()).await;

    assert_refused_with_no_identity(result);
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_eq!(subscription_ids(&harness), vec![id]);
}

#[tokio::test]
async fn a_consumer_group_create_by_a_caller_with_no_identity_is_refused_and_stores_nothing() {
    let (harness, pdp) = recording_harness().await;

    let result = harness
        .delivery()
        .create_consumer_group(
            &nil_caller(),
            ConsumerGroupCreateInput {
                client_agent: Some("management-guard-test".to_owned()),
                description: Some("a valid description".to_owned()),
            },
        )
        .await;

    assert_refused_with_no_identity(result);
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_eq!(group_ids(&harness).await, Vec::<String>::new());
}

#[tokio::test]
async fn a_malformed_consumer_group_create_by_a_caller_with_no_identity_is_still_a_400() {
    let (harness, pdp) = recording_harness().await;

    let result = harness
        .delivery()
        .create_consumer_group(
            &nil_caller(),
            ConsumerGroupCreateInput {
                client_agent: Some(String::new()),
                description: None,
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
            "must be 1-256 bytes, got 0",
            "field_too_long"
        )
    );
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_eq!(group_ids(&harness).await, Vec::<String>::new());
}

#[tokio::test]
async fn a_get_of_an_unknown_consumer_group_by_a_caller_with_no_identity_is_refused_not_missing() {
    let (harness, pdp) = recording_harness().await;

    let result = harness
        .delivery()
        .get_consumer_group(
            &nil_caller(),
            &GtsInstanceId::try_new(UNKNOWN_GROUP).unwrap(),
        )
        .await;

    assert_refused_with_no_identity(result);
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
}

#[tokio::test]
async fn a_consumer_group_delete_by_a_caller_with_no_identity_is_refused_and_keeps_the_group() {
    let (harness, pdp) = recording_harness().await;
    let group = create_group(&harness).await;
    clear(&pdp);

    let result = harness
        .delivery()
        .delete_consumer_group(&nil_caller(), &group)
        .await;

    assert_refused_with_no_identity(result);
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_eq!(group_ids(&harness).await, vec![group.to_string()]);
}

#[tokio::test]
async fn a_delete_of_an_unknown_consumer_group_by_a_caller_with_no_identity_is_refused_not_missing()
{
    let (harness, pdp) = recording_harness().await;

    let result = harness
        .delivery()
        .delete_consumer_group(
            &nil_caller(),
            &GtsInstanceId::try_new(UNKNOWN_GROUP).unwrap(),
        )
        .await;

    assert_refused_with_no_identity(result);
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
}

#[tokio::test]
async fn a_list_of_no_consumer_groups_by_a_caller_with_no_identity_is_refused() {
    let (harness, pdp) = recording_harness().await;

    let result = harness.delivery().list_consumer_groups(&nil_caller()).await;

    assert_refused_with_no_identity(result);
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
}

#[tokio::test]
async fn a_list_of_named_consumer_groups_by_a_caller_with_no_identity_is_refused() {
    let (harness, pdp) = recording_harness().await;
    seed_named_group(&harness).await;

    let result = harness.delivery().list_consumer_groups(&nil_caller()).await;

    assert_refused_with_no_identity(result);
    assert_eq!(recorded(&pdp), Vec::<Value>::new());
    assert_eq!(group_ids(&harness).await, vec![NAMED_GROUP.to_owned()]);
}
